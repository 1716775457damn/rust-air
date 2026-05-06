# Implementation Plan

- [x] 1. Write bug condition exploration tests (BEFORE implementing fix)
  - **Property 1: Bug Condition** — Reconnect Direction & Pipeline Serialization
  - **CRITICAL**: This test MUST FAIL on unfixed code — failure confirms the bugs exist
  - **DO NOT attempt to fix the test or the code when it fails**
  - **NOTE**: This test encodes the expected behavior — it will validate the fix when it passes after implementation
  - **GOAL**: Surface counterexamples that demonstrate both bugs exist
  - **Bug 1 — Reconnect Direction**: Write a test that simulates a TCP drop mid-transfer and verifies that `receive_with_reconnect` attempts to connect to the sender's ephemeral peer address (which has no listener). On UNFIXED code, the reconnect target is the sender's ephemeral port → all retries fail with "connection refused". Counterexample: `receive_with_reconnect(sender_ephemeral_addr, dest, ...)` → connection refused on every retry attempt
  - **Bug 2 — Pipeline Serialization**: Write a test that sends a multi-MB file over loopback and measures throughput. On UNFIXED code, the serial pipeline (`stream_encrypted_hash` / `stream_encrypted_hash_pipeline`) caps throughput well below what the loopback interface can sustain. Also verify that `tune_socket` sets TCP buffers to only 2 MB and that pipeline channel depth is 2
  - **Scoped PBT Approach**: For Bug 1, scope to concrete case: drop connection after first chunk, verify reconnect target is sender's ephemeral port. For Bug 2, scope to concrete case: transfer a 10 MB file, assert throughput > 50 MB/s (loopback baseline) — will fail on unfixed code due to serialization
  - Run tests on UNFIXED code
  - **EXPECTED OUTCOME**: Tests FAIL (this is correct — it proves the bugs exist)
  - Document counterexamples found (e.g., "reconnect connects to sender ephemeral port 54321 → refused", "throughput = 12 MB/s, expected > 50 MB/s")
  - Mark task complete when tests are written, run, and failures are documented
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7_

- [x] 2. Write preservation property tests (BEFORE implementing fix)
  - **Property 2: Preservation** — Transfer Integrity & Non-Transfer Behavior
  - **IMPORTANT**: Follow observation-first methodology
  - Observe on UNFIXED code: fresh file transfer (2 MB random file over loopback) completes with correct SHA-256 checksum — `send_path` + `receive_to_disk` produce byte-identical output
  - Observe on UNFIXED code: resumed transfer from `.part` file with matching manifest produces correct full-stream SHA-256 checksum
  - Observe on UNFIXED code: manifest mismatch (different name or size) causes `.part` file deletion and fresh start
  - Observe on UNFIXED code: nonce counter alignment — `Encryptor.set_counter(already_have / CHUNK)` and `Decryptor.set_counter(already_have / CHUNK)` produce correct encrypt/decrypt for resumed streams
  - Observe on UNFIXED code: wire format header (MAGIC + key + kind + name_len + name + total_size) and resume handshake (8-byte already_have) are correctly exchanged
  - Observe on UNFIXED code: `reconnect_delay_secs` returns exponential backoff values (2, 4, 8, 16, 32)
  - Write property-based tests: for random file sizes (1 KB to 4 MB), fresh transfer via `send_path` + `receive_to_disk` over loopback produces byte-identical output with valid SHA-256
  - Write property-based tests: for random resume offsets (chunk-aligned), nonce counter = `offset / CHUNK` produces correct decryption
  - Write property-based tests: `reconnect_delay_secs(n)` = `2^n` seconds for n in 1..=5
  - Verify all preservation tests PASS on UNFIXED code
  - **EXPECTED OUTCOME**: Tests PASS (this confirms baseline behavior to preserve)
  - Mark task complete when tests are written, run, and passing on unfixed code
  - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7_

