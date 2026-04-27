---
name: rust-archive-engine
description: "rust-air 归档引擎开发指南。当修改 archive.rs 或涉及 tar+zstd 压缩、目录遍历、流式归档、解包时使用此 skill。"
inclusion: fileMatch
fileMatchPattern: "**/archive.rs"
---

# rust-air 归档引擎开发规范

## 架构

```
发送端:
  walk_dir() → entries
  compress_entries_to_writer(ChannelWriter) → mpsc channel → ErrorAwareReader → stream_encrypted_hash

接收端:
  Decryptor → tokio::io::duplex → SyncIoBridge → unpack_archive_sync (spawn_blocking)
```

## 关键组件

### walk_dir
- 单次遍历，缓存 metadata 避免重复 stat
- 过滤 `*.log` / `*.log.*` 日志文件（防止锁定文件导致传输失败）
- 返回 `(total_uncompressed_bytes, entries_with_metadata)`

### compress_entries_to_writer
- 分区策略: 小文件 (<1MB) 用 rayon 并行预读，大文件顺序流式处理
- 小文件先写入 tar（已在内存中，快速填充管道）
- 大文件用 256KB BufReader 流式写入
- **错误处理**: rayon 并行读取必须收集错误而非 `filter_map(.ok()?)` 静默跳过

### ChannelWriter
- `impl std::io::Write`，内部 256KB 缓冲
- 缓冲满时通过 `tokio::sync::mpsc::Sender` 发送到 async 侧
- `Drop` 时 flush 剩余数据

### ErrorAwareReader
- `impl AsyncRead`，从 mpsc channel 接收数据
- EOF 时检查 `error_slot`：有错误返回 `io::Error`，无错误返回正常 EOF
- 内部维护 `remainder` + `offset` 处理跨 chunk 读取

## 开发规则

1. **不允许静默跳过文件** — 任何文件读取失败必须 `bail!` 而非 `.ok()?`
2. **压缩线程错误必须传播** — 通过 `Arc<Mutex<Option<String>>>` error_slot
3. **tar 路径** — 使用源目录名作为 tar 根目录: `Path::new(entry_name).join(rel)`
4. **zstd 级别** — 当前为 3，LAN 场景下平衡压缩率和速度
5. **内存控制** — mpsc channel 8 slots × 256KB = ~2MB in-flight，不要增大
6. **目录条目** — 目录写入 tar 时 `set_entry_type(Directory)` + `set_size(0)`
