# Requirements Document

## Introduction

This feature optimizes directory transfer performance for folders containing many small files. The current architecture uses a serialized tar+zstd stream that processes files one-by-one, causing significant overhead for small files. This optimization introduces parallel archive generation and improved pipeline efficiency to achieve ≥100 MB/s throughput on gigabit LANs for directories with 1000+ small files.

The optimization builds on the recent 3-stage pipeline improvements (TCP_BUF_SIZE=8MB, PIPELINE_DEPTH=4) and maintains backward compatibility with the existing wire protocol.

## Glossary

- **Archive_Stream**: The tar+zstd compressed byte stream produced by `archive.rs` for directory transfers
- **Small_File**: A file smaller than `SMALL_FILE_THRESHOLD` (currently 1 MB), pre-loaded into memory during archive generation
- **Large_File**: A file ≥ 1 MB, streamed from disk during archive generation
- **ChannelWriter**: Synchronous writer in `archive.rs` that sends chunks via mpsc channel to the async runtime
- **ErrorAwareReader**: Async adapter that reads from the mpsc channel and checks for compression errors on EOF
- **compress_entries_to_writer**: Synchronous function that walks a directory, builds a tar, and compresses via zstd
- **Parallel_Archive_Generator**: New component that generates archive chunks using multiple threads for file reading and compression
- **Work_Stealing_Pool**: A thread pool where worker threads steal tasks from each other's queues for load balancing
- **Archive_Chunk**: A unit of archive data (typically 256 KB - 1 MB) produced by compression threads
- **Encryption_Pipeline**: The 3-stage pipeline: read/encrypt → hash → network write

## Requirements

### Requirement 1: Parallel Archive Generation

**User Story:** As a user transferring a folder with many small files, I want the sender to process multiple files concurrently, so that transfer throughput is maximized on multi-core systems.

#### Acceptance Criteria

1. WHEN transferring a directory containing ≥100 small files, THE Parallel_Archive_Generator SHALL use multiple threads to read and compress files concurrently
2. THE Parallel_Archive_Generator SHALL preserve deterministic tar entry order regardless of parallel execution
3. WHERE the number of CPU cores is N, THE Parallel_Archive_Generator SHALL use min(N, 8) compression threads
4. WHEN the Archive_Stream is consumed by the encryption pipeline, THE Archive_Stream SHALL produce chunks at a rate sufficient to saturate a gigabit network link
5. IF a file read error occurs during parallel processing, THE Parallel_Archive_Generator SHALL propagate the error and abort the transfer

### Requirement 2: Batch Small Files into Larger Chunks

**User Story:** As a user transferring many small files, I want the archive generator to batch small files together, so that encryption and network overhead per file is minimized.

#### Acceptance Criteria

1. WHEN multiple small files are being archived, THE Parallel_Archive_Generator SHALL batch tar entries into larger chunks before sending to the encryption pipeline
2. THE batch chunk size SHALL be configurable between 256 KB and 4 MB
3. WHEN a batch is ready for encryption, THE batch SHALL be sent as a single chunk to minimize AEAD frame overhead
4. WHILE batching is active, THE Parallel_Archive_Generator SHALL maintain tar entry boundaries so the receiver can unpack correctly

### Requirement 3: Optimize Compression for Small Files

**User Story:** As a user transferring small files, I want compression tuned for small file patterns, so that CPU time is not wasted on marginal compression gains.

#### Acceptance Criteria

1. WHEN compressing files smaller than 64 KB, THE Parallel_Archive_Generator SHALL use zstd compression level 1 for speed optimization
2. WHEN compressing files between 64 KB and 1 MB, THE Parallel_Archive_Generator SHALL use zstd compression level 3 for balanced speed/ratio
3. WHEN compressing files larger than 1 MB, THE Parallel_Archive_Generator SHALL use zstd compression level 3 (current default)
4. THE compression level selection SHALL be configurable via a compile-time constant

### Requirement 4: Sender Pipeline Parallelism

**User Story:** As a sender, I want archive generation, encryption, and network write to run in parallel, so that each stage can utilize CPU and I/O resources concurrently.

