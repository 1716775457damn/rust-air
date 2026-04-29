# 实施计划：断点续传功能

## 概述

基于设计文档，将断点续传功能分解为增量式编码任务。从底层数据结构和加密层扩展开始，逐步构建 Archive 续传、会话持久化、自动重连，最后完成前端集成。每个任务在前一个任务基础上递进，确保无孤立代码。

## 任务

- [x] 1. 扩展数据结构与加密层基础设施
  - [x] 1.1 扩展 `TransferEvent` 和新增 `ReconnectInfo`、`SessionManifest` 结构体
    - 在 `core/src/proto.rs` 中为 `TransferEvent` 添加 `resumed: bool`、`resume_offset: u64`、`reconnect_info: Option<ReconnectInfo>` 字段
    - 新增 `ReconnectInfo` 结构体（`attempt: u32`、`max_attempts: u32`），派生 `Debug, Clone, Serialize, Deserialize`
    - 新增 `SessionManifest` 结构体（`name: String`、`total_size: u64`、`kind: Kind`、`sender_addr: String`、`created_at: u64`），派生 `Debug, Clone, Serialize, Deserialize`
    - 更新所有现有 `TransferEvent` 构造处（`transfer.rs` 中的 `emit_progress` 函数），为新字段提供默认值（`resumed: false`、`resume_offset: 0`、`reconnect_info: None`）
    - _需求: 7.1, 7.2, 7.3, 7.4, 4.1_

  - [x] 1.2 为 `Encryptor` 和 `Decryptor` 添加 `set_counter()` 方法
    - 在 `core/src/crypto.rs` 的 `Encryptor<W>` impl 块中添加 `pub fn set_counter(&mut self, counter: u64)` 方法
    - 在 `core/src/crypto.rs` 的 `Decryptor<R>` impl 块中添加 `pub fn set_counter(&mut self, counter: u64)` 方法
    - 两个方法均直接设置 `self.counter = counter`，必须在首次 `write_chunk()`/`read_chunk()` 调用前使用
    - _需求: 2.3, 2.4_

  - [ ]* 1.3 属性测试：Resume_Offset 块对齐（Property 1）
    - **Property 1: Resume_Offset 块对齐**
    - 在 `core/tests/` 下创建 `transfer_resume_props.rs` 测试文件，使用 `proptest` crate
    - 生成随机 `u64` 文件大小，验证 `(file_size / CHUNK) * CHUNK` 始终满足 `<= file_size` 且 `% CHUNK == 0`
    - **验证: 需求 1.2**

  - [ ]* 1.4 属性测试：Nonce Counter 初始化对齐（Property 3）
    - **Property 3: Nonce Counter 初始化对齐**
    - 生成随机 CHUNK 对齐偏移量，创建 Encryptor/Decryptor 并调用 `set_counter(offset / CHUNK)`，验证内部 counter 值正确
    - **验证: 需求 2.1, 2.2**

  - [ ]* 1.5 属性测试：续传加密等价性 Round-Trip（Property 4）
    - **Property 4: 续传加密等价性**
    - 生成随机明文数据和 CHUNK 对齐偏移量，分别进行完整加密和分段加密（后段 counter 从 `offset / CHUNK` 开始），验证后段密文逐字节相同
    - **验证: 需求 2.5**

- [x] 2. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 3. 实现会话持久化（Session Manifest）
  - [x] 3.1 实现 Manifest 读写辅助函数
    - 在 `core/src/transfer.rs` 中添加 `manifest_path(dest, name) -> PathBuf` 函数，返回 `{dest}/{name}.manifest.json`
    - 实现 `async fn write_manifest(path, manifest) -> Result<()>`，将 `SessionManifest` 序列化为 JSON 写入文件
    - 实现 `async fn read_manifest(path) -> Option<SessionManifest>`，读取并反序列化，解析失败返回 `None`
    - 实现 `async fn remove_manifest(path)`，删除 manifest 文件，忽略不存在错误
    - _需求: 4.1, 4.2, 4.3, 4.4, 4.5_

  - [x] 3.2 在 `receive_to_disk` 中集成 Manifest 生命周期
    - 接收开始时创建 `SessionManifest`（记录 name、total_size、kind、sender_addr、created_at）
    - 传输成功完成后删除 manifest 文件
    - 传输失败时保留 manifest 和 `.part` 文件
    - _需求: 4.1, 4.3, 4.4_

  - [ ]* 3.3 属性测试：SessionManifest 序列化 Round-Trip（Property 6）
    - **Property 6: SessionManifest 序列化 Round-Trip**
    - 生成随机 `SessionManifest` 实例，序列化为 JSON 再反序列化，验证与原始实例等价
    - **验证: 需求 4.2**

  - [ ]* 3.4 属性测试：Manifest 不匹配检测（Property 2）
    - **Property 2: Manifest 不匹配检测**
    - 生成随机 manifest 对，验证当且仅当 `name` 和 `total_size` 都相等时判定为匹配
    - **验证: 需求 1.5**

