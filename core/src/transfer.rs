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
    proto::{ArchiveSnapshot, ArchiveStatus, ArchiveStatusCode, Kind, ReconnectInfo, SessionManifest, TransferEvent, CHUNK, MAGIC, MAX_NAME_LEN},
};
use anyhow::Result;
#[cfg(feature = "desktop")]
use arboard::Clipboard;
use rand::RngCore;
use sha2::{Digest, Sha256};
use socket2::SockRef;
use std::{path::{Path, PathBuf}, sync::Arc, time::{Duration, Instant}};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use walkdir::DirEntry as WalkDirEntry;

/// Maximum number of reconnect attempts.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;
/// Initial retry delay in seconds (doubles each attempt: 2, 4, 8, 16, 32).
const INITIAL_RETRY_DELAY_SECS: u64 = 2;
const SPEED_SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const SPEED_SMOOTHING_FACTOR: f64 = 0.25;

const ARCHIVE_RESUME_DISABLED_REASON: &str =
    "archive resume disabled for safety; restarting directory transfer from zero";

#[derive(Debug, Clone)]
struct SpeedTracker {
    started_at: Instant,
    last_sample_at: Instant,
    last_sample_bytes: u64,
    smoothed_bps: f64,
}

impl SpeedTracker {
    fn new(now: Instant, initial_bytes: u64) -> Self {
        Self {
            started_at: now,
            last_sample_at: now,
            last_sample_bytes: initial_bytes,
            smoothed_bps: 0.0,
        }
    }

    fn should_emit(&self, now: Instant, done: bool) -> bool {
        done || now.duration_since(self.last_sample_at) >= SPEED_SAMPLE_INTERVAL
    }

    fn sample(&mut self, now: Instant, bytes_done: u64) -> u64 {
        let elapsed = now.duration_since(self.last_sample_at).as_secs_f64();
        if elapsed > 0.0 {
            let delta = bytes_done.saturating_sub(self.last_sample_bytes) as f64;
            let instant_bps = delta / elapsed;
            self.smoothed_bps = if self.smoothed_bps > 0.0 {
                self.smoothed_bps * (1.0 - SPEED_SMOOTHING_FACTOR)
                    + instant_bps * SPEED_SMOOTHING_FACTOR
            } else {
                instant_bps
            };
        }

        self.last_sample_at = now;
        self.last_sample_bytes = bytes_done;

        // Fallback to average speed if we don't yet have enough samples.
        if self.smoothed_bps <= 0.0 {
            let total_elapsed = now.duration_since(self.started_at).as_secs_f64().max(0.001);
            self.smoothed_bps = bytes_done as f64 / total_elapsed;
        }

        self.smoothed_bps.max(0.0).round() as u64
    }
}

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

fn archive_status(code: ArchiveStatusCode, detail: impl Into<String>) -> ArchiveStatus {
    ArchiveStatus {
        code,
        detail: Some(detail.into()),
    }
}

// ── Receive outcome ───────────────────────────────────────────────────────────

