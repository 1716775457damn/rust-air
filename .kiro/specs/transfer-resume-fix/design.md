# Transfer Resume Fix — Bugfix Design

## Overview

This design addresses two bugs in the rust-air file transfer system:

1. **Reconnection direction is inverted**: `receive_with_reconnect` attempts to reconnect to the sender's ephemeral peer address after a TCP drop. Since the sender has no listener on that port, reconnection always fails. The fix moves reconnection responsibility to the sender side — the sender detects transfer failure and retries by connecting to the receiver's known listener port with exponential backoff. The receiver simply accepts new connections and resumes via its existing `.part` file + manifest mechanism.

2. **Transfer speed bottlenecked at ~10 MB/s**: The send and receive pipelines serialize operations that should run concurrently. The fix introduces full pipeline parallelism on both sides, increases TCP socket buffers from 2 MB to 8 MB, and wraps the `Encryptor`'s inner writer in a `BufWriter` to reduce syscall overhead.

## Glossary

- **Bug_Condition (C)**: Two conditions — (1) TCP connection drops mid-transfer and the receiver attempts to reconnect to the sender's ephemeral port; (2) file/archive transfer where pipeline stages are serialized
- **Property (P)**: (1) Sender retries connecting to receiver's listener port; (2) Pipeline stages run concurrently, achieving ≥100 MB/s on gigabit LAN
- **Preservation**: Existing behaviors that must remain unchanged — fresh transfer integrity, resume checksum validation, manifest mismatch handling, clipboard sync, nonce alignment, wire format, cancellation
- **`receive_with_reconnect`**: Function in `core/src/transfer.rs` that currently handles reconnection on the receiver side — must be replaced with sender-side retry
- **`stream_encrypted_hash`**: Serial send pipeline for archives — read→hash→encrypt→write in one loop
- **`stream_encrypted_hash_pipeline`**: Partially pipelined send for files — read is separate but hash+encrypt+write are serial
- **`receive_file_branch` / `receive_archive_branch`**: Receive pipelines — decrypt→hash in main task, only disk write offloaded
- **`tune_socket`**: Sets TCP buffer sizes (currently 2 MB) and disables Nagle
- **`Encryptor`**: In `core/src/crypto.rs` — writes `[4B len][16B tag][ciphertext]` per chunk via unbuffered `write_all`
- **CHUNK**: 1 MB constant in `core/src/proto.rs`

## Bug Details

### Bug Condition

The bugs manifest in two independent scenarios:

**Bug 1 — Reconnection Direction**: When a TCP connection drops mid-transfer, `receive_with_reconnect` calls `TcpStream::connect(addr)` where `addr` is the sender's ephemeral peer address (obtained from `listener.accept()`). The sender has no listener on that port, so every reconnect attempt fails. The receiver's own listener port remains active and could accept new connections, but the sender has no retry logic.

**Bug 2 — Speed Bottleneck**: On any file or archive transfer over a fast LAN, the pipeline serializes CPU-bound (hash, encrypt/decrypt) and I/O-bound (read, write, network) operations. Combined with 2 MB TCP buffers and unbuffered encrypted writes, throughput is capped at ~10 MB/s instead of the ~100+ MB/s a gigabit link can sustain.

**Formal Specification:**
```
FUNCTION isBugCondition(input)
  INPUT: input of type TransferState
  OUTPUT: boolean

  // Bug 1: Reconnection direction
  IF input.connection_dropped = true
     AND input.reconnect_initiator = RECEIVER
     AND input.reconnect_target = SENDER_EPHEMERAL_PORT
  THEN RETURN true

  // Bug 2: Speed bottleneck
  IF input.transfer_kind IN {File, Archive}
     AND input.pipeline_serialized = true
     AND input.tcp_buffer_size <= 2MB
  THEN RETURN true

  RETURN false
END FUNCTION
```

### Examples

- **Bug 1 — Reconnect fails**: Sender connects from `192.168.1.10:54321` to receiver at `192.168.1.20:12345`. Transfer drops at 50%. Receiver calls `TcpStream::connect("192.168.1.10:54321")` — connection refused because sender has no listener. All 5 retries fail. Expected: sender detects failure and reconnects to `192.168.1.20:12345`, receiver accepts and resumes from `.part` file.
- **Bug 1 — Resume data preserved but unused**: After all reconnect attempts fail, the `.part` file and manifest are intact on the receiver, but no mechanism exists for the sender to retry. The transfer is permanently abandoned.
- **Bug 2 — File send at 10 MB/s**: Sending a 1 GB file over gigabit LAN. `stream_encrypted_hash_pipeline` reads chunks in a separate task but then serially hashes, encrypts, and writes each chunk. Expected: read, hash+encrypt, and network write run as 3 concurrent pipeline stages, achieving ~100+ MB/s.
- **Bug 2 — Archive send at 10 MB/s**: `stream_encrypted_hash` runs read→hash→encrypt→write in a single loop with no concurrency. Expected: same 3-stage pipeline as file send.
- **Bug 2 — Receive at 10 MB/s**: `receive_file_branch` decrypts and hashes in the main task, only offloading disk write. Expected: network read+decrypt, hash, and disk write run as 3 concurrent stages.

