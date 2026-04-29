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
    proto::{Kind, ReconnectInfo, SessionManifest, TransferEvent, CHUNK, MAGIC, MAX_NAME_LEN},
};
use anyhow::Result;
#[cfg(feature = "desktop")]
use arboard::Clipboard;
use rand::RngCore;
use sha2::{Digest, Sha256};
use socket2::SockRef;
use std::{net::SocketAddr, path::{Path, PathBuf}, sync::Arc, time::Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use walkdir::DirEntry as WalkDirEntry;

/// Maximum number of reconnect attempts.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;
/// Initial retry delay in seconds (doubles each attempt: 2, 4, 8, 16, 32).
const INITIAL_RETRY_DELAY_SECS: u64 = 2;

// ── Manifest helpers ──────────────────────────────────────────────────────────

/// Return the manifest file path: `{dest}/{name}.manifest.json`.
fn manifest_path(dest: &Path, name: &str) -> PathBuf {
    dest.join(format!("{name}.manifest.json"))
}

/// Serialize `manifest` to JSON and write it to `path`.
async fn write_manifest(path: &Path, manifest: &SessionManifest) -> Result<()> {
    let json = serde_json::to_string_pretty(manifest)?;
    tokio::fs::write(path, json.as_bytes()).await?;
    Ok(())
}

/// Read and deserialize a `SessionManifest` from `path`.
/// Returns `None` if the file doesn't exist or can't be parsed.
async fn read_manifest(path: &Path) -> Option<SessionManifest> {
    let data = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&data).ok()
}

