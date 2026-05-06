# Implementation Plan: Parallel Small File Transfer

## Overview

This implementation adds multi-threaded archive generation to optimize directory transfer performance for folders containing many small files. The approach uses rayon for parallel compression, sorted assembly with batching for determinism, and integrates with the existing encryption pipeline while maintaining backward compatibility.

**Implementation Language**: Rust (determined from design document)

**Key Components**:
- `stream_archive_parallel` function in `core/src/archive.rs`
- `ParallelArchiveConfig` and `CompressionLevels` configuration structs
- `compress_entries_parallel` function for parallel compression
- `BatchAssembler` for sorted assembly and batching
- Integration with `send_path` in `core/src/transfer.rs`
- Property-based tests for correctness validation

---

## Tasks

- [x] 1. Add configuration types and constants
  - Define `ParallelArchiveConfig` struct with compression_threads, batch_size, small_file_threshold, and compression_levels fields
  - Define `CompressionLevels` struct with tiny, small, and large i32 fields
  - Add constants: TINY_THRESHOLD (64 KB), SMALL_THRESHOLD (1 MB), PIPELINE_DEPTH (4)
  - Implement `Default` trait for both configuration structs
  - _Requirements: 3.1, 3.2, 3.3, 5.1_

- [x] 2. Implement `CompressedEntry` internal data structure
  - Create struct with path (PathBuf), tar_data (Vec<u8>), original_size (u64), compressed_size (u64) fields
  - This holds the result of parallel compression per file
  - _Requirements: 1.2, 9.2_

- [x] 3. Implement compression level selection logic
  - [x] 3.1 Implement `select_compression_level` function
    - Takes file size and CompressionLevels reference, returns i32
    - Returns levels.tiny for files < 64 KB
    - Returns levels.small for files 64 KB to < 1 MB
    - Returns levels.large for files >= 1 MB
    - _Requirements: 3.1, 3.2, 3.3_

  - [x] 3.2 Write unit tests for compression level selection
    - Test boundary conditions at 64 KB and 1 MB thresholds
    - Test each tier independently
    - _Requirements: 3.1, 3.2, 3.3_

- [x] 4. Implement single file compression function
  - [x] 4.1 Implement `compress_single_entry` function
    - Read file content from disk
    - Build tar header with metadata
    - Select compression level based on file size
    - Compress tar header + data with zstd
    - Return CompressedEntry struct
    - _Requirements: 1.1, 3.1, 3.2, 3.3_

  - [x] 4.2 Write unit tests for single entry compression
    - Test with files of various sizes
    - Verify tar header is correctly set
    - Verify compression produces valid output
    - _Requirements: 1.1, 3.1_

- [x] 5. Implement `BatchAssembler` for sorted assembly
  - [x] 5.1 Create `BatchAssembler` struct
    - Use BTreeMap<PathBuf, CompressedEntry> for sorted storage
    - Track batch_size and output channel sender
    - Implement `new`, `add_entry`, and `flush` methods
    - `flush` iterates sorted entries and batches to target size
    - _Requirements: 1.2, 2.1, 2.2, 9.1, 9.2_

  - [x] 5.2 Write property test for deterministic archive order
    - **Property 1: Deterministic Archive Order**
    - **Validates: Requirements 1.2, 9.1, 9.2**
    - Generate random file sets, archive twice, verify identical output
    - Verify tar entries are in sorted order by path

  - [x] 5.3 Write property test for batch size bounds
    - **Property 6: Batch Size Bounds**
    - **Validates: Requirements 2.1, 2.2**
    - Verify batches are between 256 KB and 4 MB (except final batch)

- [x] 6. Implement parallel compression orchestration
  - [x] 6.1 Implement `compress_entries_parallel` function
    - Partition entries into small (< 1 MB) and large files
    - Process small files with rayon `par_iter()` for parallel compression
    - Process large files sequentially to avoid memory issues
    - Send compressed entries via channel to assembler
    - Capture errors in error_slot for propagation
    - _Requirements: 1.1, 1.3, 1.5, 5.3, 5.4, 8.1_

  - [x] 6.2 Write property test for error propagation
    - **Property 5: Error Propagation**
    - **Validates: Requirements 1.5, 8.1**
    - Create files with permission errors, verify error contains path
    - Verify no partial archive is produced on error

  - [x] 6.3 Write property test for large file streaming
    - **Property 8: Large File Streaming**
    - **Validates: Requirements 5.3, 5.4**
    - Verify large files are not fully buffered in memory
    - Verify large files are processed sequentially, not in parallel