## Expected Behavior

### Preservation Requirements

**Unchanged Behaviors:**
- Fresh transfers (no interruption) must deliver files with correct SHA-256 checksum verification and no data corruption
- Resumed transfers must validate the full-stream SHA-256 checksum (including the already-received prefix) and reject corrupted data
- Manifest mismatch detection (different name, size, or kind) must discard stale `.part` files and start fresh
- Clipboard sync transfers must continue to work identically (no reconnect or resume logic)
- AEAD nonce counter alignment for resume must set counter to `already_have / CHUNK`
- Wire format (MAGIC, key, kind, name_len, name, total_size) and resume handshake (8-byte already_have) must remain unchanged
- User cancellation via cancellation token must abort promptly and preserve `.part` + manifest

**Scope:**
All inputs that do NOT involve (1) TCP connection drops during file/archive transfers or (2) file/archive transfer throughput should be completely unaffected by this fix. This includes:
- Clipboard send/receive operations
- Fresh transfers that complete without interruption (correctness unchanged, speed improved)
- Discovery, mDNS registration, device scanning
- All non-transfer Tauri commands (search, sync, todo, whiteboard, etc.)

## Hypothesized Root Cause

Based on the bug description and code analysis, the root causes are:

1. **Reconnection direction is inverted in `receive_with_reconnect`**: The function receives `addr: SocketAddr` which is the sender's peer address from `listener.accept()`. On reconnect, it calls `TcpStream::connect(addr)` — connecting back to the sender's ephemeral port. The sender never listens on that port. The architectural error is that reconnection logic lives on the receiver side when it should be on the sender side, since the receiver is the one with a persistent listener.

2. **`stream_encrypted_hash` (archive path) is fully serial**: The function runs `read → hash → encrypt → write` in a single `loop` with no concurrency. Each operation blocks the next.

3. **`stream_encrypted_hash_pipeline` (file path) has partial parallelism**: File reading is pipelined via a channel, but the consumer task still serially hashes, encrypts, and writes. Hash+encrypt and network write should be separate stages.

4. **`receive_file_branch` / `receive_archive_branch` have minimal parallelism**: Decrypt and hash run in the main task; only disk write is offloaded via a channel with depth 2. Network read+decrypt, hash, and disk write should all be separate concurrent stages.

5. **TCP buffer size is too small**: `tune_socket` sets buffers to 2 MB. For a gigabit LAN with pipeline latency, 8 MB+ buffers are needed to keep the pipe full.

6. **`Encryptor` writes are unbuffered**: Each `write_chunk` call issues a single `write_all` for the frame (`[4B len][16B tag][ciphertext]`). While the frame is ~1 MB, the lack of a `BufWriter` means the OS may fragment the write into multiple syscalls. Wrapping the inner writer in a `BufWriter` reduces overhead.

7. **Channel depths are too shallow**: Pipeline channels use depth 2, which can cause stalls when stages have variable latency. Increasing to 4–8 provides better buffering.

## Correctness Properties

Property 1: Bug Condition — Sender-Initiated Reconnect

_For any_ transfer where the TCP connection drops mid-transfer, the sender SHALL detect the failure and retry by connecting to the receiver's listener port (the address the sender originally connected to) with exponential backoff, and the receiver SHALL accept the new connection and resume from the chunk-aligned `.part` file boundary, resulting in either a completed transfer or a preserved `.part` file for future retry.

**Validates: Requirements 2.1, 2.2**

Property 2: Bug Condition — Pipeline Parallelism Throughput

_For any_ file or archive transfer over a gigabit LAN, the send and receive pipelines SHALL run read, hash+encrypt, and network write (or network read+decrypt, hash, and disk write) as concurrent stages, achieving sustained throughput of ≥100 MB/s with correct data integrity (SHA-256 checksum match).

**Validates: Requirements 2.3, 2.4, 2.5, 2.6, 2.7**

Property 3: Preservation — Transfer Integrity Unchanged

_For any_ transfer that completes without interruption (fresh or resumed), the fixed code SHALL produce the same SHA-256 checksum verification, manifest handling, nonce alignment, and wire format behavior as the original code, preserving all existing correctness guarantees.

**Validates: Requirements 3.1, 3.2, 3.3, 3.5, 3.6**

