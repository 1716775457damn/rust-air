# Bugfix Requirements Document

## Introduction

When transferring a folder via the `Kind::Archive` path, the transfer terminates prematurely if any file within the directory cannot be read (e.g., locked system files). The `compress_entries` function in `archive.rs` only prints errors to stderr and then drops the pipe writer, which causes the compressed stream to EOF early. The sender and receiver both interpret this premature EOF as a normal end-of-stream, resulting in a partial transfer that appears successful. Additionally, progress reporting compares compressed bytes received against the uncompressed total size, making the final "completed" amount appear smaller than the total — reinforcing the impression that the transfer stopped early.

## Bug Analysis

### Current Behavior (Defect)

1.1 WHEN a folder is transferred via `Kind::Archive` and `compress_entries` encounters an unreadable file (e.g., locked or permission-denied) THEN the system silently prints the error to stderr, drops the pipe writer, and the transfer completes with only a partial archive — no error is reported to the caller

1.2 WHEN a folder is transferred via `Kind::Archive` and `compress_entries` fails partway through THEN the sender sends the EOF sentinel and SHA-256 checksum for the partial compressed data, and the receiver unpacks the partial archive and reports success

1.3 WHEN a folder transfer via `Kind::Archive` completes (whether fully or partially) THEN the system reports progress as compressed bytes received vs uncompressed total size, causing the final progress to show a value (e.g., ~500MB) significantly less than the total (e.g., ~1GB), misleading the user about transfer completeness

### Expected Behavior (Correct)

2.1 WHEN a folder is transferred via `Kind::Archive` and `compress_entries` encounters an unreadable file THEN the system SHALL propagate the error to the caller so the transfer fails with a descriptive error message indicating which file could not be read

2.2 WHEN a folder is transferred via `Kind::Archive` and `compress_entries` fails partway through THEN the system SHALL NOT send a successful EOF sentinel and checksum; instead, the transfer SHALL be aborted and the receiver SHALL be notified of the failure

2.3 WHEN a folder transfer via `Kind::Archive` completes successfully THEN the system SHALL report progress using consistent units — either both compressed or both uncompressed — so that the final progress accurately reflects transfer completion (bytes_done equals total_bytes when done)

2.4 WHEN a folder is transferred via `Kind::Archive` THEN the system SHALL skip runtime-generated log files (e.g., `*.log`, `*.log.*`) during `walk_dir` and `compress_entries`, excluding them from both the total size calculation and the archive, so that transient/locked log files do not cause transfer failures

### Unchanged Behavior (Regression Prevention)

3.1 WHEN a folder is transferred via `Kind::Archive` and all files are readable THEN the system SHALL CONTINUE TO produce a complete zstd-compressed tar archive and transfer it successfully with a valid SHA-256 checksum

3.2 WHEN a single file is transferred via `Kind::File` THEN the system SHALL CONTINUE TO transfer the file correctly with accurate progress reporting and resume support

3.3 WHEN clipboard text is transferred via `Kind::Clipboard` THEN the system SHALL CONTINUE TO transfer the text correctly and write it to the receiver's clipboard

3.4 WHEN a folder transfer via `Kind::Archive` completes successfully THEN the system SHALL CONTINUE TO unpack the archive into the destination directory without corruption

3.5 WHEN a folder contains only small files (below the 1MB threshold) THEN the system SHALL CONTINUE TO pre-read them in parallel via rayon and archive them correctly

---

## Bug Condition

```pascal
FUNCTION isBugCondition(X)
  INPUT: X of type ArchiveTransferInput  -- a folder path with file entries
  OUTPUT: boolean

  // Returns true when any file in the folder cannot be read during compression
  RETURN EXISTS file IN X.entries WHERE NOT is_readable(file)
END FUNCTION
```

## Property Specification

```pascal
// Property: Fix Checking — Error Propagation
FOR ALL X WHERE isBugCondition(X) DO
  result ← send_path'(X)
  ASSERT result.is_error
    AND result.error_message CONTAINS "unreadable file"
    AND receiver_notified_of_failure(result)
END FOR
```

## Preservation Goal

```pascal
// Property: Preservation Checking — Successful Transfers Unchanged
FOR ALL X WHERE NOT isBugCondition(X) DO
  ASSERT send_path(X) = send_path'(X)
    -- All readable folders transfer identically before and after the fix
END FOR
```
