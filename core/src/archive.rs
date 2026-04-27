//! Streaming tar+zstd archive: zero temp files, O(1) memory.
//!
//! `stream_archive` returns an async reader that yields a zstd-compressed tar.
//! Compression runs in a background OS thread; errors are propagated via a
//! shared error slot checked on EOF.
//!
//! `unpack_archive_sync` is called inside `spawn_blocking` on the receiver side.
//!
//! `dir_total_size` walks a directory and sums file sizes for progress reporting.

use anyhow::Result;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use walkdir::WalkDir;

/// Wraps an AsyncRead and checks for compression errors on EOF.
/// When the inner reader returns EOF (0 bytes), checks the error slot.
/// If an error is present, returns io::Error instead of silent EOF.
struct ErrorAwareReader {
    rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    error_slot: Arc<Mutex<Option<String>>>,
    /// Leftover bytes from the last received chunk.
    remainder: Vec<u8>,
    offset: usize,
}

impl AsyncRead for ErrorAwareReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Drain remainder first
        if self.offset < self.remainder.len() {
            let avail = &self.remainder[self.offset..];
            let n = avail.len().min(buf.remaining());
            buf.put_slice(&avail[..n]);
            self.offset += n;
            if self.offset >= self.remainder.len() {
                self.remainder.clear();
                self.offset = 0;
            }
            return Poll::Ready(Ok(()));
        }

        // Poll channel for next chunk
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => {
                let n = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.remainder = chunk;
                    self.offset = n;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                // Channel closed (EOF) — check for compression error
                if let Some(err_msg) = self.error_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, err_msg)))
                } else {
                    Poll::Ready(Ok(())) // normal EOF
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Synchronous writer that sends chunks over a tokio mpsc channel.
/// Used by `compress_entries` running in a background OS thread.
struct ChannelWriter {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    buf: Vec<u8>,
}

const CHANNEL_BUF_SIZE: usize = 256 * 1024;

impl ChannelWriter {
    fn new(tx: tokio::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self { tx, buf: Vec::with_capacity(CHANNEL_BUF_SIZE) }
    }
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHANNEL_BUF_SIZE {
            self.flush()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() { return Ok(()); }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHANNEL_BUF_SIZE));
        self.tx.blocking_send(chunk)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped"))
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = self.flush();
    }
}

/// Returns an `AsyncRead` that streams a zstd-compressed tar of `path`.
/// `entries` can be pre-collected via `walk_dir` to avoid a second walkdir pass.
///
/// Architecture: compress_entries → ChannelWriter → mpsc → ErrorAwareReader
/// Single hop, no intermediate os_pipe or tokio::duplex.
pub fn stream_archive_with_entries(
    path: &Path,
    entries: Vec<(walkdir::DirEntry, std::fs::Metadata)>,
) -> Result<impl AsyncRead + Send + Unpin + 'static> {
    // 8 slots × 256KB = 2MB in-flight — enough to keep the pipe saturated
    // without hogging memory like the old 16MB duplex buffer.
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
    let path = path.to_path_buf();

    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_slot_writer = error_slot.clone();

    std::thread::spawn(move || {
        let writer = ChannelWriter::new(tx);
        if let Err(e) = compress_entries_to_writer(writer, &path, entries) {
            *error_slot_writer.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(format!("archive compression error: {e}"));
        }
    });

    Ok(ErrorAwareReader { rx, error_slot, remainder: Vec::new(), offset: 0 })
}

/// Returns an `AsyncRead` that streams a zstd-compressed tar of `path`.
/// Directories are archived recursively; single files are wrapped in a tar.
pub fn stream_archive(path: &Path) -> Result<impl AsyncRead + Send + Unpin + 'static> {
    let (_, entries) = walk_dir(path);
    stream_archive_with_entries(path, entries)
}

/// Decompress and unpack a zstd-tar stream into `dest`.
/// Must be called inside `tokio::task::spawn_blocking`.
pub fn unpack_archive_sync(reader: impl std::io::Read, dest: &Path) -> Result<()> {
    let dec = zstd::Decoder::new(reader)?;
    let mut archive = tar::Archive::new(dec);
    archive.unpack(dest)?;
    Ok(())
}

