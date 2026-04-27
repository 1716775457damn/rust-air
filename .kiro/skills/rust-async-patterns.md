---
name: rust-async-patterns
description: "Rust 异步编程模式与 tokio 最佳实践。当涉及 async/await、tokio spawn、sync-async 桥接、channel 通信时使用此 skill。"
inclusion: fileMatch
fileMatchPattern: "**/*.rs"
---

# Rust 异步编程规范 (tokio)

## Sync → Async 桥接

### 正确做法: ChannelWriter + mpsc
```rust
// 同步线程写入 → mpsc channel → async 侧读取
let (tx, rx) = tokio::sync::mpsc::channel(8);
std::thread::spawn(move || {
    let writer = ChannelWriter::new(tx);
    sync_operation(writer)?;
});
// async 侧从 rx 读取
```

### 正确做法: SyncIoBridge
```rust
// async reader/writer → 同步 Read/Write
let (reader, writer) = tokio::io::duplex(buf_size);
tokio::task::spawn_blocking(move || {
    let sync_reader = tokio_util::io::SyncIoBridge::new(reader);
    sync_operation(sync_reader)
});
// async 侧写入 writer
```

### 错误做法 ❌
```rust
// 不要在 async task 中做同步阻塞 I/O
while let Some(chunk) = dec.read_chunk().await? {
    sync_writer.write_all(&chunk)?;  // ❌ 阻塞 tokio worker
}
```

## 错误传播模式

### 跨线程错误传播
```rust
let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
let slot_clone = error_slot.clone();

std::thread::spawn(move || {
    if let Err(e) = operation() {
        *slot_clone.lock().unwrap() = Some(e.to_string());
    }
});

// async 侧在 EOF 时检查 error_slot
```

### 不要静默吞错误 ❌
```rust
std::thread::spawn(move || {
    if let Err(e) = operation() {
        eprintln!("error: {e}");  // ❌ 调用方永远不知道出错了
    }
});
```

## 缓冲区管理

- `BufWriter::with_capacity(4 * CHUNK, file)` — 文件写入用 4×CHUNK 缓冲
- mpsc channel 容量 = 预期 in-flight 数据量 / chunk 大小
- `Vec` 复用优于每次分配: `std::mem::replace` + `spare_buf` 模式

## 进度报告
- 用 `Instant::elapsed()` 节流，不要每个 chunk 都报告
- 50ms 间隔是合理的平衡点
- 最终事件必须标记 `done: true`
