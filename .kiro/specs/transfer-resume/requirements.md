# 需求文档：断点续传功能

## 简介

rust-air 是一个局域网文件传输应用，当前协议版本为 v4，使用 ChaCha20-Poly1305 加密。现有实现中，单文件传输已具备基础的断点续传能力（接收方检查 `.part` 文件大小，发送 `already_have` 偏移量，发送方跳过已发送部分），但存在以下不足：

1. 目录（Archive）传输不支持断点续传
2. 连接断开后需要用户手动重新发起传输
3. 没有自动重连机制
4. 加密 nonce counter 在续传时未正确跳过已发送的 frame 数量
5. 前端没有显示续传状态

本需求旨在完善断点续传功能，使超大文件/目录传输在网络中断后能够自动或手动恢复，减少重复传输的数据量。

## 术语表

- **Transfer_Engine**: `core/src/transfer.rs` 中的核心传输引擎，负责文件/目录的发送和接收
- **Encryptor**: `core/src/crypto.rs` 中的 ChaCha20-Poly1305 加密器，使用 frame counter 生成 nonce
- **Decryptor**: `core/src/crypto.rs` 中的 ChaCha20-Poly1305 解密器，使用 frame counter 验证和解密
- **Archive_Stream**: `core/src/archive.rs` 中的流式 tar+zstd 归档模块，用于目录传输
- **Part_File**: 接收端用于存储未完成传输数据的临时文件，扩展名为 `.part`
- **Frame_Counter**: Encryptor/Decryptor 中的单调递增计数器，用于构造 AEAD nonce，每个加密 chunk 递增一次
- **Resume_Offset**: 接收方在握手阶段发送给发送方的已接收字节数，表示续传起始位置
- **Chunk_Boundary**: 以 `CHUNK`（256 KB）为单位对齐的字节偏移量，避免部分 chunk 导致的数据损坏
- **Transfer_Session**: 一次完整的文件/目录传输过程，包含唯一标识符、源路径、目标地址等元数据
- **Session_Manifest**: 存储在接收端的 JSON 文件，记录 Transfer_Session 的元数据，用于续传时恢复状态
- **Frontend**: `tauri-app/src/App.vue` 中的 Vue 前端界面

## 需求

### 需求 1：Archive 传输断点续传

**用户故事：** 作为用户，我希望目录传输中断后能够从断点恢复，而不是从头重新传输整个目录的归档流。

#### 验收标准

1. WHEN 接收方收到 Kind::Archive 类型的传输请求，THE Transfer_Engine SHALL 检查是否存在对应的 `.part` 文件和 Session_Manifest
2. WHEN 存在有效的 `.part` 文件和 Session_Manifest，THE Transfer_Engine SHALL 将 `.part` 文件大小对齐到 Chunk_Boundary 后作为 Resume_Offset 发送给发送方
3. WHEN 发送方收到非零 Resume_Offset，THE Archive_Stream SHALL 从归档流的起始位置重新生成数据，但 Transfer_Engine 仅跳过前 Resume_Offset 字节而不通过 Encryptor 发送
4. WHEN Archive 传输完成，THE Transfer_Engine SHALL 删除对应的 Session_Manifest 文件
5. IF Archive 传输的 Session_Manifest 与当前传输的源目录名称或总大小不匹配，THEN THE Transfer_Engine SHALL 丢弃旧的 `.part` 文件并从头开始传输

### 需求 2：加密 Nonce Counter 续传对齐

**用户故事：** 作为用户，我希望续传时加密解密能正确工作，确保数据完整性不受影响。

#### 验收标准

1. WHEN 发送方以非零 Resume_Offset 开始传输，THE Encryptor SHALL 将 Frame_Counter 初始化为 `Resume_Offset / CHUNK`（即已跳过的 frame 数量）
2. WHEN 接收方以非零 Resume_Offset 开始接收，THE Decryptor SHALL 将 Frame_Counter 初始化为 `Resume_Offset / CHUNK`
3. THE Encryptor SHALL 提供一个方法用于设置初始 Frame_Counter 值
4. THE Decryptor SHALL 提供一个方法用于设置初始 Frame_Counter 值
5. FOR ALL 续传场景，解密第 N 个接收到的 frame 时使用的 nonce SHALL 与该 frame 在完整传输中使用的 nonce 一致（即 round-trip 属性：续传拼接后的完整数据流与一次性传输的数据流在加密层面等价）