Property 4: Preservation — Non-Transfer Behavior Unchanged

_For any_ input that is NOT a file/archive transfer (clipboard sync, cancellation, discovery), the fixed code SHALL produce exactly the same behavior as the original code, preserving all existing functionality.

**Validates: Requirements 3.4, 3.7**

## Fix Implementation

### Changes Required

Assuming our root cause analysis is correct:

**File**: `core/src/transfer.rs`

**Bug 1 — Sender-Side Reconnect:**

1. **Remove `receive_with_reconnect`**: Delete the receiver-side reconnect function entirely. The receiver should simply call `receive_to_disk` on each accepted connection — the `.part` + manifest mechanism already handles resume automatically.

2. **Add `send_path_with_retry`**: New function that wraps `send_path` with exponential backoff retry logic. On failure, it reconnects to the same receiver address (the listener port the sender originally connected to) and re-sends. The receiver's `receive_to_disk` will detect the `.part` file and resume from the chunk-aligned boundary.
   - Parameters: `addr: &str`, `path: &Path`, `on_progress`, `cancel_token: CancellationToken`
   - Retry: up to `MAX_RECONNECT_ATTEMPTS` with exponential backoff (2s, 4s, 8s, 16s, 32s)
   - On each retry: `TcpStream::connect(addr)` → `send_path(stream, path, on_progress)`
   - Emit `TransferEvent` with `reconnect_info` during retries

3. **Simplify receiver accept loop**: In `commands.rs`, replace `receive_with_reconnect(peer, ...)` with `receive_to_disk(stream, ...)`. Each accepted connection is an independent receive — resume is handled by `.part` + manifest.

**Bug 2 — Pipeline Parallelism:**

4. **Refactor `stream_encrypted_hash` (archive send)**: Replace the serial loop with a 3-stage pipeline:
   - Stage 1 (spawned task): Read chunks from the archive stream, send via channel
   - Stage 2 (spawned task): Receive chunks, hash + encrypt, send encrypted frames via channel
   - Stage 3 (main task): Receive encrypted frames, write to network
   - Channel depth: 4–8 for better buffering

5. **Refactor `stream_encrypted_hash_pipeline` (file send)**: Extend the existing 2-stage pipeline to 3 stages:
   - Stage 1 (existing): Read chunks from file
   - Stage 2 (new): Hash + encrypt chunks, produce encrypted frames
   - Stage 3 (new): Write encrypted frames to network
   - This requires splitting `Encryptor.write_chunk` into encrypt-only and write-only operations, or passing the encrypted frame bytes through the channel

6. **Refactor `receive_file_branch` and `receive_archive_branch`**: Extend to 3-stage pipeline:
   - Stage 1 (main or spawned): Network read + decrypt via `Decryptor.read_chunk`
   - Stage 2 (spawned): Hash computation
   - Stage 3 (existing): Disk write
   - Channel depth: 4–8

**File**: `core/src/crypto.rs`

7. **Add `BufWriter` wrapping in `Encryptor`**: Wrap the inner `W` writer in `tokio::io::BufWriter` with a large buffer (e.g., 8 MB) to coalesce writes and reduce syscall overhead. Alternatively, expose an `encrypt_chunk_to_bytes` method that returns the encrypted frame as a `Vec<u8>` for the pipeline to write separately.

**File**: `core/src/proto.rs`

8. **Add pipeline constants**: Add constants for pipeline channel depth and TCP buffer size:
   - `pub const TCP_BUF_SIZE: usize = 8 * 1024 * 1024;` (8 MB)
   - `pub const PIPELINE_DEPTH: usize = 4;`

**File**: `core/src/transfer.rs` — `tune_socket`

9. **Increase TCP buffer size**: Change `buf_size` from `2 * 1024 * 1024` to `TCP_BUF_SIZE` (8 MB).

**File**: `tauri-app/src-tauri/src/commands.rs`

10. **Update `do_send` to use `send_path_with_retry`**: Replace the single `TcpStream::connect` + `send_path` call with `send_path_with_retry` that handles reconnection with exponential backoff. Pass the `CancellationToken` from `send_cancel`.

11. **Simplify receiver accept loop**: Replace `receive_with_reconnect` calls with direct `receive_to_disk` calls. Remove the `recv_cancel` field from `AppState` if no longer needed.

**File**: `tauri-app/src/App.vue`

12. **Add sender reconnect UI**: Listen for `send-progress` events with `reconnect_info` to show reconnection status on the send side (attempt N/M, retrying...). The existing `recvReconnecting` state can be repurposed or a new `sendReconnecting` ref added.

13. **Remove receiver reconnect UI**: The `recvReconnecting`, `recvReconnectAttempt`, `recvReconnectMax` refs are no longer needed since reconnection is sender-initiated. Clean up or repurpose these.

