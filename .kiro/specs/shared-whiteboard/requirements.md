# 需求文档：共享白板

## 简介

共享白板功能为 rust-air 提供一个多设备协作的文本/图片白板区域。用户可以在白板中输入文字或粘贴图片，内容会持久化保存到本地磁盘。同一局域网内所有运行 rust-air 的设备可以实时查看和编辑白板内容，所有修改通过现有的加密传输通道实时同步到其他设备。白板还提供一键清空功能，清空操作同样会同步到所有设备。

## 术语表

- **Whiteboard_Service**: 共享白板后台服务，负责管理白板内容的持久化、同步广播和接收
- **Whiteboard_Store**: 白板内容的本地持久化存储，保存文本和图片数据到磁盘
- **Whiteboard_Item**: 白板中的单个内容条目，可以是文本块或图片
- **Whiteboard_Sync_Message**: 通过网络传输的白板同步消息，包含操作类型和内容数据
- **Whiteboard_UI**: 前端白板界面组件，包含输入框、内容展示区和清空按钮
- **Sync_Peer**: 局域网内已发现的其他 rust-air 设备（复用现有设备发现机制）

## 需求

### 需求 1：白板内容输入

**用户故事：** 作为用户，我希望在白板区域中输入文字或粘贴图片，这样我可以快速记录和分享信息。

#### 验收标准

1. THE Whiteboard_UI SHALL 提供一个文本输入框，允许用户输入任意长度的文本内容
2. WHEN 用户在输入框中粘贴图片, THE Whiteboard_UI SHALL 将图片作为独立的 Whiteboard_Item 添加到白板内容列表中
3. WHEN 用户在输入框中输入文本并提交, THE Whiteboard_UI SHALL 将文本作为独立的 Whiteboard_Item 添加到白板内容列表中
4. THE Whiteboard_UI SHALL 按照添加时间顺序从上到下展示所有 Whiteboard_Item
5. THE Whiteboard_UI SHALL 支持显示文本和图片两种类型的 Whiteboard_Item

### 需求 2：内容持久化

**用户故事：** 作为用户，我希望白板内容在应用重启后仍然保留，这样我不会丢失已记录的信息。

#### 验收标准

1. WHEN 白板内容发生变化（添加、编辑或删除）, THE Whiteboard_Store SHALL 在 2 秒内将完整白板内容写入本地磁盘
2. WHEN 应用启动时, THE Whiteboard_Store SHALL 从本地磁盘加载上次保存的白板内容并在界面中展示
3. THE Whiteboard_Store SHALL 将白板数据保存为 JSON 格式文件，存储在应用数据目录下
4. THE Whiteboard_Store SHALL 将图片数据以 Base64 编码形式嵌入 JSON 文件中
5. IF 本地存储文件损坏或不存在, THEN THE Whiteboard_Store SHALL 初始化一个空白板并记录警告日志

### 需求 3：局域网实时同步

**用户故事：** 作为用户，我希望同一局域网内的其他 rust-air 设备能实时看到我的白板修改，这样我们可以协作使用白板。

#### 验收标准

1. WHEN 本地白板内容发生变化, THE Whiteboard_Service SHALL 将变更封装为 Whiteboard_Sync_Message 并发送给局域网内所有已发现的 Sync_Peer
2. THE Whiteboard_Service SHALL 复用现有的 mDNS/UDP 设备发现机制来发现局域网内的其他 rust-air 设备
3. THE Whiteboard_Service SHALL 使用现有的 ChaCha20-Poly1305 加密传输协议发送 Whiteboard_Sync_Message
4. WHEN Whiteboard_Service 收到来自 Sync_Peer 的 Whiteboard_Sync_Message, THE Whiteboard_Service SHALL 将远程变更合并到本地白板内容中
5. THE Whiteboard_Service SHALL 在本地白板变更后 1 秒内完成向所有在线 Sync_Peer 的广播
6. IF 向某个 Sync_Peer 发送 Whiteboard_Sync_Message 失败, THEN THE Whiteboard_Service SHALL 记录错误日志并继续向其他设备发送

### 需求 4：远程编辑

**用户故事：** 作为用户，我希望任何设备都可以编辑白板内容，并且修改能同步到所有设备，这样多人可以共同维护白板。

