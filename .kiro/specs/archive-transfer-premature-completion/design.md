# Archive Transfer Premature Completion — Bugfix Design

## Overview

When `compress_entries` in `archive.rs` encounters an unreadable file during a `Kind::Archive` transfer, it prints to stderr and returns, dropping the `PipeWriter`. This causes the compressed stream to EOF early. The sender's `stream_encrypted_hash` reads until EOF, computes a SHA-256 over the partial data, sends the EOF sentinel + checksum, and the receiver unpacks the partial archive and reports success. The fix must propagate compression errors from the background thread through the async pipe so `send_path` can detect the failure and abort the transfer. A secondary issue is that progress reporting compares compressed bytes against uncompressed total size, producing misleading final progress values.

## Glossary

- **Bug_Condition (C)**: The condition that triggers the bug — `compress_entries` encounters at least one unreadable file during tar+zstd compression, causing premature pipe EOF
- **Property (P)**: The desired behavior when compression fails — the error propagates to `send_path`, the transfer aborts, and the receiver is notified of failure (no EOF sentinel / checksum sent)
- **Preservation**: Existing behavior that must remain unchanged — successful archive transfers, single-file transfers, clipboard transfers, resume support, and archive unpacking
- **`compress_entries`**: The function in `core/src/archive.rs` that performs synchronous tar+zstd compression of directory entries into a `PipeWriter`
- **`stream_archive_with_entries`**: The function in `core/src/archive.rs` that spawns the compression thread and returns an `AsyncRead` via a duplex pipe
- **`pump_pipe`**: The async function in `core/src/archive.rs` that bridges the synchronous `PipeReader` to an async writer using a buffer pool
- **`stream_encrypted_hash`**: The function in `core/src/transfer.rs` that reads from an `AsyncRead`, encrypts chunks, computes SHA-256, and emits progress events
- **`send_path`**: The function in `core/src/transfer.rs` that orchestrates sending a file or archive over TCP
- **EOF sentinel**: A 4-byte zero frame (`0u32`) written by `Encryptor::shutdown()` to signal end-of-stream in the AEAD protocol

## Bug Details

### Bug Condition

The bug manifests when `compress_entries` encounters a file it cannot read (e.g., locked by another process, permission denied). The function returns an `Err`, but the spawning code in `stream_archive_with_entries` only prints to stderr and drops the `PipeWriter`. The `pump_pipe` task reads EOF from the pipe, shuts down the duplex writer, and `stream_encrypted_hash` interprets this as normal completion. The sender then sends the EOF sentinel and SHA-256 checksum for the partial data. The receiver unpacks the partial archive and reports success.

**Formal Specification:**
```
FUNCTION isBugCondition(input)
  INPUT: input of type ArchiveTransferInput  -- folder path + list of DirEntry
  OUTPUT: boolean

  RETURN input.kind == Kind::Archive
         AND EXISTS entry IN input.entries WHERE
             (entry.is_file AND NOT can_open_for_read(entry.path))
END FUNCTION
```

### Examples

- User transfers a folder containing `C:\Windows\sysmon.log` (locked by Sysmon service) → compression fails on that file, pipe drops, sender sends partial archive as if complete, receiver unpacks partial data and reports success
- User transfers a folder where one file has `0o000` permissions → `std::fs::File::open` returns `PermissionDenied`, compression aborts, same silent partial transfer
- User transfers a folder where a file is deleted between `walk_dir` and `compress_entries` → `std::fs::File::open` returns `NotFound`, same silent partial transfer
- User transfers a folder where all files are readable → compression completes normally, full archive transferred (not a bug condition)

## Expected Behavior

### Preservation Requirements

**Unchanged Behaviors:**
- Successful archive transfers (all files readable) must continue to produce a complete zstd-compressed tar with valid SHA-256 checksum
- Single-file transfers via `Kind::File` must continue to work with accurate progress and resume support
- Clipboard transfers via `Kind::Clipboard` must continue to work correctly
- Archive unpacking via `unpack_archive_sync` must continue to work for valid archives
- The `walk_dir` function must continue to return (total_size, entries) without changes
- Small-file parallel pre-reading via rayon must continue to work
- The AEAD encryption/decryption protocol must remain unchanged
- Resume support for `Kind::File` must remain unchanged

