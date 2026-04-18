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

/// Returns an `AsyncRead` that streams a zstd-compressed tar of `path`.
/// Directories are archived recursively; single files are wrapped in a tar.
pub fn stream_archive(path: &Path) -> Result<impl AsyncRead + Send + Unpin + 'static> {
    let (pipe_reader, pipe_writer) = os_pipe::pipe()?;
    let path = path.to_path_buf();

    std::thread::spawn(move || {
        if let Err(e) = compress_to_pipe(pipe_writer, &path) {
            eprintln!("archive compression error: {e}");
        }
    });

    // Bridge: sync pipe_reader → async duplex writer → async reader returned to caller.
    let (async_writer, async_reader) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(pump_pipe(pipe_reader, async_writer));
    Ok(async_reader)
}

/// Decompress and unpack a zstd-tar stream into `dest`.
/// Must be called inside `tokio::task::spawn_blocking`.
pub fn unpack_archive_sync(reader: impl std::io::Read, dest: &Path) -> Result<()> {
    let dec = zstd::Decoder::new(reader)?;
    let mut archive = tar::Archive::new(dec);
    archive.unpack(dest)?;
    Ok(())
}

/// Walk `path` and sum all file sizes. Used to populate `total_size` for
/// directory transfers so the receiver can show a real progress percentage.
/// Returns 0 on any error (graceful degradation).
pub fn dir_total_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

// ── Internal ──────────────────────────────────────────────────────────────────

fn compress_to_pipe(writer: os_pipe::PipeWriter, path: &Path) -> Result<()> {
    // Level 1: ~3-5x faster than level 3 with minimal ratio loss — ideal for LAN.
    let enc = zstd::Encoder::new(writer, 1)?;
    let mut tar = tar::Builder::new(enc);
    let entry_name = path.file_name().unwrap_or_default();
    if path.is_dir() {
        tar.append_dir_all(entry_name, path)?;
    } else {
        tar.append_path_with_name(path, entry_name)?;
    }
    tar.into_inner()?.finish()?;
    Ok(())
}

/// Pump bytes from a synchronous `PipeReader` into an async writer.
/// Runs the blocking read inside spawn_blocking to avoid stalling the tokio thread.
async fn pump_pipe(mut src: os_pipe::PipeReader, mut dst: impl AsyncWrite + Unpin + Send + 'static) {
    // Channel bridges the blocking read thread and the async write side.
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

    std::thread::spawn(move || {
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            use std::io::Read;
            match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if chunk_tx.blocking_send(buf[..n].to_vec()).is_err() { break; }
                }
            }
        }
    });

    while let Some(chunk) = chunk_rx.recv().await {
        if dst.write_all(&chunk).await.is_err() { break; }
    }
    let _ = dst.shutdown().await;
}
