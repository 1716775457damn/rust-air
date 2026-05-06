# Bugfix Requirements Document

## Introduction

This bugfix addresses two critical issues in the rust-air file transfer system:

1. **Reconnection logic is inverted** — when a transfer is interrupted, the receiver attempts to reconnect to the sender's ephemeral port, which always fails because the sender is not listening. Reconnection must be sender-initiated since the receiver is the one with a persistent listener.

2. **Transfer throughput is bottlenecked at ~10 MB/s on LAN** — the send and receive pipelines serialize operations (read, hash, encrypt, write) that should run in parallel. Combined with a small TCP socket buffer (2 MB) and unbuffered encrypted writes, this prevents the system from saturating a gigabit LAN link (~100+ MB/s).

## Bug Analysis

### Current Behavior (Defect)

1.1 WHEN a TCP connection drops mid-transfer THEN the receiver's `receive_with_reconnect` function attempts to reconnect by calling `TcpStream::connect(addr)` where `addr` is the sender's ephemeral peer address, which always fails because the sender has no listener on that port

1.2 WHEN all reconnect attempts fail (due to 1.1) THEN the transfer is permanently abandoned even though the receiver's listener port is still active and the .part file and manifest are intact for resume

1.3 WHEN sending a file via `stream_encrypted_hash` (archive path) THEN read, hash, encrypt, and network write operations execute serially in a single loop, limiting throughput to the speed of the slowest stage

1.4 WHEN sending a file via `stream_encrypted_hash_pipeline` (file path) THEN file reading is pipelined but hash computation, encryption, and network write still execute serially in the consumer task

1.5 WHEN receiving a file in `receive_file_branch` or `receive_archive_branch` THEN decryption, hash computation, and disk write execute serially (decrypt → hash → send to write task), with only the final disk write offloaded to a separate task

1.6 WHEN the TCP socket is tuned via `tune_socket` THEN the send and receive buffers are set to only 2 MB, which is insufficient to keep a gigabit LAN pipe full given the latency of the encrypt/decrypt pipeline

1.7 WHEN the `Encryptor` writes an encrypted chunk THEN it issues a single `write_all` call per chunk without any buffering layer, causing excessive system call overhead on the network socket

### Expected Behavior (Correct)

2.1 WHEN a TCP connection drops mid-transfer THEN the sender SHALL detect the failure and retry by reconnecting to the receiver's listener port (the address the sender originally connected to), re-initiating the transfer protocol so the receiver's existing .part file and manifest enable automatic resume

2.2 WHEN the sender retries after a connection drop THEN the receiver SHALL accept the new incoming connection on its existing listener, detect the matching .part file and manifest, and resume the transfer from the chunk-aligned boundary without data loss

2.3 WHEN sending a file or archive THEN the send pipeline SHALL overlap file reading, hash computation, encryption, and network writing using concurrent tasks or pipelining so that no single stage blocks the others

2.4 WHEN receiving a file or archive THEN the receive pipeline SHALL overlap network reading/decryption, hash computation, and disk writing using concurrent tasks so that no single stage blocks the others

2.5 WHEN tuning the TCP socket for transfer THEN the send and receive buffer sizes SHALL be set to at least 8 MB to allow sufficient in-flight data for high-throughput LAN transfers

2.6 WHEN the `Encryptor` writes encrypted data to the network THEN it SHALL use a buffered writer to coalesce multiple small writes into fewer system calls, reducing per-chunk syscall overhead

2.7 WHEN transferring files on a gigabit LAN THEN the system SHALL be capable of sustaining throughput of 100 MB/s or higher, limited primarily by the network link speed rather than CPU or I/O serialization

### Unchanged Behavior (Regression Prevention)

3.1 WHEN a fresh transfer completes without interruption THEN the system SHALL CONTINUE TO deliver the file with correct SHA-256 checksum verification and no data corruption

3.2 WHEN a transfer is resumed from a .part file and manifest THEN the system SHALL CONTINUE TO validate the full-stream SHA-256 checksum (including the already-received prefix) and reject corrupted data

3.3 WHEN the receiver detects a manifest mismatch (different name, size, or kind) THEN the system SHALL CONTINUE TO discard the stale .part file and start a fresh transfer

3.4 WHEN a clipboard sync transfer is sent or received THEN the system SHALL CONTINUE TO handle it identically (clipboard transfers do not use reconnect or resume logic)

3.5 WHEN the AEAD nonce counter is aligned for resume THEN the system SHALL CONTINUE TO set the Encryptor/Decryptor counter to `already_have / CHUNK` so that nonces match the original stream position

3.6 WHEN the transfer protocol header is exchanged THEN the system SHALL CONTINUE TO use the same wire format (MAGIC, key, kind, name_len, name, total_size) and resume handshake (8-byte already_have) so that existing protocol compatibility is preserved

3.7 WHEN a transfer is cancelled by the user via the cancellation token THEN the system SHALL CONTINUE TO abort promptly and preserve the .part file and manifest for future manual retry

---

## Bug Condition (Formal)

### Bug 1: Reconnection Direction

```pascal
FUNCTION isBugCondition_Reconnect(X)
  INPUT: X of type TransferState
  OUTPUT: boolean
  
  // The bug triggers when a TCP connection drops mid-transfer
  // and the receiver attempts to reconnect to the sender
  RETURN X.connection_dropped = true AND X.reconnect_initiator = RECEIVER
END FUNCTION

// Property: Fix Checking — Sender-Initiated Reconnect
FOR ALL X WHERE isBugCondition_Reconnect(X) DO
  result ← transfer_with_reconnect'(X)
  ASSERT result.reconnect_initiator = SENDER
    AND result.reconnect_target = RECEIVER_LISTENER_PORT
    AND (result.transfer_completed OR result.part_file_preserved)
END FOR

// Property: Preservation Checking
FOR ALL X WHERE NOT isBugCondition_Reconnect(X) DO
  ASSERT F(X) = F'(X)
END FOR
```

### Bug 2: Transfer Speed Bottleneck

```pascal
FUNCTION isBugCondition_Speed(X)
  INPUT: X of type TransferConfig
  OUTPUT: boolean
  
  // The bug triggers on any file/archive transfer where pipeline
  // stages are serialized instead of concurrent
  RETURN X.transfer_kind IN {File, Archive} AND X.network = LAN
END FUNCTION

// Property: Fix Checking — Pipeline Parallelism
FOR ALL X WHERE isBugCondition_Speed(X) DO
  result ← transfer'(X)
  ASSERT result.throughput >= 100_MB_per_sec ON gigabit_LAN
    AND result.data_integrity = VALID
    AND result.pipeline_stages_concurrent = true
END FOR

// Property: Preservation Checking
FOR ALL X WHERE NOT isBugCondition_Speed(X) DO
  ASSERT F(X) = F'(X)
END FOR
```
