//! Core transfer engine v2.
//!
//! Protocol header (plaintext):
//!   [4B MAGIC][1B kind][2B name_len][name][8B total_size][32B sha256]
//!
//! Resume handshake:
//!   RX ← [8B already_have]   (0 = fresh transfer)
//!
//! Data stream: AEAD-encrypted chunks, terminated by a 4-byte zero sentinel.
//! On completion the receiver verifies SHA-256 of the assembled file.

use crate::{
    archive,
    crypto::{Decryptor, Encryptor},
    proto::{Kind, TransferEvent, CHUNK, MAGIC, MAX_NAME_LEN},
};
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ── Send ──────────────────────────────────────────────────────────────────────

pub async fn send_path(
    stream: TcpStream,
    path: &Path,
    key: &[u8; 32],
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<()> {
    let meta = tokio::fs::metadata(path).await?;
    let is_dir = meta.is_dir();
    let kind = if is_dir { Kind::Archive } else { Kind::File };
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let total_size: u64 = if is_dir { 0 } else { meta.len() };

    // Archives: checksum unknown until fully streamed → send zeroes.
    let checksum: [u8; 32] = if is_dir { [0u8; 32] } else { sha256_file(path).await? };

    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, kind, &name, total_size, &checksum).await?;

    // Read resume offset from receiver.
    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;
    let resume_offset = u64::from_be_bytes(resume_buf);
    if resume_offset > 0 {
        eprintln!("⏩ Resuming from byte {resume_offset}");
    }

    let pb = make_pb(total_size, "Sending  ");
    let mut enc = Encryptor::new(key, tx);
    let on_progress = std::sync::Arc::new(on_progress);

    if is_dir {
        let mut reader = archive::stream_archive(path)?;
        stream_encrypted(&mut reader, &mut enc, &pb, 0, total_size, on_progress).await?;
    } else {
        let mut f = tokio::fs::File::open(path).await?;
        if resume_offset > 0 {
            f.seek(std::io::SeekFrom::Start(resume_offset)).await?;
        }
        stream_encrypted(&mut f, &mut enc, &pb, resume_offset, total_size, on_progress).await?;
    }

    enc.shutdown().await?;
    pb.finish_with_message("Done ✅");
    Ok(())
}

pub async fn send_clipboard(stream: TcpStream, text: String, key: &[u8; 32]) -> Result<()> {
    let bytes = text.into_bytes();
    let checksum = sha256_bytes(&bytes);
    let (mut rx, mut tx) = stream.into_split();

    send_header(&mut tx, Kind::Clipboard, "clipboard", bytes.len() as u64, &checksum).await?;
    // Consume the resume reply (clipboard never resumes).
    rx.read_exact(&mut [0u8; 8]).await?;

    let mut enc = Encryptor::new(key, tx);
    enc.write_chunk(&bytes).await?;
    enc.shutdown().await?;
    Ok(())
}

// ── Receive ───────────────────────────────────────────────────────────────────

