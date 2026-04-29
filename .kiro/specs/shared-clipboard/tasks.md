# 实施计划：共享剪贴板（shared-clipboard）

## 概述

基于设计文档，将共享剪贴板功能分为七个阶段递增实施：数据模型与核心类型 → EchoGuard 与大小校验 → ClipboardSyncService 核心逻辑 → 发送与接收集成 → Tauri 命令层与状态管理 → 前端集成 → 最终验证。每个阶段在前一阶段基础上构建，确保代码始终可编译、可运行。

## 任务

- [x] 1. 定义核心数据类型与模块骨架
  - [x] 1.1 在 `core/src/` 中创建 `clipboard_sync.rs` 模块文件，并在 `core/src/lib.rs` 中注册 `pub mod clipboard_sync`
    - 定义 `SyncPeer` 结构体（device_name, addr, last_seen, online），派生 Debug, Clone, Serialize, Deserialize, PartialEq, Eq
    - 定义 `SyncGroupConfig` 结构体（enabled, peers），派生 Debug, Clone, Serialize, Deserialize, PartialEq
    - 定义 `ClipPayload` 结构体（content_type, text, image_png, source_device, timestamp），派生 Debug, Clone, Serialize, Deserialize
    - 定义 `SizeError` 枚举（TextTooLarge, ImageTooLarge）
    - 定义 `ClipSyncError` 结构体（kind, message, device），派生 Debug, Clone, Serialize
    - 定义常量 `TEXT_MAX_BYTES = 10 * 1024 * 1024` 和 `IMAGE_MAX_BYTES = 50 * 1024 * 1024`
    - _需求: 1.1, 1.3, 1.4, 6.1, 6.2_

  - [x] 1.2 扩展 `core/src/clipboard_history.rs` 中的 `ClipEntry`，新增 `source_device: Option<String>` 字段
    - 添加 `#[serde(skip_serializing_if = "Option::is_none", default)]` 属性
    - 在 `ClipEntry::new` 中将 `source_device` 初始化为 `None`
    - 确保现有 JSON 持久化文件向后兼容（缺少该字段时反序列化为 None）
    - _需求: 7.2_

  - [x] 1.3 在 `core/Cargo.toml` 的 `[dev-dependencies]` 中添加 `proptest = "1"`
    - 用于后续属性测试
    - _需求: 测试基础设施_

- [x] 2. 实现 EchoGuard 与 SizeValidator
  - [x] 2.1 在 `clipboard_sync.rs` 中实现 `EchoGuard` 结构体
    - 内部维护 `suppressed: Vec<(u64, Instant)>` 和 `window: Duration`（默认 3 秒）
    - 实现 `new(window: Duration) -> Self`
    - 实现 `register(&mut self, content_hash: u64)` — 添加 (hash, now + window) 到列表
    - 实现 `is_suppressed(&mut self, content_hash: u64) -> bool` — 先 cleanup 过期条目，再查找匹配
    - 实现 `cleanup(&mut self)` — 移除所有 expiry < now 的条目
    - 复用 `clipboard_history.rs` 中的 `fnv1a` 哈希函数（需将其改为 `pub`）
    - _需求: 1.5_

  - [x] 2.2 在 `clipboard_sync.rs` 中实现 `validate_size` 函数
    - 接受 `&ClipContent` 参数，返回 `Result<(), SizeError>`
    - 文本类型：检查 `text.len()` 是否超过 `TEXT_MAX_BYTES`
    - 图片类型：检查 `rgba.len()` 编码为 PNG 后的大小是否超过 `IMAGE_MAX_BYTES`（或直接检查 RGBA 数据大小作为近似）
    - _需求: 6.1, 6.2, 6.3_

  - [ ]* 2.3 编写属性测试：EchoGuard 抑制与放行
    - **Property 3: Echo Guard 抑制与放行**
    - 使用 `proptest` 生成随机 u64 哈希值
    - 验证注册后在窗口内查询相同哈希返回 true，不同哈希返回 false
    - **验证需求: 1.5**

  - [ ]* 2.4 编写属性测试：内容大小限制执行
    - **Property 8: 内容大小限制执行**
    - 使用 `proptest` 生成随机大小的文本和图片数据
    - 验证超过限制时返回错误，在限制内返回 Ok
    - **验证需求: 6.1, 6.2, 6.3**

