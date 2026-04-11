/// Core transfer engine v2.
///
/// Protocol header (plaintext, before encryption):
///   [4B MAGIC][1B kind][2B name_len][name][8B total_size][32B sha256_of_full_file]
///
/// Resume handshake:
///   RX ← [8B already_have]
///
/// Data: AEAD-encrypted chunks (see crypto.rs), then 4-byte zero sentinel.
///
/// On completion the receiver verifies SHA-256 of the assembled file.

use crate::{
    archive,
    crypto::{Decryptor, Encryptor},
    proto::{Kind, TransferEvent, CHUNK, MAGIC},
};
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ── Send ─────────────────────────────────────────────────────────────────────

pub async fn send_path(
    stream: TcpStream,
    path: &Path,
    key: &[u8; 32],
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    let is_dir = meta.is_dir();
    let kind = if is_dir { Kind::Archive } else { Kind::File };
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let total_size: u64 = if is_dir { 0 } else { meta.len() };

    // SHA-256: for files compute upfront; for archives use zeroes (unknown until streamed)
    let checksum: [u8; 32] = if is_dir {
        [0u8; 32]
    } else {
        sha256_file(path).await?
    };

    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, kind, &name, total_size, &checksum).await?;

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
    let total = bytes.len() as u64;
    let checksum = sha256_bytes(&bytes);
    let (mut rx, mut tx) = stream.into_split();

    send_header(&mut tx, Kind::Clipboard, "clipboard", total, &checksum).await?;
    let mut _skip = [0u8; 8];
    rx.read_exact(&mut _skip).await?;

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
            let mut f = if already_have > 0 {
                tokio::fs::OpenOptions::new().append(true).open(&part_path).await?
            } else {
                tokio::fs::OpenOptions::new()
                    .create(true).write(true).truncate(true)
                    .open(&part_path).await?
            };
            pb.set_position(already_have);

            let mut dec = Decryptor::new(key, rx);
            let mut done = already_have;
            let start = Instant::now();
            while let Some(chunk) = dec.read_chunk().await? {
                f.write_all(&chunk).await?;
                done += chunk.len() as u64;
                pb.set_position(done);
                on_progress(TransferEvent {
                    bytes_done: done,
                    total_bytes: total_size,
                    bytes_per_sec: (done as f64 / start.elapsed().as_secs_f64()) as u64,
                    done: false,
                    error: None,
                });
            }
            f.flush().await?;
            drop(f);

            // SHA-256 verification
            if expected_sha != [0u8; 32] {
                let actual = sha256_file(&part_path).await?;
                if actual != expected_sha {
                    tokio::fs::remove_file(&part_path).await?;
                    anyhow::bail!("SHA-256 mismatch — file corrupted");
                }
            }

            let final_path = dest.join(&name);
            tokio::fs::rename(&part_path, &final_path).await?;
            pb.finish_with_message("Done ✅");
            on_progress(TransferEvent {
                bytes_done: done, total_bytes: total_size,
                bytes_per_sec: 0, done: true, error: None,
            });
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
                on_progress(TransferEvent {
                    bytes_done: done, total_bytes: total_size,
                    bytes_per_sec: (done as f64 / start.elapsed().as_secs_f64()) as u64,
                    done: false, error: None,
                });
            }
            drop(sync_w);
            unpack.await??;
            pb.finish_with_message("Done ✅");
            on_progress(TransferEvent {
                bytes_done: done, total_bytes: 0,
                bytes_per_sec: 0, done: true, error: None,
            });
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
            let text = String::from_utf8_lossy(&buf).to_string();
            crate::clipboard::write(&text)?;
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
    anyhow::ensure!(&magic == MAGIC, "protocol magic mismatch (wrong version?)");

    let mut kind_b = [0u8; 1];
    rx.read_exact(&mut kind_b).await?;
    let kind = Kind::try_from(kind_b[0])?;

    let mut len_b = [0u8; 2];
    rx.read_exact(&mut len_b).await?;
    let mut name_b = vec![0u8; u16::from_be_bytes(len_b) as usize];
    rx.read_exact(&mut name_b).await?;
    let name = String::from_utf8(name_b)?;

    // Security: reject path traversal attempts (e.g. "../../../etc/passwd")
    let safe_name = Path::new(&name)
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid filename: '{name}'"))?
        .to_string_lossy()
        .to_string();
    anyhow::ensure!(
        !safe_name.contains(['/', '\\', '\0']),
        "filename contains illegal characters: '{safe_name}'"
    );
    let name = safe_name;

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
        on_progress(TransferEvent {
            bytes_done: done, total_bytes: total,
            bytes_per_sec: (done as f64 / start.elapsed().as_secs_f64().max(0.001)) as u64,
            done: false, error: None,
        });
    }
    Ok(())
}

// ── Checksum helpers ──────────────────────────────────────────────────────────

/// Stream-hash a file in 64 KB chunks — O(1) memory regardless of file size.
pub async fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    use tokio::io::AsyncReadExt;
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
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
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
