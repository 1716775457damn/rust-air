# 实施计划：共享白板（shared-whiteboard）

## 概述

基于设计文档，将共享白板功能分为七个阶段递增实施：核心数据类型与 WhiteboardStore → 同步消息与冲突解决 → 广播与接收集成 → Tauri 命令层 → 接收端路由集成 → 前端界面 → 最终验证。每个阶段在前一阶段基础上构建，确保代码始终可编译、可运行。

## 任务

- [x] 1. 定义核心数据类型与 WhiteboardStore
  - [x] 1.1 在 `core/Cargo.toml` 的 `[dependencies]` 中添加 `uuid = { version = "1", features = ["v4", "serde"] }`
    - 用于生成白板条目的全局唯一标识符
    - _需求: 4.3_

  - [x] 1.2 在 `core/src/` 中创建 `whiteboard.rs` 模块文件，并在 `core/src/lib.rs` 中注册 `pub mod whiteboard`（desktop feature gate 下）
    - 定义 `WhiteboardContentType` 枚举（Text, Image），派生 Debug, Clone, Serialize, Deserialize, PartialEq, Eq
    - 定义 `WhiteboardItem` 结构体（id, content_type, text, image_b64, timestamp, source_device），派生 Debug, Clone, Serialize, Deserialize
    - 定义 `SyncOp` 枚举（Add, Delete, Clear, Snapshot），派生 Debug, Clone, Serialize, Deserialize, PartialEq, Eq
    - 定义 `WhiteboardSyncMessage` 结构体（op, source_device, timestamp, item, item_id, items），派生 Debug, Clone, Serialize, Deserialize
    - 定义 `WhiteboardError` 结构体（kind, message, device），派生 Debug, Clone, Serialize
    - _需求: 1.2, 1.3, 1.5, 4.3, 6.1, 6.2, 6.3, 6.4, 6.5_

  - [x] 1.3 在 `whiteboard.rs` 中实现 `WhiteboardStore` 结构体
    - 字段：`items: Vec<WhiteboardItem>`, `path: PathBuf`, `dirty: bool`, `last_save: Instant`
    - 存储路径：`{data_local_dir}/rust-air/whiteboard.json`
    - 实现 `load() -> Self`：从磁盘读取 JSON，文件不存在或损坏时返回空白板并记录 warn 日志
    - 实现 `add(&mut self, item: WhiteboardItem) -> bool`：如果已存在相同 UUID 则按时间戳决定是否替换，否则插入并保持按 timestamp 升序排列，标记 dirty
    - 实现 `delete(&mut self, id: &str) -> bool`：按 UUID 删除条目，标记 dirty
    - 实现 `clear(&mut self)`：清空所有条目，标记 dirty
    - 实现 `apply_snapshot(&mut self, items: Vec<WhiteboardItem>)`：用快照替换全部内容，标记 dirty
    - 实现 `snapshot(&self) -> Vec<WhiteboardItem>`：返回所有条目的克隆
    - 实现 `flush_if_needed(&mut self)`：如果 dirty 且距上次保存 ≥2s，写入磁盘
    - 实现 `flush_now(&mut self)`：立即写入磁盘
    - _需求: 1.2, 1.3, 1.4, 2.1, 2.2, 2.3, 2.4, 2.5, 4.1, 4.2, 4.3, 4.4, 5.3, 5.5_

  - [ ]* 1.4 编写属性测试：添加条目使白板列表增长
    - **Property 1: 添加条目使白板列表增长**
    - 在 `core/tests/whiteboard_test.rs` 中实现
    - 使用 `proptest` 生成随机 WhiteboardItem（UUID 不重复）
    - 验证 add 后列表长度 +1 且新条目存在于列表中
    - **验证需求: 1.2, 1.3**

  - [ ]* 1.5 编写属性测试：白板条目按时间戳排序
    - **Property 2: 白板条目按时间戳排序**
    - 在 `core/tests/whiteboard_test.rs` 中实现
    - 使用 `proptest` 生成多个随机 WhiteboardItem 并依次 add
    - 验证 store 中的条目始终按 timestamp 升序排列
    - **验证需求: 1.4**

  - [ ]* 1.6 编写属性测试：WhiteboardItem 持久化往返
    - **Property 3: WhiteboardItem 持久化往返**
    - 在 `core/tests/whiteboard_test.rs` 中实现
    - 使用 `proptest` 生成随机 WhiteboardItem 列表
    - 序列化为 JSON 再反序列化，验证内容完全一致
    - **验证需求: 2.2, 2.3, 2.4**