- [x] 7. Implement main `stream_archive_parallel` function
  - [x] 7.1 Implement `stream_archive_parallel` entry point
    - Create mpsc channel with PIPELINE_DEPTH bound
    - Spawn compression thread that runs compress_entries_parallel
    - Spawn assembler thread that runs BatchAssembler
    - Return ErrorAwareReader wrapping the channel
    - Handle thread lifecycle and error propagation
    - _Requirements: 1.1, 1.4, 4.1, 4.2, 5.1_

  - [x] 7.2 Write property test for bounded memory usage
    - **Property 4: Bounded Memory Usage**
    - **Validates: Requirements 5.1**
    - Monitor peak memory during archive generation
    - Verify memory stays within N × CHUNK × DEPTH bounds

  - [x] 7.3 Write property test for pipeline error shutdown
    - **Property 7: Pipeline Error Shutdown**
    - **Validates: Requirements 4.6**
    - Inject error in compression stage, verify clean shutdown
    - Verify error propagates to caller

- [x] 8. Checkpoint - Verify core implementation with unit tests
  - Run all unit tests and verify basic functionality works
  - Ensure compilation succeeds with no warnings
  - Ask the user if questions arise.

- [x] 9. Implement `stream_archive_parallel_with_config` variant
  - Add function that accepts custom ParallelArchiveConfig
  - Allow tuning of thread count, batch size, compression levels
  - _Requirements: 2.2, 3.4_

- [ ] 10. Integrate parallel archive with transfer pipeline
  - [x] 10.1 Modify `send_path` in `core/src/transfer.rs`
    - Add conditional logic to use `stream_archive_parallel` for directories
    - Keep existing `stream_archive_with_entries` path available
    - Pass appropriate configuration based on file count/size heuristics
    - _Requirements: 1.1, 4.1, 4.2, 4.3, 4.4, 4.5_

  - [ ] 10.2 Write property test for archive round-trip integrity
    - **Property 2: Archive Round-Trip Integrity**
    - **Validates: Requirements 2.4, 6.2, 6.3**
    - Archive directory with parallel generator
    - Unpack with existing `unpack_archive_sync`
    - Verify all files extracted with identical content

  - [ ] 10.3 Write property test for checksum verification
    - **Property 3: Checksum Verification**
    - **Validates: Requirements 6.4**
    - Generate archive, compute SHA-256 during streaming
    - Verify checksum matches after decryption simulation

- [x] 11. Checkpoint - Verify integration with transfer pipeline
  - Run integration tests to verify end-to-end transfer works
  - Test with various directory structures and file sizes
  - Ask the user if questions arise.

- [ ] 12. Add property-based test infrastructure
  - [ ] 12.1 Set up proptest configuration in test module
    - Configure minimum 100 iterations per property test
    - Define strategies for generating random file sets
    - Create helper functions for temp directory setup
    - _Requirements: All correctness properties_

  - [ ] 12.2 Implement test helper functions
    - `create_test_directory_with_files`: Creates temp dir with specified files
    - `list_tar_entries`: Extracts entry paths from tar archive
    - `verify_directory_contents`: Compares two directories for equality
    - _Requirements: All correctness properties_

- [ ] 13. Write integration tests
  - [ ] 13.1 Write backward compatibility test
    - Create archive with parallel generator
    - Unpack with existing unpack_archive_sync
    - Verify contents match original
    - _Requirements: 6.1, 6.2, 6.3_

  - [ ] 13.2 Write throughput benchmark test
    - Test with 1000 × 10KB files
    - Test with 100 × 100KB files
    - Test with 10 × 1MB files
    - Measure and log throughput (target: ≥100 MB/s)
    - _Requirements: 7.1, 7.2, 7.3, 7.4_

  - [ ] 13.3 Write mixed file size test
    - Create directory with mix of tiny, small, and large files
    - Verify archive and unpack works correctly
    - Verify throughput meets target (≥80 MB/s for mixed)
    - _Requirements: 7.4_

- [ ] 14. Final checkpoint - Full test suite verification
  - Run all property tests, unit tests, and integration tests
  - Verify performance targets are met
  - Verify backward compatibility with existing receivers
  - Ask the user if questions arise.

---

## Notes

- Tasks marked with `*` are optional and can be skipped for faster MVP
- Each task references specific requirements for traceability
- Checkpoints ensure incremental validation
- Property tests validate universal correctness properties defined in the design document
- Unit tests validate specific examples and edge cases
- The implementation uses rayon for work-stealing parallelism, matching existing codebase patterns
- Backward compatibility is preserved - existing receivers work without modification
- Memory is bounded by N × CHUNK × PIPELINE_DEPTH where N is compression thread count