#### 验收标准

1. WHEN Sync_Peer 发送了一条包含新增内容的 Whiteboard_Sync_Message, THE Whiteboard_Service SHALL 将新内容追加到本地白板
2. WHEN Sync_Peer 发送了一条包含删除操作的 Whiteboard_Sync_Message, THE Whiteboard_Service SHALL 从本地白板中移除对应的 Whiteboard_Item
3. THE Whiteboard_Service SHALL 为每个 Whiteboard_Item 分配全局唯一标识符，确保多设备间能正确识别同一条目
4. WHEN 本地和远程同时修改白板内容, THE Whiteboard_Service SHALL 采用时间戳优先策略合并冲突（后发生的操作覆盖先发生的操作）
5. WHEN 远程变更被合并到本地白板, THE Whiteboard_UI SHALL 实时更新界面展示

### 需求 5：一键清空

**用户故事：** 作为用户，我希望能一键清空白板所有内容，这样我可以快速重置白板开始新的记录。

#### 验收标准

1. THE Whiteboard_UI SHALL 提供一个"清空白板"按钮
2. WHEN 用户点击"清空白板"按钮, THE Whiteboard_UI SHALL 显示确认对话框以防止误操作
3. WHEN 用户确认清空操作, THE Whiteboard_Service SHALL 删除本地白板中的所有 Whiteboard_Item
4. WHEN 用户确认清空操作, THE Whiteboard_Service SHALL 向所有在线 Sync_Peer 广播清空指令
5. WHEN Whiteboard_Service 收到来自 Sync_Peer 的清空指令, THE Whiteboard_Service SHALL 清空本地白板内容并更新界面

### 需求 6：同步消息协议

**用户故事：** 作为开发者，我希望白板同步消息有明确的协议格式，这样多设备间能正确解析和处理同步数据。

#### 验收标准

1. THE Whiteboard_Sync_Message SHALL 包含操作类型字段，支持 "add"、"delete" 和 "clear" 三种操作
2. THE Whiteboard_Sync_Message SHALL 包含发送设备名称和时间戳字段
3. WHEN 操作类型为 "add", THE Whiteboard_Sync_Message SHALL 包含完整的 Whiteboard_Item 数据（唯一标识符、内容类型、内容数据、创建时间）
4. WHEN 操作类型为 "delete", THE Whiteboard_Sync_Message SHALL 包含待删除 Whiteboard_Item 的唯一标识符
5. THE Whiteboard_Sync_Message SHALL 使用 JSON 序列化格式进行编码

### 需求 7：传输安全与可靠性

**用户故事：** 作为用户，我希望白板同步数据在传输过程中是加密的，并且系统能处理网络异常情况。

#### 验收标准

1. THE Whiteboard_Service SHALL 使用现有的 ChaCha20-Poly1305 加密协议传输所有 Whiteboard_Sync_Message
2. THE Whiteboard_Service SHALL 为每次同步传输生成独立的一次性密钥
3. IF 传输过程中数据校验失败, THEN THE Whiteboard_Service SHALL 丢弃该消息并记录错误日志
4. WHEN 新设备加入局域网, THE Whiteboard_Service SHALL 向该设备发送完整的白板内容快照以实现初始同步
5. IF 设备断线后重新连接, THEN THE Whiteboard_Service SHALL 通过全量快照同步恢复白板内容一致性

### 需求 8：前端界面

**用户故事：** 作为用户，我希望白板界面简洁易用，能清晰展示所有内容并提供便捷的操作入口。

#### 验收标准

1. THE Whiteboard_UI SHALL 在侧边栏导航中添加"白板"标签页入口
2. THE Whiteboard_UI SHALL 在白板页面顶部提供文本/图片输入区域
3. THE Whiteboard_UI SHALL 在输入区域下方以列表形式展示所有 Whiteboard_Item
4. THE Whiteboard_UI SHALL 为每个 Whiteboard_Item 显示创建时间和来源设备名称
5. THE Whiteboard_UI SHALL 为每个 Whiteboard_Item 提供单独的删除按钮
6. THE Whiteboard_UI SHALL 在页面底部或顶部提供"清空白板"按钮，使用醒目的警告色样式
