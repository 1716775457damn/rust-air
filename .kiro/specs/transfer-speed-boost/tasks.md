# 实施计划：传输速度优化（transfer-speed-boost）

## 概述

基于设计文档，将优化分为六个阶段递增实施：常量与依赖变更 → 发送端流水线 → 接收端流水线 → TCP 调优 → 进度回调优化 → 集成验证。每个阶段在前一阶段基础上构建，确保代码始终可编译、可运行。

## 任务

- [x] 1. 增大 CHUNK 常量与添加 socket2 依赖
  - [x] 1.1 修改 `core/src/proto.rs` 中 `CHUNK` 常量从 `256 * 1024` 改为 `1024 * 1024`（1MB）
    - Encryptor/Decryptor 的缓冲区预分配已引用 `CHUNK`，会自动适配
    - BufWriter 容量表达式 `4 * CHUNK` 也会自动从 1MB 变为 4MB
    - _需求: 1.1, 1.2, 1.3, 4.1_

  - [x] 1.2 在 `core/Cargo.toml` 中添加 `socket2 = "0.5"` 依赖
    - 用于后续 TCP socket 调优（`SockRef::from` 设置缓冲区大小）
    - _需求: 3.1, 3.2, 3.3_

  - [ ]* 1.3 编写单元测试验证 CHUNK 常量值和缓冲区容量
    - 验证 `CHUNK == 1_048_576`
    - 验证 Encryptor frame_buf 容量 >= `4 + 16 + CHUNK`
    - 验证 Decryptor data_buf 和 spare_buf 容量 >= `CHUNK`
    - _需求: 1.1, 1.2, 1.3_

- [x] 2. 实现发送端流水线加密
  - [x] 2.1 在 `core/src/transfer.rs` 中新增 `stream_encrypted_hash_pipeline` 异步函数
    - 使用 `tokio::sync::mpsc::channel::<Result<Vec<u8>>>(2)` 创建 bounded channel
    - 读取 task 通过 `tokio::spawn` 独立运行，循环读满 CHUNK 大小后发送到 channel
    - 加密 task 从 channel 接收数据，顺序计算 SHA-256 后调用 `enc.write_chunk`
    - 读取 task 中的 I/O 错误通过 channel 发送 `Err` 传播
    - 读取 task panic 通过 `JoinHandle` 捕获并转换为 `anyhow::Error`
    - _需求: 2.1, 2.2, 2.3, 2.4, 2.5_

  - [x] 2.2 将 `send_path` 中文件发送分支从调用 `stream_encrypted_hash` 改为调用 `stream_encrypted_hash_pipeline`
    - 文件发送（`Kind::File`）使用流水线版本
    - Archive 发送暂保持原有串行版本（archive reader 不适合 spawn）
    - 保留原有 `stream_encrypted_hash` 函数供 archive 和 clipboard 使用
    - _需求: 2.1, 2.2_

  - [ ]* 2.3 编写属性测试：加密解密往返一致性
    - **属性 1：加密解密往返一致性**
    - 使用 `proptest` 生成随机长度（0 到 4MB）的随机字节数据
    - 通过 `tokio::io::duplex` 连接 Encryptor 和 Decryptor
    - 验证解密输出与原始输入完全一致
    - **验证需求: 1.4, 2.4, 5.2**

  - [ ]* 2.4 编写属性测试：流水线 SHA-256 完整性
    - **属性 2：流水线 SHA-256 完整性**
    - 使用 `proptest` 生成随机长度的随机字节数据
    - 通过发送端流水线函数处理，比较流水线计算的 SHA-256 与直接 `sha2::Sha256::digest` 的结果
    - **验证需求: 2.3, 5.3**

- [x] 3. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 4. 实现接收端流水线解密
  - [x] 4.1 重构 `receive_to_disk` 中 `Kind::File` 分支，将磁盘写入分离到独立 task
    - 创建 `tokio::sync::mpsc::channel::<Vec<u8>>(2)` 用于解密→写入的数据传递
    - 写入 task 通过 `tokio::spawn` 运行，从 channel 接收数据并 `write_all` + 最终 `flush`
    - 主 task 负责解密 + SHA-256 哈希 + 发送到写入 channel
    - 不再调用 `dec.recycle()`（chunk 所有权已转移给写入 task）
    - `drop(write_tx)` 后 `await` 写入 task 以捕获写入错误
    - _需求: 5.1, 5.2, 5.3, 5.4_

  - [ ]* 4.2 编写单元测试验证接收端流水线
    - 测试空数据传输（0 字节）正确处理
    - 测试单 chunk 以内的小文件传输
    - 测试恰好 N 个 CHUNK 的精确边界传输
    - 测试解密认证失败时的帧号报告
    - _需求: 5.1, 5.3, 5.4_

- [x] 5. 实现 TCP Socket 调优
  - [x] 5.1 在 `core/src/transfer.rs` 中新增 `tune_socket` 辅助函数
    - 使用 `socket2::SockRef::from` 获取底层 socket 引用
    - 设置 `TCP_NODELAY = true`
    - 设置 `SO_SNDBUF = 2MB`（2,097,152 字节）
    - 设置 `SO_RCVBUF = 2MB`（2,097,152 字节）
    - 任何设置失败时 `eprintln!` 警告并继续，不中断传输
    - _需求: 3.1, 3.2, 3.3, 3.4_

  - [x] 5.2 在 `send_path` 和 `receive_to_disk` 入口处、`stream.into_split()` 之前调用 `tune_socket`
    - 确保发送端和接收端都应用 TCP 调优
    - _需求: 3.1, 3.2, 3.3_

  - [ ]* 5.3 编写单元测试验证 `tune_socket` 不 panic
    - 在正常 TcpStream 上调用 `tune_socket` 不应 panic
    - _需求: 3.4_

- [x] 6. 优化进度回调间隔
  - [x] 6.1 将 `transfer.rs` 中所有进度回调间隔从 50ms 改为 100ms
    - 修改 `stream_encrypted_hash` 中的 `last_emit.elapsed().as_millis() >= 50` 为 `>= 100`
    - 修改 `stream_encrypted_hash_pipeline` 中的对应阈值
    - 修改 `receive_to_disk` 的 `Kind::File` 分支中的对应阈值
    - 修改 `receive_to_disk` 的 `Kind::Archive` 分支中的对应阈值
    - 确保传输完成时仍发出 `done=true` 的最终回调
    - _需求: 6.1, 6.2, 6.3_

  - [ ]* 6.2 编写属性测试：进度回调节流
    - **属性 3：进度回调节流**
    - 使用 `proptest` 生成随机大小的数据（1MB 到 10MB）
    - 运行传输流程并收集所有进度回调的时间戳
    - 验证连续非终止回调间隔 >= 100ms
    - **验证需求: 6.1**

- [x] 7. 集成验证与最终检查点
  - [x] 7.1 编写端到端集成测试
    - 通过 loopback TCP 发送随机文件，验证接收文件与原始文件字节一致
    - 覆盖 File、Archive、Clipboard 三种 Kind
    - 验证所有优化组合后传输仍然正确
    - _需求: 1.4, 2.3, 5.3, 7.1_

  - [x] 7.2 最终检查点 — 确保所有测试通过
    - 确保所有测试通过，如有问题请询问用户。

## 备注

- 标记 `*` 的任务为可选任务，可跳过以加速 MVP 交付
- 每个任务引用了具体的需求条款以确保可追溯性
- 检查点确保增量验证，避免问题累积
- 属性测试验证通用正确性属性，单元测试验证具体示例和边界情况
- 所有优化对上层 API 透明，`send_path` / `receive_to_disk` 函数签名不变
