//! Core transfer engine v3.
//!
//! Header (plaintext, sender → receiver):
//!   [4B MAGIC][32B key][1B kind][2B name_len][name][8B total_size][32B sha256]
//!
//! Resume handshake (receiver → sender):
//!   [8B already_have]
//!
//! Data: AEAD-encrypted chunks, EOF sentinel = 4-byte zero.

use crate::{
    archive,
    crypto::{Decryptor, Encryptor},
    proto::{Kind, TransferEvent, CHUNK, MAGIC, MAX_NAME_LEN},
};
use anyhow::Result;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, sync::Arc, time::Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ── Send ──────────────────────────────────────────────────────────────────────

/// Send a file or folder. Generates a one-time key and embeds it in the header.
pub async fn send_path(
    stream: TcpStream,
    path: &Path,
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<()> {
    let key = random_key();
    let meta = tokio::fs::metadata(path).await?;
    let is_dir = meta.is_dir();
    let kind = if is_dir { Kind::Archive } else { Kind::File };
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let total_size: u64 = if is_dir { 0 } else { meta.len() };
    let checksum: [u8; 32] = if is_dir { [0u8; 32] } else { sha256_file(path).await? };

    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, &key, kind, &name, total_size, &checksum).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;
    let resume_offset = u64::from_be_bytes(resume_buf);

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);

    if is_dir {
        let mut reader = archive::stream_archive(path)?;
        stream_encrypted(&mut reader, &mut enc, 0, total_size, on_progress).await?;
    } else {
        let mut f = tokio::fs::File::open(path).await?;
        if resume_offset > 0 {
            f.seek(std::io::SeekFrom::Start(resume_offset)).await?;
        }
        stream_encrypted(&mut f, &mut enc, resume_offset, total_size, on_progress).await?;
    }

    enc.shutdown().await?;
    Ok(())
}

// ── Receive ───────────────────────────────────────────────────────────────────

