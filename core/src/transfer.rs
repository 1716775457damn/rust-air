//! Core transfer engine v4.
//!
//! Header (plaintext, sender → receiver):
//!   [4B MAGIC][32B key][1B kind][2B name_len][name][8B total_size]
//!
//! Resume handshake (receiver → sender):
//!   [8B already_have]
//!
//! Data: AEAD-encrypted chunks, EOF sentinel = 4-byte zero.
//!
//! Checksum (sender → receiver, AFTER EOF sentinel):
//!   [32B sha256]  ← computed on-the-fly while streaming; no double-read.
//!
//! Receiver verifies checksum after all data is written.

use crate::{
    archive,
    crypto::{Decryptor, Encryptor},
    proto::{Kind, TransferEvent, CHUNK, MAGIC, MAX_NAME_LEN},
};
use anyhow::Result;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, sync::Arc, time::Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;
use walkdir;

// ── Send ──────────────────────────────────────────────────────────────────────

/// Send a file or folder. Generates a one-time key and embeds it in the header.
/// SHA-256 is computed on-the-fly and sent after the data stream — no double-read.
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
    let total_size: u64;
    let dir_entries: Option<Vec<(walkdir::DirEntry, std::fs::Metadata)>>;
    if is_dir {
        let (sz, entries) = archive::walk_dir(path);
        total_size = sz;
        dir_entries = Some(entries);
    } else {
        total_size = meta.len();
        dir_entries = None;
    }

    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, &key, kind, &name, total_size).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;
    let resume_offset = u64::from_be_bytes(resume_buf);

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);

    let checksum: [u8; 32] = if is_dir {
        let mut reader = archive::stream_archive_with_entries(path, dir_entries.unwrap())?;
        stream_encrypted_hash(&mut reader, &mut enc, 0, total_size, on_progress, Sha256::new()).await?
    } else {
        let mut f = tokio::fs::File::open(path).await?;
        let mut full_hasher = Sha256::new();
        if resume_offset > 0 {
            // Single file handle: hash the already-sent prefix, then continue from resume_offset.
            let mut buf = vec![0u8; CHUNK];
            let mut remaining = resume_offset;
            while remaining > 0 {
                let to_read = (remaining as usize).min(buf.len());
                let n = f.read(&mut buf[..to_read]).await?;
                if n == 0 { break; }
                full_hasher.update(&buf[..n]);
                remaining -= n as u64;
            }
            // f is now positioned at resume_offset — no seek needed.
        }
        stream_encrypted_hash(&mut f, &mut enc, resume_offset, total_size, on_progress, full_hasher).await?
    };

    // v4 protocol: EOF sentinel first, then SHA-256 checksum.
    enc.shutdown().await?;
    enc.write_trailing(&checksum).await?;
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
    let (key, kind, name, total_size) = recv_header(&mut rx).await?;

    let part_path = dest.join(format!("{name}.part"));

    //断点续传：校验 .part 文件末尾完整性，防止末尾损坏的 chunk 被当作有效数据。
    let already_have: u64 = if kind == Kind::File && part_path.exists() {
        let file_len = tokio::fs::metadata(&part_path).await?.len();
        // Align to CHUNK boundary — only trust fully-received chunks.
        (file_len / CHUNK as u64) * CHUNK as u64
    } else {
        0
    };

    // Truncate to the verified boundary if needed.
    if already_have > 0 {
        let f = tokio::fs::OpenOptions::new().write(true).open(&part_path).await?;
        f.set_len(already_have).await?;
    }

    tx.write_all(&already_have.to_be_bytes()).await?;

    let on_progress = Arc::new(on_progress);

    match kind {
        Kind::File => {
            let file = open_part_file(&part_path, already_have).await?;
            let mut f = BufWriter::with_capacity(4 * 256 * 1024, file); // 1 MB write buffer
            let mut dec = Decryptor::new(&key, rx);
            let mut hasher = Sha256::new();
            let mut done = already_have;
            let start = Instant::now();
            let mut last_emit = start;

            // Resume: pre-hash the bytes we already have so the final digest
            // covers the complete file, matching what the sender computed.
            if already_have > 0 {
                let mut existing = tokio::fs::File::open(&part_path).await?;
                let mut buf = vec![0u8; CHUNK];
                loop {
                    let n = existing.read(&mut buf).await?;
                    if n == 0 { break; }
                    hasher.update(&buf[..n]);
                }
            }

            while let Some(chunk) = dec.read_chunk().await? {
                hasher.update(&chunk);
                f.write_all(&chunk).await?;
                done += chunk.len() as u64;
                if last_emit.elapsed().as_millis() >= 50 {
                    emit_progress(&on_progress, done, total_size, &start, false);
                    last_emit = Instant::now();
                }
            }
            f.flush().await?;
            drop(f);

            let expected_sha = dec.read_trailing().await?;
            if expected_sha != [0u8; 32] {
                let actual: [u8; 32] = hasher.finalize().into();
                if actual != expected_sha {
                    tokio::fs::remove_file(&part_path).await?;
                    anyhow::bail!("SHA-256 mismatch — file corrupted, partial file removed");
                }
            }

            let final_path = dest.join(&name);
            // If destination already exists, append a counter suffix to avoid silent overwrite.
            let final_path = unique_path(final_path);
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
            let mut hasher = Sha256::new();
            let mut done: u64 = 0;
            let start = Instant::now();
            let mut last_emit = start;

            while let Some(chunk) = dec.read_chunk().await? {
                use std::io::Write;
                hasher.update(&chunk);
                sync_w.write_all(&chunk)?;
                done += chunk.len() as u64;
                if last_emit.elapsed().as_millis() >= 50 {
                    emit_progress(&on_progress, done, total_size, &start, false);
                    last_emit = Instant::now();
                }
            }
            drop(sync_w);
            unpack.await??;

            // Verify compressed-stream integrity (sender hashes the same bytes).
            let expected_sha = dec.read_trailing().await?;
            if expected_sha != [0u8; 32] {
                let actual: [u8; 32] = hasher.finalize().into();
                if actual != expected_sha {
                    anyhow::bail!("archive SHA-256 mismatch — stream corrupted");
                }
            }

            emit_progress(&on_progress, done, total_size, &start, true);
            Ok(dest.to_path_buf())
        }

        Kind::Clipboard => {
            let mut dec = Decryptor::new(&key, rx);
            let mut buf = Vec::new();
            while let Some(chunk) = dec.read_chunk().await? {
                buf.extend_from_slice(&chunk);
            }
            let expected_sha = dec.read_trailing().await?;
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
) -> Result<()> {
    let nb = name.as_bytes();
    anyhow::ensure!(nb.len() <= MAX_NAME_LEN, "filename too long ({} bytes)", nb.len());
    // Single write: MAGIC + key + kind + name_len + name + total_size
    let mut hdr = Vec::with_capacity(4 + 32 + 1 + 2 + nb.len() + 8);
    hdr.extend_from_slice(MAGIC);
    hdr.extend_from_slice(key);
    hdr.push(kind as u8);
    hdr.extend_from_slice(&(nb.len() as u16).to_be_bytes());
    hdr.extend_from_slice(nb);
    hdr.extend_from_slice(&total_size.to_be_bytes());
    tx.write_all(&hdr).await?;
    Ok(())
}

async fn recv_header(
    rx: &mut (impl AsyncReadExt + Unpin),
) -> Result<([u8; 32], Kind, String, u64)> {
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

    Ok((key, kind, name, total_size))
}

/// Stream `reader` through `enc`, computing SHA-256 on-the-fly.
/// `hasher` can be pre-seeded for resume scenarios.
/// Returns the digest covering all bytes (pre-seeded + streamed).
async fn stream_encrypted_hash<R: AsyncRead + Unpin>(
    reader: &mut R,
    enc: &mut Encryptor<impl AsyncWriteExt + Unpin>,
    initial: u64,
    total: u64,
    on_progress: Arc<impl Fn(TransferEvent)>,
    hasher: Sha256,
) -> Result<[u8; 32]> {
    let mut buf = vec![0u8; CHUNK];
    let mut hasher = hasher;
    let mut done = initial;
    let start = Instant::now();
    let mut last_emit = start;
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            match reader.read(&mut buf[filled..]).await? {
                0 => break,
                n => filled += n,
            }
        }
        if filled == 0 { break; }
        hasher.update(&buf[..filled]);
        enc.write_chunk(&buf[..filled]).await?;
        done += filled as u64;
        if last_emit.elapsed().as_millis() >= 50 {
            emit_progress(&on_progress, done, total, &start, false);
            last_emit = Instant::now();
        }
    }
    Ok(hasher.finalize().into())
}