- [x] 2. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 3. 实现同步消息处理与冲突解决
  - [x] 3.1 在 `whiteboard.rs` 中实现 `apply_sync_message` 方法
    - 接受 `&mut WhiteboardStore` 和 `WhiteboardSyncMessage` 参数
    - Add 操作：调用 `store.add(item)`
    - Delete 操作：调用 `store.delete(item_id)`
    - Clear 操作：调用 `store.clear()`
    - Snapshot 操作：调用 `store.apply_snapshot(items)`
    - _需求: 3.4, 4.1, 4.2, 4.4, 5.3, 5.5_

  - [x] 3.2 实现 `broadcast_sync_message` 异步函数
    - 接受 `WhiteboardSyncMessage`、设备列表 `&[DeviceInfo]`、本地设备名
    - 将 SyncMessage 序列化为 JSON 字节
    - 对每个设备：建立 TCP 连接 → 调用 `transfer::send_clipboard` 发送，name 字段使用 `wb:sync:{device_name}` 格式
    - 连接失败时记录 warn 日志，跳过该设备，继续发送其他设备
    - 返回 `Vec<BroadcastResult>` 包含每个设备的发送结果
    - _需求: 3.1, 3.2, 3.3, 3.5, 3.6, 7.1, 7.2_

  - [x] 3.3 实现 `handle_received_whiteboard` 函数
    - 接受 name（`wb:sync:DEVICE`）和 data（JSON 字节）
    - 解析 JSON 为 `WhiteboardSyncMessage`
    - 解析失败时返回错误（丢弃消息，记录 warn 日志）
    - 返回解析后的 `WhiteboardSyncMessage`
    - _需求: 3.4, 6.5_

  - [ ]* 3.4 编写属性测试：SyncMessage 应用正确性
    - **Property 4: SyncMessage 应用正确性**
    - 在 `core/tests/whiteboard_test.rs` 中实现
    - 使用 `proptest` 生成随机 WhiteboardStore 状态和随机 SyncMessage
    - 验证 Add 后 store 包含该 item，Delete 后不包含，Clear 后为空，Snapshot 后内容一致
    - **验证需求: 3.4, 4.1, 4.2, 5.3, 5.5**

  - [ ]* 3.5 编写属性测试：时间戳优先冲突解决
    - **Property 5: 时间戳优先冲突解决**
    - 在 `core/tests/whiteboard_test.rs` 中实现
    - 使用 `proptest` 生成已存在条目和具有相同 UUID 但不同时间戳的远程 Add 操作
    - 验证远程时间戳更大时本地被替换，更小时保持不变
    - **验证需求: 4.4**

  - [ ]* 3.6 编写属性测试：WhiteboardSyncMessage JSON 序列化往返
    - **Property 6: WhiteboardSyncMessage JSON 序列化往返**
    - 在 `core/tests/whiteboard_test.rs` 中实现
    - 使用 `proptest` 生成随机 SyncMessage（覆盖 Add、Delete、Clear、Snapshot 四种操作）
    - 序列化为 JSON 再反序列化，验证语义等价
    - **验证需求: 6.1, 6.3, 6.4, 6.5**

  - [ ]* 3.7 编写属性测试：快照包含完整白板内容
    - **Property 7: 快照包含完整白板内容**
    - 在 `core/tests/whiteboard_test.rs` 中实现
    - 使用 `proptest` 生成随机 WhiteboardStore 状态
    - 验证 `snapshot()` 返回的列表与 store.items 完全一致
    - **验证需求: 7.4**

- [x] 4. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 5. 实现 Tauri 命令层
  - [x] 5.1 创建 `tauri-app/src-tauri/src/whiteboard_commands.rs` 模块
    - 定义 `WhiteboardState` 结构体，包含 `Mutex<WhiteboardStore>`
    - 实现 `get_whiteboard_items` 命令：返回白板所有条目
    - 实现 `add_whiteboard_text` 命令：创建文本 WhiteboardItem（生成 UUID、获取时间戳和设备名），调用 store.add，广播 Add SyncMessage，通过 `app.emit("whiteboard-update", ...)` 通知前端
    - 实现 `add_whiteboard_image` 命令：创建图片 WhiteboardItem（Base64 编码），同上流程
    - 实现 `delete_whiteboard_item` 命令：调用 store.delete，广播 Delete SyncMessage，通知前端
    - 实现 `clear_whiteboard` 命令：调用 store.clear，广播 Clear SyncMessage，通知前端
    - 实现 `flush_whiteboard` 命令：调用 store.flush_if_needed
    - _需求: 1.2, 1.3, 2.1, 3.1, 3.5, 4.1, 4.2, 4.5, 5.3, 5.4, 8.2, 8.5_

  - [x] 5.2 在 `tauri-app/src-tauri/src/lib.rs` 中注册白板模块和命令
    - 添加 `mod whiteboard_commands`（desktop feature gate 下）
    - 在 `run()` 中初始化 `WhiteboardState`（调用 `WhiteboardStore::load()`），通过 `.manage()` 注册
    - 在 `invoke_handler` 中注册所有白板命令：`get_whiteboard_items`, `add_whiteboard_text`, `add_whiteboard_image`, `delete_whiteboard_item`, `clear_whiteboard`, `flush_whiteboard`
    - _需求: 8.1_

  - [ ]* 5.3 编写白板命令的单元测试
    - 测试 add_whiteboard_text 后 get_whiteboard_items 返回新条目
    - 测试 delete_whiteboard_item 后条目被移除
    - 测试 clear_whiteboard 后列表为空
    - _需求: 1.2, 1.3, 5.3_

