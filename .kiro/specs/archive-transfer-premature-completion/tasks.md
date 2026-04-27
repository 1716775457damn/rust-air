# Implementation Plan

- [x] 1. Write bug condition exploration test
  - **Property 1: Bug Condition** — Compression Error Propagation
  - **CRITICAL**: This test MUST FAIL on unfixed code — failure confirms the bug exists
  - **DO NOT attempt to fix the test or the code when it fails**
  - **NOTE**: This test encodes the expected behavior — it will validate the fix when it passes after implementation
  - **GOAL**: Surface counterexamples that demonstrate the bug exists
  - **Scoped PBT Approach**: Create a temp directory with one readable file and one unreadable file (permissions set to 0o000). Call `stream_archive_with_entries` with those entries and read the `AsyncRead` to completion. The property asserts that reading returns an `io::Error` (not silent EOF).
  - Test file: `core/src/archive_bug_test.rs` (or `core/tests/archive_bug_test.rs`)
  - Steps:
    1. Create a temp directory with two files: `good.txt` (readable) and `bad.txt` (permissions 0o000)
    2. Call `walk_dir` on the temp directory to get entries
    3. Call `stream_archive_with_entries` with the path and entries
    4. Read the returned `AsyncRead` to completion using `tokio::io::read_to_end`
    5. Assert that the read returns an `Err` (not `Ok` with partial data)
  - On UNFIXED code: the reader will return `Ok` with partial/empty data (silent EOF) — test FAILS, confirming the bug
  - **EXPECTED OUTCOME**: Test FAILS (this is correct — it proves the bug exists)
  - Document counterexamples found: `stream_archive_with_entries` returns EOF without error when compression hits an unreadable file
  - Mark task complete when test is written, run, and failure is documented
  - _Requirements: 1.1, 2.1_

- [x] 2. Write preservation property tests (BEFORE implementing fix)
  - **Property 2: Preservation** — Successful Archive Transfers Unchanged
  - **IMPORTANT**: Follow observation-first methodology
  - Test file: `core/src/archive_preservation_test.rs` (or `core/tests/archive_preservation_test.rs`)
  - Observe behavior on UNFIXED code:
    - Create temp directories with varying numbers of readable files (1–20 files, varying sizes 0B–2MB, varying nesting depths 0–3)
    - Call `stream_archive_with_entries`, read the full output, compute SHA-256 of the compressed stream
    - Record that the stream completes without error and produces a valid zstd-compressed tar
    - Verify `unpack_archive_sync` can unpack the archive and all original files are present with correct content
  - Write property-based tests using `proptest` or `quickcheck`:
    - **Property**: For all generated directory trees where every file is readable, `stream_archive_with_entries` returns a stream that:
      1. Completes without error (read returns `Ok(0)`, not `Err`)
      2. Produces a valid zstd-compressed tar (decompresses and unpacks without error)
      3. Contains all original files with byte-identical content
    - **Property**: `walk_dir` returns a total size equal to the sum of all file sizes in the directory, and the entry count matches the number of files + directories
  - Verify tests PASS on UNFIXED code (confirms baseline behavior to preserve)
  - **EXPECTED OUTCOME**: Tests PASS (this confirms baseline behavior to preserve)
  - Mark task complete when tests are written, run, and passing on unfixed code
  - _Requirements: 3.1, 3.4, 3.5_