// ── Clipboard send ───────────────────────────────────────────────────────────

/// Send clipboard text as `Kind::Clipboard` so the receiver writes it to clipboard.
pub async fn send_clipboard(
    stream: TcpStream,
    text: &str,
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<()> {
    let key = random_key();
    let data = text.as_bytes().to_vec();
    let total_size = data.len() as u64;

    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, &key, Kind::Clipboard, "clipboard", total_size).await?;

    // Consume the resume handshake (always 0 for clipboard).
    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);
    let checksum = {
        let mut hasher = Sha256::new();
        let start = Instant::now();
        for chunk in data.chunks(CHUNK) {
            hasher.update(chunk);
            enc.write_chunk(chunk).await?;
            emit_progress(&on_progress, chunk.len() as u64, total_size, &start, false);
        }
        let digest: [u8; 32] = hasher.finalize().into();
        digest
    };
    enc.shutdown().await?;
    enc.write_trailing(&checksum).await?;
    Ok(())
}

// ── Checksum ──────────────────────────────────────────────────────────────────

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() { return path; }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext  = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir  = path.parent().unwrap_or(std::path::Path::new("."));
    (1u32..).map(|i| dir.join(format!("{stem} ({i}){ext}")))
            .find(|p| !p.exists())
            .unwrap_or(path)
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
