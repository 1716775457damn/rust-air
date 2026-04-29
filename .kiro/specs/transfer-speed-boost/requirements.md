# 需求文档：传输速度优化（transfer-speed-boost）

## 简介

rust-air 是一个局域网文件传输应用，当前在千兆局域网环境下传输速度仅约 12MB/s，远低于理论上限（80–100MB/s）。瓶颈主要来自：串行加密流水线、过小的 CHUNK 大小（256KB）、缺少 TCP socket 调优、以及 BufWriter 容量不足。本需求旨在通过多层次优化将传输吞吐量提升至 60MB/s 以上。

## 术语表

- **Transfer_Engine**: rust-air 的核心传输引擎，负责文件读取、加密、网络发送和接收（`core/src/transfer.rs`）
- **Encryptor**: 基于 ChaCha20-Poly1305 的流式 AEAD 加密器（`core/src/crypto.rs`）
- **Decryptor**: 基于 ChaCha20-Poly1305 的流式 AEAD 解密器（`core/src/crypto.rs`）
- **CHUNK**: 单次加密/传输的数据块大小，当前为 256KB（`core/src/proto.rs`）
- **BufWriter**: tokio 的缓冲写入器，用于合并小写入以减少系统调用
- **Pipeline**: 流水线架构，指读取、加密、网络写入等阶段并行执行
- **TCP_Tuning**: TCP socket 参数调优，包括 TCP_NODELAY、SO_SNDBUF、SO_RCVBUF 等
- **Progress_Callback**: 传输进度回调函数，定期向上层报告传输状态

## 需求

### 需求 1：增大数据块大小

**用户故事：** 作为用户，我希望传输引擎使用更大的数据块，以减少加密和系统调用的次数，从而提升吞吐量。

#### 验收标准

1. THE Transfer_Engine SHALL use a CHUNK size of 1MB (1,048,576 bytes) for data encryption and transmission
2. WHEN the CHUNK size is changed, THE Encryptor SHALL pre-allocate its internal frame buffer to match the new CHUNK size
3. WHEN the CHUNK size is changed, THE Decryptor SHALL pre-allocate its internal data buffer and spare buffer to match the new CHUNK size
4. WHEN transferring data with the new CHUNK size, THE Transfer_Engine SHALL maintain full backward compatibility with the v4 wire protocol framing format

### 需求 2：流水线加密架构（双缓冲）

**用户故事：** 作为用户，我希望文件读取和加密能够并行执行，以消除串行等待时间，充分利用 CPU 和 I/O 带宽。

#### 验收标准

1. WHEN sending a file, THE Transfer_Engine SHALL read the next data chunk concurrently while the current chunk is being encrypted and written to the network
2. THE Transfer_Engine SHALL use a double-buffering or channel-based pipeline to overlap file I/O with encryption
3. WHILE the pipeline is active, THE Transfer_Engine SHALL maintain correct SHA-256 checksum computation by hashing chunks in sequential order
4. WHILE the pipeline is active, THE Encryptor SHALL maintain monotonically increasing nonce counters to preserve AEAD security guarantees
5. IF a read error occurs in the pipeline, THEN THE Transfer_Engine SHALL propagate the error and terminate the transfer cleanly

### 需求 3：TCP Socket 调优

**用户故事：** 作为用户，我希望传输连接的 TCP 参数经过优化，以减少网络延迟和提升吞吐量。

#### 验收标准

1. WHEN a TCP connection is established for transfer, THE Transfer_Engine SHALL set TCP_NODELAY to true on the socket to disable Nagle's algorithm
2. WHEN a TCP connection is established for transfer, THE Transfer_Engine SHALL set the SO_SNDBUF (send buffer) size to at least 2MB (2,097,152 bytes)
3. WHEN a TCP connection is established for transfer, THE Transfer_Engine SHALL set the SO_RCVBUF (receive buffer) size to at least 2MB (2,097,152 bytes)
4. IF setting a socket option fails, THEN THE Transfer_Engine SHALL log a warning and continue the transfer with default socket settings

### 需求 4：增大 BufWriter 容量

**用户故事：** 作为用户，我希望接收端的写入缓冲区更大，以减少磁盘写入系统调用次数，提升接收端吞吐量。

#### 验收标准

1. THE Transfer_Engine SHALL use a BufWriter capacity of at least 4MB (4,194,304 bytes) on the receiver side for file writes
2. WHEN the BufWriter capacity is increased, THE Transfer_Engine SHALL maintain the same flush behavior at transfer completion to ensure data integrity

### 需求 5：接收端流水线解密

**用户故事：** 作为用户，我希望接收端的解密和磁盘写入也能并行执行，以消除接收端的串行瓶颈。

#### 验收标准

1. WHEN receiving a file, THE Transfer_Engine SHALL overlap network reading and decryption with disk writing
2. WHILE the receive pipeline is active, THE Decryptor SHALL maintain correct nonce ordering for AEAD authentication
3. WHILE the receive pipeline is active, THE Transfer_Engine SHALL maintain correct SHA-256 checksum computation by hashing decrypted chunks in sequential order
4. IF a decryption authentication failure occurs, THEN THE Transfer_Engine SHALL abort the transfer and report the error with the failing frame number

### 需求 6：进度回调频率优化

**用户故事：** 作为用户，我希望进度回调不会成为传输性能的瓶颈，同时仍能提供流畅的进度更新。

#### 验收标准

1. THE Transfer_Engine SHALL emit progress callbacks at intervals of no less than 100ms
2. WHEN the progress callback interval is increased, THE Transfer_Engine SHALL continue to emit a final progress callback with done=true upon transfer completion
3. THE Transfer_Engine SHALL avoid acquiring locks or performing blocking operations within the progress callback path

### 需求 7：传输吞吐量目标

**用户故事：** 作为用户，我希望在千兆局域网环境下传输速度显著提升，达到接近网络带宽上限的水平。

#### 验收标准

1. WHEN transferring a file of 100MB or larger over a 1Gbps LAN, THE Transfer_Engine SHALL achieve a sustained throughput of at least 60MB/s
2. WHEN transferring a file of 100MB or larger over a 1Gbps LAN, THE Transfer_Engine SHALL achieve a sustained throughput that is at least 5x the current baseline of 12MB/s
3. WHILE transferring data, THE Transfer_Engine SHALL maintain CPU utilization below 80% on a single core to leave headroom for the operating system and other processes
