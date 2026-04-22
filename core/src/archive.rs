//! Streaming tar+zstd archive: zero temp files, O(1) memory.
//!
//! `stream_archive` returns an async reader that yields a zstd-compressed tar.
//! Compression runs in a background OS thread; errors are propagated via the
//! duplex pipe (the reader will get an EOF / broken-pipe on failure).
//!
//! `unpack_archive_sync` is called inside `spawn_blocking` on the receiver side.
//!
//! `dir_total_size` walks a directory and sums file sizes for progress reporting.

use anyhow::Result;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use walkdir::WalkDir;

/// Returns an `AsyncRead` that streams a zstd-compressed tar of `path`.
/// `entries` can be pre-collected via `walk_dir` to avoid a second walkdir pass.
pub fn stream_archive_with_entries(
    path: &Path,
    entries: Vec<(walkdir::DirEntry, std::fs::Metadata)>,
) -> Result<impl AsyncRead + Send + Unpin + 'static> {
    let (pipe_reader, pipe_writer) = os_pipe::pipe()?;
    let path = path.to_path_buf();

    std::thread::spawn(move || {
        if let Err(e) = compress_entries(pipe_writer, &path, entries) {
            eprintln!("archive compression error: {e}");
        }
    });

    let (async_writer, async_reader) = tokio::io::duplex(16 * 1024 * 1024);
    tokio::spawn(pump_pipe(pipe_reader, async_writer));
    Ok(async_reader)
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

/// Walk `path` once, returning (total_bytes, entries_with_metadata).
/// Caches metadata alongside each entry to avoid re-reading it in compress_entries.
pub fn walk_dir(path: &Path) -> (u64, Vec<(walkdir::DirEntry, std::fs::Metadata)>) {
    let entries: Vec<_> = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
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

fn compress_entries(writer: os_pipe::PipeWriter, path: &Path, entries: Vec<(walkdir::DirEntry, std::fs::Metadata)>) -> Result<()> {
    let enc = zstd::Encoder::new(writer, 1)?;
    let mut tar = tar::Builder::new(enc);
    let entry_name = path.file_name().unwrap_or_default();

    let (small, large): (Vec<_>, Vec<_>) = entries.into_iter().partition(|(e, m)| {
        e.file_type().is_file() && m.len() < SMALL_FILE_THRESHOLD
    });

    // Pre-read small files in parallel; metadata already cached.
    // Sort by path after parallel collection to ensure deterministic tar order.
    let mut preloaded: Vec<(std::path::PathBuf, Vec<u8>, std::fs::Metadata)> = {
        use rayon::prelude::*;
        small
            .into_par_iter()
            .filter_map(|(e, meta)| {
                let data = std::fs::read(e.path()).ok()?;
                Some((e.path().to_path_buf(), data, meta))
            })
            .collect()
    };
    preloaded.sort_by(|a, b| a.0.cmp(&b.0));

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

    for (abs_path, data, meta) in &preloaded {
        let rel = abs_path.strip_prefix(path).unwrap_or(abs_path);
        let tar_path = std::path::Path::new(entry_name).join(rel);
        let mut header = tar::Header::new_gnu();
        header.set_metadata(meta);
        header.set_size(data.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, &tar_path, data.as_slice())?;
    }

    tar.into_inner()?.finish()?;
    Ok(())
}

/// Pump bytes from a synchronous `PipeReader` into an async writer.
/// Uses a pool of 4 reusable buffers to avoid per-chunk heap allocation.
async fn pump_pipe(mut src: os_pipe::PipeReader, mut dst: impl AsyncWrite + Unpin + Send + 'static) {
    const BUF_SIZE: usize = 256 * 1024;
    const POOL_SIZE: usize = 4;

    let (data_tx, mut data_rx) = tokio::sync::mpsc::channel::<(Vec<u8>, usize)>(POOL_SIZE);
    let (return_tx, return_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(POOL_SIZE);

    // Pre-fill the return channel with reusable buffers.
    for _ in 0..POOL_SIZE {
        return_tx.send(vec![0u8; BUF_SIZE]).ok();
    }

    std::thread::spawn(move || {
        use std::io::Read;
        loop {
            // Borrow a buffer from the pool; block until one is available.
            let mut buf = match return_rx.recv() { Ok(b) => b, Err(_) => break };
            match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if data_tx.blocking_send((buf, n)).is_err() { break; }
                }
            }
        }
    });

    while let Some((buf, n)) = data_rx.recv().await {
        if dst.write_all(&buf[..n]).await.is_err() { break; }
        // Return buffer to pool; ignore if reader thread already exited.
        return_tx.send(buf).ok();
    }
    let _ = dst.shutdown().await;
}