## Testing Strategy

### Validation Approach

The testing strategy follows a two-phase approach: first, surface counterexamples that demonstrate the bugs on unfixed code, then verify the fix works correctly and preserves existing behavior.

### Exploratory Bug Condition Checking

**Goal**: Surface counterexamples that demonstrate the bugs BEFORE implementing the fix. Confirm or refute the root cause analysis. If we refute, we will need to re-hypothesize.

**Test Plan**: Write tests that exercise reconnection and measure pipeline throughput. Run these tests on the UNFIXED code to observe failures and understand the root cause.

**Test Cases**:
1. **Reconnect Direction Test**: Simulate a TCP drop mid-transfer, observe that `receive_with_reconnect` tries to connect to the sender's ephemeral port (will fail on unfixed code — connection refused)
2. **Sender Retry Absence Test**: After a connection drop, verify that the sender has no retry mechanism — `send_path` returns an error and the transfer is abandoned (will fail on unfixed code)
3. **Pipeline Serialization Test**: Measure throughput of `stream_encrypted_hash` on a large buffer — observe ~10 MB/s ceiling (will demonstrate bottleneck on unfixed code)
4. **TCP Buffer Size Test**: Inspect `tune_socket` to confirm buffers are 2 MB (will confirm on unfixed code)

**Expected Counterexamples**:
- `receive_with_reconnect` connects to sender's ephemeral port → connection refused
- `send_path` has no retry logic → transfer abandoned on first failure
- Serial pipeline throughput ≤ 15 MB/s on gigabit LAN
- Possible causes: wrong reconnect target, no sender retry, serial pipeline, small TCP buffers

### Fix Checking

**Goal**: Verify that for all inputs where the bug condition holds, the fixed function produces the expected behavior.

**Pseudocode:**
```
FOR ALL input WHERE isBugCondition(input) DO
  result := transfer_fixed(input)
  ASSERT result.reconnect_initiator = SENDER
    AND result.reconnect_target = RECEIVER_LISTENER_PORT
    AND result.throughput >= 100_MB_per_sec (on gigabit LAN)
    AND result.data_integrity = VALID
END FOR
```

### Preservation Checking

**Goal**: Verify that for all inputs where the bug condition does NOT hold, the fixed function produces the same result as the original function.

**Pseudocode:**
```
FOR ALL input WHERE NOT isBugCondition(input) DO
  ASSERT F_original(input) = F_fixed(input)
END FOR
```

**Testing Approach**: Property-based testing is recommended for preservation checking because:
- It generates many test cases automatically across the input domain
- It catches edge cases that manual unit tests might miss
- It provides strong guarantees that behavior is unchanged for all non-buggy inputs

**Test Plan**: Observe behavior on UNFIXED code first for fresh transfers, clipboard operations, and resume scenarios, then write property-based tests capturing that behavior.

**Test Cases**:
1. **Fresh Transfer Integrity**: Verify that a complete file transfer (no interruption) produces identical SHA-256 checksums before and after the fix
2. **Resume Checksum Preservation**: Verify that resuming from a `.part` file produces the same full-stream SHA-256 as a fresh transfer of the same file
3. **Manifest Mismatch Handling**: Verify that mismatched manifests (different name/size/kind) still trigger `.part` file deletion and fresh start
4. **Clipboard Transfer Preservation**: Verify that clipboard send/receive works identically before and after the fix
5. **Nonce Alignment Preservation**: Verify that `set_counter(already_have / CHUNK)` produces correct nonces for resumed transfers

### Unit Tests

- Test `send_path_with_retry` reconnection logic with mock TCP streams that fail on first attempt
- Test exponential backoff delay calculation (`reconnect_delay_secs`)
- Test that `Encryptor` with `BufWriter` produces identical ciphertext to unbuffered `Encryptor`
- Test pipeline stage isolation — each stage processes chunks independently
- Test `tune_socket` sets 8 MB buffers

### Property-Based Tests

- Generate random file sizes and verify end-to-end transfer integrity (SHA-256 match) through the pipelined path
- Generate random resume offsets (chunk-aligned) and verify nonce alignment produces correct decryption
- Generate random transfer interruption points and verify sender retry successfully completes the transfer
- Generate random non-file inputs (clipboard text of various sizes) and verify identical behavior before/after fix

### Integration Tests

- Full send+receive of a large file over loopback, measuring throughput exceeds 50 MB/s (loopback baseline)
- Simulated connection drop mid-transfer with sender retry completing the transfer
- Resume from `.part` file after sender reconnects — verify no data corruption
- Concurrent transfers to verify pipeline stages don't interfere across sessions
