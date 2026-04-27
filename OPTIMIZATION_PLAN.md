# rust-air 性能与算法优化计划

## 一、传输管道架构优化

### 1.1 消除 archive 双管道跳转（高优先级）

**现状问题**：`stream_archive_with_entries` 中数据经过三次跳转：
```
compress_entries → os_pipe → pump_pipe → tokio::duplex → stream_encrypted_hash
```
`os_pipe` + `tokio::duplex(16MB)` 构成两级缓冲，`pump_pipe` 在中间做 sync→async 桥接，引入额外线程切换和内存拷贝。

**优化方案**：去掉 `os_pipe`，让 `compress_entries` 直接写入自定义 `impl Write`，内部通过 `tokio::sync::mpsc` 将 `Vec<u8>` 发送到 async 侧：
```
compress_entries → mpsc channel → AsyncRead adapter → stream_encrypted_hash
```

**预期收益**：archive 吞吐量提升 10-20%，内存占用减少 ~16MB。

### 1.2 接收端 Archive 解包避免阻塞 tokio worker（中优先级）

**现状问题**：`receive_to_disk` 的 `Kind::Archive` 分支中 `sync_w.write_all(&chunk)?` 是同步阻塞调用，在 async task 中可能阻塞 tokio worker。

**优化方案**：用 `tokio::io::duplex` 替代 `os_pipe`，或将写入也放到 `spawn_blocking` 中。

**预期收益**：避免 tokio worker 被阻塞，高并发场景更稳定。