/// Delete the manifest file at `path`, ignoring not-found errors.
async fn remove_manifest(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

// ── Receive outcome ───────────────────────────────────────────────────────────

/// Extended result from `receive_to_disk`. For file/archive transfers, contains
/// the output path. For clipboard transfers, also carries the header `name` and
/// raw decrypted bytes so the caller can post-process (EchoGuard, history, etc.).
pub enum ReceiveOutcome {
    /// File or archive saved to disk.
    File(PathBuf),
    /// Clipboard data received and written to system clipboard.
    /// `name` is the header name field (e.g. "clip:text:DEVICE"), `data` is the
    /// raw decrypted payload bytes.
    Clipboard { path: PathBuf, name: String, data: Vec<u8> },
}

impl ReceiveOutcome {
    /// Return the output path regardless of variant.
    pub fn path(&self) -> &Path {
        match self {
            ReceiveOutcome::File(p) => p,
            ReceiveOutcome::Clipboard { path, .. } => path,
        }
    }
}

// ── Send ──────────────────────────────────────────────────────────────────────

/// Send a file or folder. Generates a one-time key and embeds it in the header.
/// SHA-256 is computed on-the-fly and sent after the data stream — no double-read.
pub async fn send_path(
    stream: TcpStream,
    path: &Path,
    on_progress: impl Fn(TransferEvent) + Send + Sync + 'static,
) -> Result<()> {
    let key = random_key();
    let meta = tokio::fs::metadata(path).await?;
    let is_dir = meta.is_dir();
    let kind = if is_dir { Kind::Archive } else { Kind::File };
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let total_size: u64;
    let dir_entries: Option<Vec<(WalkDirEntry, std::fs::Metadata)>>;
    if is_dir {
        let (sz, entries) = archive::walk_dir(path);
        total_size = sz;
        dir_entries = Some(entries);
    } else {
        total_size = meta.len();
        dir_entries = None;
    }

    tune_socket(&stream);
    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, &key, kind, &name, total_size).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;
    let resume_offset = u64::from_be_bytes(resume_buf);

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);

    let checksum: [u8; 32] = if is_dir {
        let mut reader = archive::stream_archive_with_entries(path, dir_entries.unwrap())?;
        if resume_offset > 0 {
            // Archive resume: regenerate the archive stream but skip the first
            // resume_offset bytes (read and discard). The archive stream is not
            // seekable (tar+zstd), so we must read through the prefix.
            let mut remaining = resume_offset;
            let mut skip_buf = vec![0u8; CHUNK];
            while remaining > 0 {
                let to_read = (remaining as usize).min(skip_buf.len());
                let mut filled = 0;
                while filled < to_read {
                    match reader.read(&mut skip_buf[filled..to_read]).await? {
                        0 => break,
                        n => filled += n,
                    }
                }
                if filled == 0 { break; }
                remaining -= filled as u64;
            }
            // Set encryptor counter to align nonces with the original stream.
            enc.set_counter(resume_offset / CHUNK as u64);
        }
        stream_encrypted_hash(&mut reader, &mut enc, resume_offset, total_size, on_progress, Sha256::new()).await?
    } else {
        let mut f = tokio::fs::File::open(path).await?;
        let mut full_hasher = Sha256::new();
        if resume_offset > 0 {
            // Nonce alignment: set encryptor counter to match the frame position.
            enc.set_counter(resume_offset / CHUNK as u64);
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
        stream_encrypted_hash_pipeline(f, &mut enc, resume_offset, total_size, on_progress, full_hasher).await?
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
) -> Result<ReceiveOutcome> {
    tune_socket(&stream);
    let (mut rx, mut tx) = stream.into_split();
    let (key, kind, name, total_size) = recv_header(&mut rx).await?;

    let part_path = dest.join(format!("{name}.part"));
    let mpath = manifest_path(dest, &name);

    // ── Resume decision: check .part + manifest ──────────────────────────
    let already_have: u64 = if kind == Kind::File || kind == Kind::Archive {
        if part_path.exists() {
            if let Some(m) = read_manifest(&mpath).await {
                if m.name == name && m.total_size == total_size && m.kind == kind {
                    // Valid manifest matches — resume from chunk-aligned boundary.
                    let file_len = tokio::fs::metadata(&part_path).await?.len();
                    (file_len / CHUNK as u64) * CHUNK as u64
                } else {
                    // Manifest mismatch — start fresh.
                    let _ = tokio::fs::remove_file(&part_path).await;
                    remove_manifest(&mpath).await;
                    0
                }
            } else {
                // No valid manifest — start fresh.
                let _ = tokio::fs::remove_file(&part_path).await;
                remove_manifest(&mpath).await;
                0
            }
        } else {
            // No .part file — fresh transfer.
            remove_manifest(&mpath).await;
            0
        }
    } else {
        0
    };

    tx.write_all(&already_have.to_be_bytes()).await?;

    // ── Create session manifest (File / Archive only) ────────────────────
    if kind == Kind::File || kind == Kind::Archive {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let manifest = SessionManifest {
            name: name.clone(),
            total_size,
            kind,
            sender_addr: String::new(),
            created_at: now,
        };
        if let Err(e) = write_manifest(&mpath, &manifest).await {
            eprintln!("warn: failed to write manifest: {e}");
        }
    }

    let on_progress = Arc::new(on_progress);
    let is_resumed = already_have > 0;

    match kind {
        Kind::File => {
            let result = receive_file_branch(
                &key, rx, &part_path, &name, dest, total_size, already_have,
                is_resumed, &on_progress,
            ).await;

            match &result {
                Ok(_) => remove_manifest(&mpath).await,
                Err(_) => { /* preserve manifest + .part for future resume */ }
            }
            result
        }

        Kind::Archive => {
            let result = receive_archive_branch(
                &key, rx, &part_path, &name, dest, total_size, already_have,
                is_resumed, &on_progress,
            ).await;

            match &result {
                Ok(_) => remove_manifest(&mpath).await,
                Err(_) => { /* preserve manifest + .part for future resume */ }
            }
            result
        }

        Kind::Clipboard => {
            let mut dec = Decryptor::new(&key, rx);
            // Pre-allocate if total_size is known to avoid repeated realloc
            let mut buf = if total_size > 0 {
                Vec::with_capacity(total_size as usize)
            } else {
                Vec::new()
            };
            while let Some(chunk) = dec.read_chunk().await? {
                buf.extend_from_slice(&chunk);
            }
            let expected_sha = dec.read_trailing().await?;
            if expected_sha != [0u8; 32] && sha256_bytes(&buf) != expected_sha {
                anyhow::bail!("clipboard SHA-256 mismatch — data corrupted, discarding");
            }

            // Write to system clipboard (desktop only)
            #[cfg(feature = "desktop")]
            {
                if name.starts_with("clip:image:") {
                    let cursor = std::io::Cursor::new(&buf);
                    let decoder = image::codecs::png::PngDecoder::new(cursor)
                        .map_err(|e| anyhow::anyhow!("PNG decode failed: {e}"))?;
                    use image::ImageDecoder;
                    let (w, h) = decoder.dimensions();
                    let mut rgba = vec![0u8; decoder.total_bytes() as usize];
                    decoder.read_image(&mut rgba)
                        .map_err(|e| anyhow::anyhow!("PNG read failed: {e}"))?;
                    let img_data = arboard::ImageData {
                        width: w as usize,
                        height: h as usize,
                        bytes: std::borrow::Cow::Owned(rgba),
                    };
                    Clipboard::new()?.set_image(img_data)?;
                } else {
                    crate::clipboard::write(&String::from_utf8_lossy(&buf))?;
                }
            }

            // Return Clipboard variant with name + raw data for caller post-processing
            Ok(ReceiveOutcome::Clipboard { path: dest.to_path_buf(), name, data: buf })
        }
    }
}

// ── Receive with auto-reconnect ───────────────────────────────────────────────

/// Compute exponential backoff delay for reconnect attempt `n` (1-based).
/// Returns `2^n` seconds.
pub fn reconnect_delay_secs(n: u32) -> u64 {
    INITIAL_RETRY_DELAY_SECS.checked_shl(n.saturating_sub(1)).unwrap_or(32)
}

/// Receive a file/folder with automatic reconnection on TCP failure.
///
/// When the connection drops mid-transfer, this function retries up to
/// `MAX_RECONNECT_ATTEMPTS` times with exponential backoff (2s, 4s, 8s, 16s, 32s).
/// Each reconnect leverages the existing `.part` file and manifest for resume.
///
/// If `initial_stream` is provided, it is used for the first attempt (e.g. from
/// an accepted listener connection). On failure, subsequent attempts connect to `addr`.
///
/// The `cancel_token` allows the caller to abort all reconnect attempts immediately.
pub async fn receive_with_reconnect(
    addr: SocketAddr,
    dest: &Path,
    cancel_token: CancellationToken,
    on_progress: impl Fn(TransferEvent) + Send + Sync + 'static,
    initial_stream: Option<TcpStream>,
) -> Result<ReceiveOutcome> {
    let on_progress = Arc::new(on_progress);

    // First attempt: use initial_stream if provided, otherwise connect.
    let first_stream = match initial_stream {
        Some(s) => s,
        None => TcpStream::connect(addr).await?,
    };
    let cb = on_progress.clone();
    match receive_to_disk(first_stream, dest, move |ev| cb(ev)).await {
        Ok(outcome) => return Ok(outcome),
        Err(first_err) => {
            eprintln!("transfer failed, will attempt reconnect: {first_err}");
        }
    }

    // Reconnect loop with exponential backoff.
    for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
        let delay = reconnect_delay_secs(attempt);

        // Report reconnect status.
        on_progress(TransferEvent {
            bytes_done: 0,
            total_bytes: 0,
            bytes_per_sec: 0,
            done: false,
            error: None,
            resumed: false,
            resume_offset: 0,
            reconnect_info: Some(ReconnectInfo {
                attempt,
                max_attempts: MAX_RECONNECT_ATTEMPTS,
            }),
        });

        // Wait with cancellation support.
        tokio::select! {
            _ = cancel_token.cancelled() => {
                anyhow::bail!("transfer cancelled by user during reconnect");
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
        }

        // Check cancellation before attempting connect.
        if cancel_token.is_cancelled() {
            anyhow::bail!("transfer cancelled by user during reconnect");
        }

        // Attempt reconnect.
        let stream = match TcpStream::connect(addr).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("reconnect attempt {attempt}/{MAX_RECONNECT_ATTEMPTS} failed: {e}");
                if attempt == MAX_RECONNECT_ATTEMPTS {
                    // All retries exhausted — report error, preserve .part + manifest.
                    on_progress(TransferEvent {
                        bytes_done: 0,
                        total_bytes: 0,
                        bytes_per_sec: 0,
                        done: false,
                        error: Some(format!(
                            "all {MAX_RECONNECT_ATTEMPTS} reconnect attempts failed: {e}"
                        )),
                        resumed: false,
                        resume_offset: 0,
                        reconnect_info: None,
                    });
                    anyhow::bail!(
                        "all {MAX_RECONNECT_ATTEMPTS} reconnect attempts failed: {e}"
                    );
                }
                continue;
            }
        };

        // Reconnect succeeded — receive_to_disk will auto-resume via .part + manifest.
        let cb = on_progress.clone();
        match receive_to_disk(stream, dest, move |ev| cb(ev)).await {
            Ok(outcome) => return Ok(outcome),
            Err(e) => {
                eprintln!("transfer failed after reconnect attempt {attempt}: {e}");
                if attempt == MAX_RECONNECT_ATTEMPTS {
                    on_progress(TransferEvent {
                        bytes_done: 0,
                        total_bytes: 0,
                        bytes_per_sec: 0,
                        done: false,
                        error: Some(format!(
                            "all {MAX_RECONNECT_ATTEMPTS} reconnect attempts failed: {e}"
                        )),
                        resumed: false,
                        resume_offset: 0,
                        reconnect_info: None,
                    });
                    anyhow::bail!(
                        "all {MAX_RECONNECT_ATTEMPTS} reconnect attempts failed: {e}"
                    );
                }
            }
        }
    }

    unreachable!("reconnect loop should have returned or bailed")
}