- [x] 3. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 4. 实现 ClipboardSyncService 核心逻辑
  - [x] 4.1 实现 SyncGroupConfig 的持久化加载与保存
    - 存储路径：`{data_local_dir}/rust-air/sync_clipboard.json`
    - `load() -> SyncGroupConfig`：从磁盘读取 JSON，文件不存在或损坏时返回默认配置（enabled=false, peers=[]）
    - `save(config: &SyncGroupConfig)`：序列化为 JSON 写入磁盘
    - _需求: 3.4, 4.4_

  - [x] 4.2 实现 `ClipboardSyncService` 结构体及其核心方法
    - `new() -> Self`：加载配置，初始化 EchoGuard（3 秒窗口），设置 enabled 状态
    - `config(&self) -> SyncGroupConfig`：返回当前配置的克隆
    - `save_config(&self, config: SyncGroupConfig)`：更新内存配置并持久化
    - `add_peer(&self, peer: SyncPeer)`：添加设备到 peers 列表并保存
    - `remove_peer(&self, device_name: &str)`：从 peers 列表移除设备并保存
    - `update_peer_status(&self, device_name: &str, addr: &str)`：更新设备的 last_seen 和 addr
    - `set_enabled(&self, enabled: bool)`：设置 AtomicBool 并更新配置
    - `online_peers(&self) -> Vec<SyncPeer>`：返回 `peers.iter().filter(|p| p.online).collect()`
    - _需求: 1.1, 3.2, 3.3, 3.4, 3.5, 4.2, 4.3, 4.4_

  - [x] 4.3 实现 `should_broadcast` 方法
    - 检查 enabled 状态（false 则返回 false）
    - 对内容计算 fnv1a 哈希，查询 EchoGuard（被抑制则返回 false）
    - 调用 `validate_size` 检查大小限制（超限则返回 false）
    - 全部通过则返回 true
    - _需求: 1.5, 4.2, 4.5, 6.1, 6.2_

  - [ ]* 4.4 编写属性测试：SyncGroup 成员增删
    - **Property 5: SyncGroup 成员增删**
    - 使用 `proptest` 生成随机 SyncPeer 数据
    - 验证添加后 peers 长度 +1 且包含该设备，移除后长度 -1 且不包含
    - **验证需求: 3.2, 3.3**

  - [ ]* 4.5 编写属性测试：SyncGroupConfig 持久化往返
    - **Property 6: SyncGroupConfig 持久化往返**
    - 使用 `proptest` 生成随机 SyncGroupConfig
    - 序列化为 JSON 再反序列化，验证与原始配置相等
    - **验证需求: 3.4, 4.4**

  - [ ]* 4.6 编写属性测试：同步开关控制 should_broadcast
    - **Property 7: 同步开关控制服务行为**
    - 验证 enabled=false 时 should_broadcast 返回 false
    - 验证 enabled=true 且内容未被抑制且未超限时返回 true
    - **验证需求: 4.2, 4.3, 4.5**

  - [ ]* 4.7 编写属性测试：广播目标匹配在线同步组成员
    - **Property 4: 广播目标匹配在线同步组成员**
    - 使用 `proptest` 生成包含随机 online/offline 设备的 SyncGroupConfig
    - 验证 `online_peers()` 返回的集合恰好等于 `peers.filter(|p| p.online)`
    - **验证需求: 1.1, 3.5**