pub async fn receive_to_disk(
    stream: TcpStream,
    key: &[u8; 32],
    dest: &Path,
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<PathBuf> {
    let (mut rx, mut tx) = stream.into_split();
    let (kind, name, total_size, expected_sha) = recv_header(&mut rx).await?;

    let part_path = dest.join(format!("{name}.part"));
    let already_have: u64 = if kind == Kind::File && part_path.exists() {
        tokio::fs::metadata(&part_path).await?.len()
    } else {
        0
    };
    tx.write_all(&already_have.to_be_bytes()).await?;
    if already_have > 0 {
        eprintln!("⏩ Resuming from byte {already_have}");
    }

    let pb = make_pb(total_size, "Receiving");
    let on_progress = std::sync::Arc::new(on_progress);

    match kind {
        Kind::File => {
            let mut f = open_part_file(&part_path, already_have).await?;
            pb.set_position(already_have);

            let mut dec = Decryptor::new(key, rx);
            let mut done = already_have;
            let start = Instant::now();

            while let Some(chunk) = dec.read_chunk().await? {
                f.write_all(&chunk).await?;
                done += chunk.len() as u64;
                pb.set_position(done);
                emit_progress(&on_progress, done, total_size, &start, false);
            }
            f.flush().await?;
            drop(f);

            // Integrity check — skip only if sender sent all-zero checksum (archive).
            if expected_sha != [0u8; 32] {
                let actual = sha256_file(&part_path).await?;
                if actual != expected_sha {
                    tokio::fs::remove_file(&part_path).await?;
                    anyhow::bail!("SHA-256 mismatch — file corrupted, partial file removed");
                }
            }

            let final_path = dest.join(&name);
            tokio::fs::rename(&part_path, &final_path).await?;
            pb.finish_with_message("Done ✅");
            emit_progress(&on_progress, done, total_size, &start, true);
            Ok(final_path)
        }

        Kind::Archive => {
            let (pipe_reader, pipe_writer) = os_pipe::pipe()?;
            let dest2 = dest.to_path_buf();
            let unpack = tokio::task::spawn_blocking(move || {
                archive::unpack_archive_sync(pipe_reader, &dest2)
            });

            let mut dec = Decryptor::new(key, rx);
            let mut sync_w: os_pipe::PipeWriter = pipe_writer;
            let mut done: u64 = 0;
            let start = Instant::now();

            while let Some(chunk) = dec.read_chunk().await? {
                use std::io::Write;
                sync_w.write_all(&chunk)?;
                done += chunk.len() as u64;
                pb.inc(chunk.len() as u64);
                emit_progress(&on_progress, done, total_size, &start, false);
            }
            drop(sync_w); // signal EOF to unpack thread
            unpack.await??;

            pb.finish_with_message("Done ✅");
            emit_progress(&on_progress, done, 0, &start, true);
            Ok(dest.to_path_buf())
        }

        Kind::Clipboard => {
            let mut dec = Decryptor::new(key, rx);
            let mut buf = Vec::new();
            while let Some(chunk) = dec.read_chunk().await? {
                buf.extend_from_slice(&chunk);
            }
            if expected_sha != [0u8; 32] && sha256_bytes(&buf) != expected_sha {
                anyhow::bail!("clipboard SHA-256 mismatch");
            }
            // Lossy conversion: non-UTF-8 bytes become U+FFFD.
            crate::clipboard::write(&String::from_utf8_lossy(&buf))?;
            pb.finish_with_message("Done ✅");
            Ok(dest.to_path_buf())
        }
    }
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

async fn send_header(
    tx: &mut (impl AsyncWriteExt + Unpin),
    kind: Kind,
    name: &str,
    total_size: u64,
    checksum: &[u8; 32],
) -> Result<()> {
    let nb = name.as_bytes();
    anyhow::ensure!(nb.len() <= MAX_NAME_LEN, "filename too long ({} bytes)", nb.len());
    tx.write_all(MAGIC).await?;
    tx.write_all(&[kind as u8]).await?;
    tx.write_all(&(nb.len() as u16).to_be_bytes()).await?;
    tx.write_all(nb).await?;
    tx.write_all(&total_size.to_be_bytes()).await?;
    tx.write_all(checksum).await?;
    Ok(())
}

async fn recv_header(
    rx: &mut (impl AsyncReadExt + Unpin),
) -> Result<(Kind, String, u64, [u8; 32])> {
    let mut magic = [0u8; 4];
    rx.read_exact(&mut magic).await?;
    anyhow::ensure!(&magic == MAGIC, "protocol magic mismatch — check versions");

    let mut kind_b = [0u8; 1];
    rx.read_exact(&mut kind_b).await?;
    let kind = Kind::try_from(kind_b[0])?;

    let mut len_b = [0u8; 2];
    rx.read_exact(&mut len_b).await?;
    let name_len = u16::from_be_bytes(len_b) as usize;
    // Guard against memory exhaustion from a malformed/malicious header.
    anyhow::ensure!(name_len <= MAX_NAME_LEN, "filename length {name_len} exceeds limit");

    let mut name_b = vec![0u8; name_len];
    rx.read_exact(&mut name_b).await?;
    let raw_name = String::from_utf8(name_b)?;

    // Security: strip any directory components to prevent path traversal.
    let name = Path::new(&raw_name)
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid filename: '{raw_name}'"))?
        .to_string_lossy()
        .into_owned();
    anyhow::ensure!(
        !name.contains(['\0', '/', '\\']),
        "filename contains illegal characters: '{name}'"
    );

    let mut size_b = [0u8; 8];
    rx.read_exact(&mut size_b).await?;
    let total_size = u64::from_be_bytes(size_b);

    let mut sha = [0u8; 32];
    rx.read_exact(&mut sha).await?;

    Ok((kind, name, total_size, sha))
}

async fn stream_encrypted<R: AsyncRead + Unpin>(
    reader: &mut R,
    enc: &mut Encryptor<impl AsyncWriteExt + Unpin>,
    pb: &ProgressBar,
    initial: u64,
    total: u64,
    on_progress: std::sync::Arc<impl Fn(TransferEvent)>,
) -> Result<()> {
    let mut buf = vec![0u8; CHUNK];
    let mut done = initial;
    let start = Instant::now();
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 { break; }
        enc.write_chunk(&buf[..n]).await?;
        done += n as u64;
        pb.set_position(done);
        emit_progress(&on_progress, done, total, &start, false);
    }
    Ok(())
}

// ── Checksum ──────────────────────────────────────────────────────────────────

/// Stream-hash a file in CHUNK-sized reads — O(1) memory regardless of file size.
pub async fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let file = tokio::fs::File::open(path).await?;
    let mut reader = tokio::io::BufReader::with_capacity(CHUNK, file);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Open (or create/append) the `.part` file for a resumable receive.
async fn open_part_file(path: &Path, already_have: u64) -> Result<tokio::fs::File> {
    if already_have > 0 {
        Ok(tokio::fs::OpenOptions::new().append(true).open(path).await?)
    } else {
        Ok(tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await?)
    }
}

/// Emit a `TransferEvent`, computing bytes/sec from elapsed time.
#[inline]
fn emit_progress(
    cb: &Arc<impl Fn(TransferEvent)>,
    bytes_done: u64,
    total_bytes: u64,
    start: &Instant,
    done: bool,
) {
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    cb(TransferEvent {
        bytes_done,
        total_bytes,
        bytes_per_sec: (bytes_done as f64 / elapsed) as u64,
        done,
        error: None,
    });
}

// ── Progress bar ──────────────────────────────────────────────────────────────

fn make_pb(total: u64, prefix: &str) -> ProgressBar {
    let pb = if total > 0 { ProgressBar::new(total) } else { ProgressBar::new_spinner() };
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{prefix} [{bar:42.cyan/blue}] {bytes:>10}/{total_bytes:<10} \
                 {bytes_per_sec:>12}  ETA {eta}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("█▓░"),
    );
    pb.set_prefix(prefix.to_string());
    pb
}