**Scope:**
All inputs where every file in the directory is readable should be completely unaffected by this fix. This includes:
- Folders with only regular, readable files
- Folders with subdirectories (directory entries don't require read)
- Single-file transfers
- Clipboard transfers
- Resumed file transfers

## Hypothesized Root Cause

Based on the bug description and code analysis, the root causes are:

1. **Silent Error Swallowing in `stream_archive_with_entries`**: The spawned thread catches the `Err` from `compress_entries` and only prints to stderr. The `PipeWriter` is dropped, causing EOF on the pipe, which is indistinguishable from normal completion.
   ```rust
   std::thread::spawn(move || {
       if let Err(e) = compress_entries(pipe_writer, &path, entries) {
           eprintln!("archive compression error: {e}");  // ← swallowed
       }
       // pipe_writer dropped here → EOF on pipe_reader
   });
   ```

2. **No Error Channel Between Compression Thread and Async Reader**: There is no mechanism (e.g., `Arc<Mutex<Option<Error>>>`, channel, or custom `AsyncRead` wrapper) to propagate the compression error to the async reader returned by `stream_archive_with_entries`. The `pump_pipe` function treats pipe EOF as normal completion.

3. **Sender Treats Premature EOF as Success**: `stream_encrypted_hash` reads until the `AsyncRead` returns 0 bytes, then returns the SHA-256 of whatever it read. `send_path` then sends the EOF sentinel and checksum, signaling successful completion to the receiver.

4. **Progress Unit Mismatch (Secondary)**: In `send_path`, `total_size` for archives is set to the uncompressed size from `walk_dir`, but `stream_encrypted_hash` increments `done` by the number of compressed bytes read from the archive stream. The final `done` value reflects compressed bytes, which is always less than `total_size` (uncompressed), so progress never reaches 100%.

## Correctness Properties

Property 1: Bug Condition — Compression Error Propagation

_For any_ archive transfer input where at least one file cannot be read during compression (isBugCondition returns true), the fixed `send_path` function SHALL return an error, SHALL NOT send the EOF sentinel and SHA-256 checksum, and the TCP connection SHALL be dropped so the receiver detects the failure.

**Validates: Requirements 2.1, 2.2**

Property 2: Preservation — Successful Archive Transfers Unchanged

_For any_ archive transfer input where all files are readable (isBugCondition returns false), the fixed `send_path` function SHALL produce the same complete zstd-compressed tar archive with the same SHA-256 checksum as the original function, and the receiver SHALL unpack the archive successfully.

**Validates: Requirements 3.1, 3.4, 3.5**

Property 3: Preservation — Non-Archive Transfer Paths Unchanged

_For any_ transfer input that is not a `Kind::Archive` (single files via `Kind::File`, clipboard via `Kind::Clipboard`), the fixed code SHALL produce exactly the same behavior as the original code, preserving progress reporting, resume support, and checksum verification.

**Validates: Requirements 3.2, 3.3**

Property 4: Bug Condition — Progress Reporting Consistency

_For any_ archive transfer that completes successfully, the final `TransferEvent` SHALL have `bytes_done == total_bytes`, using consistent units so the user sees 100% completion.

**Validates: Requirements 2.3**

## Fix Implementation

### Changes Required

Assuming our root cause analysis is correct:

**File**: `core/src/archive.rs`

**Function**: `stream_archive_with_entries`

**Specific Changes**:

1. **Add Error Channel**: Create an `Arc<Mutex<Option<anyhow::Error>>>` shared between the compression thread and the async reader. When `compress_entries` fails, store the error in the shared slot before dropping the `PipeWriter`.

2. **Custom AsyncRead Wrapper**: Wrap the `async_reader` (from `tokio::io::duplex`) in a struct that checks the error slot when the inner reader returns EOF (0 bytes). If an error is present, the wrapper returns `io::Error` instead of EOF, causing `stream_encrypted_hash` to propagate the error up to `send_path`.

3. **Remove Silent Error Swallowing**: Replace the `eprintln!` in the spawned thread with error storage into the shared slot.

**File**: `core/src/archive.rs`

**Function**: `pump_pipe`

**Specific Changes**:

4. **Propagate Read Errors**: When the reader thread encounters an `Err` from `src.read()`, instead of silently breaking, signal the error through the data channel so the async side can detect it.

**File**: `core/src/transfer.rs`

**Function**: `send_path`

**Specific Changes**:

5. **Error Propagation Aborts Transfer**: When `stream_encrypted_hash` returns an error (from the new error-aware `AsyncRead`), `send_path` returns the error immediately without calling `enc.shutdown()` or `enc.write_trailing()`. This drops the TCP connection, causing the receiver to get a connection-reset or unexpected EOF, which it will treat as an error.

6. **Fix Progress Units for Archives**: Track the uncompressed total size separately. Instead of passing the uncompressed `total_size` to `stream_encrypted_hash` (which counts compressed bytes), either:
   - Option A: Pass `0` as total for archives and emit progress with compressed bytes as both done and total (indeterminate progress), or
   - Option B: Wrap the archive `AsyncRead` to count bytes read and use that as `done`, while keeping `total_size` as the uncompressed total — but this is what currently happens and is the source of the mismatch. The simplest correct fix is to not report a total for archives (set `total_bytes` to 0 to indicate unknown) and let the UI show bytes transferred without a percentage, OR
   - Option C (recommended): Since the compressed size is unknown upfront, set `total_bytes` to the uncompressed size and report `bytes_done` as the uncompressed bytes processed so far. This requires tracking uncompressed bytes in `compress_entries` and exposing them via a shared counter. However, this adds complexity. The simplest correct approach is to accept that archive progress is approximate and ensure `bytes_done == total_bytes` in the final `done=true` event.

**Recommended approach for progress**: In the final `emit_progress` call (when `done=true`), force `bytes_done = total_bytes` so the UI shows 100%. During transfer, the compressed-vs-uncompressed mismatch is acceptable as an approximation. This is the minimal change that satisfies requirement 2.3.

**File**: `core/src/archive.rs`

**Function**: `walk_dir` and `compress_entries`

**Specific Changes**:

7. **Skip Log Files**: Add a filter in `walk_dir` to exclude runtime-generated log files (e.g., files matching `*.log`, `*.log.*`) from both the entry list and total size calculation. This prevents transient/locked log files from being included in the archive, avoiding the most common trigger for the compression error on system tool folders like sysmon. The filter should be applied during the `WalkDir` iteration, before entries are passed to `compress_entries`.

**Validates: Requirement 2.4**

## Testing Strategy

### Validation Approach

The testing strategy follows a two-phase approach: first, surface counterexamples that demonstrate the bug on unfixed code, then verify the fix works correctly and preserves existing behavior.

### Exploratory Bug Condition Checking

**Goal**: Surface counterexamples that demonstrate the bug BEFORE implementing the fix. Confirm or refute the root cause analysis. If we refute, we will need to re-hypothesize.

**Test Plan**: Create a temporary directory with a mix of readable and unreadable files, then call `stream_archive_with_entries` and read the resulting `AsyncRead` to completion. On unfixed code, the reader will return EOF without error, confirming the silent swallowing. Also call `send_path` with a mock TCP stream to verify the EOF sentinel and checksum are sent for partial data.

**Test Cases**:
1. **Unreadable File Test**: Create a directory with one readable file and one file with no read permissions. Call `stream_archive_with_entries` and read to EOF. On unfixed code, no error is returned (will demonstrate the bug).
2. **Locked File Test (Windows)**: Create a directory with a file locked by another process. Same test — on unfixed code, no error is returned.
3. **Deleted File Test**: Create a directory, walk it, delete a file, then call `stream_archive_with_entries` with the stale entries. On unfixed code, no error is returned.
4. **All Readable Test**: Create a directory with only readable files. Verify the archive stream completes normally (baseline — should pass on both unfixed and fixed code).

**Expected Counterexamples**:
- `stream_archive_with_entries` returns an `AsyncRead` that yields EOF without error when compression fails
- `send_path` sends EOF sentinel + SHA-256 checksum for partial data
- Possible root cause confirmed: `eprintln!` swallows the error, pipe EOF is indistinguishable from normal completion

### Fix Checking

**Goal**: Verify that for all inputs where the bug condition holds, the fixed function produces the expected behavior.

**Pseudocode:**
```
FOR ALL input WHERE isBugCondition(input) DO
  result := stream_archive_with_entries'(input.path, input.entries)
  bytes := read_to_completion(result.reader)
  ASSERT bytes.is_error
    AND bytes.error CONTAINS file path or "compression" or "permission"
END FOR
```

### Preservation Checking

**Goal**: Verify that for all inputs where the bug condition does NOT hold, the fixed function produces the same result as the original function.

**Pseudocode:**
```
FOR ALL input WHERE NOT isBugCondition(input) DO
  ASSERT stream_archive_with_entries(input) = stream_archive_with_entries'(input)
    -- Same compressed bytes, same archive contents
END FOR
```

**Testing Approach**: Property-based testing is recommended for preservation checking because:
- It generates many directory structures with varying file counts, sizes, and nesting depths
- It catches edge cases like empty directories, single-file directories, and directories with only subdirectories
- It provides strong guarantees that the archive output is byte-identical for all readable inputs

**Test Plan**: Observe behavior on UNFIXED code first for fully-readable directories, then write property-based tests capturing that behavior.

**Test Cases**:
1. **Full Archive Preservation**: Generate random directory trees with readable files, archive with both old and new code, verify byte-identical output
2. **Single File Transfer Preservation**: Verify `Kind::File` transfers are completely unaffected by the changes
3. **Clipboard Transfer Preservation**: Verify `Kind::Clipboard` transfers are completely unaffected
4. **Progress Event Preservation**: For successful archive transfers, verify the final progress event has `bytes_done == total_bytes` and `done == true`

### Unit Tests

- Test that `stream_archive_with_entries` returns an error-propagating `AsyncRead` when compression fails on an unreadable file
- Test that reading from the error-propagating `AsyncRead` returns `io::Error` (not silent EOF) when the compression thread fails
- Test that `compress_entries` correctly returns `Err` for unreadable files (this already works — the bug is in the caller)
- Test that the final progress event for a successful archive transfer has `bytes_done == total_bytes`
- Test edge cases: empty directory, directory with only subdirectories, directory with one file

### Property-Based Tests

- Generate random directory trees (varying depth, file count, file sizes) with all files readable, verify archive output is identical before and after fix
- Generate random directory trees with a random subset of files made unreadable, verify the fixed code returns an error
- Generate random single-file and clipboard transfer inputs, verify behavior is unchanged

### Integration Tests

- End-to-end test: sender transfers a folder with an unreadable file, receiver detects the failure (connection dropped, no successful unpack)
- End-to-end test: sender transfers a fully-readable folder, receiver unpacks successfully with valid checksum
- End-to-end test: verify progress events during a successful archive transfer show 100% at completion