- [x] 5. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 6. 实现发送与接收集成
  - [x] 6.1 实现 `ClipboardSyncService::broadcast` 异步方法
    - 获取 `online_peers()` 列表
    - 对每个在线设备：建立 TCP 连接 → 调用现有 `transfer::send_clipboard` 发送文本内容
    - name 字段使用 `clip:text:DEVICE_NAME` 或 `clip:image:DEVICE_NAME` 格式
    - 连接失败时记录 warn 日志，跳过该设备，继续发送其他设备
    - 返回 `Vec<BroadcastResult>` 包含每个设备的发送结果
    - _需求: 1.1, 1.2, 1.3, 5.1, 5.2_

  - [x] 6.2 扩展 `transfer.rs` 中的 `send_clipboard` 以支持图片发送
    - 新增 `send_clipboard_image` 函数，接受 PNG 编码的 `&[u8]` 数据
    - 使用 `Kind::Clipboard`，name 字段设为 `clip:image:DEVICE_NAME`
    - 复用现有加密传输流程（ChaCha20-Poly1305 + SHA-256）
    - _需求: 1.4, 5.1, 5.2_

  - [x] 6.3 实现 `ClipboardSyncService::handle_received` 方法
    - 解析接收到的数据：根据 name 前缀（`clip:text:` 或 `clip:image:`）判断内容类型
    - 提取 source_device 名称
    - 文本：直接构造 `ClipContent::Text`
    - 图片：将 PNG 数据解码为 RGBA，构造 `ClipContent::Image`
    - 将内容哈希注册到 EchoGuard（防止回声）
    - 写入本地剪贴板（通过 arboard）
    - 返回 `ClipContent` 供历史记录使用
    - _需求: 2.1, 2.2, 2.3, 2.4, 2.5, 1.5_

  - [x] 6.4 修改 `transfer.rs` 中 `receive_to_disk` 的 `Kind::Clipboard` 分支
    - 解析 name 字段以区分 `clip:text` 和 `clip:image` 类型
    - 提取 source_device 信息
    - 文本类型：保持现有行为（写入剪贴板）
    - 图片类型：将 PNG 数据解码为 RGBA 后通过 arboard 写入剪贴板
    - SHA-256 校验失败时丢弃数据并返回错误
    - _需求: 2.1, 2.2, 2.3, 2.4_

  - [ ]* 6.5 编写属性测试：ClipPayload 文本序列化往返
    - **Property 1: Clip_Payload 文本序列化往返**
    - 使用 `proptest` 生成随机 UTF-8 文本
    - 封装为 ClipPayload 序列化再反序列化，验证文本内容一致
    - **验证需求: 1.3, 2.1, 2.2**

  - [ ]* 6.6 编写属性测试：图片 PNG 编码往返
    - **Property 2: 图片 PNG 编码往返**
    - 使用 `proptest` 生成随机 RGBA 图像数据（宽高 > 0）
    - 编码为 PNG 再解码回 RGBA，验证像素数据一致
    - **验证需求: 1.4, 2.3**

  - [ ]* 6.7 编写属性测试：远程剪贴板条目包含来源设备
    - **Property 10: 远程剪贴板条目包含来源设备**
    - 使用 `proptest` 生成随机 source_device 名称
    - 验证 handle_received 后 ClipEntry 的 source_device 字段与 payload 一致
    - **验证需求: 7.2**