/// Extended result from `receive_to_disk`. For file/archive transfers, contains
/// the output path. For clipboard transfers, also carries the header `name` and
/// raw decrypted bytes so the caller can post-process (EchoGuard, history, etc.).
pub enum ReceiveOutcome {
    /// File or archive saved to disk.
    File { path: PathBuf, name: String, kind: Kind },
    /// Clipboard data received and written to system clipboard.
    /// `name` is the header name field (e.g. "clip:text:DEVICE"), `data` is the
    /// raw decrypted payload bytes.
    Clipboard { path: PathBuf, name: String, data: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendOutcome {
    pub logical_name: String,
    pub total_size: u64,
    pub checksum_hex: String,
}

impl ReceiveOutcome {
    /// Return the output path regardless of variant.
    pub fn path(&self) -> &Path {
        match self {
            ReceiveOutcome::File { path, .. } => path,
            ReceiveOutcome::Clipboard { path, .. } => path,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            ReceiveOutcome::File { name, .. } => Some(name.as_str()),
            ReceiveOutcome::Clipboard { .. } => None,
        }
    }

    pub fn kind(&self) -> Option<Kind> {
        match self {
            ReceiveOutcome::File { kind, .. } => Some(*kind),
            ReceiveOutcome::Clipboard { .. } => None,
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
    send_path_as(stream, path, None, on_progress).await
}

/// Send a file or folder using an explicit logical name in the transfer header.
/// This is used by higher-level protocols such as bidirectional sync where the
/// destination-relative path must be preserved exactly.
pub async fn send_path_as(
    stream: TcpStream,
    path: &Path,
    logical_name: Option<&str>,
    on_progress: impl Fn(TransferEvent) + Send + Sync + 'static,
) -> Result<()> {
    send_path_as_with_outcome(stream, path, logical_name, on_progress).await.map(|_| ())
}

fn encode_archive_snapshot(snapshot: &ArchiveSnapshot) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(snapshot)?)
}

fn decode_archive_snapshot(data: &[u8]) -> Result<ArchiveSnapshot> {
    Ok(serde_json::from_slice(data)?)
}

fn archive_snapshots_match(
    existing: Option<&ArchiveSnapshot>,
    current: Option<&ArchiveSnapshot>,
) -> bool {
    match (existing, current) {
        (Some(left), Some(right)) => {
            left.algorithm == right.algorithm && left.fingerprint == right.fingerprint
        }
        _ => false,
    }
}

pub async fn send_path_as_with_outcome(
    stream: TcpStream,
    path: &Path,
    logical_name: Option<&str>,
    on_progress: impl Fn(TransferEvent) + Send + Sync + 'static,
) -> Result<SendOutcome> {
    let key = random_key();
    let meta = tokio::fs::metadata(path).await?;
    let is_dir = meta.is_dir();
    let kind = if is_dir { Kind::Archive } else { Kind::File };
    let name = logical_name
        .map(str::to_string)
        .unwrap_or_else(|| path.file_name().unwrap_or_default().to_string_lossy().into_owned());
    let total_size: u64;
    let dir_entries: Option<Vec<(WalkDirEntry, std::fs::Metadata)>>;
    let archive_snapshot: Option<ArchiveSnapshot>;
    if is_dir {
        let (sz, entries) = archive::walk_dir_checked(path)?;
        total_size = sz;
        archive_snapshot = Some(archive::build_archive_snapshot(path, &entries)?);
        dir_entries = Some(entries);
    } else {
        total_size = meta.len();
        archive_snapshot = None;
        dir_entries = None;
    }

    tune_socket(&stream);
    let (mut rx, mut tx) = stream.into_split();
    send_header(&mut tx, &key, kind, &name, total_size, archive_snapshot.as_ref()).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;
    let resume_offset = u64::from_be_bytes(resume_buf);

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);

    let checksum: [u8; 32] = if is_dir {
        // Use parallel archive generation for directories with many files.
        // Heuristic: use parallel if >= 10 files (typical small-file scenario benefits from parallelism).
        // For smaller directories or resume scenarios, use the sequential path.
        let entries = dir_entries
            .ok_or_else(|| anyhow::anyhow!("directory transfer missing precomputed entries"))?;
        let file_count = entries.iter().filter(|(e, _)| e.file_type().is_file()).count();
        let use_parallel = file_count >= 10 && resume_offset == 0;
        if !use_parallel && file_count >= 10 {
            on_progress(TransferEvent {
                bytes_done: 0,
                total_bytes: total_size,
                bytes_per_sec: 0,
                done: false,
                error: None,
                resumed: resume_offset > 0,
                resume_offset,
                reconnect_info: None,
                archive_status: Some(archive_status(
                    ArchiveStatusCode::ParallelDisabledForResume,
                    if resume_offset > 0 {
                        "parallel archive disabled because matched directory resume uses the sequential archive path"
                    } else {
                        "parallel archive disabled because transfer is using the sequential archive path"
                    },
                )),
            });
        }
        
        let mut reader: Box<dyn AsyncRead + Send + Unpin> = if use_parallel {
            Box::new(archive::stream_archive_parallel(path, entries)?)
        } else {
            Box::new(archive::stream_archive_with_entries(path, entries)?)
        };
        
        let mut archive_hasher = Sha256::new();
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
                archive_hasher.update(&skip_buf[..filled]);
                remaining -= filled as u64;
            }
            // Set encryptor counter to align nonces with the original stream.
            enc.set_counter(resume_offset / CHUNK as u64);
        }
        stream_encrypted_hash(reader, &mut enc, resume_offset, total_size, on_progress, archive_hasher).await?
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
    Ok(SendOutcome {
        logical_name: name,
        total_size,
        checksum_hex: hex::encode(checksum),
    })
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
    let (key, kind, name, total_size, archive_snapshot) = recv_header(&mut rx).await?;

