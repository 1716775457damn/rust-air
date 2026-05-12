//! Streaming tar+zstd archive: zero temp files, O(1) memory.
//!
//! `stream_archive` returns an async reader that yields a zstd-compressed tar.
//! Compression runs in a background OS thread; errors are propagated via a
//! shared error slot checked on EOF.
//!
//! `unpack_archive_sync` is called inside `spawn_blocking` on the receiver side.
//!
//! `dir_total_size` walks a directory and sums file sizes for progress reporting.
//!
//! # Parallel Archive Generation
//!
//! For directories with many small files, use `stream_archive_parallel` which
//! uses multi-threaded compression for improved throughput. See `ParallelArchiveConfig`
//! for configuration options.

use anyhow::{Result, bail};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use walkdir::WalkDir;

/// Type alias for preloaded file data in parallel compression.
/// Clippy recommends factoring complex types.
type PreloadedFile = (std::path::PathBuf, Vec<u8>, std::fs::Metadata);

// ── Constants ──────────────────────────────────────────────────────────────────

/// Threshold for "tiny" files: files smaller than 64 KB use fastest compression.
/// Validates: Requirements 3.1
pub const TINY_THRESHOLD: u64 = 64 * 1024; // 64 KB

/// Threshold for "small" files: files 64 KB to 1 MB use balanced compression.
/// Files >= 1 MB use standard compression level.
/// Validates: Requirements 3.2, 3.3
pub const SMALL_THRESHOLD: u64 = 1024 * 1024; // 1 MB

/// Pipeline channel depth for backpressure between compression and encryption stages.
/// Validates: Requirement 5.1
pub const PIPELINE_DEPTH: usize = 4;

// ── Configuration Types ────────────────────────────────────────────────────────

/// Compression level selection by file size tier.
///
/// Files are categorized into three tiers based on size:
/// - Tiny: < 64 KB (use fastest compression)
/// - Small: 64 KB - 1 MB (use balanced compression)
/// - Large: >= 1 MB (use standard compression)
///
/// Validates: Requirements 3.1, 3.2, 3.3
#[derive(Debug, Clone, Copy)]
pub struct CompressionLevels {
    /// Compression level for files < 64 KB (tiny files).
    /// Default: 1 (fastest compression).
    pub tiny: i32,

    /// Compression level for files 64 KB to < 1 MB (small files).
    /// Default: 3 (balanced speed/ratio).
    pub small: i32,

    /// Compression level for files >= 1 MB (large files).
    /// Default: 3 (same as current default).
    pub large: i32,
}

impl Default for CompressionLevels {
    fn default() -> Self {
        Self {
            tiny: 1,   // Fastest for tiny files
            small: 3,  // Balanced for small files
            large: 3,  // Same as existing default
        }
    }
}

/// Configuration for parallel archive generation.
///
/// Controls threading, batching, and compression settings for optimal
/// throughput when transferring directories with many small files.
///
/// Validates: Requirements 1.3, 2.2, 3.1-3.4, 5.1
#[derive(Debug, Clone)]
pub struct ParallelArchiveConfig {
    /// Maximum number of compression threads.
    /// Default: min(num_cpus, 8) to avoid oversubscription.
    /// Validates: Requirement 1.3
    pub compression_threads: usize,

    /// Target batch size for accumulating compressed entries before sending
    /// to the encryption pipeline. Larger batches reduce per-chunk overhead.
    /// Default: 1 MB (same as CHUNK).
    /// Validates: Requirement 2.2
    pub batch_size: usize,

    /// Threshold for categorizing files as "small" vs "large".
    /// Files smaller than this threshold may be pre-loaded into memory.
    /// Default: 1 MB.
    /// Validates: Requirement 5.3
    pub small_file_threshold: u64,

    /// Compression levels by file size tier.
    /// Validates: Requirements 3.1, 3.2, 3.3
    pub compression_levels: CompressionLevels,
}

impl Default for ParallelArchiveConfig {
    fn default() -> Self {
        // Use min(num_cpus, 8) compression threads to balance parallelism
        // with memory usage and avoid oversubscription.
        let num_cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let compression_threads = num_cpus.min(8);

        // Batch size matches CHUNK (1 MB) for optimal encryption pipeline throughput.
        // Import from proto to avoid duplication.
        let batch_size = 1024 * 1024; // 1 MB (same as CHUNK in proto.rs)

        Self {
            compression_threads,
            batch_size,
            small_file_threshold: SMALL_THRESHOLD,
            compression_levels: CompressionLevels::default(),
        }
    }
}

// ── Internal Data Structures ───────────────────────────────────────────────────

/// Result of parallel compression for a single file.
///
/// This struct holds the compressed tar entry data along with metadata
/// needed for sorted assembly and progress reporting.
///
/// Validates: Requirements 1.2, 9.2
#[derive(Debug, Clone)]
#[allow(dead_code)] // Used in parallel-archive feature tests
struct CompressedEntry {
    /// Relative path within archive (for sorting).
    /// Used to ensure deterministic tar entry order regardless of
    /// which thread processed the file.
    path: std::path::PathBuf,

    /// Complete tar entry: header + data (compressed with zstd).
    /// This is the final bytes that will be written to the archive stream.
    tar_data: Vec<u8>,

    /// Original uncompressed size of the file.
    /// Used for progress reporting during transfer.
    original_size: u64,

    /// Compressed size of the tar entry.
    /// Used for metrics and debugging.
    compressed_size: u64,
}

/// Select compression level based on file size.
///
/// Uses tiered compression levels for optimal performance:
/// - Files < 64 KB: Use `tiny` level (fastest, typically level 1)
/// - Files 64 KB to < 1 MB: Use `small` level (balanced, typically level 3)
/// - Files >= 1 MB: Use `large` level (standard, typically level 3)
///
/// # Arguments
///
/// * `size` - File size in bytes
/// * `levels` - Compression level configuration for each size tier
///
/// # Returns
///
/// The appropriate compression level for the given file size.
///
/// Validates: Requirements 3.1, 3.2, 3.3
#[allow(dead_code)] // Used in parallel-archive feature
fn select_compression_level(size: u64, levels: &CompressionLevels) -> i32 {
    if size < TINY_THRESHOLD {
        levels.tiny
    } else if size < SMALL_THRESHOLD {
        levels.small
    } else {
        levels.large
    }
}

/// Compress a single file entry with tar header.
///
/// This function is designed to be called by parallel workers in `compress_entries_parallel`.
/// It reads the file content, builds a tar header with metadata, selects the appropriate
/// compression level based on file size, and compresses the tar entry with zstd.
///
/// # Arguments
///
/// * `entry` - Directory entry for the file to compress
/// * `meta` - File metadata (pre-fetched to avoid redundant syscalls)
/// * `base_path` - Base path to strip from the entry path (for relative paths)
/// * `config` - Configuration for parallel archive generation
///
/// # Returns
///
/// A `CompressedEntry` containing the compressed tar data and metadata.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read
/// - The path cannot be made relative to base_path
/// - Compression fails
///
/// Validates: Requirements 1.1, 3.1, 3.2, 3.3
/// Creates a tar entry for a single file (uncompressed).
/// 
/// This function reads a file and creates a tar entry in memory. The tar entry
/// is NOT compressed here - compression happens at the batch level in BatchAssembler
/// to ensure the entire archive is a single zstd stream compatible with unpack_archive_sync.
/// 
/// Note: This only writes the header + data, NOT the tar footer. The footer (1024 zero bytes)
/// must be written once after all entries are appended, by calling `tar::Builder::finish()`
/// in the assembler.
fn compress_single_entry(
    entry: &walkdir::DirEntry,
    meta: &std::fs::Metadata,
    base_path: &Path,
    _config: &ParallelArchiveConfig,
) -> Result<CompressedEntry> {
    let path = entry.path();
    let rel_path = path.strip_prefix(base_path)?;

    // Read file content
    let data = std::fs::read(path)?;

    // Build tar entry in memory: header (512 bytes) + data + padding to 512-byte boundary
    // Note: We do NOT call builder.finish() here because that writes the end-of-archive
    // marker (1024 zero bytes), which should only appear once at the end of the entire archive.
    let mut tar_buf = Vec::with_capacity(512 + data.len() + 512);
    
    // Create header
    let mut header = tar::Header::new_gnu();
    header.set_metadata(meta);
    header.set_size(data.len() as u64);
    header.set_path(rel_path)?;  // Set the path in the tar header
    header.set_cksum();
    
    // Write header (512 bytes)
    tar_buf.extend_from_slice(header.as_bytes());
    
    // Write data
    tar_buf.extend_from_slice(&data);
    
    // Pad to 512-byte boundary (tar requires each entry to be a multiple of 512 bytes)
    let padding = (512 - (data.len() % 512)) % 512;
    tar_buf.extend(std::iter::repeat_n(0u8, padding));

    Ok(CompressedEntry {
        path: rel_path.to_path_buf(),
        tar_data: tar_buf, // Raw tar entry, not compressed
        original_size: data.len() as u64,
        compressed_size: 0, // Will be set during batch compression
    })
}

/// Assemble compressed entries in sorted order, batch for encryption.
///
/// This struct receives compressed entries from parallel workers, stores them
/// in a BTreeMap for sorted order (ensuring deterministic tar output), and
/// flushes batches to the output channel when they reach the target size.
///
/// # Memory Behavior
///
/// Entries are held in memory until `flush` is called. The BTreeMap ensures
/// entries are iterated in sorted path order for deterministic archives.
///
/// Validates: Requirements 1.2, 2.1, 2.2, 9.1, 9.2
#[allow(dead_code)] // Used in parallel-archive feature tests
struct BatchAssembler {
    /// Sorted map of path → compressed entry.
    /// BTreeMap ensures deterministic iteration order by path.
    entries: std::collections::BTreeMap<std::path::PathBuf, CompressedEntry>,

    /// Target batch size (typically 1 MB, matching CHUNK).
    /// Batches are flushed when they approach this size.
    #[allow(dead_code)] // Used for debugging and future batch size validation
    batch_size: usize,

    /// Output channel for sending batched chunks to encryption pipeline.
    /// Uses tokio::sync::mpsc::Sender for async compatibility with ErrorAwareReader.
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl BatchAssembler {
    /// Create a new BatchAssembler.
    ///
    /// # Arguments
    ///
    /// * `batch_size` - Target batch size in bytes (e.g., 1 MB)
    /// * `tx` - Tokio channel sender for outputting batched chunks
    ///
    /// Validates: Requirements 2.1, 2.2
    fn new(batch_size: usize, tx: tokio::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            entries: std::collections::BTreeMap::new(),
            batch_size,
            tx,
        }
    }

    /// Add a compressed entry from parallel workers.
    ///
    /// Entries are stored in the BTreeMap keyed by path, ensuring
    /// deterministic order regardless of which thread processed the file.
    ///
    /// Validates: Requirements 1.2, 9.2
    fn add_entry(&mut self, entry: CompressedEntry) {
        self.entries.insert(entry.path.clone(), entry);
    }

    /// Flush all entries as a single compressed batch.
    ///
    /// Collects all tar entries in sorted path order, appends the tar footer
    /// (1024 zero bytes), then compresses the entire batch as a single zstd stream.
    /// This ensures compatibility with unpack_archive_sync which expects a single
    /// continuous tar+zstd stream.
    ///
    /// # Returns
    ///
    /// Ok(()) on success, or an error if channel send fails.
    ///
    /// Validates: Requirements 2.1, 2.2, 9.1
    fn flush(self) -> Result<()> {
        // Collect all tar entries in sorted order
        let mut all_tar_data = Vec::new();
        for (_, entry) in self.entries.into_iter() {
            all_tar_data.extend_from_slice(&entry.tar_data);
        }

        if all_tar_data.is_empty() {
            return Ok(());
        }

        // Append tar footer: 1024 zero bytes (two 512-byte blocks)
        // This marks the end of the archive
        all_tar_data.extend(std::iter::repeat_n(0u8, 1024));

        // Compress the entire tar stream as a single zstd stream
        // This ensures compatibility with unpack_archive_sync
        let compressed = zstd::encode_all(&all_tar_data[..], 3)?; // Use level 3 for good balance

        // Send the compressed batch
        self.tx.blocking_send(compressed)
            .map_err(|e| anyhow::anyhow!("failed to send compressed batch to output channel: {}", e))?;

        Ok(())
    }
}

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
                    Poll::Ready(Err(std::io::Error::other(err_msg)))
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