/// Receive a file/folder. Reads the one-time key from the header.
pub async fn receive_to_disk(
    stream: TcpStream,
    dest: &Path,
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<PathBuf> {
    let (mut rx, mut tx) = stream.into_split();
    let (key, kind, name, total_size, expected_sha) = recv_header(&mut rx).await?;

    let part_path = dest.join(format!("{name}.part"));
    let already_have: u64 = if kind == Kind::File && part_path.exists() {
        tokio::fs::metadata(&part_path).await?.len()
    } else {
        0
    };
    tx.write_all(&already_have.to_be_bytes()).await?;

    let on_progress = Arc::new(on_progress);

    match kind {
        Kind::File => {
            let mut f = open_part_file(&part_path, already_have).await?;
            let mut dec = Decryptor::new(&key, rx);
            let mut done = already_have;
            let start = Instant::now();

            while let Some(chunk) = dec.read_chunk().await? {
                f.write_all(&chunk).await?;
                done += chunk.len() as u64;
                emit_progress(&on_progress, done, total_size, &start, false);
            }
            f.flush().await?;
            drop(f);

            if expected_sha != [0u8; 32] {
                let actual = sha256_file(&part_path).await?;
                if actual != expected_sha {
                    tokio::fs::remove_file(&part_path).await?;
                    anyhow::bail!("SHA-256 mismatch — file corrupted, partial file removed");
                }
            }

            let final_path = dest.join(&name);
            tokio::fs::rename(&part_path, &final_path).await?;
            emit_progress(&on_progress, done, total_size, &start, true);
            Ok(final_path)
        }

        Kind::Archive => {
            let (pipe_reader, pipe_writer) = os_pipe::pipe()?;
            let dest2 = dest.to_path_buf();
            let unpack = tokio::task::spawn_blocking(move || {
                archive::unpack_archive_sync(pipe_reader, &dest2)
            });

            let mut dec = Decryptor::new(&key, rx);
            let mut sync_w = pipe_writer;
            let mut done: u64 = 0;
            let start = Instant::now();

            while let Some(chunk) = dec.read_chunk().await? {
                use std::io::Write;
                sync_w.write_all(&chunk)?;
                done += chunk.len() as u64;
                emit_progress(&on_progress, done, total_size, &start, false);
            }
            drop(sync_w);
            unpack.await??;

            emit_progress(&on_progress, done, 0, &start, true);
            Ok(dest.to_path_buf())
        }

        Kind::Clipboard => {
            let mut dec = Decryptor::new(&key, rx);
            let mut buf = Vec::new();
            while let Some(chunk) = dec.read_chunk().await? {
                buf.extend_from_slice(&chunk);
            }
            if expected_sha != [0u8; 32] && sha256_bytes(&buf) != expected_sha {
                anyhow::bail!("clipboard SHA-256 mismatch");
            }
            crate::clipboard::write(&String::from_utf8_lossy(&buf))?;
            Ok(dest.to_path_buf())
        }
    }
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

async fn send_header(
    tx: &mut (impl AsyncWriteExt + Unpin),
    key: &[u8; 32],
    kind: Kind,
    name: &str,
    total_size: u64,
    checksum: &[u8; 32],
) -> Result<()> {
    let nb = name.as_bytes();
    anyhow::ensure!(nb.len() <= MAX_NAME_LEN, "filename too long ({} bytes)", nb.len());
    tx.write_all(MAGIC).await?;
    tx.write_all(key).await?;
    tx.write_all(&[kind as u8]).await?;
    tx.write_all(&(nb.len() as u16).to_be_bytes()).await?;
    tx.write_all(nb).await?;
    tx.write_all(&total_size.to_be_bytes()).await?;
    tx.write_all(checksum).await?;
    Ok(())
}

async fn recv_header(
    rx: &mut (impl AsyncReadExt + Unpin),
) -> Result<([u8; 32], Kind, String, u64, [u8; 32])> {
    let mut magic = [0u8; 4];
    rx.read_exact(&mut magic).await?;
    anyhow::ensure!(&magic == MAGIC, "protocol magic mismatch — check versions");

    let mut key = [0u8; 32];
    rx.read_exact(&mut key).await?;

    let mut kind_b = [0u8; 1];
    rx.read_exact(&mut kind_b).await?;
    let kind = Kind::try_from(kind_b[0])?;

    let mut len_b = [0u8; 2];
    rx.read_exact(&mut len_b).await?;
    let name_len = u16::from_be_bytes(len_b) as usize;
    anyhow::ensure!(name_len <= MAX_NAME_LEN, "filename length {name_len} exceeds limit");

    let mut name_b = vec![0u8; name_len];
    rx.read_exact(&mut name_b).await?;
    let raw_name = String::from_utf8(name_b)?;
    let name = Path::new(&raw_name)
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid filename: '{raw_name}'"))?
        .to_string_lossy()
        .into_owned();
    anyhow::ensure!(!name.contains(['\0', '/', '\\']), "illegal filename: '{name}'");

    let mut size_b = [0u8; 8];
    rx.read_exact(&mut size_b).await?;
    let total_size = u64::from_be_bytes(size_b);

    let mut sha = [0u8; 32];
    rx.read_exact(&mut sha).await?;

    Ok((key, kind, name, total_size, sha))
}

async fn stream_encrypted<R: AsyncRead + Unpin>(
    reader: &mut R,
    enc: &mut Encryptor<impl AsyncWriteExt + Unpin>,
    initial: u64,
    total: u64,
    on_progress: Arc<impl Fn(TransferEvent)>,
) -> Result<()> {
    let mut buf = vec![0u8; CHUNK];
    let mut done = initial;
    let start = Instant::now();
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 { break; }
        enc.write_chunk(&buf[..n]).await?;
        done += n as u64;
        emit_progress(&on_progress, done, total, &start, false);
    }
    Ok(())
}

// ── Checksum ──────────────────────────────────────────────────────────────────

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

fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

async fn open_part_file(path: &Path, already_have: u64) -> Result<tokio::fs::File> {
    if already_have > 0 {
        Ok(tokio::fs::OpenOptions::new().append(true).open(path).await?)
    } else {
        Ok(tokio::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path).await?)
    }
}

#[inline]
fn emit_progress(cb: &Arc<impl Fn(TransferEvent)>, bytes_done: u64, total_bytes: u64, start: &Instant, done: bool) {
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    cb(TransferEvent {
        bytes_done,
        total_bytes,
        bytes_per_sec: (bytes_done as f64 / elapsed) as u64,
        done,
        error: None,
    });
}