- [x] 4. 实现 Archive 传输断点续传
  - [x] 4.1 修改 `receive_to_disk` 的 Archive 分支支持续传
    - 在 `Kind::Archive` 分支中检查 `.part` 文件和 `SessionManifest` 是否存在
    - 根据续传状态判定逻辑（设计文档中的表格）决定 `Resume_Offset`：有 `.part` + 有效 manifest 且匹配 → 对齐到 CHUNK 边界；否则删除旧文件从头开始
    - 将 `already_have` 发送给发送方
    - 若 `Resume_Offset > 0`，调用 `Decryptor::set_counter(resume_offset / CHUNK)`
    - Archive 数据写入 `.part` 文件（追加模式），传输完成后再解压
    - 在 `TransferEvent` 中设置 `resumed: true` 和 `resume_offset` 字段
    - _需求: 1.1, 1.2, 1.5, 2.2_

  - [x] 4.2 修改 `send_path` 的 Archive 分支支持续传偏移
    - 当收到非零 `Resume_Offset` 且 `kind == Archive` 时，重新生成归档流但跳过前 `Resume_Offset` 字节（读取并丢弃）
    - 调用 `Encryptor::set_counter(resume_offset / CHUNK)` 设置初始 counter
    - 仅加密并发送 `Resume_Offset` 之后的数据
    - _需求: 1.3, 2.1_

  - [x] 4.3 修改现有单文件续传逻辑集成 Manifest 验证和 nonce 对齐
    - 在 `Kind::File` 分支中增加 manifest 检查：有 `.part` 但无有效 manifest → 从头开始
    - 调用 `Encryptor::set_counter()` 和 `Decryptor::set_counter()` 确保 nonce 对齐
    - 传输完成后删除 manifest，失败时保留
    - _需求: 1.1, 1.2, 1.5, 2.1, 2.2, 4.3, 4.4_

  - [ ]* 4.4 属性测试：非续传场景默认字段（Property 7）
    - **Property 7: 非续传场景默认字段**
    - 生成随机全新传输事件（`resume_offset == 0`），验证 `resumed == false` 且 `resume_offset == 0`
    - **验证: 需求 7.4**

- [x] 5. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 6. 实现自动重连机制
  - [x] 6.1 实现 `receive_with_reconnect` 函数
    - 在 `core/src/transfer.rs` 中新增 `pub async fn receive_with_reconnect()` 函数
    - 接受参数：`addr: SocketAddr`、`dest: &Path`、`cancel_token: CancellationToken`、`on_progress` 回调
    - 在 `Cargo.toml` 中添加 `tokio-util` 依赖（用于 `CancellationToken`）
    - 当 TCP 连接断开时，按指数退避策略重试（2s、4s、8s、16s、32s），最多 5 次
    - 每次重连前通过 `TransferEvent.reconnect_info` 报告重连状态
    - 重连成功后利用 `.part` 文件和 manifest 自动进入续传模式
    - 用户取消（`cancel_token`）时立即停止所有重连尝试
    - 所有重连失败后通过 `TransferEvent.error` 报告错误，保留 `.part` 和 manifest
    - _需求: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6_

  - [ ]* 6.2 属性测试：指数退避延迟计算（Property 5）
    - **Property 5: 指数退避延迟计算**
    - 对 n=1..5 验证延迟等于 `2^n` 秒
    - **验证: 需求 3.2**

  - [ ]* 6.3 单元测试：自动重连边界场景
    - 测试取消 token 在重连等待期间触发时的行为
    - 测试达到最大重试次数后的错误报告
    - _需求: 3.4, 3.6_

- [x] 7. 集成 Tauri 命令层
  - [x] 7.1 更新 `commands.rs` 传递扩展后的 `TransferEvent`
    - 修改 `start_listener` 中的接收循环，使用 `receive_with_reconnect` 替代 `receive_to_disk`（需要传入 peer 地址和 `CancellationToken`）
    - 确保扩展后的 `TransferEvent`（含 `resumed`、`resume_offset`、`reconnect_info`）正确通过 `app.emit("recv-progress", &ev)` 传递到前端
    - 在 `AppState` 中管理接收端的 `CancellationToken`，支持用户取消重连
    - _需求: 5.1, 5.2, 7.1, 7.2, 7.3_

  - [x] 7.2 添加重试发送命令
    - 在 `commands.rs` 中新增 `retry_send` Tauri 命令，使用上次失败的相同参数（path、addr）重新发起传输
    - 在 `AppState` 中保存上次发送的参数，供重试使用
    - _需求: 5.1, 5.3_

- [x] 8. 前端续传状态展示
  - [x] 8.1 更新前端 TypeScript 类型和状态
    - 在 `App.vue` 中更新 `TransferEvent` 接口，添加 `resumed: boolean`、`resume_offset: number`、`reconnect_info?: { attempt: number; max_attempts: number }` 字段
    - 添加重连相关的响应式状态变量
    - _需求: 6.1, 7.1, 7.2, 7.3_

  - [x] 8.2 实现续传进度展示和重连 UI
    - 当 `TransferEvent.resumed === true` 时，在进度区域显示"续传中"标识和已跳过数据量
    - 进度条起始位置设置为 `resume_offset / total_bytes` 对应的百分比
    - 当 `reconnect_info` 有值时，显示"重连中 (第 N 次 / 共 5 次)"状态文本
    - 重连成功后自动切换回正常传输进度显示
    - _需求: 6.1, 6.2, 6.3, 6.4_

  - [x] 8.3 实现发送失败重试按钮
    - 在发送错误状态下显示"重试"按钮
    - 点击后调用 `retry_send` 命令，使用相同参数重新发起传输
    - 所有重连失败后显示错误信息和"重试"按钮
    - _需求: 5.3, 6.5_

- [x] 9. 最终检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

## 备注

- 标记 `*` 的任务为可选任务，可跳过以加快 MVP 进度
- 每个任务引用了具体的需求编号，确保可追溯性
- 检查点任务确保增量验证
- 属性测试使用 `proptest` crate，验证设计文档中定义的正确性属性
- 单元测试验证具体示例和边界场景