// ── Receive branch helpers ─────────────────────────────────────────────────────

/// File receive branch with manifest validation and nonce alignment.
async fn receive_file_branch(
    key: &[u8; 32],
    rx: impl AsyncReadExt + Unpin,
    part_path: &Path,
    name: &str,
    dest: &Path,
    total_size: u64,
    already_have: u64,
    is_resumed: bool,
    on_progress: &Arc<impl Fn(TransferEvent)>,
) -> Result<ReceiveOutcome> {
    // Single file handle: open read+write, truncate to resume boundary,
    // hash the existing prefix in spawn_blocking, then seek to end and append.
    let file = if already_have > 0 {
        let f = tokio::fs::OpenOptions::new()
            .read(true).write(true).open(part_path).await?;
        f.set_len(already_have).await?;
        f
    } else {
        tokio::fs::OpenOptions::new()
            .create(true).read(true).write(true).truncate(true)
            .open(part_path).await?
    };

    // Hash existing prefix using the already-open file handle.
    let mut hasher = if already_have > 0 {
        let part2 = part_path.to_path_buf();
        let already = already_have;
        tokio::task::spawn_blocking(move || -> anyhow::Result<Sha256> {
            use std::io::{Read, Seek};
            let mut f = std::fs::File::open(&part2)?;
            let mut h = Sha256::new();
            let mut buf = vec![0u8; CHUNK];
            let mut remaining = already;
            while remaining > 0 {
                let to_read = (remaining as usize).min(buf.len());
                let n = f.read(&mut buf[..to_read])?;
                if n == 0 { break; }
                h.update(&buf[..n]);
                remaining -= n as u64;
            }
            f.seek(std::io::SeekFrom::Start(0))?;
            Ok(h)
        }).await??
    } else {
        Sha256::new()
    };

    // Seek to end for appending, wrap in BufWriter.
    let mut file = file;
    file.seek(std::io::SeekFrom::End(0)).await?;
    let f = BufWriter::with_capacity(4 * CHUNK, file);

    // Pipeline: decrypt + hash in main task, disk writes in spawned task.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    let write_task = tokio::spawn(async move {
        let mut f = f;
        while let Some(chunk) = write_rx.recv().await {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        Ok::<_, anyhow::Error>(())
    });

    let mut dec = Decryptor::new(key, rx);
    // Nonce alignment: set counter to match the frame position for resumed data.
    if already_have > 0 {
        dec.set_counter(already_have / CHUNK as u64);
    }
    let mut done = already_have;
    let start = Instant::now();
    let mut last_emit = start;

    while let Some(chunk) = dec.read_chunk().await? {
        hasher.update(&chunk);
        done += chunk.len() as u64;
        write_tx.send(chunk).await
            .map_err(|_| anyhow::anyhow!("write task failed"))?;
        if last_emit.elapsed().as_millis() >= 100 {
            emit_progress_resume(on_progress, done, total_size, &start, false, is_resumed, already_have);
            last_emit = Instant::now();
        }
    }
    drop(write_tx);
    write_task.await??;

    let expected_sha = dec.read_trailing().await?;
    if expected_sha != [0u8; 32] {
        let actual: [u8; 32] = hasher.finalize().into();
        if actual != expected_sha {
            tokio::fs::remove_file(part_path).await?;
            anyhow::bail!("SHA-256 mismatch — file corrupted, partial file removed");
        }
    }

    let final_path = unique_path(dest.join(name));
    tokio::fs::rename(part_path, &final_path).await?;
    emit_progress_resume(on_progress, done, total_size, &start, true, is_resumed, already_have);
    Ok(ReceiveOutcome::File(final_path))
}