- [x] 6. 接收端路由集成
  - [x] 6.1 修改 `commands.rs` 中 `start_listener` 的 accept loop
    - 在 `ReceiveOutcome::Clipboard { name, data, .. }` 分支中，检测 name 前缀是否为 `wb:`
    - 如果是 `wb:` 前缀：调用 `handle_received_whiteboard` 解析 SyncMessage → 调用 `apply_sync_message` 合并到 WhiteboardStore → 持久化 → 通过 `app.emit("whiteboard-update", ...)` 通知前端
    - 如果不是 `wb:` 前缀：保持现有剪贴板同步处理逻辑不变
    - 解析失败时通过 `app.emit("whiteboard-error", ...)` 通知前端
    - _需求: 3.4, 4.1, 4.2, 4.5, 7.3_

  - [x] 6.2 实现新设备加入时的全量快照发送
    - 在 `scan_devices` 命令中，当发现新设备时，构造 Snapshot SyncMessage 并发送给新设备
    - 或在白板命令层提供 `send_whiteboard_snapshot` 命令，由前端在检测到新设备时调用
    - _需求: 7.4, 7.5_

- [x] 7. 检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

- [x] 8. 前端界面集成
  - [x] 8.1 在 `App.vue` 中添加白板相关的 TypeScript 类型和状态
    - 添加 `WhiteboardItem` 接口（id, content_type, text, image_b64, timestamp, source_device）
    - 添加 `WhiteboardError` 接口（kind, message, device）
    - 添加 `whiteboardItems` 响应式状态 `ref<WhiteboardItem[]>([])`
    - 在 `Tab` 类型中添加 `"whiteboard"` 选项
    - 在 `TAB_KEYS` 中添加键盘快捷键映射（如 `"7": "whiteboard"`）
    - _需求: 8.1_

  - [x] 8.2 注册白板相关的 Tauri 事件监听
    - 在 `onMounted` 中注册 `whiteboard-update` 事件监听，更新 `whiteboardItems`
    - 注册 `whiteboard-error` 事件监听，调用 `showToast` 显示错误通知
    - 在 `onMounted` 中调用 `invoke("get_whiteboard_items")` 加载初始白板内容
    - 设置定时器每 3 秒调用 `invoke("flush_whiteboard")` 触发持久化
    - _需求: 2.1, 4.5, 8.1_

  - [x] 8.3 实现白板标签页 UI
    - 在侧边栏导航中添加"白板"标签页按钮（图标 📋 或 🖊️）
    - 白板页面顶部：文本输入框 + 提交按钮，支持 Enter 键提交
    - 白板页面顶部：图片粘贴区域，监听 paste 事件，将图片转为 Base64 后调用 `add_whiteboard_image`
    - 白板页面主体：以列表形式展示所有 WhiteboardItem，按时间顺序从上到下排列
    - 文本条目：显示文本内容、创建时间、来源设备名称
    - 图片条目：显示图片预览（Base64 → img src）、创建时间、来源设备名称
    - 每个条目右侧提供删除按钮（🗑），点击调用 `delete_whiteboard_item`
    - _需求: 1.1, 1.2, 1.3, 1.4, 1.5, 8.2, 8.3, 8.4, 8.5_

  - [x] 8.4 实现清空白板功能
    - 在白板页面顶部或底部添加"清空白板"按钮，使用醒目的警告色样式（红色/橙色）
    - 点击后显示确认对话框（"确定要清空所有白板内容吗？此操作将同步到所有设备。"）
    - 确认后调用 `invoke("clear_whiteboard")`
    - _需求: 5.1, 5.2, 5.3, 5.4, 8.6_

- [x] 9. 最终检查点 — 确保所有测试通过
  - 确保所有测试通过，如有问题请询问用户。

## 备注

- 标记 `*` 的任务为可选任务，可跳过以加速 MVP 交付
- 每个任务引用了具体的需求条款以确保可追溯性
- 检查点确保增量验证，避免问题累积
- 属性测试验证通用正确性属性（设计文档中的 Property 1-7），单元测试验证具体示例和边界情况
- 所有属性测试文件位于 `core/tests/whiteboard_test.rs`
- 复用现有 `Kind::Clipboard` 传输协议和 ChaCha20-Poly1305 加密，通过 name 前缀 `wb:` 区分白板消息和剪贴板消息
- 白板模块仅在 desktop feature 下可用，与现有 clipboard_sync 模块保持一致的 feature gate 策略