    let part_path = dest.join(format!("{name}.part"));
    let mpath = manifest_path(dest, &name);

    // ── Resume decision: check .part + manifest ──────────────────────────
    let mut archive_restart_reason: Option<String> = None;
    let already_have: u64 = if kind == Kind::File || kind == Kind::Archive {
        if part_path.exists() {
            if let Some(m) = read_manifest(&mpath).await {
                if m.name == name && m.total_size == total_size && m.kind == kind {
                    if kind == Kind::Archive {
                        if archive_snapshots_match(m.archive_snapshot.as_ref(), archive_snapshot.as_ref()) {
                            let file_len = tokio::fs::metadata(&part_path).await?.len();
                            (file_len / CHUNK as u64) * CHUNK as u64
                        } else {
                            archive_restart_reason = Some(
                                if m.archive_snapshot.is_none() {
                                    "archive partial data had no snapshot fingerprint; restarting directory transfer from zero".to_string()
                                } else {
                                    "archive snapshot changed; restarting directory transfer from zero".to_string()
                                }
                            );
                            let _ = tokio::fs::remove_file(&part_path).await;
                            remove_manifest(&mpath).await;
                            0
                        }
                    } else {
                        // Valid manifest matches — resume from chunk-aligned boundary.
                        let file_len = tokio::fs::metadata(&part_path).await?.len();
                        (file_len / CHUNK as u64) * CHUNK as u64
                    }
                } else {
                    // Manifest mismatch — start fresh.
                    if kind == Kind::Archive {
                        archive_restart_reason = Some(
                            "archive partial data did not match current manifest; restarting directory transfer from zero"
                                .to_string(),
                        );
                    }
                    let _ = tokio::fs::remove_file(&part_path).await;
                    remove_manifest(&mpath).await;
                    0
                }
            } else {
                // No valid manifest — start fresh.
                if kind == Kind::Archive {
                    archive_restart_reason = Some(
                        "archive partial data had no valid manifest; restarting directory transfer from zero"
                            .to_string(),
                    );
                }
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
            archive_snapshot: archive_snapshot.clone(),
        };
        if let Err(e) = write_manifest(&mpath, &manifest).await {
            eprintln!("warn: failed to write manifest: {e}");
        }
    }

    let on_progress = Arc::new(on_progress);
    let is_resumed = already_have > 0;

    if kind == Kind::Archive && archive_restart_reason.is_some() {
        on_progress(TransferEvent {
            bytes_done: 0,
            total_bytes: total_size,
            bytes_per_sec: 0,
            done: false,
            error: None,
            resumed: false,
            resume_offset: 0,
            reconnect_info: None,
            archive_status: Some(archive_status(
                ArchiveStatusCode::ResumeRejectedSafetyRestart,
                archive_restart_reason.unwrap_or_else(|| ARCHIVE_RESUME_DISABLED_REASON.to_string()),
            )),
        });
    }

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
                if name.starts_with("wb:") || name.starts_with("sync:") {
                    // Whiteboard sync messages reuse the clipboard transport but must not
                    // overwrite the user's real system clipboard.
                } else if name.starts_with("clip:image:") {
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

// ── Reconnect helpers ─────────────────────────────────────────────────────────

/// Compute exponential backoff delay for reconnect attempt `n` (1-based).
/// Returns `2^n` seconds.
pub fn reconnect_delay_secs(n: u32) -> u64 {
    INITIAL_RETRY_DELAY_SECS.checked_shl(n.saturating_sub(1)).unwrap_or(32)
}

// ── Send with retry ───────────────────────────────────────────────────────────

/// Send a file or folder with automatic retry on failure.
///
/// First attempt: `TcpStream::connect(addr)` → `send_path(stream, path, on_progress)`.
/// On failure: retry up to `MAX_RECONNECT_ATTEMPTS` times with exponential backoff
/// (2s, 4s, 8s, 16s, 32s). The receiver will auto-resume via `.part` + manifest.
///
/// The `cancel_token` allows the caller to abort retries during backoff waits.
pub async fn send_path_with_retry(
    addr: &str,
    path: &Path,
    on_progress: impl Fn(TransferEvent) + Send + Sync + 'static,
    cancel_token: CancellationToken,
) -> Result<()> {
    let on_progress = Arc::new(on_progress);

    // First attempt.
    let stream = TcpStream::connect(addr).await?;
    let cb = on_progress.clone();
    match send_path(stream, path, move |ev| cb(ev)).await {
        Ok(()) => return Ok(()),
        Err(first_err) => {
            eprintln!("send failed, will attempt retry: {first_err}");
        }
    }

    // Retry loop with exponential backoff.
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
            archive_status: None,
        });

        // Wait with cancellation support.
        tokio::select! {
            _ = cancel_token.cancelled() => {
                anyhow::bail!("transfer cancelled by user during retry");
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
        }

        // Check cancellation before attempting connect.
        if cancel_token.is_cancelled() {
            anyhow::bail!("transfer cancelled by user during retry");
        }

        // Attempt reconnect to receiver's listener port.
        let stream = match TcpStream::connect(addr).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("retry attempt {attempt}/{MAX_RECONNECT_ATTEMPTS} connect failed: {e}");
                if attempt == MAX_RECONNECT_ATTEMPTS {
                    on_progress(TransferEvent {
                        bytes_done: 0,
                        total_bytes: 0,
                        bytes_per_sec: 0,
                        done: false,
                        error: Some(format!(
                            "all {MAX_RECONNECT_ATTEMPTS} retry attempts failed: {e}"
                        )),
                        resumed: false,
                        resume_offset: 0,
                        reconnect_info: None,
                        archive_status: None,
                    });
                    anyhow::bail!(
                        "all {MAX_RECONNECT_ATTEMPTS} retry attempts failed: {e}"
                    );
                }
                continue;
            }
        };