### 需求 3：自动重连机制

**用户故事：** 作为用户，我希望传输因网络中断时应用能自动尝试重新连接并恢复传输，而不需要我手动操作。

#### 验收标准

1. WHEN 传输过程中 TCP 连接断开，THE Transfer_Engine SHALL 在 2 秒后自动尝试重新连接发送方
2. WHILE 自动重连进行中，THE Transfer_Engine SHALL 最多尝试 5 次重连，每次间隔按指数退避递增（2s、4s、8s、16s、32s）
3. WHEN 自动重连成功，THE Transfer_Engine SHALL 使用断点续传逻辑从上次中断位置继续传输
4. IF 所有重连尝试均失败，THEN THE Transfer_Engine SHALL 停止重连并通过 TransferEvent 报告错误，保留 `.part` 文件以便用户手动重试
5. WHILE 自动重连进行中，THE Transfer_Engine SHALL 通过 TransferEvent 报告当前重连状态（重连次数、下次重试倒计时）
6. WHEN 用户手动取消传输，THE Transfer_Engine SHALL 立即停止所有重连尝试

### 需求 4：传输会话持久化

**用户故事：** 作为用户，我希望传输中断后的会话信息被保存下来，以便后续能够正确恢复传输。

#### 验收标准

1. WHEN 接收方开始接收文件或目录，THE Transfer_Engine SHALL 创建一个 Session_Manifest 文件，记录传输名称、总大小、传输类型（File/Archive）和发送方地址
2. THE Session_Manifest SHALL 以 JSON 格式存储在接收目录中，文件名为 `{name}.manifest.json`
3. WHEN 传输成功完成，THE Transfer_Engine SHALL 删除对应的 Session_Manifest 文件
4. WHEN 传输失败且 `.part` 文件被保留，THE Transfer_Engine SHALL 保留 Session_Manifest 文件
5. IF Session_Manifest 文件损坏或无法解析，THEN THE Transfer_Engine SHALL 忽略该文件并从头开始传输

### 需求 5：手动重试传输

**用户故事：** 作为用户，我希望在自动重连失败后，能够手动点击按钮重新发起传输，并自动从断点恢复。

#### 验收标准

1. WHEN 用户在发送端重新选择相同文件并发送到相同目标设备，THE Transfer_Engine SHALL 自动利用接收端已有的 `.part` 文件进行断点续传
2. WHEN 用户在接收端存在未完成的 `.part` 文件时收到同名传输请求，THE Transfer_Engine SHALL 自动进入续传模式而非覆盖重传
3. THE Frontend SHALL 在发送失败后显示"重试"按钮，点击后使用相同参数重新发起传输


### 需求 6：前端续传状态展示

**用户故事：** 作为用户，我希望在界面上清楚地看到当前传输是从头开始还是断点续传，以及续传的进度信息。

#### 验收标准

1. WHEN 传输以续传模式开始，THE Frontend SHALL 在进度区域显示"续传中"标识，并显示已跳过的数据量
2. WHEN 传输以续传模式开始，THE Frontend SHALL 将进度条的起始位置设置为 Resume_Offset 对应的百分比，而非从 0% 开始
3. WHILE 自动重连进行中，THE Frontend SHALL 显示"重连中 (第 N 次 / 共 5 次)"状态文本和倒计时
4. WHEN 自动重连成功并恢复传输，THE Frontend SHALL 将状态从"重连中"切换回正常传输进度显示
5. IF 所有重连尝试失败，THEN THE Frontend SHALL 显示错误信息和"重试"按钮

### 需求 7：TransferEvent 扩展

**用户故事：** 作为开发者，我希望 TransferEvent 能携带续传和重连相关的状态信息，以便前端正确展示。

#### 验收标准

1. THE TransferEvent SHALL 包含一个 `resumed` 布尔字段，表示当前传输是否为续传模式
2. THE TransferEvent SHALL 包含一个 `resume_offset` 字段（u64），表示续传跳过的字节数
3. THE TransferEvent SHALL 包含一个可选的 `reconnect_info` 字段，包含当前重连次数和最大重连次数
4. FOR ALL 非续传场景，`resumed` 字段 SHALL 为 false 且 `resume_offset` 字段 SHALL 为 0