- [x] 3. Fix for archive transfer premature completion

  - [x] 3.1 Skip log files in `walk_dir`
    - In `core/src/archive.rs`, function `walk_dir`
    - Add a filter after `.filter_map(|e| e.metadata().ok().map(|m| (e, m)))` to exclude files matching `*.log` and `*.log.*` patterns
    - Filter logic: for each entry that is a file, check if the filename ends with `.log` or contains `.log.` — if so, skip it
    - This prevents transient/locked log files from being included in the archive
    - _Requirements: 2.4_

  - [x] 3.2 Add error channel and custom `AsyncRead` wrapper in `stream_archive_with_entries`
    - In `core/src/archive.rs`, function `stream_archive_with_entries`
    - Create `Arc<Mutex<Option<anyhow::Error>>>` shared between compression thread and async reader
    - In the spawned thread: replace `eprintln!("archive compression error: {e}")` with storing the error into the shared `Arc<Mutex<Option<...>>>` slot
    - Create a new struct `ErrorAwareReader<R>` that wraps the `async_reader` (the read half of `tokio::io::duplex`):
      - Holds `inner: R` (the duplex reader) and `error_slot: Arc<Mutex<Option<anyhow::Error>>>`
      - Implements `AsyncRead`: delegates to `inner.poll_read()`. When inner returns `Poll::Ready(Ok(()))` with 0 bytes read (EOF), check the error slot. If an error is present, return `Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, error_message)))` instead of EOF
    - Return `ErrorAwareReader` instead of the raw `async_reader`
    - _Bug_Condition: isBugCondition(input) where compress_entries encounters an unreadable file_
    - _Expected_Behavior: stream_archive_with_entries returns an AsyncRead that yields io::Error on compression failure_
    - _Preservation: For all readable inputs, ErrorAwareReader passes through EOF normally (error slot is None)_
    - _Requirements: 2.1, 2.2_

  - [x] 3.3 Abort transfer on compression error in `send_path`
    - In `core/src/transfer.rs`, function `send_path`
    - The `stream_encrypted_hash` call for archives will now propagate the `io::Error` from `ErrorAwareReader` as an `anyhow::Error`
    - When `stream_encrypted_hash` returns `Err`, `send_path` returns the error immediately — it does NOT call `enc.shutdown()` or `enc.write_trailing()`
    - This drops the TCP connection, causing the receiver to get a connection-reset or unexpected EOF
    - Current code already uses `?` on `stream_encrypted_hash`, so the error propagation to `send_path`'s return is automatic. The key change is that `enc.shutdown()` and `enc.write_trailing()` are only reached on success, which is already the case with `?`. Verify this is correct and no additional changes are needed in `send_path` itself.
    - _Bug_Condition: isBugCondition(input) where compress_entries fails_
    - _Expected_Behavior: send_path returns Err, no EOF sentinel or checksum sent_
    - _Preservation: Successful transfers still send EOF sentinel + checksum_
    - _Requirements: 2.1, 2.2_

  - [x] 3.4 Fix progress reporting for archives
    - In `core/src/transfer.rs`, function `receive_to_disk`, `Kind::Archive` branch
    - In the final `emit_progress` call (the one with `done: true`), force `bytes_done = total_bytes` so the UI shows 100%
    - Change: `emit_progress(&on_progress, done, total_size, &start, true)` → `emit_progress(&on_progress, total_size, total_size, &start, true)`
    - This ensures the final progress event has `bytes_done == total_bytes` regardless of compressed-vs-uncompressed mismatch
    - _Requirements: 2.3_

  - [x] 3.5 Verify bug condition exploration test now passes
    - **Property 1: Expected Behavior** — Compression Error Propagation
    - **IMPORTANT**: Re-run the SAME test from task 1 — do NOT write a new test
    - The test from task 1 encodes the expected behavior: reading from `stream_archive_with_entries` with an unreadable file returns `io::Error`
    - When this test passes, it confirms the expected behavior is satisfied
    - Run bug condition exploration test from step 1
    - **EXPECTED OUTCOME**: Test PASSES (confirms bug is fixed)
    - _Requirements: 2.1, 2.2_

  - [x] 3.6 Verify preservation tests still pass
    - **Property 2: Preservation** — Successful Archive Transfers Unchanged
    - **IMPORTANT**: Re-run the SAME tests from task 2 — do NOT write new tests
    - Run preservation property tests from step 2
    - **EXPECTED OUTCOME**: Tests PASS (confirms no regressions)
    - Confirm all tests still pass after fix (no regressions)

- [x] 4. Checkpoint — Ensure all tests pass
  - Run the full test suite (`cargo test` in the `core` crate)
  - Ensure all property-based tests pass (both bug condition and preservation)
  - Ensure no existing tests are broken by the changes
  - Ask the user if questions arise