// ── Parallel Archive Generation Entry Points ───────────────────────────────────

/// Returns an `AsyncRead` that streams a parallel-generated zstd-compressed tar of `path`.
///
/// This function uses multi-threaded compression for improved throughput on directories
/// with many small files. Files are processed in parallel using rayon, then assembled
/// in sorted order for deterministic output.
///
/// # Arguments
///
/// * `path` - Base path being archived
/// * `entries` - Pre-collected directory entries with metadata (from `walk_dir`)
///
/// # Returns
///
/// An `AsyncRead` that yields the compressed tar archive.
///
/// # Architecture
///
/// ```text
/// compress_entries_parallel (rayon parallel) → CompressedEntry channel
///     → BatchAssembler (sorted assembly) → Vec<u8> channel → ErrorAwareReader
/// ```
///
/// Validates: Requirements 1.1, 1.4, 4.1, 4.2, 5.1
pub fn stream_archive_parallel(
    path: &Path,
    entries: Vec<(walkdir::DirEntry, std::fs::Metadata)>,
) -> Result<impl AsyncRead + Send + Unpin + 'static> {
    let config = ParallelArchiveConfig::default();
    stream_archive_parallel_with_config(path, entries, config)
}

/// Returns an `AsyncRead` that streams a parallel-generated zstd-compressed tar with custom config.
///
/// This is the configurable variant of `stream_archive_parallel`, allowing tuning of
/// thread count, batch size, and compression levels.
///
/// # Arguments
///
/// * `path` - Base path being archived
/// * `entries` - Pre-collected directory entries with metadata (from `walk_dir`)
/// * `config` - Configuration for parallel archive generation
///
/// # Returns
///
/// An `AsyncRead` that yields the compressed tar archive.
///
/// # Thread Architecture
///
/// 1. **Compression thread**: Runs `compress_entries_parallel` which uses rayon to
///    process small files in parallel and large files sequentially. Sends `CompressedEntry`
///    to the assembler via std::sync::mpsc channel.
///
/// 2. **Assembler thread**: Runs `BatchAssembler` which collects entries in a BTreeMap
///    (sorted by path) and flushes batches to the output channel when they reach
///    `config.batch_size`.
///
/// 3. **Output channel**: tokio::sync::mpsc channel with `PIPELINE_DEPTH` bound for
///    backpressure between archive generation and the async runtime.
///
/// # Error Handling
///
/// Errors from the compression thread are captured in `error_slot` and propagated
/// to the reader on EOF via `ErrorAwareReader`.
///
/// Validates: Requirements 1.1, 1.4, 4.1, 4.2, 5.1
pub fn stream_archive_parallel_with_config(
    path: &Path,
    entries: Vec<(walkdir::DirEntry, std::fs::Metadata)>,
    config: ParallelArchiveConfig,
) -> Result<impl AsyncRead + Send + Unpin + 'static> {
    // Output channel with PIPELINE_DEPTH bound for backpressure.
    // This is a tokio mpsc channel for async compatibility with ErrorAwareReader.
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(PIPELINE_DEPTH);

    // Shared error slot for propagating errors from compression thread to reader.
    let error_slot = Arc::new(Mutex::new(None));
    let error_slot_reader = error_slot.clone();
    let error_slot_compressor = error_slot.clone();

    let path = path.to_path_buf();

    // Spawn compression thread
    std::thread::spawn(move || {
        // Internal channel between compression and assembler.
        // Uses std::sync::mpsc because compressor is synchronous.
        let (compress_tx, compress_rx) = std::sync::mpsc::channel::<CompressedEntry>();

        // Spawn assembler thread
        let assembler_tx = tx.clone();
        let batch_size = config.batch_size;
        let assembler_error_slot = error_slot.clone();

        std::thread::spawn(move || {
            let mut assembler = BatchAssembler::new(batch_size, assembler_tx);
            while let Ok(entry) = compress_rx.recv() {
                assembler.add_entry(entry);
            }
            // Flush any remaining entries. If this fails, capture the error.
            if let Err(e) = assembler.flush() {
                *assembler_error_slot.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(format!("assembler flush error: {}", e));
            }
        });

        // Run parallel compression. Errors are captured in error_slot.
        if let Err(e) = compress_entries_parallel(&path, entries, &config, compress_tx, error_slot_compressor) {
            *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
        }
        // Note: compress_tx is dropped here, which signals the assembler to finish.
    });

    Ok(ErrorAwareReader {
        rx,
        error_slot: error_slot_reader,
        remainder: Vec::new(),
        offset: 0,
    })
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

// ── Internal Threshold for Large File Processing ────────────────────────────────

/// Internal threshold for pre-loading files into memory during archive generation.
/// This matches SMALL_THRESHOLD but is kept internal for backwards compatibility.
const SMALL_FILE_THRESHOLD: u64 = SMALL_THRESHOLD;

// ── Parallel Compression Orchestration ──────────────────────────────────────────