/// Archive receive branch with resume support.
/// When resuming, data is written to a .part file first, then decompressed after
/// the complete archive stream is received. Fresh transfers also use .part to
/// enable future resume on interruption.
async fn receive_archive_branch(
    key: &[u8; 32],
    rx: impl AsyncReadExt + Unpin,
    part_path: &Path,
    _name: &str,
    dest: &Path,
    total_size: u64,
    already_have: u64,
    is_resumed: bool,
    on_progress: &Arc<impl Fn(TransferEvent)>,
) -> Result<ReceiveOutcome> {
    // Open .part file: append if resuming, create fresh otherwise.
    let part_file = if already_have > 0 {
        let f = tokio::fs::OpenOptions::new()
            .write(true).open(part_path).await?;
        f.set_len(already_have).await?;
        let mut f = f;
        f.seek(std::io::SeekFrom::End(0)).await?;
        f
    } else {
        tokio::fs::OpenOptions::new()
            .create(true).write(true).truncate(true)
            .open(part_path).await?
    };

    let f = BufWriter::with_capacity(4 * CHUNK, part_file);

    // Pipeline: decrypt in main task, disk writes in spawned task.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    let write_task = tokio::spawn(async move {
        let mut f = f;
        while let Some(chunk) = write_rx.recv().await {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        Ok::<_, anyhow::Error>(())
    });

    let mut dec = Decryptor::new(key, rx);
    // Nonce alignment for resume.
    if already_have > 0 {
        dec.set_counter(already_have / CHUNK as u64);
    }

    // Hash the already-received prefix from the .part file for full-stream checksum.
    let mut hasher = if already_have > 0 {
        let part2 = part_path.to_path_buf();
        let already = already_have;
        tokio::task::spawn_blocking(move || -> anyhow::Result<Sha256> {
            use std::io::Read;
            let mut f = std::fs::File::open(&part2)?;
            let mut h = Sha256::new();
            let mut buf = vec![0u8; CHUNK];
            let mut remaining = already;
            while remaining > 0 {
                let to_read = (remaining as usize).min(buf.len());
                let n = f.read(&mut buf[..to_read])?;
                if n == 0 { break; }
                h.update(&buf[..n]);
                remaining -= n as u64;
            }
            Ok(h)
        }).await??
    } else {
        Sha256::new()
    };

    let mut done: u64 = already_have;
    let start = Instant::now();
    let mut last_emit = start;

    while let Some(chunk) = dec.read_chunk().await? {
        hasher.update(&chunk);
        done += chunk.len() as u64;
        write_tx.send(chunk).await
            .map_err(|_| anyhow::anyhow!("write task failed"))?;
        if last_emit.elapsed().as_millis() >= 100 {
            emit_progress_resume(on_progress, done, total_size, &start, false, is_resumed, already_have);
            last_emit = Instant::now();
        }
    }
    drop(write_tx);
    write_task.await??;

    // Verify checksum over the complete archive stream.
    let expected_sha = dec.read_trailing().await?;
    if expected_sha != [0u8; 32] {
        let actual: [u8; 32] = hasher.finalize().into();
        if actual != expected_sha {
            anyhow::bail!("archive SHA-256 mismatch — stream corrupted");
        }
    }

    // Decompress the complete .part file into the destination directory.
    let dest2 = dest.to_path_buf();
    let part2 = part_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&part2)?;
        let reader = std::io::BufReader::new(f);
        archive::unpack_archive_sync(reader, &dest2)
    }).await??;

    // Clean up the .part file after successful decompression.
    let _ = tokio::fs::remove_file(part_path).await;

    emit_progress_resume(on_progress, total_size, total_size, &start, true, is_resumed, already_have);
    Ok(ReceiveOutcome::File(dest.to_path_buf()))
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
/// Fills the buffer with `read` (not read_exact) to handle streams that
/// return short reads at EOF without erroring.
async fn stream_encrypted_hash<R: AsyncRead + Unpin>(
    reader: &mut R,
    enc: &mut Encryptor<impl AsyncWriteExt + Unpin>,
    initial: u64,
    total: u64,
    on_progress: Arc<impl Fn(TransferEvent)>,
    mut hasher: Sha256,
) -> Result<[u8; 32]> {
    let mut buf = vec![0u8; CHUNK];
    let mut done = initial;
    let start = Instant::now();
    let mut last_emit = start;
    loop {
        // Fill the buffer as much as possible in one pass.
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
        if last_emit.elapsed().as_millis() >= 100 {
            emit_progress(&on_progress, done, total, &start, false);
            last_emit = Instant::now();
        }
    }
    Ok(hasher.finalize().into())
}