        // Reconnect succeeded — send_path will re-send; receiver auto-resumes via .part + manifest.
        let cb = on_progress.clone();
        match send_path(stream, path, move |ev| cb(ev)).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("send failed after retry attempt {attempt}: {e}");
                if attempt == MAX_RECONNECT_ATTEMPTS {
                    on_progress(TransferEvent {
                        bytes_done: 0,
                        total_bytes: 0,
                        bytes_per_sec: 0,
                        done: false,
                        error: Some(format!(
                            "all {MAX_RECONNECT_ATTEMPTS} retry attempts failed: {e}"
                        )),
                        resumed: false,
                        resume_offset: 0,
                        reconnect_info: None,
                        archive_status: None,
                    });
                    anyhow::bail!(
                        "all {MAX_RECONNECT_ATTEMPTS} retry attempts failed: {e}"
                    );
                }
            }
        }
    }

    unreachable!("retry loop should have returned or bailed")
}

// ── Receive branch helpers ─────────────────────────────────────────────────────

/// File receive branch with manifest validation and nonce alignment.
///
/// 3-stage pipeline:
///   Stage 1 (main task): Network read + decrypt via `Decryptor.read_chunk()` + progress
///   Stage 2 (spawned):   Hash computation — updates SHA-256, forwards chunks to write
///   Stage 3 (spawned):   Disk write — writes chunks to .part file
#[allow(clippy::too_many_arguments)]
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
    let prefix_hasher = if already_have > 0 {
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

    // Stage 3 (spawned): Disk write — receives chunks and writes to file.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(crate::proto::PIPELINE_DEPTH);
    let write_task = tokio::spawn(async move {
        let mut f = f;
        while let Some(chunk) = write_rx.recv().await {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        Ok::<_, anyhow::Error>(())
    });

    // Stage 2 (spawned): Hash computation — receives chunks, updates SHA-256,
    // forwards to write task. Returns the final hasher for checksum verification.
    let (hash_tx, mut hash_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(crate::proto::PIPELINE_DEPTH);
    let hash_task = tokio::spawn(async move {
        let mut hasher = prefix_hasher;
        while let Some(chunk) = hash_rx.recv().await {
            hasher.update(&chunk);
            write_tx.send(chunk).await
                .map_err(|_| anyhow::anyhow!("write task failed"))?;
        }
        // Drop write_tx so write_task sees channel close.
        Ok::<_, anyhow::Error>(hasher)
    });

    // Stage 1 (main task): Network read + decrypt + progress reporting.
    let mut dec = Decryptor::new(key, rx);
    // Nonce alignment: set counter to match the frame position for resumed data.
    if already_have > 0 {
        dec.set_counter(already_have / CHUNK as u64);
    }
    let mut done = already_have;
    let start = Instant::now();
    let mut speed = SpeedTracker::new(start, already_have);

    while let Some(chunk) = dec.read_chunk().await? {
        done += chunk.len() as u64;
        hash_tx.send(chunk).await
            .map_err(|_| anyhow::anyhow!("hash task failed"))?;
        let now = Instant::now();
        if speed.should_emit(now, false) {
            emit_progress_resume(on_progress, done, total_size, false, is_resumed, already_have, &mut speed, now);
        }
    }
    // Close hash channel so Stage 2 finishes, which closes write channel for Stage 3.
    drop(hash_tx);

    // Await both pipeline stages.
    let hasher = hash_task.await??;
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
    let now = Instant::now();
    emit_progress_resume(on_progress, done, total_size, true, is_resumed, already_have, &mut speed, now);
    Ok(ReceiveOutcome::File { path: final_path, name: name.to_string(), kind: Kind::File })
}

/// Archive receive branch with resume support.
/// When resuming, data is written to a .part file first, then decompressed after
/// the complete archive stream is received. Fresh transfers also use .part to
/// enable future resume on interruption.
///
/// 3-stage pipeline:
///   Stage 1 (main task): Network read + decrypt via `Decryptor.read_chunk()` + progress
///   Stage 2 (spawned):   Hash computation — updates SHA-256, forwards chunks to write
///   Stage 3 (spawned):   Disk write — writes chunks to .part file
#[allow(clippy::too_many_arguments)]
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

    // Hash the already-received prefix from the .part file for full-stream checksum.
    let prefix_hasher = if already_have > 0 {
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

    // Stage 3 (spawned): Disk write — receives chunks and writes to .part file.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(crate::proto::PIPELINE_DEPTH);
    let write_task = tokio::spawn(async move {
        let mut f = f;
        while let Some(chunk) = write_rx.recv().await {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        Ok::<_, anyhow::Error>(())
    });

    // Stage 2 (spawned): Hash computation — receives chunks, updates SHA-256,
    // forwards to write task. Returns the final hasher for checksum verification.
    let (hash_tx, mut hash_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(crate::proto::PIPELINE_DEPTH);
    let hash_task = tokio::spawn(async move {
        let mut hasher = prefix_hasher;
        while let Some(chunk) = hash_rx.recv().await {
            hasher.update(&chunk);
            write_tx.send(chunk).await
                .map_err(|_| anyhow::anyhow!("write task failed"))?;
        }
        // Drop write_tx so write_task sees channel close.
        Ok::<_, anyhow::Error>(hasher)
    });

    // Stage 1 (main task): Network read + decrypt + progress reporting.
    let mut dec = Decryptor::new(key, rx);
    // Nonce alignment for resume.
    if already_have > 0 {
        dec.set_counter(already_have / CHUNK as u64);
    }

    let mut done: u64 = already_have;
    let start = Instant::now();
    let mut speed = SpeedTracker::new(start, already_have);

    while let Some(chunk) = dec.read_chunk().await? {
        done += chunk.len() as u64;
        hash_tx.send(chunk).await
            .map_err(|_| anyhow::anyhow!("hash task failed"))?;
        let now = Instant::now();
        if speed.should_emit(now, false) {
            emit_progress_resume(on_progress, done, total_size, false, is_resumed, already_have, &mut speed, now);
        }
    }
    // Close hash channel so Stage 2 finishes, which closes write channel for Stage 3.
    drop(hash_tx);

    // Await both pipeline stages.
    let hasher = hash_task.await??;
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
    on_progress(TransferEvent {
        bytes_done: done,
        total_bytes: total_size,
        bytes_per_sec: 0,
        done: false,
        error: None,
        resumed: is_resumed,
        resume_offset: already_have,
        reconnect_info: None,
        archive_status: Some(archive_status(
            ArchiveStatusCode::UnpackStarted,
            format!("starting archive unpack for {}", part_path.display()),
        )),
    });
    let unpack_result = tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&part2)?;
        let reader = std::io::BufReader::new(f);
        archive::unpack_archive_sync(reader, &dest2)
    }).await.map_err(|e| anyhow::anyhow!("archive unpack task panicked: {e}"))?;

    if let Err(e) = unpack_result {
        on_progress(TransferEvent {
            bytes_done: done,
            total_bytes: total_size,
            bytes_per_sec: 0,
            done: false,
            error: Some(e.to_string()),
            resumed: is_resumed,
            resume_offset: already_have,
            reconnect_info: None,
            archive_status: Some(archive_status(
                ArchiveStatusCode::UnpackFailed,
                format!("archive unpack failed for {}", part_path.display()),
            )),
        });
        return Err(e);
    }

    // Clean up the .part file after successful decompression.
    let _ = tokio::fs::remove_file(part_path).await;

    on_progress(TransferEvent {
        bytes_done: total_size,
        total_bytes: total_size,
        bytes_per_sec: 0,
        done: false,
        error: None,
        resumed: is_resumed,
        resume_offset: already_have,
        reconnect_info: None,
        archive_status: Some(archive_status(
            ArchiveStatusCode::UnpackFinished,
            format!("finished archive unpack into {}", dest.display()),
        )),
    });

    let now = Instant::now();
    emit_progress_resume(on_progress, total_size, total_size, true, is_resumed, already_have, &mut speed, now);
    Ok(ReceiveOutcome::File { path: dest.to_path_buf(), name: _name.to_string(), kind: Kind::Archive })
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

async fn send_header(
    tx: &mut (impl AsyncWriteExt + Unpin),
    key: &[u8; 32],
    kind: Kind,
    name: &str,
    total_size: u64,
    archive_snapshot: Option<&ArchiveSnapshot>,
) -> Result<()> {
    let nb = name.as_bytes();
    anyhow::ensure!(nb.len() <= MAX_NAME_LEN, "filename too long ({} bytes)", nb.len());
    let snapshot_bytes = if kind == Kind::Archive {
        encode_archive_snapshot(archive_snapshot.ok_or_else(|| anyhow::anyhow!("missing archive snapshot for archive header"))?)?
    } else {
        Vec::new()
    };
    anyhow::ensure!(snapshot_bytes.len() <= u16::MAX as usize, "archive snapshot too large");
    let mut hdr = Vec::with_capacity(4 + 32 + 1 + 2 + nb.len() + 8 + 2 + snapshot_bytes.len());
    hdr.extend_from_slice(MAGIC);
    hdr.extend_from_slice(key);
    hdr.push(kind as u8);
    hdr.extend_from_slice(&(nb.len() as u16).to_be_bytes());
    hdr.extend_from_slice(nb);
    hdr.extend_from_slice(&total_size.to_be_bytes());
    hdr.extend_from_slice(&(snapshot_bytes.len() as u16).to_be_bytes());
    hdr.extend_from_slice(&snapshot_bytes);
    tx.write_all(&hdr).await?;
    Ok(())
}

async fn recv_header(
    rx: &mut (impl AsyncReadExt + Unpin),
) -> Result<([u8; 32], Kind, String, u64, Option<ArchiveSnapshot>)> {
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

    let mut snapshot_len_b = [0u8; 2];
    rx.read_exact(&mut snapshot_len_b).await?;
    let snapshot_len = u16::from_be_bytes(snapshot_len_b) as usize;
    let archive_snapshot = if snapshot_len > 0 {
        let mut snapshot_b = vec![0u8; snapshot_len];
        rx.read_exact(&mut snapshot_b).await?;
        Some(decode_archive_snapshot(&snapshot_b)?)
    } else {
        None
    };

    Ok((key, kind, name, total_size, archive_snapshot))
}

/// Stream `reader` through `enc`, computing SHA-256 on-the-fly.
/// Uses a 2-stage pipeline: reading runs in a spawned task, while
/// hash + encrypt + write runs in the main task. This decouples
/// archive/disk I/O from the hash+encrypt+network-write path.
async fn stream_encrypted_hash<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    enc: &mut Encryptor<impl AsyncWriteExt + Unpin>,
    initial: u64,
    total: u64,
    on_progress: Arc<impl Fn(TransferEvent)>,
    mut hasher: Sha256,
) -> Result<[u8; 32]> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>>>(crate::proto::PIPELINE_DEPTH);

    // Stage 1 (spawned): Read chunks from the reader and send via channel.
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
            if filled == 0 { break; }
            buf.truncate(filled);
            if tx.send(Ok(buf)).await.is_err() {
                break;
            }
        }
    });

    // Main task: receive chunks, hash, encrypt, write to network.
    let mut done = initial;
    let start = Instant::now();
    let mut speed = SpeedTracker::new(start, initial);
    while let Some(result) = rx.recv().await {
        let chunk = result?;
        hasher.update(&chunk);
        enc.write_chunk(&chunk).await?;
        done += chunk.len() as u64;
        let now = Instant::now();
        if speed.should_emit(now, false) {
            emit_progress(&on_progress, done, total, false, &mut speed, now);
        }
    }

    let now = Instant::now();
    emit_progress(&on_progress, done, total, true, &mut speed, now);

    // Capture read task panic.
    read_task
        .await
        .map_err(|e| anyhow::anyhow!("read task panicked: {e}"))?;

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
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>>>(crate::proto::PIPELINE_DEPTH);

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
    let mut speed = SpeedTracker::new(start, initial);
    while let Some(result) = rx.recv().await {
        let chunk = result?;
        hasher.update(&chunk);
        enc.write_chunk(&chunk).await?;
        done += chunk.len() as u64;
        let now = Instant::now();
        if speed.should_emit(now, false) {
            emit_progress(&on_progress, done, total, false, &mut speed, now);
        }
    }

    let now = Instant::now();
    emit_progress(&on_progress, done, total, true, &mut speed, now);

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
    send_header(&mut tx, &key, Kind::Clipboard, name, total_size, None).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let start = Instant::now();
    let mut speed = SpeedTracker::new(start, 0);
    for chunk in data.chunks(CHUNK) {
        hasher.update(chunk);
        enc.write_chunk(chunk).await?;
        done += chunk.len() as u64;
        let now = Instant::now();
        emit_progress(&on_progress, done, total_size, false, &mut speed, now);
    }
    let now = Instant::now();
    emit_progress(&on_progress, done, total_size, true, &mut speed, now);
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
    send_header(&mut tx, &key, Kind::Clipboard, name, total_size, None).await?;

    let mut resume_buf = [0u8; 8];
    rx.read_exact(&mut resume_buf).await?;

    let mut enc = Encryptor::new(&key, tx);
    let on_progress = Arc::new(on_progress);
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let start = Instant::now();
    let mut speed = SpeedTracker::new(start, 0);
    for chunk in png_data.chunks(CHUNK) {
        hasher.update(chunk);
        enc.write_chunk(chunk).await?;
        done += chunk.len() as u64;
        let now = Instant::now();
        emit_progress(&on_progress, done, total_size, false, &mut speed, now);
    }
    let now = Instant::now();
    emit_progress(&on_progress, done, total_size, true, &mut speed, now);
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
    let buf_size = crate::proto::TCP_BUF_SIZE;
    if let Err(e) = sock.set_send_buffer_size(buf_size) {
        eprintln!("warn: SO_SNDBUF failed: {e}");
    }
    if let Err(e) = sock.set_recv_buffer_size(buf_size) {
        eprintln!("warn: SO_RCVBUF failed: {e}");
    }
}