#### Acceptance Criteria

1. WHEN sending a directory archive, THE sender SHALL run archive generation, encryption, and network write as three concurrent pipeline stages
2. Stage 1 (archive generation) SHALL produce encrypted chunks and send via channel to Stage 2
3. Stage 2 (encryption) SHALL receive chunks, encrypt with AEAD, and send encrypted frames to Stage 3
4. Stage 3 (network write) SHALL receive encrypted frames and write to the TCP socket
5. THE pipeline channel depth SHALL be PIPELINE_DEPTH (currently 4) to buffer between stages
6. WHEN any pipeline stage encounters an error, THE entire pipeline SHALL shut down cleanly and propagate the error

### Requirement 5: Memory Efficiency

**User Story:** As a user with limited memory, I want the parallel archive generator to bound memory usage, so that transfers do not cause memory exhaustion.

#### Acceptance Criteria

1. WHERE the number of parallel compression threads is N, THE total in-flight chunk memory SHALL NOT exceed N × CHUNK_SIZE × PIPELINE_DEPTH
2. WHEN the channel between archive generation and encryption is full, THE archive generation stage SHALL block (backpressure)
3. THE Parallel_Archive_Generator SHALL NOT buffer entire files in memory unless they are small files (< 1 MB)
4. FOR large files, THE Parallel_Archive_Generator SHALL stream file content in chunks without loading the entire file

### Requirement 6: Protocol Backward Compatibility

**User Story:** As a user with mixed-version deployments, I want the optimized sender to work with unmodified receivers, so that I can upgrade senders without requiring simultaneous receiver upgrades.

#### Acceptance Criteria

1. THE wire protocol (MAGIC, header format, chunk framing, checksum) SHALL remain unchanged
2. THE tar+zstd archive format SHALL remain standard and compatible with the existing `unpack_archive_sync` function
3. THE receiver SHALL be able to unpack archives from both optimized and unoptimized senders without modification
4. WHEN a receiver processes an archive from an optimized sender, THE SHA-256 checksum SHALL match the sender's computed checksum

### Requirement 7: Performance Targets

**User Story:** As a user transferring folders with many small files, I want measurable throughput improvements, so that the optimization delivers real-world value.

#### Acceptance Criteria

1. WHEN transferring a directory with 1000 files of 10 KB each (10 MB total) over a gigabit LAN, THE throughput SHALL be ≥100 MB/s
2. WHEN transferring a directory with 100 files of 100 KB each (10 MB total) over a gigabit LAN, THE throughput SHALL be ≥100 MB/s
3. WHEN transferring a directory with 10 files of 1 MB each (10 MB total) over a gigabit LAN, THE throughput SHALL be ≥100 MB/s
4. THE throughput measurement SHALL include tar header overhead, compression, encryption, and network transfer
5. WHEN the receiver unpacks the archive, THE unpack time SHALL NOT exceed 10% of the transfer time

### Requirement 8: Error Handling and Recovery

**User Story:** As a user, I want the parallel archive generator to handle errors gracefully, so that I receive clear error messages when transfers fail.

#### Acceptance Criteria

1. IF a file cannot be read during archive generation, THE Parallel_Archive_Generator SHALL return a descriptive error including the file path and OS error
2. IF the compression thread panics, THE error SHALL be captured and propagated to the main async task
3. IF the network connection drops during archive generation, THE pipeline SHALL shut down and preserve the `.part` file and manifest for resume
4. WHEN an error occurs, THE error message SHALL distinguish between I/O errors, compression errors, and network errors

### Requirement 9: Deterministic Archive Output

**User Story:** As a user, I want archive output to be deterministic, so that identical source directories produce identical archives for verification purposes.

#### Acceptance Criteria

1. WHEN archiving the same directory multiple times, THE tar entry order SHALL be deterministic (sorted by path)
2. WHEN parallel processing is used, THE final tar stream SHALL be assembled in sorted order regardless of which thread processed each file
3. THE tar header fields (mode, mtime, uid, gid) SHALL be set consistently from file metadata
4. THE zstd compressed output SHALL be deterministic for the same uncompressed input