/// Returns true if the filename matches a log file pattern (`*.log` or `*.log.*`).
/// Only intended for files, not directories.
fn is_log_file(name: &std::ffi::OsStr) -> bool {
    let s = name.to_string_lossy();
    s.ends_with(".log") || s.contains(".log.")
}

/// Walk `path` once, returning (total_bytes, entries_with_metadata).
/// Caches metadata alongside each entry to avoid re-reading it in compress_entries.
/// Excludes runtime-generated log files (`*.log`, `*.log.*`) to prevent transient/locked
/// log files from causing transfer failures.
pub fn walk_dir(path: &Path) -> (u64, Vec<(walkdir::DirEntry, std::fs::Metadata)>) {
    let entries: Vec<_> = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
        .filter(|(e, _)| {
            // Keep directories unchanged; only filter files
            if !e.file_type().is_file() {
                return true;
            }
            // Exclude log files
            !is_log_file(e.file_name())
        })
        .collect();
    let total = entries.iter()
        .filter(|(e, _)| e.file_type().is_file())
        .map(|(_, m)| m.len())
        .sum();
    (total, entries)
}

/// Walk `path` and sum all file sizes.
pub fn dir_total_size(path: &Path) -> u64 {
    walk_dir(path).0
}

// ── Internal ──────────────────────────────────────────────────────────────────

/// Threshold: files smaller than this are pre-read into memory in parallel.
const SMALL_FILE_THRESHOLD: u64 = 1024 * 1024; // 1 MB

fn compress_entries_to_writer(writer: impl std::io::Write, path: &Path, entries: Vec<(walkdir::DirEntry, std::fs::Metadata)>) -> Result<()> {
    let enc = zstd::Encoder::new(writer, 3)?;  // level 3: better ratio, still fast on LAN
    let mut tar = tar::Builder::new(enc);
    let entry_name = path.file_name().unwrap_or_default();

    let (small, large): (Vec<_>, Vec<_>) = entries.into_iter().partition(|(e, m)| {
        e.file_type().is_file() && m.len() < SMALL_FILE_THRESHOLD
    });

    // Pre-read small files in parallel; metadata already cached.
    // Sort by path after parallel collection to ensure deterministic tar order.
    // Collect errors instead of silently skipping unreadable files.
    let preloaded_results: Vec<Result<(std::path::PathBuf, Vec<u8>, std::fs::Metadata), String>> = {
        use rayon::prelude::*;
        small
            .into_par_iter()
            .map(|(e, meta)| {
                match std::fs::read(e.path()) {
                    Ok(data) => Ok((e.path().to_path_buf(), data, meta)),
                    Err(err) => Err(format!("failed to read {}: {err}", e.path().display())),
                }
            })
            .collect()
    };
    // Check for any read errors before proceeding
    for r in &preloaded_results {
        if let Err(msg) = r {
            anyhow::bail!("{msg}");
        }
    }
    let mut preloaded: Vec<(std::path::PathBuf, Vec<u8>, std::fs::Metadata)> = preloaded_results
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    preloaded.sort_by(|a, b| a.0.cmp(&b.0));

    // Write small files first: they're already in memory, so the pipe fills
    // quickly and the receiver can start decompressing without waiting for
    // large-file I/O. Large files follow sequentially.
    for (abs_path, data, meta) in &preloaded {
        let rel = abs_path.strip_prefix(path).unwrap_or(abs_path);
        let tar_path = std::path::Path::new(entry_name).join(rel);
        let mut header = tar::Header::new_gnu();
        header.set_metadata(meta);
        header.set_size(data.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, &tar_path, data.as_slice())?;
    }

    for (e, meta) in &large {
        let rel = e.path().strip_prefix(path).unwrap_or(e.path());
        let tar_path = std::path::Path::new(entry_name).join(rel);
        if e.file_type().is_dir() {
            let mut header = tar::Header::new_gnu();
            header.set_metadata(meta);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            tar.append_data(&mut header, &tar_path, std::io::empty())?;
        } else {
            let f = std::fs::File::open(e.path())?;
            let mut buf_f = std::io::BufReader::with_capacity(256 * 1024, f);
            let mut header = tar::Header::new_gnu();
            header.set_metadata(meta);
            header.set_cksum();
            tar.append_data(&mut header, &tar_path, &mut buf_f)?;
        }
    }

    tar.into_inner()?.finish()?;
    Ok(())
}