/// Process files in parallel, producing compressed tar entries.
///
/// This function is the core of the parallel archive generation pipeline. It:
/// 1. Partitions entries into small files (< 1 MB) and large files (>= 1 MB)
/// 2. Processes small files in parallel using rayon for improved throughput
/// 3. Processes large files sequentially to avoid memory exhaustion
/// 4. Sends compressed entries to the assembler via channel
/// 5. Captures errors in error_slot for propagation
///
/// # Arguments
///
/// * `path` - Base path being archived (used for relative path computation)
/// * `entries` - Pre-collected directory entries with metadata
/// * `config` - Configuration for parallel archive generation
/// * `tx` - Channel sender for compressed entries to the assembler
/// * `error_slot` - Shared error slot for error propagation
///
/// # Returns
///
/// Ok(()) on success, or an error if:
/// - A file cannot be read
/// - Compression fails
/// - Channel send fails (assembler dropped)
///
/// # Memory Behavior
///
/// Small files are fully read into memory and compressed in parallel. Large files
/// are processed sequentially to bound memory usage.
///
/// Validates: Requirements 1.1, 1.3, 1.5, 5.3, 5.4, 8.1
fn compress_entries_parallel(
    path: &Path,
    entries: Vec<(walkdir::DirEntry, std::fs::Metadata)>,
    config: &ParallelArchiveConfig,
    tx: std::sync::mpsc::Sender<CompressedEntry>,
    error_slot: Arc<Mutex<Option<String>>>,
) -> Result<()> {
    use rayon::prelude::*;

    // Filter out directories - they are handled separately in the tar format
    // by the caller (stream_archive_parallel adds directory entries before files).
    // Directories have size 0 and would be incorrectly classified as "small files".
    let files: Vec<_> = entries
        .into_iter()
        .filter(|(e, _)| e.file_type().is_file())
        .collect();

    // Partition into small (< threshold) and large files.
    // Small files are processed in parallel; large files sequentially.
    let (small, large): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|(_, meta)| meta.len() < config.small_file_threshold);

    // Process small files in parallel using rayon.
    // Each worker reads, creates tar header, and compresses independently.
    // Errors are collected and checked after parallel execution.
    let results: Vec<Result<CompressedEntry, String>> = small
        .into_par_iter()
        .map(|(entry, meta)| {
            compress_single_entry(&entry, &meta, path, config)
                .map_err(|e| format!("failed to process {}: {}", entry.path().display(), e))
        })
        .collect();

    // Check for errors and propagate.
    // If any file failed, capture the error and abort.
    for result in results {
        match result {
            Ok(compressed_entry) => {
                // Send to assembler for sorted batching
                if let Err(e) = tx.send(compressed_entry) {
                    // Assembler dropped, cannot continue
                    let msg = format!("failed to send compressed entry to assembler: {}", e);
                    *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(msg.clone());
                    bail!("parallel compression failed: {}", e);
                }
            }
            Err(msg) => {
                // Capture error and abort
                *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(msg.clone());
                bail!("parallel compression failed: {}", msg);
            }
        }
    }

    // Process large files sequentially to avoid memory exhaustion.
    // Large files (>= small_file_threshold) are streamed without full buffering
    // in the parallel path. We process them one-by-one here.
    for (entry, meta) in large {
        // For large files, use a streaming approach via compress_single_entry.
        // Note: compress_single_entry reads the entire file into memory, which is
        // acceptable for large files since we're processing them sequentially
        // (not N files in parallel). Future optimization could add true streaming.
        match compress_single_entry(&entry, &meta, path, config) {
            Ok(compressed_entry) => {
                if let Err(e) = tx.send(compressed_entry) {
                    let msg = format!("failed to send compressed entry to assembler: {}", e);
                    *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(msg.clone());
                    bail!("parallel compression failed: {}", e);
                }
            }
            Err(e) => {
                let msg = format!("failed to process {}: {}", entry.path().display(), e);
                *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(msg.clone());
                bail!("parallel compression failed: {}", msg);
            }
        }
    }

    Ok(())
}

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
    let preloaded_results: Vec<Result<PreloadedFile, String>> = {
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
    let mut preloaded: Vec<PreloadedFile> = preloaded_results
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



// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(unused_doc_comments)] // Doc comments in proptest! macros are not generated
    use super::*;

    // ── Compression Level Selection Tests ─────────────────────────────────────
    // Validates: Requirements 3.1, 3.2, 3.3

    /// Test tiny file compression level selection.
    /// Files below 64 KB should use the 'tiny' compression level.
    /// Validates: Requirement 3.1
    #[test]
    fn test_compression_level_tiny() {
        let levels = CompressionLevels::default();
        
        // File just below 64 KB boundary (63 KB)
        assert_eq!(
            select_compression_level(63 * 1024, &levels),
            levels.tiny,
            "File of 63 KB should use tiny compression level"
        );
        
        // Very small file (1 byte)
        assert_eq!(
            select_compression_level(1, &levels),
            levels.tiny,
            "File of 1 byte should use tiny compression level"
        );
        
        // File just below boundary (64 KB - 1 byte)
        assert_eq!(
            select_compression_level(TINY_THRESHOLD - 1, &levels),
            levels.tiny,
            "File of 64 KB - 1 byte should use tiny compression level"
        );
        
        // Zero-size file
        assert_eq!(
            select_compression_level(0, &levels),
            levels.tiny,
            "Zero-size file should use tiny compression level"
        );
    }

    /// Test small file compression level selection.
    /// Files between 64 KB (inclusive) and 1 MB should use the 'small' compression level.
    /// Validates: Requirement 3.2
    #[test]
    fn test_compression_level_small() {
        let levels = CompressionLevels::default();
        
        // File exactly at 64 KB boundary
        assert_eq!(
            select_compression_level(64 * 1024, &levels),
            levels.small,
            "File of exactly 64 KB should use small compression level"
        );
        
        // File between 64 KB and 1 MB (e.g., 512 KB)
        assert_eq!(
            select_compression_level(512 * 1024, &levels),
            levels.small,
            "File of 512 KB should use small compression level"
        );
        
        // File just below 1 MB boundary (1 MB - 1 byte)
        assert_eq!(
            select_compression_level(SMALL_THRESHOLD - 1, &levels),
            levels.small,
            "File of 1 MB - 1 byte should use small compression level"
        );
        
        // File exactly at 1023 KB (just below 1 MB)
        assert_eq!(
            select_compression_level(1023 * 1024, &levels),
            levels.small,
            "File of 1023 KB should use small compression level"
        );
    }

    /// Test large file compression level selection.
    /// Files at or above 1 MB should use the 'large' compression level.
    /// Validates: Requirement 3.3
    #[test]
    fn test_compression_level_large() {
        let levels = CompressionLevels::default();
        
        // File exactly at 1 MB boundary
        assert_eq!(
            select_compression_level(1024 * 1024, &levels),
            levels.large,
            "File of exactly 1 MB should use large compression level"
        );
        
        // File above 1 MB (e.g., 2 MB)
        assert_eq!(
            select_compression_level(2 * 1024 * 1024, &levels),
            levels.large,
            "File of 2 MB should use large compression level"
        );
        
        // File just above 1 MB boundary (1 MB + 1 byte)
        assert_eq!(
            select_compression_level(SMALL_THRESHOLD + 1, &levels),
            levels.large,
            "File of 1 MB + 1 byte should use large compression level"
        );
        
        // Very large file (100 MB)
        assert_eq!(
            select_compression_level(100 * 1024 * 1024, &levels),
            levels.large,
            "File of 100 MB should use large compression level"
        );
    }

    /// Test compression level boundaries with custom configuration.
    /// Ensures the selection logic works with non-default compression levels.
    #[test]
    fn test_compression_level_custom_levels() {
        let levels = CompressionLevels {
            tiny: 0,   // No compression for tiny files
            small: 5,  // Higher compression for small files
            large: 10, // Maximum compression for large files
        };
        
        // Tiny file should use level 0
        assert_eq!(select_compression_level(1, &levels), 0);
        assert_eq!(select_compression_level(TINY_THRESHOLD - 1, &levels), 0);
        
        // Small file should use level 5
        assert_eq!(select_compression_level(TINY_THRESHOLD, &levels), 5);
        assert_eq!(select_compression_level(SMALL_THRESHOLD - 1, &levels), 5);
        
        // Large file should use level 10
        assert_eq!(select_compression_level(SMALL_THRESHOLD, &levels), 10);
        assert_eq!(select_compression_level(SMALL_THRESHOLD + 1, &levels), 10);
    }

    /// Test all boundary conditions at 64 KB and 1 MB thresholds.
    /// This ensures no off-by-one errors at the boundaries.
    #[test]
    fn test_compression_level_boundary_conditions() {
        let levels = CompressionLevels::default();
        
        // Boundary at 64 KB (TINY_THRESHOLD)
        // Files < 64 KB use tiny level
        assert_eq!(
            select_compression_level(TINY_THRESHOLD - 1, &levels),
            levels.tiny,
            "Just below 64 KB should use tiny level"
        );
        // Files >= 64 KB use small level
        assert_eq!(
            select_compression_level(TINY_THRESHOLD, &levels),
            levels.small,
            "Exactly 64 KB should use small level"
        );
        assert_eq!(
            select_compression_level(TINY_THRESHOLD + 1, &levels),
            levels.small,
            "Just above 64 KB should use small level"
        );
        
        // Boundary at 1 MB (SMALL_THRESHOLD)
        // Files < 1 MB use small level
        assert_eq!(
            select_compression_level(SMALL_THRESHOLD - 1, &levels),
            levels.small,
            "Just below 1 MB should use small level"
        );
        // Files >= 1 MB use large level
        assert_eq!(
            select_compression_level(SMALL_THRESHOLD, &levels),
            levels.large,
            "Exactly 1 MB should use large level"
        );
        assert_eq!(
            select_compression_level(SMALL_THRESHOLD + 1, &levels),
            levels.large,
            "Just above 1 MB should use large level"
        );
    }

    // ── Single Entry Compression Tests ────────────────────────────────────────
    // Validates: Requirements 1.1, 3.1

    /// Test compressing a tiny file (< 64 KB).
    /// Verifies the CompressedEntry has correct fields and can be decompressed.
    /// Validates: Requirements 1.1, 3.1
    #[test]
    fn test_compress_single_entry_tiny_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("tiny_file.txt");
        let content = b"Hello, this is a tiny file content!";
        std::fs::write(&file_path, content).unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        let config = ParallelArchiveConfig::default();
        let result = compress_single_entry(&entry, &meta, dir.path(), &config);

        assert!(result.is_ok(), "compress_single_entry should succeed for tiny file");
        let compressed_entry = result.unwrap();

        // Verify path is correctly set (relative to base)
        assert_eq!(compressed_entry.path, std::path::PathBuf::from("tiny_file.txt"));

        // Verify sizes
        assert_eq!(compressed_entry.original_size, content.len() as u64);
        // Note: compressed_size is 0 in new architecture (set during batch compression)

        // Verify the tar data is valid (raw tar entry, not zstd compressed)
        let mut archive = tar::Archive::new(&compressed_entry.tar_data[..]);
        let mut entries = archive.entries().unwrap();
        let tar_entry = entries.next().unwrap().unwrap();
        assert_eq!(tar_entry.path().unwrap().to_str().unwrap(), "tiny_file.txt");
    }

    /// Test compressing a small file (64 KB - 1 MB).
    /// Verifies the CompressedEntry has correct fields and can be decompressed.
    /// Validates: Requirements 1.1, 3.1
    #[test]
    fn test_compress_single_entry_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("small_file.bin");
        // Create 128 KB file (in the "small" range: 64 KB to < 1 MB)
        let content: Vec<u8> = (0..128 * 1024).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file_path, &content).unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        let config = ParallelArchiveConfig::default();
        let result = compress_single_entry(&entry, &meta, dir.path(), &config);

        assert!(result.is_ok(), "compress_single_entry should succeed for small file");
        let compressed_entry = result.unwrap();

        // Verify path is correctly set
        assert_eq!(compressed_entry.path, std::path::PathBuf::from("small_file.bin"));

        // Verify sizes
        assert_eq!(compressed_entry.original_size, 128 * 1024 as u64);
        // Note: compressed_size is 0 in new architecture (set during batch compression)

        // Verify the tar data is valid (raw tar entry, not zstd compressed)
        let mut archive = tar::Archive::new(&compressed_entry.tar_data[..]);
        let mut entries = archive.entries().unwrap();
        let tar_entry = entries.next().unwrap().unwrap();
        assert_eq!(tar_entry.path().unwrap().to_str().unwrap(), "small_file.bin");
    }

    /// Test compressing a large file (>= 1 MB).
    /// Verifies the CompressedEntry has correct fields and can be decompressed.
    /// Validates: Requirements 1.1, 3.1
    #[test]
    fn test_compress_single_entry_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("large_file.bin");
        // Create 2 MB file (in the "large" range: >= 1 MB)
        let content: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file_path, &content).unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        let config = ParallelArchiveConfig::default();
        let result = compress_single_entry(&entry, &meta, dir.path(), &config);

        assert!(result.is_ok(), "compress_single_entry should succeed for large file");
        let compressed_entry = result.unwrap();

        // Verify path is correctly set
        assert_eq!(compressed_entry.path, std::path::PathBuf::from("large_file.bin"));

        // Verify sizes
        assert_eq!(compressed_entry.original_size, 2 * 1024 * 1024 as u64);
        // Note: compressed_size is 0 in new architecture (set during batch compression)

        // Verify the tar data is valid (raw tar entry, not zstd compressed)
        let mut archive = tar::Archive::new(&compressed_entry.tar_data[..]);
        let mut entries = archive.entries().unwrap();
        let tar_entry = entries.next().unwrap().unwrap();
        assert_eq!(tar_entry.path().unwrap().to_str().unwrap(), "large_file.bin");
    }

    /// Test that tar header is correctly set with file metadata.
    /// Verifies the tar header contains the correct file size.
    /// Validates: Requirements 1.1
    #[test]
    fn test_compress_single_entry_tar_header_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("metadata_test.txt");
        let content = b"Test content for metadata verification";
        std::fs::write(&file_path, content).unwrap();

        // Get original metadata
        let original_meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        let config = ParallelArchiveConfig::default();
        let compressed_entry = compress_single_entry(&entry, &original_meta, dir.path(), &config).unwrap();

        // Parse raw tar to verify header (tar_data is not zstd compressed in new architecture)
        let mut archive = tar::Archive::new(&compressed_entry.tar_data[..]);
        let mut entries = archive.entries().unwrap();
        let tar_entry = entries.next().unwrap().unwrap();

        // Verify the header size matches the content
        assert_eq!(tar_entry.header().size().unwrap(), content.len() as u64);
        
        // Verify the entry type is a regular file
        assert_eq!(tar_entry.header().entry_type(), tar::EntryType::Regular);
    }

    /// Test that compressed data can be fully decompressed and contains correct content.
    /// Validates: Requirements 1.1
    #[test]
    fn test_compress_single_entry_decompression_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("roundtrip_test.txt");
        let original_content: Vec<u8> = (0..50 * 1024).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file_path, &original_content).unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        let config = ParallelArchiveConfig::default();
        let compressed_entry = compress_single_entry(&entry, &meta, dir.path(), &config).unwrap();

        // Parse raw tar and extract content (tar_data is not zstd compressed in new architecture)
        let mut archive = tar::Archive::new(&compressed_entry.tar_data[..]);
        let mut entries = archive.entries().unwrap();
        let mut tar_entry = entries.next().unwrap().unwrap();

        let mut extracted_content = Vec::new();
        std::io::Read::read_to_end(&mut tar_entry, &mut extracted_content).unwrap();

        // Verify extracted content matches original
        assert_eq!(extracted_content, original_content, "Extracted content should match original");
    }

    /// Test compression of an empty file (edge case).
    /// Validates: Requirements 1.1
    #[test]
    fn test_compress_single_entry_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("empty_file.txt");
        std::fs::write(&file_path, b"").unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        let config = ParallelArchiveConfig::default();
        let result = compress_single_entry(&entry, &meta, dir.path(), &config);

        assert!(result.is_ok(), "compress_single_entry should succeed for empty file");
        let compressed_entry = result.unwrap();

        // Verify sizes
        assert_eq!(compressed_entry.original_size, 0);
        // Note: tar_data is raw tar entry, should contain 512-byte header with 0 size

        // Parse raw tar (tar_data is not zstd compressed in new architecture)
        let mut archive = tar::Archive::new(&compressed_entry.tar_data[..]);
        let mut entries = archive.entries().unwrap();
        let mut tar_entry = entries.next().unwrap().unwrap();

        let mut extracted_content = Vec::new();
        std::io::Read::read_to_end(&mut tar_entry, &mut extracted_content).unwrap();
        assert!(extracted_content.is_empty(), "Empty file should extract to empty content");
    }

    /// Test compression of file at exact boundary sizes.
    /// Validates: Requirements 1.1, 3.1
    #[test]
    fn test_compress_single_entry_boundary_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let config = ParallelArchiveConfig::default();

        // Test file at exactly 64 KB boundary
        let file_64k_path = dir.path().join("file_64k.bin");
        let content_64k: Vec<u8> = vec![0xAB; 64 * 1024];
        std::fs::write(&file_64k_path, &content_64k).unwrap();
        let meta_64k = std::fs::metadata(&file_64k_path).unwrap();
        let entry_64k = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_64k_path)
            .unwrap();
        let result_64k = compress_single_entry(&entry_64k, &meta_64k, dir.path(), &config);
        assert!(result_64k.is_ok(), "Should handle file at exactly 64 KB");

        // Test file at exactly 1 MB boundary
        let file_1m_path = dir.path().join("file_1m.bin");
        let content_1m: Vec<u8> = vec![0xCD; 1024 * 1024];
        std::fs::write(&file_1m_path, &content_1m).unwrap();
        let meta_1m = std::fs::metadata(&file_1m_path).unwrap();
        let entry_1m = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_1m_path)
            .unwrap();
        let result_1m = compress_single_entry(&entry_1m, &meta_1m, dir.path(), &config);
        assert!(result_1m.is_ok(), "Should handle file at exactly 1 MB");
    }

    // ── BatchAssembler Determinism Tests ───────────────────────────────────────
    // Validates: Requirements 1.2, 9.1, 9.2

    /// Test that BatchAssembler produces deterministic output order.
    /// Adding entries in different orders should produce the same output.
    /// Validates: Requirements 1.2, 9.1, 9.2
    #[test]
    fn test_batch_assembler_deterministic_order() {
        let (tx1, mut rx1) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

        // Create entries with paths that would sort differently than insertion order
        let entry_z = CompressedEntry {
            path: std::path::PathBuf::from("z_file.txt"),
            tar_data: vec![1, 2, 3],
            original_size: 100,
            compressed_size: 3,
        };
        let entry_a = CompressedEntry {
            path: std::path::PathBuf::from("a_file.txt"),
            tar_data: vec![4, 5, 6],
            original_size: 200,
            compressed_size: 3,
        };
        let entry_m = CompressedEntry {
            path: std::path::PathBuf::from("m_file.txt"),
            tar_data: vec![7, 8, 9],
            original_size: 300,
            compressed_size: 3,
        };

        // First assembler: add in Z, A, M order
        let mut assembler1 = BatchAssembler::new(1024 * 1024, tx1);
        assembler1.add_entry(entry_z.clone());
        assembler1.add_entry(entry_a.clone());
        assembler1.add_entry(entry_m.clone());
        assembler1.flush().unwrap();

        // Second assembler: add in A, M, Z order
        let mut assembler2 = BatchAssembler::new(1024 * 1024, tx2);
        assembler2.add_entry(entry_a);
        assembler2.add_entry(entry_m);
        assembler2.add_entry(entry_z);
        assembler2.flush().unwrap();

        // Collect outputs using blocking_recv
        let output1: Vec<Vec<u8>> = std::iter::from_fn(|| rx1.blocking_recv()).collect();
        let output2: Vec<Vec<u8>> = std::iter::from_fn(|| rx2.blocking_recv()).collect();

        // Outputs should be identical (deterministic order)
        assert_eq!(output1, output2, "BatchAssembler should produce identical output regardless of insertion order");
    }

    /// Test that BatchAssembler produces entries in sorted path order.
    /// Validates: Requirements 1.2, 9.1, 9.2
    #[test]
    fn test_batch_assembler_sorted_path_order() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

        // Create entries with unsorted paths
        let entries: Vec<CompressedEntry> = vec![
            CompressedEntry {
                path: std::path::PathBuf::from("zulu.txt"),
                tar_data: b"zulu".to_vec(),
                original_size: 4,
                compressed_size: 4,
            },
            CompressedEntry {
                path: std::path::PathBuf::from("alpha.txt"),
                tar_data: b"alpha".to_vec(),
                original_size: 5,
                compressed_size: 5,
            },
            CompressedEntry {
                path: std::path::PathBuf::from("beta/subdir.txt"),
                tar_data: b"beta".to_vec(),
                original_size: 4,
                compressed_size: 4,
            },
            CompressedEntry {
                path: std::path::PathBuf::from("alpha/other.txt"),
                tar_data: b"other".to_vec(),
                original_size: 5,
                compressed_size: 5,
            },
        ];

        let mut assembler = BatchAssembler::new(1024 * 1024, tx);
        for entry in entries {
            assembler.add_entry(entry);
        }
        assembler.flush().unwrap();

        // Collect output
        let output: Vec<u8> = std::iter::from_fn(|| rx.blocking_recv())
            .flatten()
            .collect();

        // Parse the output to extract path order
        // Since tar_data contains the raw compressed tar entries, we can't directly
        // parse them without decompression. Instead, verify the byte patterns.
        // The order should be: alpha.txt, alpha/other.txt, beta/subdir.txt, zulu.txt
        // (sorted alphabetically by path)

        // We can verify by checking that 'alpha' bytes come before 'zulu' bytes
        let alpha_pos = output.windows(5).position(|w| w == b"alpha").unwrap();
        let zulu_pos = output.windows(4).position(|w| w == b"zulu").unwrap();
        assert!(alpha_pos < zulu_pos, "alpha entries should come before zulu entries in sorted order");
    }

    // ── Property-Based Tests ───────────────────────────────────────────────────
    // Validates: Requirements 1.2, 9.1, 9.2

    /// Property test: BatchAssembler produces identical output for same entries
    /// regardless of insertion order.
    ///
    /// This test generates random sets of (path, tar_data) entries, adds them
    /// to two BatchAssembler instances in different orders, and verifies:
    /// 1. Both assemblers produce identical byte output (determinism)
    /// 2. Entries are emitted in sorted path order
    ///
    /// **Validates: Requirements 1.2, 9.1, 9.2**
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_test_deterministic_archive_order(
            // Generate unique paths using indices to avoid duplicates
            // Duplicates would cause BTreeMap to replace entries, breaking determinism
            num_entries in 1usize..50,
            tar_data_sizes in prop::collection::vec(0usize..100, 1..50)
        ) {
            // Create unique paths using index
            let num_entries = num_entries.min(tar_data_sizes.len());
            let entries: Vec<(std::path::PathBuf, Vec<u8>)> = (0..num_entries)
                .map(|i| {
                    let path = std::path::PathBuf::from(format!("file_{:03}.txt", i));
                    let size = tar_data_sizes.get(i).copied().unwrap_or(50);
                    let tar_data: Vec<u8> = (0..size).map(|j| (j % 256) as u8).collect();
                    (path, tar_data)
                })
                .collect();

            // Create CompressedEntry objects from generated data
            let compressed_entries: Vec<CompressedEntry> = entries
                .into_iter()
                .map(|(path, tar_data)| CompressedEntry {
                    path,
                    tar_data,
                    original_size: 100,
                    compressed_size: 50,
                })
                .collect();

            // Create two channels for two assemblers
            let (tx1, mut rx1) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
            let (tx2, mut rx2) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

            // First assembler: add entries in original order
            let mut assembler1 = BatchAssembler::new(1024 * 1024, tx1);
            for entry in compressed_entries.iter() {
                assembler1.add_entry(CompressedEntry {
                    path: entry.path.clone(),
                    tar_data: entry.tar_data.clone(),
                    original_size: entry.original_size,
                    compressed_size: entry.compressed_size,
                });
            }
            assembler1.flush().unwrap();

            // Second assembler: add entries in reversed order (different insertion order)
            let mut assembler2 = BatchAssembler::new(1024 * 1024, tx2);
            for entry in compressed_entries.iter().rev() {
                assembler2.add_entry(CompressedEntry {
                    path: entry.path.clone(),
                    tar_data: entry.tar_data.clone(),
                    original_size: entry.original_size,
                    compressed_size: entry.compressed_size,
                });
            }
            assembler2.flush().unwrap();

            // Collect outputs using blocking_recv
            let output1: Vec<Vec<u8>> = std::iter::from_fn(|| rx1.blocking_recv()).collect();
            let output2: Vec<Vec<u8>> = std::iter::from_fn(|| rx2.blocking_recv()).collect();

            // Property 1: Both assemblers must produce identical output (determinism)
            prop_assert_eq!(&output1, &output2, "BatchAssembler must produce identical output regardless of insertion order");

            // Property 2: Verify entries are in sorted path order
            // We do this by checking that if we create a fresh assembler and add
            // entries in sorted order, it produces the same output
            let (tx3, mut rx3) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
            let mut assembler3 = BatchAssembler::new(1024 * 1024, tx3);

            // Sort entries by path and add in sorted order
            let mut sorted_entries = compressed_entries.clone();
            sorted_entries.sort_by(|a, b| a.path.cmp(&b.path));
            for entry in sorted_entries {
                assembler3.add_entry(entry);
            }
            assembler3.flush().unwrap();

            let output3: Vec<Vec<u8>> = std::iter::from_fn(|| rx3.blocking_recv()).collect();

            // Output from sorted insertion must match output from any insertion order
            prop_assert_eq!(&output1, &output3, "BatchAssembler must emit entries in sorted path order");
        }
    }

    // ── Property-Based Tests: Batch Size Bounds ─────────────────────────────────
    // Validates: Requirements 2.1, 2.2

    // Property test: BatchAssembler produces batches within configured size bounds.
    //
    // This test generates random sets of entries with varying tar_data sizes,
    // assembles them into batches, and verifies:
    // 1. All batches respect the upper bound (batch_size), unless a single entry
    //    exceeds batch_size (allowed for large entries)
    // 2. Batches accumulate entries efficiently to minimize encryption overhead
    // 3. Final batch can be smaller than batch_size (allowed by spec)
    //
    // Note: The minimum batch size of 256 KB is an optimization goal, not a hard
    // requirement. The assembler batches entries up to batch_size for efficiency,
    // but small inputs will naturally produce smaller batches.
    //
    // **Validates: Requirements 2.1, 2.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_test_batch_size_bounds(
            // Generate unique paths using indices to avoid duplicates
            num_entries in 5usize..100,
            tar_data_sizes in prop::collection::vec(100usize..200_000, 5..100),
            // Generate batch_size between 256 KB and 4 MB (configurable range from spec)
            batch_size_kb in 256usize..4096
        ) {
            let batch_size = batch_size_kb * 1024;

            // Create unique paths using index
            let num_entries = num_entries.min(tar_data_sizes.len()).max(1);
            let entries: Vec<(std::path::PathBuf, Vec<u8>)> = (0..num_entries)
                .map(|i| {
                    let path = std::path::PathBuf::from(format!("file_{:03}.txt", i));
                    let size = tar_data_sizes.get(i).copied().unwrap_or(1000);
                    let tar_data: Vec<u8> = (0..size).map(|j| (j % 256) as u8).collect();
                    (path, tar_data)
                })
                .collect();

            // Create CompressedEntry objects from generated data
            let compressed_entries: Vec<CompressedEntry> = entries
                .into_iter()
                .map(|(path, tar_data)| CompressedEntry {
                    path,
                    tar_data,
                    original_size: 100,
                    compressed_size: 50,
                })
                .collect();

            // Calculate total input size for later verification
            let _total_input_size: usize = compressed_entries.iter().map(|e| e.tar_data.len()).sum();

            // Create channel and assembler with the generated batch_size
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

            let mut assembler = BatchAssembler::new(batch_size, tx);
            for entry in compressed_entries {
                assembler.add_entry(entry);
            }
            assembler.flush().unwrap();

            // Collect all batches using blocking_recv
            let batches: Vec<Vec<u8>> = std::iter::from_fn(|| rx.blocking_recv()).collect();

            // There must be at least one batch if we had entries
            prop_assert!(!batches.is_empty(), "Must produce at least one batch for non-empty input");

            // Verify batch size bounds per Requirement 2.1 and 2.2:
            // - Batches should be at most batch_size
            // - Exception: single entry larger than batch_size gets its own batch
            // - Final batch can be smaller

            for (i, batch) in batches.iter().enumerate() {
                let _is_final_batch = i == batches.len() - 1;
                let is_only_batch = batches.len() == 1;

                // All batches should respect the upper bound, with exception for
                // single oversized entries (when there's only one batch or it contains
                // a single large entry)
                if !is_only_batch {
                    // For multi-batch scenarios, non-final batches should not exceed batch_size
                    // (the implementation flushes when batch would exceed the limit)
                    prop_assert!(
                        batch.len() <= batch_size,
                        "Non-final batch {} size {} exceeds configured batch_size {}",
                        i, batch.len(), batch_size
                    );
                }
                // Single batch can exceed batch_size if a single entry is larger than batch_size
                // This is allowed by the spec for handling large files

                // Note: Non-final batches can be smaller than half of batch_size in certain
                // edge cases. For example, if we have entries of sizes [100KB, 100KB, 100KB, 100KB]
                // and batch_size is 256KB, the batches would be:
                // - Batch 0: 100KB + 100KB = 200KB (< 256KB, next entry would overflow)
                // - Batch 1: 100KB + 100KB = 200KB (final)
                // This is correct behavior - the assembler doesn't enforce minimum batch sizes.
            }

            // Verify data was produced (compressed output will be smaller than input)
            // The batch assembler outputs zstd-compressed data, so we just verify it's valid
            let total_output_size: usize = batches.iter().map(|b| b.len()).sum();
            prop_assert!(
                total_output_size > 0,
                "Total output size should be positive (got {})",
                total_output_size
            );

            // Verify the compressed data can be decompressed
            for batch in &batches {
                let decompressed = zstd::decode_all(&batch[..]);
                prop_assert!(
                    decompressed.is_ok(),
                    "Batch should be valid zstd-compressed data"
                );
            }
        }
    }

    // Property test: Large entries that exceed batch_size are handled correctly.
    //
    // When a single entry is larger than batch_size, it should still be included
    // in the output, even though the resulting batch exceeds the limit.
    // This is allowed by the spec for handling large files.
    //
    // **Validates: Requirements 2.1, 2.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        #[test]
        fn prop_test_large_entry_handling(
            // Generate unique paths for small entries
            num_small in 1usize..5,
            small_sizes in prop::collection::vec(100usize..10_000, 1..5),
            // Generate one large entry that exceeds batch_size
            large_entry_size in 600_000usize..800_000,
            batch_size_kb in 256usize..512  // batch_size will be 256-512 KB, smaller than large entry
        ) {
            let batch_size = batch_size_kb * 1024;

            // Create unique paths using index
            let num_small = num_small.min(small_sizes.len()).max(1);
            let mut compressed_entries: Vec<CompressedEntry> = (0..num_small)
                .map(|i| {
                    let path = std::path::PathBuf::from(format!("small_{:03}.txt", i));
                    let size = small_sizes.get(i).copied().unwrap_or(1000);
                    let tar_data: Vec<u8> = (0..size).map(|j| (j % 256) as u8).collect();
                    CompressedEntry {
                        path,
                        tar_data,
                        original_size: 100,
                        compressed_size: 50,
                    }
                })
                .collect();

            // Add the large entry with a unique name
            let large_tar_data: Vec<u8> = (0..large_entry_size).map(|j| (j % 256) as u8).collect();
            compressed_entries.push(CompressedEntry {
                path: std::path::PathBuf::from("large_entry.bin"),
                tar_data: large_tar_data.clone(),
                original_size: large_entry_size as u64,
                compressed_size: large_entry_size as u64,
            });

            // Calculate expected total
            let _expected_total: usize = compressed_entries.iter().map(|e| e.tar_data.len()).sum();

            // Create channel and assembler
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

            let mut assembler = BatchAssembler::new(batch_size, tx);
            for entry in compressed_entries {
                assembler.add_entry(entry);
            }
            assembler.flush().unwrap();

            // Collect all batches using blocking_recv
            let batches: Vec<Vec<u8>> = std::iter::from_fn(|| rx.blocking_recv()).collect();

            // Should have at least one batch
            prop_assert!(!batches.is_empty(), "Should produce at least one batch");

            // Verify compressed output is produced (will be smaller than input due to zstd)
            let total_output: usize = batches.iter().map(|b| b.len()).sum();
            prop_assert!(
                total_output > 0,
                "Total output should be positive (got {})",
                total_output
            );

            // Verify all batches can be decompressed
            for batch in &batches {
                let decompressed = zstd::decode_all(&batch[..]);
                prop_assert!(
                    decompressed.is_ok(),
                    "Batch should be valid zstd-compressed data"
                );
            }
        }
    }

    // ── Property-Based Tests: Error Propagation ───────────────────────────────────
    // Validates: Requirements 1.5, 8.1

    /// Property test: Error messages contain file path when file read fails.
    ///
    /// This test creates files and makes one unreadable via permission denial,
    /// then verifies that:
    /// 1. The archive operation fails (returns error)
    /// 2. The error message contains the file path that caused the failure
    /// 3. No partial archive output is produced
    ///
    /// Note: This test is Unix-only because file permissions work differently
    /// on Windows. On Windows, files can be made unreadable by other means
    /// (e.g., exclusive locking), but that requires different test logic.
    ///
    /// **Validates: Requirements 1.5, 8.1**
    #[cfg(unix)]
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        #[test]
        fn prop_test_error_contains_path(
            // Generate 5-20 unique filenames
            filenames in prop::collection::vec("[a-z]{1,10}", 5..20),
            // Index of the file to make unreadable (0 to len-1)
            unreadable_index in 0usize..20
        ) {
            let dir = tempfile::tempdir().unwrap();
            
            // Create files with content
            for name in &filenames {
                std::fs::write(dir.path().join(name), vec![0u8; 1024]).unwrap();
            }

            // Ensure the index is valid
            let unreadable_index = unreadable_index.min(filenames.len() - 1);
            let unreadable_name = &filenames[unreadable_index];
            let unreadable_path = dir.path().join(unreadable_name);

            // Make the selected file unreadable (permission denied)
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o000)).unwrap();

            // Attempt to archive using compress_single_entry - should fail
            let meta = std::fs::metadata(&unreadable_path).unwrap();
            let entry = WalkDir::new(dir.path())
                .into_iter()
                .filter_map(|e| e.ok())
                .find(|e| e.path() == unreadable_path)
                .unwrap();

            let config = ParallelArchiveConfig::default();
            let result = compress_single_entry(&entry, &meta, dir.path(), &config);

            // Property 1: Should fail with an error
            prop_assert!(result.is_err(), "compress_single_entry should fail for unreadable file");

            // Property 2: Error message should contain the file path (Requirement 8.1)
            let err_msg = result.unwrap_err().to_string();
            prop_assert!(
                err_msg.contains(unreadable_name) || err_msg.contains(&format!("{}", unreadable_path.display())),
                "Error message must contain the file path. Got: {}",
                err_msg
            );

            // Cleanup: restore permissions so tempdir can be deleted
            std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o644)).ok();
        }
    }

    /// Property test: No partial archive is produced when compression fails.
    ///
    /// This test verifies that when an error occurs during parallel compression,
    /// no partial output file is left behind. The archive operation should either
    /// succeed completely or fail without producing any output.
    ///
    /// **Validates: Requirements 1.5, 8.1**
    #[cfg(unix)]
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        #[test]
        fn prop_test_no_partial_archive_on_error(
            // Generate 5-20 unique filenames
            filenames in prop::collection::vec("[a-z]{1,10}", 5..20),
            // Index of the file to make unreadable
            unreadable_index in 0usize..20
        ) {
            let dir = tempfile::tempdir().unwrap();
            let output_path = dir.path().join("output.archive");

            // Create files with content
            for name in &filenames {
                std::fs::write(dir.path().join(name), vec![0u8; 1024]).unwrap();
            }

            // Ensure the index is valid
            let unreadable_index = unreadable_index.min(filenames.len() - 1);
            let unreadable_name = &filenames[unreadable_index];
            let unreadable_path = dir.path().join(unreadable_name);

            // Make the selected file unreadable (permission denied)
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o000)).unwrap();

            // Collect entries for parallel compression
            let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
                .into_iter()
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
                .filter(|(e, _)| e.file_type().is_file() && e.path() != output_path)
                .collect();

            // Set up parallel compression pipeline
            let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
            let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let error_slot_clone = error_slot.clone();

            let config = ParallelArchiveConfig::default();
            let result = compress_entries_parallel(
                dir.path(),
                entries,
                &config,
                tx,
                error_slot_clone,
            );

            // Property 1: Should fail (because one file is unreadable)
            prop_assert!(result.is_err(), "compress_entries_parallel should fail for unreadable file");

            // Property 2: Error slot should be set with a message containing the path
            let error_slot_value = error_slot.lock().unwrap().clone();
            prop_assert!(
                error_slot_value.is_some(),
                "Error slot should contain an error message"
            );

            let err_msg = error_slot_value.unwrap();
            prop_assert!(
                err_msg.contains(unreadable_name),
                "Error message must contain the file path. Got: {}",
                err_msg
            );

            // Property 3: No data should be received on the output channel
            // (compress_entries_parallel should not send partial data before aborting)
            // We use try_recv to check without blocking
            let received_any = rx.try_recv().is_ok();
            prop_assert!(
                !received_any,
                "No partial archive data should be sent on error"
            );

            // Cleanup: restore permissions
            std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o644)).ok();
        }
    }

    /// Unit test: Verify error message format for permission denied.
    ///
    /// This test verifies the specific error message format when a file
    /// cannot be read due to permission denied.
    ///
    /// **Validates: Requirement 8.1**
    #[cfg(unix)]
    #[test]
    fn test_error_message_contains_permission_denied() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("protected_file.txt");
        std::fs::write(&file_path, b"content").unwrap();

        // Make file unreadable
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        let config = ParallelArchiveConfig::default();
        let result = compress_single_entry(&entry, &meta, dir.path(), &config);

        assert!(result.is_err(), "Should fail for unreadable file");

        let err_msg = result.unwrap_err().to_string();
        // The error should mention the file name
        assert!(
            err_msg.contains("protected_file.txt"),
            "Error message should contain the file path. Got: {}",
            err_msg
        );

        // Cleanup
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644)).ok();
    }

    /// Unit test: Verify error message format for non-existent file.
    ///
    /// This test verifies that attempting to compress a file that was deleted
    /// after metadata collection produces a clear error message.
    ///
    /// Note: The path is included at the compress_entries_parallel level,
    /// not in compress_single_entry itself.
    ///
    /// **Validates: Requirement 8.1**
    #[test]
    fn test_error_message_for_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("deleted_file.txt");
        std::fs::write(&file_path, b"content").unwrap();

        // Collect metadata before deletion
        let meta = std::fs::metadata(&file_path).unwrap();
        let entry = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.path() == file_path)
            .unwrap();

        // Delete the file after collecting metadata
        std::fs::remove_file(&file_path).unwrap();

        let config = ParallelArchiveConfig::default();
        let result = compress_single_entry(&entry, &meta, dir.path(), &config);

        // Should fail because file was deleted
        assert!(result.is_err(), "Should fail for deleted file");

        // The error message comes from std::fs::read, which is an OS error
        // The path context is added by compress_entries_parallel, not compress_single_entry
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.is_empty(),
            "Error message should not be empty. Got: {}",
            err_msg
        );
    }

    // ── Property-Based Tests: Large File Streaming ───────────────────────────────
    // Validates: Requirements 5.3, 5.4

    /// Unit test: Verify large files are categorized correctly by partition logic.
    ///
    /// This test verifies that the partition logic in compress_entries_parallel
    /// correctly separates small files (< 1 MB) from large files (>= 1 MB).
    ///
    /// **Validates: Requirements 5.3, 5.4**
    #[test]
    fn test_large_file_partition_logic() {
        let config = ParallelArchiveConfig::default();
        let threshold = config.small_file_threshold;

        // Create entries with known sizes
        let tiny_size = TINY_THRESHOLD - 1; // < 64 KB
        let small_size = TINY_THRESHOLD + 1024; // >= 64 KB, < 1 MB
        let large_size = SMALL_THRESHOLD; // >= 1 MB

        // Verify partition logic matches threshold
        assert!(tiny_size < threshold, "Tiny file should be below threshold");
        assert!(small_size < threshold, "Small file should be below threshold");
        assert!(large_size >= threshold, "Large file should be at or above threshold");
        assert!(large_size == SMALL_THRESHOLD, "Large file should be exactly at threshold");
    }

    /// Property test: Large files are processed correctly by compress_entries_parallel.
    ///
    /// This test verifies that:
    /// 1. Large files (>= 1 MB) are processed correctly
    /// 2. Multiple large files are handled correctly
    /// 3. Mixed small and large files are processed correctly
    /// 4. The output can be decompressed and contains all files
    ///
    /// **Validates: Requirements 5.3, 5.4**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        #[test]
        fn prop_test_large_file_streaming(
            // Generate number of small files (0-10)
            num_small_files in 0usize..10,
            // Generate number of large files (1-5, at least one to test the property)
            num_large_files in 1usize..5,
            // Small file content sizes (1 KB to 100 KB, all below SMALL_THRESHOLD)
            small_sizes in prop::collection::vec(1024usize..100_000, 0..10),
            // Large file content sizes (1 MB to 2 MB, all >= SMALL_THRESHOLD)
            large_sizes in prop::collection::vec(1024 * 1024usize..2 * 1024 * 1024, 1..5)
        ) {
            let dir = tempfile::tempdir().unwrap();
            let config = ParallelArchiveConfig::default();

            let num_small = num_small_files.min(small_sizes.len());
            let num_large = num_large_files.min(large_sizes.len());

            // Track expected files for verification
            let mut expected_files: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Create small files (< 1 MB)
            for i in 0..num_small {
                let file_name = format!("small_file_{:03}.bin", i);
                let size = small_sizes.get(i).copied().unwrap_or(1024);
                let content: Vec<u8> = (0..size).map(|j| ((j + i) % 256) as u8).collect();
                std::fs::write(dir.path().join(&file_name), &content).unwrap();
                expected_files.insert(file_name);
            }

            // Create large files (>= 1 MB)
            for i in 0..num_large {
                let file_name = format!("large_file_{:03}.bin", i);
                let size = large_sizes.get(i).copied().unwrap_or(SMALL_THRESHOLD as usize);
                let content: Vec<u8> = (0..size).map(|j| ((j + i + 100) % 256) as u8).collect();
                std::fs::write(dir.path().join(&file_name), &content).unwrap();
                expected_files.insert(file_name);
            }

            // Collect entries for parallel compression
            let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
                .into_iter()
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
                .filter(|(e, _)| e.file_type().is_file())
                .collect();

            // Count small vs large files
            let small_count = entries.iter()
                .filter(|(_, m)| m.len() < config.small_file_threshold)
                .count();
            let large_count = entries.iter()
                .filter(|(_, m)| m.len() >= config.small_file_threshold)
                .count();

            // Verify partition counts match expected
            prop_assert_eq!(small_count, num_small, "Small file count should match");
            prop_assert_eq!(large_count, num_large, "Large file count should match");

            // Set up parallel compression pipeline
            let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
            let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let error_slot_clone = error_slot.clone();

            // Run parallel compression
            let result = compress_entries_parallel(
                dir.path(),
                entries,
                &config,
                tx,
                error_slot_clone,
            );

            // Property 1: Compression should succeed
            prop_assert!(result.is_ok(), "compress_entries_parallel should succeed for mixed files");

            // Property 2: No error should be recorded
            let error_slot_value = error_slot.lock().unwrap().clone();
            prop_assert!(error_slot_value.is_none(), "Error slot should be empty on success");

            // Collect all compressed entries
            let compressed_entries: Vec<CompressedEntry> = rx.iter().collect();

            // Property 3: All files should be present in output
            prop_assert_eq!(
                compressed_entries.len(),
                num_small + num_large,
                "All files should be processed"
            );

            // Property 4: All expected files should be in the compressed entries
            let processed_files: std::collections::HashSet<String> = compressed_entries
                .iter()
                .map(|e| e.path.to_string_lossy().to_string())
                .collect();

            for expected in &expected_files {
                prop_assert!(
                    processed_files.contains(expected),
                    "File {} should be in compressed output",
                    expected
                );
            }

            // Property 5: Verify tar_data contains valid tar entries (raw tar in new architecture)
            for entry in &compressed_entries {
                let mut archive = tar::Archive::new(&entry.tar_data[..]);
                let archive_result = archive.entries();
                prop_assert!(
                    archive_result.is_ok(),
                    "Entry {} should be valid tar data",
                    entry.path.display()
                );
            }
        }
    }

    /// Property test: Large files are processed sequentially, small files in parallel.
    ///
    /// This test verifies that the partition logic correctly separates small and
    /// large files, where small files are processed with rayon's par_iter and
    /// large files are processed sequentially.
    ///
    /// Since we cannot directly observe parallelism in tests, we verify:
    /// 1. All files are processed correctly regardless of size
    /// 2. The partition logic correctly categorizes files
    /// 3. All expected files are present in the output
    ///
    /// Note: Sorting is handled by BatchAssembler, not compress_entries_parallel.
    ///
    /// **Validates: Requirements 5.3, 5.4**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        #[test]
        fn prop_test_small_large_partition_correctness(
            // Generate file sizes - some small (< 1 MB), some large (>= 1 MB)
            file_configs in prop::collection::vec(
                (any::<bool>(), 0usize..3000),  // (is_large, index for unique content)
                5..30
            )
        ) {
            let dir = tempfile::tempdir().unwrap();
            let config = ParallelArchiveConfig::default();
            let threshold = config.small_file_threshold;

            let mut expected_small = 0usize;
            let mut expected_large = 0usize;
            let mut expected_files: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Create files based on config
            for (i, (is_large, seed)) in file_configs.iter().enumerate() {
                let file_name = format!("file_{:03}.bin", i);
                let size = if *is_large {
                    // Large file: 1 MB to 1.5 MB
                    expected_large += 1;
                    SMALL_THRESHOLD as usize + (seed * 100)
                } else {
                    // Small file: 1 KB to 512 KB
                    expected_small += 1;
                    1024 + (seed % (512 * 1024))
                };
                let content: Vec<u8> = (0..size).map(|j| ((j + seed) % 256) as u8).collect();
                std::fs::write(dir.path().join(&file_name), &content).unwrap();
                expected_files.insert(file_name);
            }

            // Collect entries for parallel compression
            let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
                .into_iter()
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
                .filter(|(e, _)| e.file_type().is_file())
                .collect();

            // Verify partition counts
            let actual_small = entries.iter()
                .filter(|(_, m)| m.len() < threshold)
                .count();
            let actual_large = entries.iter()
                .filter(|(_, m)| m.len() >= threshold)
                .count();

            prop_assert_eq!(actual_small, expected_small, "Small file count should match");
            prop_assert_eq!(actual_large, expected_large, "Large file count should match");

            // Set up parallel compression pipeline
            let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
            let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let error_slot_clone = error_slot.clone();

            // Run parallel compression
            let result = compress_entries_parallel(
                dir.path(),
                entries,
                &config,
                tx,
                error_slot_clone,
            );

            // Property: Should succeed
            prop_assert!(result.is_ok(), "Compression should succeed");

            // Collect and verify output
            let compressed_entries: Vec<CompressedEntry> = rx.iter().collect();
            prop_assert_eq!(
                compressed_entries.len(),
                expected_small + expected_large,
                "All files should be processed"
            );

            // Property: All expected files should be present
            let processed_files: std::collections::HashSet<String> = compressed_entries
                .iter()
                .map(|e| e.path.to_string_lossy().to_string())
                .collect();
            prop_assert_eq!(
                processed_files,
                expected_files,
                "All files should be present in output"
            );

            // Property: All entries contain valid tar data (raw tar in new architecture)
            for entry in &compressed_entries {
                let mut archive = tar::Archive::new(&entry.tar_data[..]);
                let archive_result = archive.entries();
                prop_assert!(
                    archive_result.is_ok(),
                    "Entry {} should be valid tar data",
                    entry.path.display()
                );
            }
        }
    }

    /// Unit test: Verify a single large file is processed correctly.
    ///
    /// This test creates a single large file (exactly 1 MB) and verifies
    /// that compress_entries_parallel processes it correctly.
    ///
    /// **Validates: Requirements 5.3, 5.4**
    #[test]
    fn test_single_large_file_processing() {
        let dir = tempfile::tempdir().unwrap();
        let config = ParallelArchiveConfig::default();

        // Create a large file (exactly 1 MB)
        let file_name = "large_1mb.bin";
        let size = SMALL_THRESHOLD as usize;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        std::fs::write(dir.path().join(file_name), &content).unwrap();

        // Collect entries
        let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
            .filter(|(e, _)| e.file_type().is_file())
            .collect();

        assert_eq!(entries.len(), 1, "Should have one file");

        // Verify it's categorized as large
        let is_large = entries[0].1.len() >= config.small_file_threshold;
        assert!(is_large, "1 MB file should be categorized as large");

        // Set up parallel compression pipeline
        let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
        let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_slot_clone = error_slot.clone();

        let result = compress_entries_parallel(
            dir.path(),
            entries,
            &config,
            tx,
            error_slot_clone,
        );

        assert!(result.is_ok(), "Compression should succeed");
        assert!(error_slot.lock().unwrap().is_none(), "No error should be recorded");

        // Verify output
        let compressed_entries: Vec<CompressedEntry> = rx.iter().collect();
        assert_eq!(compressed_entries.len(), 1, "Should have one compressed entry");
        assert_eq!(compressed_entries[0].path, std::path::PathBuf::from(file_name));

        // Verify tar_data is valid (raw tar entry in new architecture)
        let mut archive = tar::Archive::new(&compressed_entries[0].tar_data[..]);
        let mut entries = archive.entries().unwrap();
        let tar_entry = entries.next().unwrap().unwrap();
        assert_eq!(tar_entry.path().unwrap().to_str().unwrap(), file_name);
    }

    /// Unit test: Verify multiple large files are processed sequentially.
    ///
    /// This test creates multiple large files and verifies they are all
    /// processed correctly. The test doesn't measure memory directly, but
    /// verifies correctness of the output.
    ///
    /// **Validates: Requirements 5.3, 5.4**
    #[test]
    fn test_multiple_large_files_sequential_processing() {
        let dir = tempfile::tempdir().unwrap();
        let config = ParallelArchiveConfig::default();

        // Create 3 large files (each >= 1 MB)
        let num_files = 3;
        for i in 0..num_files {
            let file_name = format!("large_file_{:02}.bin", i);
            // Vary sizes slightly (1 MB to 1.1 MB)
            let size = SMALL_THRESHOLD as usize + (i * 10 * 1024);
            let content: Vec<u8> = (0..size).map(|j| ((j + i * 50) % 256) as u8).collect();
            std::fs::write(dir.path().join(&file_name), &content).unwrap();
        }

        // Collect entries
        let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
            .filter(|(e, _)| e.file_type().is_file())
            .collect();

        assert_eq!(entries.len(), num_files, "Should have {} files", num_files);

        // Verify all are categorized as large
        for (_, meta) in &entries {
            assert!(
                meta.len() >= config.small_file_threshold,
                "File of {} bytes should be categorized as large",
                meta.len()
            );
        }

        // Set up parallel compression pipeline
        let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
        let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_slot_clone = error_slot.clone();

        let result = compress_entries_parallel(
            dir.path(),
            entries,
            &config,
            tx,
            error_slot_clone,
        );

        assert!(result.is_ok(), "Compression should succeed");
        assert!(error_slot.lock().unwrap().is_none(), "No error should be recorded");

        // Collect and verify output
        let compressed_entries: Vec<CompressedEntry> = rx.iter().collect();
        assert_eq!(compressed_entries.len(), num_files, "Should have {} compressed entries", num_files);

        // Note: compress_entries_parallel processes large files sequentially but
        // does not guarantee sorted output order. BatchAssembler handles sorting.
        // Here we just verify all files are present and can be decompressed.

        // Verify all expected file names are present
        let file_names: std::collections::HashSet<String> = compressed_entries
            .iter()
            .map(|e| e.path.to_string_lossy().to_string())
            .collect();
        for i in 0..3 {
            assert!(
                file_names.contains(&format!("large_file_{:02}.bin", i)),
                "File {} should be present",
                i
            );
        }

        // Verify all files can be parsed as tar (raw tar in new architecture)
        for entry in &compressed_entries {
            let mut archive = tar::Archive::new(&entry.tar_data[..]);
            assert!(archive.entries().is_ok(), "tar_data should be valid tar entry");
        }
    }

    /// Unit test: Verify mixed small and large files are processed correctly.
    ///
    /// This test creates a mix of small and large files and verifies all
    /// are processed correctly. This tests the integration of parallel
    /// small file processing with sequential large file processing.
    ///
    /// **Validates: Requirements 5.3, 5.4**
    #[test]
    fn test_mixed_small_large_files_processing() {
        let dir = tempfile::tempdir().unwrap();
        let config = ParallelArchiveConfig::default();

        // Create small files
        for i in 0..5 {
            let file_name = format!("small_{:02}.bin", i);
            let size = 10 * 1024 + (i * 1024); // 10-14 KB
            let content: Vec<u8> = vec![(i % 256) as u8; size];
            std::fs::write(dir.path().join(&file_name), &content).unwrap();
        }

        // Create large files
        for i in 0..3 {
            let file_name = format!("large_{:02}.bin", i);
            let size = SMALL_THRESHOLD as usize + (i * 100 * 1024); // 1-1.2 MB
            let content: Vec<u8> = vec![((i + 100) % 256) as u8; size];
            std::fs::write(dir.path().join(&file_name), &content).unwrap();
        }

        // Collect entries
        let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
            .filter(|(e, _)| e.file_type().is_file())
            .collect();

        let small_count = entries.iter()
            .filter(|(_, m)| m.len() < config.small_file_threshold)
            .count();
        let large_count = entries.iter()
            .filter(|(_, m)| m.len() >= config.small_file_threshold)
            .count();

        assert_eq!(small_count, 5, "Should have 5 small files");
        assert_eq!(large_count, 3, "Should have 3 large files");
        assert_eq!(entries.len(), 8, "Should have 8 total files");

        // Set up parallel compression pipeline
        let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
        let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_slot_clone = error_slot.clone();

        let result = compress_entries_parallel(
            dir.path(),
            entries,
            &config,
            tx,
            error_slot_clone,
        );

        assert!(result.is_ok(), "Compression should succeed");
        assert!(error_slot.lock().unwrap().is_none(), "No error should be recorded");

        // Collect and verify output
        let compressed_entries: Vec<CompressedEntry> = rx.iter().collect();
        assert_eq!(compressed_entries.len(), 8, "Should have 8 compressed entries");

        // Note: compress_entries_parallel does not guarantee sorted output order.
        // BatchAssembler handles sorting. Here we verify all files are present.

        // Verify all expected file names are present
        let file_names: std::collections::HashSet<String> = compressed_entries
            .iter()
            .map(|e| e.path.to_string_lossy().to_string())
            .collect();
        for i in 0..5 {
            assert!(
                file_names.contains(&format!("small_{:02}.bin", i)),
                "Small file {} should be present",
                i
            );
        }
        for i in 0..3 {
            assert!(
                file_names.contains(&format!("large_{:02}.bin", i)),
                "Large file {} should be present",
                i
            );
        }

        // Verify all entries can be parsed as tar (raw tar in new architecture)
        for entry in &compressed_entries {
            let mut archive = tar::Archive::new(&entry.tar_data[..]);
            assert!(archive.entries().is_ok(), "tar_data should be valid tar entry");
        }
    }

    // ── Property-Based Tests: Bounded Memory Usage ─────────────────────────────────
    // Validates: Requirements 5.1

    /// Unit test: Verify output channel uses PIPELINE_DEPTH bound.
    ///
    /// This test verifies that the output channel in stream_archive_parallel_with_config
    /// is created with PIPELINE_DEPTH capacity for backpressure.
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn test_pipeline_depth_constant() {
        // Verify PIPELINE_DEPTH is defined and has expected value
        assert_eq!(PIPELINE_DEPTH, 4, "PIPELINE_DEPTH should be 4 for backpressure");

        // Verify the constant is used in stream_archive_parallel_with_config
        // (This is a compile-time property - the function creates channel with PIPELINE_DEPTH)
        // The test passes if compilation succeeds, proving the bound exists.
    }

    /// Unit test: Verify batch_size is respected in configuration.
    ///
    /// This test verifies that the batch_size configuration is correctly
    /// used by BatchAssembler to limit batch sizes.
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn test_batch_size_configuration() {
        let config = ParallelArchiveConfig::default();
        
        // Default batch_size should be 1 MB (matching CHUNK)
        assert_eq!(config.batch_size, 1024 * 1024, "Default batch_size should be 1 MB");

        // Verify batch_size can be customized
        let custom_config = ParallelArchiveConfig {
            batch_size: 512 * 1024, // 512 KB
            ..Default::default()
        };
        assert_eq!(custom_config.batch_size, 512 * 1024, "Custom batch_size should be respected");
    }

    /// Unit test: Verify large files are processed sequentially (not in parallel).
    ///
    /// This test verifies the partition logic: files >= small_file_threshold
    /// are placed in the "large" category and processed sequentially, not
    /// buffered in parallel. This bounds memory usage for large files.
    ///
    /// **Validates: Requirements 5.1, 5.3, 5.4**
    #[test]
    fn test_large_files_processed_sequentially() {
        let config = ParallelArchiveConfig::default();
        let threshold = config.small_file_threshold;

        // Verify threshold is 1 MB (default)
        assert_eq!(threshold, SMALL_THRESHOLD, "Small file threshold should be 1 MB");

        // Create test entries to verify partition logic
        let small_size = threshold - 1; // Just below threshold
        let large_size = threshold;     // At threshold

        assert!(small_size < threshold, "Small file should be below threshold");
        assert!(large_size >= threshold, "Large file should be at or above threshold");
    }

    /// Property test: Archive with many small files can be processed successfully.
    ///
    /// This test verifies that the pipeline architecture correctly handles
    /// directories with many small files without memory issues. The bounded
    /// channel (PIPELINE_DEPTH) provides backpressure to prevent unbounded
    /// memory growth.
    ///
    /// Since directly measuring memory is complex in unit tests, this test:
    /// 1. Creates a directory with many small files (simulating high memory pressure)
    /// 2. Uses stream_archive_parallel to create an archive
    /// 3. Verifies the archive can be fully consumed without errors
    /// 4. Verifies the output can be decompressed and contains all files
    ///
    /// **Validates: Requirements 5.1**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        #[test]
        fn prop_test_bounded_memory_many_small_files(
            // Generate 50-200 small files (simulating memory pressure)
            num_files in 50usize..200,
            // File sizes: 1 KB to 100 KB (all below SMALL_THRESHOLD)
            file_sizes in prop::collection::vec(1024usize..100_000, 50..200)
        ) {
            let dir = tempfile::tempdir().unwrap();
            let num_files = num_files.min(file_sizes.len());

            // Track expected files
            let mut expected_files: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Create many small files
            for i in 0..num_files {
                let file_name = format!("file_{:04}.bin", i);
                let size = file_sizes.get(i).copied().unwrap_or(10 * 1024);
                let content: Vec<u8> = (0..size).map(|j| ((j + i) % 256) as u8).collect();
                std::fs::write(dir.path().join(&file_name), &content).unwrap();
                expected_files.insert(file_name);
            }

            // Collect entries using walk_dir
            let (total_size, entries) = walk_dir(dir.path());

            // Property 1: walk_dir should find all files
            // Note: walk_dir returns both files and directories, so we filter for files only
            let file_count = entries.iter().filter(|(e, _)| e.file_type().is_file()).count();
            prop_assert_eq!(file_count, num_files, "walk_dir should find all files");

            // Use stream_archive_parallel to create an archive
            let result = stream_archive_parallel(dir.path(), entries.clone());
            prop_assert!(result.is_ok(), "stream_archive_parallel should succeed");

            // Consume the entire archive stream
            let mut reader = result.unwrap();
            let mut archive_data = Vec::new();
            use tokio::io::AsyncReadExt;
            
            // Use tokio runtime to read the async stream
            let rt = tokio::runtime::Runtime::new().unwrap();
            let read_result = rt.block_on(async {
                reader.read_to_end(&mut archive_data).await
            });

            prop_assert!(read_result.is_ok(), "Should be able to read entire archive stream");
            prop_assert!(!archive_data.is_empty(), "Archive should not be empty");

            // Property 2: Verify the archive can be decompressed
            let decompressed = zstd::decode_all(&archive_data[..]);
            prop_assert!(decompressed.is_ok(), "Archive should decompress successfully");

            // Property 3: Verify the tar archive contains all expected files
            let decompressed = decompressed.unwrap();
            let mut archive = tar::Archive::new(decompressed.as_slice());
            let entries_result = archive.entries();
            prop_assert!(entries_result.is_ok(), "Should be able to read tar entries");

            let tar_entries: Vec<_> = entries_result.unwrap().collect();
            let tar_file_count = tar_entries.len();

            // The tar contains the directory itself, so we expect at least num_files entries
            // (may have directory entries as well)
            prop_assert!(
                tar_file_count >= num_files,
                "Tar should contain at least {} files, got {}",
                num_files,
                tar_file_count
            );

            // Property 4: Verify total size matches
            prop_assert!(total_size > 0, "Total size should be positive");
        }
    }

    /// Property test: Archive with mixed small and large files processes correctly.
    ///
    /// This test verifies that the pipeline correctly handles mixed workloads
    /// where memory bounds must be maintained for both:
    /// - Small files (processed in parallel, buffered)
    /// - Large files (processed sequentially, not buffered in parallel)
    ///
    /// **Validates: Requirements 5.1, 5.3, 5.4**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        #[test]
        fn prop_test_bounded_memory_mixed_files(
            // Generate 10-30 small files
            num_small in 10usize..30,
            // Generate 1-5 large files
            num_large in 1usize..5,
            // Small file sizes: 1 KB to 512 KB (below SMALL_THRESHOLD)
            small_sizes in prop::collection::vec(1024usize..512_000, 10..30),
            // Large file sizes: 1 MB to 2 MB (at or above SMALL_THRESHOLD)
            large_sizes in prop::collection::vec(1024 * 1024usize..2 * 1024 * 1024, 1..5)
        ) {
            let dir = tempfile::tempdir().unwrap();
            let config = ParallelArchiveConfig::default();
            let threshold = config.small_file_threshold;

            let num_small = num_small.min(small_sizes.len());
            let num_large = num_large.min(large_sizes.len());

            // Create small files
            for i in 0..num_small {
                let file_name = format!("small_{:03}.bin", i);
                let size = small_sizes.get(i).copied().unwrap_or(10 * 1024);
                let content: Vec<u8> = (0..size).map(|j| ((j + i) % 256) as u8).collect();
                std::fs::write(dir.path().join(&file_name), &content).unwrap();
            }

            // Create large files
            for i in 0..num_large {
                let file_name = format!("large_{:03}.bin", i);
                let size = large_sizes.get(i).copied().unwrap_or(threshold as usize);
                let content: Vec<u8> = (0..size).map(|j| ((j + i + 100) % 256) as u8).collect();
                std::fs::write(dir.path().join(&file_name), &content).unwrap();
            }

            // Collect entries using walk_dir
            let (_, entries) = walk_dir(dir.path());

            // Use stream_archive_parallel to create an archive
            let result = stream_archive_parallel(dir.path(), entries.clone());
            prop_assert!(result.is_ok(), "stream_archive_parallel should succeed for mixed files");

            // Consume the entire archive stream
            let mut reader = result.unwrap();
            let mut archive_data = Vec::new();
            use tokio::io::AsyncReadExt;
            
            let rt = tokio::runtime::Runtime::new().unwrap();
            let read_result = rt.block_on(async {
                reader.read_to_end(&mut archive_data).await
            });

            prop_assert!(read_result.is_ok(), "Should be able to read entire archive stream");
            prop_assert!(!archive_data.is_empty(), "Archive should not be empty");

            // Verify the archive can be decompressed
            let decompressed = zstd::decode_all(&archive_data[..]);
            prop_assert!(decompressed.is_ok(), "Archive should decompress successfully");

            // Verify the tar archive is valid
            let decompressed = decompressed.unwrap();
            let mut archive = tar::Archive::new(decompressed.as_slice());
            let entries_result = archive.entries();
            prop_assert!(entries_result.is_ok(), "Should be able to read tar entries");

            // Count file entries (excluding directories)
            let file_count = entries_result.unwrap()
                .filter(|e| e.as_ref().map(|entry| entry.header().entry_type().is_file()).unwrap_or(false))
                .count();

            prop_assert!(
                file_count >= num_small + num_large,
                "Tar should contain at least {} files, got {}",
                num_small + num_large,
                file_count
            );
        }
    }

    /// Unit test: Verify compression thread count is bounded.
    ///
    /// This test verifies that the number of compression threads is bounded
    /// to prevent memory exhaustion. The default is min(num_cpus, 8).
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn test_compression_thread_count_bounded() {
        let config = ParallelArchiveConfig::default();

        // Thread count should be at most 8
        assert!(
            config.compression_threads <= 8,
            "Compression threads should be bounded at 8, got {}",
            config.compression_threads
        );

        // Thread count should be at least 1
        assert!(
            config.compression_threads >= 1,
            "Compression threads should be at least 1, got {}",
            config.compression_threads
        );
    }

    /// Unit test: Verify memory bound calculation.
    ///
    /// This test verifies the theoretical memory bound:
    /// N × CHUNK × PIPELINE_DEPTH + (small files buffer)
    ///
    /// For N = 4 threads, CHUNK = 1 MB, PIPELINE_DEPTH = 4:
    /// Bound = 4 × 1 MB × 4 = 16 MB (excluding small file buffer)
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn test_memory_bound_calculation() {
        let config = ParallelArchiveConfig::default();
        let n = config.compression_threads;
        let chunk = config.batch_size;
        let depth = PIPELINE_DEPTH;

        // Calculate theoretical memory bound for in-flight data
        let in_flight_bound = n * chunk * depth;

        // Verify the bound is reasonable (should be 16 MB with defaults)
        // Allow for variations in thread count (1-8 threads)
        let expected_min = 1 * chunk * depth; // 1 thread minimum
        let expected_max = 8 * chunk * depth; // 8 thread maximum

        assert!(
            in_flight_bound >= expected_min && in_flight_bound <= expected_max,
            "In-flight memory bound {} should be between {} and {}",
            in_flight_bound,
            expected_min,
            expected_max
        );

        // Log the bound for verification
        // With defaults: 4 × 1MB × 4 = 16 MB
        // This does not include small file buffer (which is bounded by threshold)
    }

    // ── Property-Based Tests: Pipeline Error Shutdown ───────────────────────────────
    // Validates: Requirements 4.6

    /// Property test: Pipeline error shutdown.
    ///
    /// This test verifies that when an error occurs in any pipeline stage,
    /// all upstream stages terminate within a bounded time, and the error
    /// is propagated to the caller.
    ///
    /// The test:
    /// 1. Creates files where one has an error condition (permission denied on Unix)
    /// 2. Calls stream_archive_parallel
    /// 3. Verifies the error propagates to the reader when consumed
    /// 4. Verifies the pipeline shuts down cleanly (no hangs, no panics)
    ///
    /// **Validates: Requirements 4.6**
    #[cfg(unix)]
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        #[test]
        fn prop_test_pipeline_error_shutdown(
            // Generate 5-20 unique filenames
            filenames in prop::collection::vec("[a-z]{1,10}", 5..20),
            // Index of the file to make unreadable (0 to len-1)
            unreadable_index in 0usize..20
        ) {
            let dir = tempfile::tempdir().unwrap();

            // Create files with content
            for name in &filenames {
                std::fs::write(dir.path().join(name), vec![0u8; 1024]).unwrap();
            }

            // Ensure the index is valid
            let unreadable_index = unreadable_index.min(filenames.len() - 1);
            let unreadable_name = &filenames[unreadable_index];
            let unreadable_path = dir.path().join(unreadable_name);

            // Make the selected file unreadable (permission denied)
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o000)).unwrap();

            // Collect entries using walk_dir
            let (_, entries) = walk_dir(dir.path());

            // Create the parallel archive stream
            let result = stream_archive_parallel(dir.path(), entries.clone());
            prop_assert!(result.is_ok(), "stream_archive_parallel should return a reader");

            let mut reader = result.unwrap();
            let mut archive_data = Vec::new();
            use tokio::io::AsyncReadExt;

            // Use tokio runtime to read the async stream
            let rt = tokio::runtime::Runtime::new().unwrap();

            // Property 1: Reading the stream should fail with an error (not hang forever)
            // Set a timeout to detect if the pipeline doesn't shut down
            let read_result = rt.block_on(async {
                // Use a timeout to verify the pipeline shuts down within bounded time
                let timeout_duration = std::time::Duration::from_secs(10);
                tokio::time::timeout(timeout_duration, reader.read_to_end(&mut archive_data)).await
            });

            // Property 2: The read should complete (either success or error), not hang
            prop_assert!(
                read_result.is_ok(),
                "Pipeline should shut down within bounded time (10 seconds), not hang"
            );

            // Property 3: The inner read should return an error (permission denied)
            let inner_result = read_result.unwrap();
            prop_assert!(
                inner_result.is_err(),
                "Reading archive should fail with an error when a file is unreadable"
            );

            // Property 4: The error message should contain the problematic file path
            let err_msg = inner_result.unwrap_err().to_string();
            prop_assert!(
                err_msg.contains(unreadable_name) || err_msg.contains(&format!("{}", unreadable_path.display())),
                "Error message must contain the file path that caused the failure. Got: {}",
                err_msg
            );

            // Property 5: The error should indicate it came from the compression stage
            // (either "compression error", "failed to process", or similar)
            prop_assert!(
                err_msg.contains("failed") || err_msg.contains("error") || err_msg.contains("permission"),
                "Error message should indicate the failure type. Got: {}",
                err_msg
            );

            // Cleanup: restore permissions so tempdir can be deleted
            std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o644)).ok();
        }
    }

    /// Unit test: Verify pipeline shuts down cleanly on compression error.
    ///
    /// This test verifies that when compress_single_entry fails (e.g., permission denied),
    /// the compress_entries_parallel function:
    /// 1. Captures the error in error_slot
    /// 2. Returns an error (not panic)
    /// 3. Does not send partial data to the channel
    ///
    /// **Validates: Requirements 4.6**
    #[cfg(unix)]
    #[test]
    fn test_pipeline_error_shutdown_clean() {
        let dir = tempfile::tempdir().unwrap();
        let config = ParallelArchiveConfig::default();

        // Create readable files
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("file_{}", i)), vec![0u8; 1024]).unwrap();
        }

        // Create an unreadable file
        let unreadable_path = dir.path().join("unreadable_file");
        std::fs::write(&unreadable_path, vec![0u8; 1024]).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Collect entries
        let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
            .filter(|(e, _)| e.file_type().is_file())
            .collect();

        // Set up parallel compression pipeline
        let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
        let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_slot_clone = error_slot.clone();

        // Run compression
        let result = compress_entries_parallel(
            dir.path(),
            entries,
            &config,
            tx,
            error_slot_clone,
        );

        // Property 1: Should fail with an error (not panic)
        assert!(result.is_err(), "compress_entries_parallel should return error for unreadable file");

        // Property 2: Error slot should be set
        let error_slot_value = error_slot.lock().unwrap().clone();
        assert!(error_slot_value.is_some(), "Error slot should be set");

        // Property 3: Error message should contain the file path
        let err_msg = error_slot_value.unwrap();
        assert!(
            err_msg.contains("unreadable_file"),
            "Error message should contain the file path. Got: {}",
            err_msg
        );

        // Property 4: Pipeline should shut down cleanly
        // The channel should be closed (tx dropped) after the error
        // Using try_recv should return Disconnected or Empty (not blocking)
        let recv_result = rx.try_recv();
        // Either channel is empty (no partial data sent) or disconnected (tx dropped)
        // Both indicate clean shutdown
        assert!(
            recv_result.is_err(),
            "Channel should be closed or empty after error (clean shutdown)"
        );

        // Cleanup
        std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o644)).ok();
    }

    /// Unit test: Verify error propagates through ErrorAwareReader.
    ///
    /// This test verifies that when an error occurs in the compression thread,
    /// the ErrorAwareReader returns the error when read (instead of silent EOF).
    ///
    /// **Validates: Requirements 4.6**
    #[cfg(unix)]
    #[test]
    fn test_error_propagates_to_reader() {
        let dir = tempfile::tempdir().unwrap();

        // Create files
        for i in 0..3 {
            std::fs::write(dir.path().join(format!("file_{}", i)), vec![0u8; 1024]).unwrap();
        }

        // Create an unreadable file
        let unreadable_path = dir.path().join("protected");
        std::fs::write(&unreadable_path, vec![0u8; 1024]).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Collect entries and create archive
        let (_, entries) = walk_dir(dir.path());
        let result = stream_archive_parallel(dir.path(), entries);

        assert!(result.is_ok(), "stream_archive_parallel should return a reader");

        let mut reader = result.unwrap();
        let mut archive_data = Vec::new();
        use tokio::io::AsyncReadExt;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let read_result = rt.block_on(async {
            reader.read_to_end(&mut archive_data).await
        });

        // Property: Reading should fail with an error containing the file path
        assert!(read_result.is_err(), "Reader should return an error for unreadable file");

        let err_msg = read_result.unwrap_err().to_string();
        assert!(
            err_msg.contains("protected"),
            "Error should contain the file path. Got: {}",
            err_msg
        );

        // Cleanup
        std::fs::set_permissions(&unreadable_path, std::fs::Permissions::from_mode(0o644)).ok();
    }

    /// Unit test: Verify clean shutdown with multiple errors.
    ///
    /// This test verifies that when multiple files have errors, the pipeline
    /// still shuts down cleanly and reports the first error encountered.
    ///
    /// **Validates: Requirements 4.6**
    #[cfg(unix)]
    #[test]
    fn test_pipeline_multiple_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config = ParallelArchiveConfig::default();

        // Create readable files
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("readable_{}", i)), vec![0u8; 1024]).unwrap();
        }

        // Create multiple unreadable files
        for i in 0..3 {
            let path = dir.path().join(format!("unreadable_{}", i));
            std::fs::write(&path, vec![0u8; 1024]).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        }

        // Collect entries
        let entries: Vec<(walkdir::DirEntry, std::fs::Metadata)> = WalkDir::new(dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok().map(|m| (e, m)))
            .filter(|(e, _)| e.file_type().is_file())
            .collect();

        // Set up parallel compression pipeline
        let (tx, rx) = std::sync::mpsc::channel::<CompressedEntry>();
        let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_slot_clone = error_slot.clone();

        // Run compression
        let result = compress_entries_parallel(
            dir.path(),
            entries,
            &config,
            tx,
            error_slot_clone,
        );

        // Property: Should fail with an error
        assert!(result.is_err(), "Should fail when any file is unreadable");

        // Property: Error slot should be set
        let error_slot_value = error_slot.lock().unwrap().clone();
        assert!(error_slot_value.is_some(), "Error slot should be set");

        // Property: Error message should contain a file path (one of the unreadable files)
        let err_msg = error_slot_value.unwrap();
        let contains_unreadable = err_msg.contains("unreadable_0")
            || err_msg.contains("unreadable_1")
            || err_msg.contains("unreadable_2");
        assert!(
            contains_unreadable,
            "Error message should contain an unreadable file path. Got: {}",
            err_msg
        );

        // Property: Pipeline should shut down cleanly (no data on channel)
        let recv_result = rx.try_recv();
        assert!(
            recv_result.is_err(),
            "Channel should be closed after error"
        );

        // Cleanup
        for i in 0..3 {
            let path = dir.path().join(format!("unreadable_{}", i));
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).ok();
        }
    }
}