/// Stream `reader` through `enc` using a pipeline: file reading runs in a
/// separate spawned task, communicating chunks via a bounded channel to the
/// encrypt+hash task. This overlaps I/O with encryption for higher throughput.
async fn stream_encrypted_hash_pipeline<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    enc: &mut Encryptor<impl AsyncWriteExt + Unpin>,
    initial: u64,
    total: u64,
    on_progress: Arc<impl Fn(TransferEvent) + Send + Sync>,
    mut hasher: Sha256,
) -> Result<[u8; 32]> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>>>(2);

    // Read task: independently spawned, fills CHUNK-sized buffers and sends them.
    let read_task = tokio::spawn(async move {
        let mut reader = reader;
        loop {
            let mut buf = vec![0u8; CHUNK];
            let mut filled = 0;
            while filled < buf.len() {
                match reader.read(&mut buf[filled..]).await {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) => {
                        let _ = tx.send(Err(e.into())).await;
                        return;
                    }
                }
            }
            if filled == 0 {
                break;
            }
            buf.truncate(filled);
            if tx.send(Ok(buf)).await.is_err() {
                break;
            }
        }
    });

    // Encrypt task: receives from channel, hashes sequentially, then encrypts.
    let mut done = initial;
    let start = Instant::now();
    let mut last_emit = start;
    while let Some(result) = rx.recv().await {
        let chunk = result?;
        hasher.update(&chunk);
        enc.write_chunk(&chunk).await?;
        done += chunk.len() as u64;
        if last_emit.elapsed().as_millis() >= 100 {
            emit_progress(&on_progress, done, total, &start, false);
            last_emit = Instant::now();
        }
    }

    // Capture read task panic.
    read_task
        .await
        .map_err(|e| anyhow::anyhow!("read task panicked: {e}"))?;

    Ok(hasher.finalize().into())
}