- [x] 3. Fix for reconnect direction inversion and pipeline serialization

  - [x] 3.1 Add pipeline constants to `core/src/proto.rs`
    - Add `pub const TCP_BUF_SIZE: usize = 8 * 1024 * 1024;` (8 MB TCP buffer)
    - Add `pub const PIPELINE_DEPTH: usize = 4;` (pipeline channel depth)
    - _Requirements: 2.5_

  - [x] 3.2 Increase TCP buffer in `tune_socket` (`core/src/transfer.rs`)
    - Change `buf_size` from `2 * 1024 * 1024` to `proto::TCP_BUF_SIZE` (8 MB)
    - _Bug_Condition: isBugCondition_Speed(input) where tcp_buffer_size <= 2MB_
    - _Expected_Behavior: TCP buffers set to 8 MB for high-throughput LAN transfers_
    - _Preservation: Non-transfer behavior unchanged_
    - _Requirements: 2.5_

  - [x] 3.3 Add `BufWriter` wrapping in `Encryptor` (`core/src/crypto.rs`)
    - Wrap the inner `W` writer in `tokio::io::BufWriter` with 8 MB buffer capacity
    - Ensure `shutdown()` and `write_trailing()` flush the BufWriter correctly
    - Optionally add `encrypt_chunk_to_bytes` method that returns encrypted frame as `Vec<u8>` for pipeline use
    - _Bug_Condition: Encryptor issues unbuffered write_all per chunk_
    - _Expected_Behavior: BufWriter coalesces writes, reducing syscall overhead_
    - _Preservation: Identical ciphertext output — BufWriter only affects write batching, not content_
    - _Requirements: 2.6_

  - [x] 3.4 Refactor `stream_encrypted_hash` (archive send) to 3-stage pipeline (`core/src/transfer.rs`)
    - Stage 1 (spawned task): Read chunks from archive stream, send via channel (depth = `PIPELINE_DEPTH`)
    - Stage 2 (spawned task): Receive chunks, hash + encrypt, send encrypted frames via channel
    - Stage 3 (main task): Receive encrypted frames, write to network
    - Replace the serial `loop { read → hash → encrypt → write }` with concurrent stages
    - _Bug_Condition: isBugCondition_Speed(input) where pipeline_serialized = true AND transfer_kind = Archive_
    - _Expected_Behavior: Read, hash+encrypt, and network write run concurrently_
    - _Preservation: SHA-256 checksum and nonce alignment unchanged_
    - _Requirements: 2.3, 2.7_

  - [x] 3.5 Refactor `stream_encrypted_hash_pipeline` (file send) to 3-stage pipeline (`core/src/transfer.rs`)
    - Extend existing 2-stage (read | hash+encrypt+write) to 3-stage (read | hash+encrypt | write)
    - Stage 1 (existing spawned task): Read chunks from file
    - Stage 2 (new spawned task): Hash + encrypt chunks, produce encrypted frame bytes
    - Stage 3 (main task): Write encrypted frames to network
    - Use `PIPELINE_DEPTH` for channel depth
    - _Bug_Condition: isBugCondition_Speed(input) where pipeline_serialized = true AND transfer_kind = File_
    - _Expected_Behavior: All three stages run concurrently_
    - _Preservation: SHA-256 checksum and nonce alignment unchanged_
    - _Requirements: 2.3, 2.7_

  - [x] 3.6 Refactor `receive_file_branch` to 3-stage pipeline (`core/src/transfer.rs`)
    - Stage 1 (main or spawned): Network read + decrypt via `Decryptor.read_chunk`
    - Stage 2 (spawned): Hash computation on decrypted chunks
    - Stage 3 (existing spawned): Disk write
    - Increase channel depth from 2 to `PIPELINE_DEPTH`
    - _Bug_Condition: isBugCondition_Speed(input) where pipeline_serialized = true AND receive_kind = File_
    - _Expected_Behavior: Decrypt, hash, and disk write run concurrently_
    - _Preservation: SHA-256 checksum verification, nonce alignment, resume from .part unchanged_
    - _Requirements: 2.4, 2.7_

  - [x] 3.7 Refactor `receive_archive_branch` to 3-stage pipeline (`core/src/transfer.rs`)
    - Same 3-stage pattern as `receive_file_branch`
    - Stage 1: Network read + decrypt
    - Stage 2: Hash computation
    - Stage 3: Disk write to .part file
    - Increase channel depth from 2 to `PIPELINE_DEPTH`
    - _Bug_Condition: isBugCondition_Speed(input) where pipeline_serialized = true AND receive_kind = Archive_
    - _Expected_Behavior: Decrypt, hash, and disk write run concurrently_
    - _Preservation: SHA-256 checksum verification, archive decompression, .part cleanup unchanged_
    - _Requirements: 2.4, 2.7_

  - [x] 3.8 Remove `receive_with_reconnect` and add `send_path_with_retry` (`core/src/transfer.rs`)
    - Remove the `receive_with_reconnect` function entirely
    - Add `pub async fn send_path_with_retry(addr: &str, path: &Path, on_progress, cancel_token: CancellationToken) -> Result<()>`
    - Retry logic: up to `MAX_RECONNECT_ATTEMPTS` with exponential backoff (2s, 4s, 8s, 16s, 32s) via `reconnect_delay_secs`
    - On each retry: `TcpStream::connect(addr)` → `send_path(stream, path, on_progress)`
    - Emit `TransferEvent` with `reconnect_info` during retries
    - Support cancellation via `cancel_token` during backoff waits
    - _Bug_Condition: isBugCondition_Reconnect(input) where reconnect_initiator = RECEIVER_
    - _Expected_Behavior: Sender detects failure and retries to receiver's listener port_
    - _Preservation: send_path behavior unchanged for successful transfers_
    - _Requirements: 2.1, 2.2_

  - [x] 3.9 Simplify receiver accept loop in `commands.rs`
    - In both `#[cfg(feature = "desktop")]` and `#[cfg(not(feature = "desktop"))]` `start_listener` functions: replace `transfer::receive_with_reconnect(peer, &out, cancel_token, ...)` with `transfer::receive_to_disk(stream, &out, ...)`
    - Each accepted connection is an independent receive — resume is handled by `.part` + manifest automatically
    - Remove `recv_cancel` field from `AppState` if no longer needed
    - _Bug_Condition: Receiver accept loop calls receive_with_reconnect which tries to reconnect to sender_
    - _Expected_Behavior: Receiver simply accepts connections and calls receive_to_disk directly_
    - _Preservation: All receive behavior (file, archive, clipboard, whiteboard) unchanged_
    - _Requirements: 2.1, 2.2_

  - [x] 3.10 Update `do_send` in `commands.rs` to use `send_path_with_retry`
    - Replace `TcpStream::connect(&addr)` + `send_path(stream, &path, ...)` with `send_path_with_retry(&addr, &path, on_progress, cancel_token)`
    - Pass a `CancellationToken` derived from `send_cancel` oneshot
    - Emit `send-progress` events with `reconnect_info` during retries
    - _Bug_Condition: do_send has no retry logic — transfer abandoned on first failure_
    - _Expected_Behavior: Sender retries with exponential backoff on failure_
    - _Preservation: Successful sends behave identically_
    - _Requirements: 2.1, 2.2_

  - [x] 3.11 Update frontend for sender-side reconnect UI (`tauri-app/src/App.vue`)
    - Listen for `send-progress` events with `reconnect_info` to show reconnection status (attempt N/M)
    - Add `sendReconnecting` ref or repurpose existing `recvReconnecting` refs
    - Remove or repurpose receiver-side reconnect UI (`recvReconnecting`, `recvReconnectAttempt`, `recvReconnectMax`) since reconnection is now sender-initiated
    - _Requirements: 2.1_

  - [x] 3.12 Verify bug condition exploration test now passes
    - **Property 1: Expected Behavior** — Reconnect Direction & Pipeline Throughput
    - **IMPORTANT**: Re-run the SAME test from task 1 — do NOT write a new test
    - The test from task 1 encodes the expected behavior
    - When this test passes, it confirms the expected behavior is satisfied:
      - Bug 1: Sender initiates reconnect to receiver's listener port (not sender's ephemeral port)
      - Bug 2: Pipeline throughput exceeds 50 MB/s on loopback (no longer serialized)
    - Run bug condition exploration test from step 1
    - **EXPECTED OUTCOME**: Test PASSES (confirms bugs are fixed)
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 2.6, 2.7_

  - [x] 3.13 Verify preservation tests still pass
    - **Property 2: Preservation** — Transfer Integrity & Non-Transfer Behavior
    - **IMPORTANT**: Re-run the SAME tests from task 2 — do NOT write new tests
    - Run preservation property tests from step 2
    - **EXPECTED OUTCOME**: Tests PASS (confirms no regressions)
    - Confirm all preservation tests still pass after fix:
      - Fresh transfer integrity (SHA-256 match)
      - Resume checksum validation
      - Manifest mismatch handling
      - Nonce alignment correctness
      - Wire format compatibility
      - Exponential backoff calculation
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7_

- [x] 4. Checkpoint — Ensure all tests pass
  - Run `cargo test` in the `core` crate to verify all existing and new tests pass
  - Run the full test suite including `archive_bug_test`, `archive_preservation_test`, `transfer_speed_test`, and the new transfer-resume-fix tests
  - Ensure no regressions in any existing functionality
  - Ask the user if questions arise