#[inline]
fn emit_progress(
    cb: &Arc<impl Fn(TransferEvent)>,
    bytes_done: u64,
    total_bytes: u64,
    done: bool,
    speed: &mut SpeedTracker,
    now: Instant,
) {
    let bytes_per_sec = speed.sample(now, bytes_done);
    cb(TransferEvent {
        bytes_done,
        total_bytes,
        bytes_per_sec,
        done,
        error: None,
        resumed: false,
        resume_offset: 0,
        reconnect_info: None,
        archive_status: None,
    });
}

#[inline]
fn emit_progress_resume(
    cb: &Arc<impl Fn(TransferEvent)>,
    bytes_done: u64,
    total_bytes: u64,
    done: bool,
    resumed: bool,
    resume_offset: u64,
    speed: &mut SpeedTracker,
    now: Instant,
) {
    let bytes_per_sec = speed.sample(now, bytes_done);
    cb(TransferEvent {
        bytes_done,
        total_bytes,
        bytes_per_sec,
        done,
        error: None,
        resumed,
        resume_offset,
        reconnect_info: None,
        archive_status: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_tracker_reports_positive_smoothed_rate() {
        let start = Instant::now();
        let mut tracker = SpeedTracker::new(start, 0);

        let later = start + Duration::from_millis(200);
        let rate = tracker.sample(later, 2 * 1024 * 1024);

        assert!(rate > 0, "speed tracker should report positive throughput");
    }

    #[test]
    fn speed_tracker_uses_delta_not_total_average() {
        let start = Instant::now();
        let mut tracker = SpeedTracker::new(start, 4 * 1024 * 1024);

        let later = start + Duration::from_millis(250);
        let rate = tracker.sample(later, 5 * 1024 * 1024);

        assert!(
            rate < 20 * 1024 * 1024,
            "resumed transfer speed should be based on newly transferred bytes, got {rate} B/s"
        );
        assert!(
            rate > 3 * 1024 * 1024,
            "resumed transfer speed should still reflect useful throughput, got {rate} B/s"
        );
    }

    #[test]
    fn send_outcome_carries_expected_metadata() {
        let outcome = SendOutcome {
            logical_name: "sync:file:test".to_string(),
            total_size: 123,
            checksum_hex: "abc123".to_string(),
        };

        assert_eq!(outcome.logical_name, "sync:file:test");
        assert_eq!(outcome.total_size, 123);
        assert_eq!(outcome.checksum_hex, "abc123");
    }
}
