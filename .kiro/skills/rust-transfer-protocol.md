---
name: rust-transfer-protocol
description: "rust-air 传输协议与加密引擎开发指南。当修改 transfer.rs、crypto.rs、proto.rs 或涉及传输协议、AEAD 加密、SHA-256 校验、断点续传、进度报告时使用此 skill。"
inclusion: fileMatch
fileMatchPattern: "**/transfer.rs,**/crypto.rs,**/proto.rs"
---

# rust-air 传输协议开发规范

## 协议架构 (v4)

```
Sender → Receiver:
  [4B MAGIC "RAR4"][32B one-time-key][1B kind][2B name_len][name][8B total_size]

Receiver → Sender:
  [8B already_have]  (resume offset, 0 = fresh)

Sender → Receiver:
  AEAD encrypted chunks: [4B plaintext_len][16B tag][ciphertext]
  EOF sentinel: [4B zero]
  [32B SHA-256 checksum]
```

## Kind 类型

| Kind | 值 | 说明 |
|------|-----|------|
| File | 0x01 | 单文件传输，支持断点续传 |
| Archive | 0x02 | 文件夹 tar+zstd 压缩传输 |
| Clipboard | 0x03 | 剪贴板文本传输 |

## 加密规范

- 算法: ChaCha20-Poly1305 (AEAD)
- 密钥: 每次传输随机生成 32 字节 one-time key
- Nonce: 8 字节 LE frame counter + 4 字节零填充 (12 字节)
- 每个 chunk 独立加密，counter 单调递增，nonce 永不重用
- CHUNK 大小: 256KB (`proto::CHUNK`)

## 开发规则

1. **永远不要在 async 上下文中做同步阻塞 I/O** — 用 `spawn_blocking` 或 `tokio::io::duplex` + `SyncIoBridge`
2. **错误必须传播** — 不允许 `eprintln!` 吞掉错误后继续执行，压缩/加密/IO 错误必须通过 Result 链传播到调用方
3. **进度报告频率** — 最多每 50ms 发一次 `TransferEvent`，最终事件必须 `done=true` 且 `bytes_done == total_bytes`
4. **断点续传** — 仅 `Kind::File` 支持，resume offset 必须对齐到 CHUNK 边界
5. **SHA-256 校验** — on-the-fly 计算，不做二次读取；发送方和接收方独立计算并比对
6. **缓冲区复用** — `Encryptor` 使用 `frame_buf` 复用，`Decryptor` 使用 `spare_buf` 双缓冲 + `recycle()`
7. **Archive 进度** — `total_size` 是未压缩大小，实际传输的是压缩数据，最终进度事件强制 `bytes_done = total_bytes`

## 性能注意事项

- `Encryptor::write_chunk` 将 header+tag+ciphertext 合并为单次 `write_all` 减少 syscall
- Archive 压缩使用 `ChannelWriter` → mpsc → `ErrorAwareReader` 单跳架构，无中间管道
- zstd 压缩级别 3：LAN 场景 CPU 不是瓶颈，更高压缩率减少网络传输
- 小文件 (<1MB) 通过 rayon 并行预读到内存