- [x] 7. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 8. 实现 Tauri 命令层与状态管理
  - [x] 8.1 创建 `tauri-app/src-tauri/src/clip_sync_commands.rs` 模块
    - 定义 `ClipSyncState` 结构体，包含 `Arc<ClipboardSyncService>`
    - 实现 Tauri 命令：`get_sync_group`, `save_sync_group`, `add_sync_peer`, `remove_sync_peer`, `set_clip_sync_enabled`, `get_clip_sync_enabled`
    - 在 `tauri-app/src-tauri/src/lib.rs` 中注册模块和命令
    - _需求: 3.2, 3.3, 4.1, 4.2, 4.3_

  - [x] 8.2 在 `tauri-app/src-tauri/src/lib.rs` 的 `setup` 中初始化 ClipSyncState
    - 创建 `ClipboardSyncService` 实例并包装为 `Arc`
    - 通过 `.manage(ClipSyncState { ... })` 注册到 Tauri 状态
    - 根据保存的 enabled 状态决定是否启动同步
    - _需求: 4.4_

  - [x] 8.3 修改 `clip_history_commands.rs` 中的 `start_clip_monitor`，集成同步广播
    - 在检测到新的剪贴板内容后，检查 `ClipSyncState` 是否启用
    - 如果启用且 `should_broadcast` 返回 true，则异步调用 `broadcast` 发送给所有在线设备
    - 广播失败不影响本地剪贴板历史记录
    - _需求: 1.1, 1.2, 1.3, 1.4_

  - [x] 8.4 在接收端集成同步处理
    - 修改 `commands.rs` 中的 accept loop，当接收到 `Kind::Clipboard` 类型数据时：
      - 解析 name 字段获取 source_device
      - 将内容哈希注册到 EchoGuard
      - 将 ClipEntry 的 source_device 设置为发送方设备名
      - 通过 `app.emit("clip-update", ...)` 通知前端更新历史
      - 通过 `app.emit("clip-sync-received", ...)` 通知前端显示同步提示
    - _需求: 2.1, 2.2, 2.3, 2.5, 7.2_

  - [x] 8.5 集成 mDNS 设备发现与 SyncPeer 状态更新
    - 在 `device-found` 事件处理中，调用 `update_peer_status` 更新 SyncPeer 的 last_seen 和 addr
    - 实现定时检查（每 30 秒），将 last_seen 超过 30 秒的设备标记为 offline
    - _需求: 3.1, 3.5_

- [x] 9. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 10. 前端界面集成
  - [x] 10.1 在 `App.vue` 中添加共享剪贴板相关的状态和类型定义
    - 添加 `SyncGroupConfig`、`SyncPeer` TypeScript 接口
    - 添加 `clipSyncEnabled`、`syncGroup` 响应式状态
    - 添加 `clip-sync-error` 和 `clip-sync-received` 事件监听
    - _需求: 7.1, 7.3_

  - [x] 10.2 在设备列表页面中集成剪贴板共享控制
    - 为每个设备添加"共享剪贴板"按钮/图标，点击后调用 `add_sync_peer` 或 `remove_sync_peer`
    - 已加入 Sync_Group 的设备显示共享状态图标
    - 在侧边栏或设置页面添加全局同步开关（Sync_Toggle），调用 `set_clip_sync_enabled`
    - _需求: 3.2, 3.3, 4.1, 7.1_

  - [x] 10.3 在剪贴板历史中显示来源设备信息
    - 修改 `ClipEntryView` 添加 `source_device: Option<String>` 字段
    - 在 `clip_history_commands.rs` 的 `From<&ClipEntry> for ClipEntryView` 中传递 source_device
    - 前端在历史条目中显示来源设备名称标签（如 "来自 DESKTOP-ABC"）
    - _需求: 7.2_

  - [x] 10.4 实现错误提示 toast 通知
    - 监听 `clip-sync-error` 事件
    - 显示 3 秒的 toast 通知，包含错误类型和消息
    - 支持 "size_limit"、"transfer_failed"、"checksum_failed" 三种错误类型
    - _需求: 6.3, 7.3_

- [x] 11. 最终检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [ ]* 12. 编写属性测试：损坏数据拒绝
  - **Property 9: 损坏数据拒绝**
  - 在 `core/tests/clipboard_sync_test.rs` 中实现
  - 使用 `proptest` 生成有效的 ClipPayload 字节流，然后随机翻转位或截断
  - 验证接收方校验失败并拒绝写入
  - **验证需求: 2.4**

## 备注

- 标记 `*` 的任务为可选任务，可跳过以加速 MVP 交付
- 每个任务引用了具体的需求条款以确保可追溯性
- 检查点确保增量验证，避免问题累积
- 属性测试验证通用正确性属性（设计文档中的 Property 1-10），单元测试验证具体示例和边界情况
- 所有属性测试文件位于 `core/tests/clipboard_sync_test.rs`
- 复用现有 `Kind::Clipboard` 传输协议和 ChaCha20-Poly1305 加密，不引入新的线协议