// ── Clipboard send ────────────────────────────────────────────────────────────

/// Send clipboard text as `Kind::Clipboard` so the receiver writes it to clipboard.
/// The `name` field is sent in the header — use `clip:text:DEVICE_NAME` for sync,
/// or `"clipboard"` for the legacy CLI send-clip command.
#[cfg(feature = "desktop")]
pub async fn send_clipboard(
    stream: TcpStream,
    text: &str,
    name: &str,
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<()> {
    let key = random_key();
    // Borrow bytes directly — no clone of the clipboard content.
    let data = text.as_bytes();
    let total_size = data.len() as u64;

    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, &key, Kind::Clipboard, name, total_size).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let start = Instant::now();
    for chunk in data.chunks(CHUNK) {
        hasher.update(chunk);
        enc.write_chunk(chunk).await?;
        done += chunk.len() as u64;
        emit_progress(&on_progress, done, total_size, &start, false);
    }
    let checksum: [u8; 32] = hasher.finalize().into();
    enc.shutdown().await?;
    enc.write_trailing(&checksum).await?;
    Ok(())
}

/// Send clipboard image (PNG-encoded bytes) as `Kind::Clipboard`.
/// The name field is set to `clip:image:DEVICE_NAME` so the receiver knows
/// to decode PNG → RGBA and write it to the system clipboard.
#[cfg(feature = "desktop")]
pub async fn send_clipboard_image(
    stream: TcpStream,
    png_data: &[u8],
    name: &str,
    on_progress: impl Fn(TransferEvent) + Send + 'static,
) -> Result<()> {
    let key = random_key();
    let total_size = png_data.len() as u64;

    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, &key, Kind::Clipboard, name, total_size).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let start = Instant::now();
    for chunk in png_data.chunks(CHUNK) {
        hasher.update(chunk);
        enc.write_chunk(chunk).await?;
        done += chunk.len() as u64;
        emit_progress(&on_progress, done, total_size, &start, false);
    }
    let checksum: [u8; 32] = hasher.finalize().into();
    enc.shutdown().await?;
    enc.write_trailing(&checksum).await?;
    Ok(())
}

// ── Checksum ──────────────────────────────────────────────────────────────────

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Find a non-colliding path using existence checks — no temp file creation.
fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() { return path; }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext  = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir  = path.parent().unwrap_or(std::path::Path::new("."));
    for i in 1u32.. {
        let candidate = dir.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() { return candidate; }
    }
    path
}

fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

/// Tune TCP socket for high-throughput transfers: disable Nagle, enlarge buffers.
/// Failures are non-fatal — we just warn and continue with OS defaults.
fn tune_socket(stream: &TcpStream) {
    let sock = SockRef::from(stream);
    if let Err(e) = stream.set_nodelay(true) {
        eprintln!("warn: TCP_NODELAY failed: {e}");
    }
    let buf_size = 2 * 1024 * 1024; // 2MB
    if let Err(e) = sock.set_send_buffer_size(buf_size) {
        eprintln!("warn: SO_SNDBUF failed: {e}");
    }
    if let Err(e) = sock.set_recv_buffer_size(buf_size) {
        eprintln!("warn: SO_RCVBUF failed: {e}");
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
        resumed: false,
        resume_offset: 0,
        reconnect_info: None,
    });
}

#[inline]
fn emit_progress_resume(
    cb: &Arc<impl Fn(TransferEvent)>,
    bytes_done: u64,
    total_bytes: u64,
    start: &Instant,
    done: bool,
    resumed: bool,
    resume_offset: u64,
) {
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    cb(TransferEvent {
        bytes_done,
        total_bytes,
        bytes_per_sec: (bytes_done as f64 / elapsed) as u64,
        done,
        error: None,
        resumed,
        resume_offset,
        reconnect_info: None,
    });
}
