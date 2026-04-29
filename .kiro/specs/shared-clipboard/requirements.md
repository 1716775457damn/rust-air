# 需求文档：共享剪贴板

## 简介

共享剪贴板功能允许局域网内多台运行 rust-air 的设备之间实时同步剪贴板内容。当用户在一台设备上复制文本或图片时，其他已启用共享的设备将自动接收并写入本地剪贴板，使用户可以直接粘贴。该功能复用现有的 mDNS-SD 设备发现机制和 ChaCha20-Poly1305 加密传输协议。

## 术语表

- **Clipboard_Sync_Service**: 共享剪贴板后台服务，负责监控本地剪贴板变化并广播给已配对的设备
- **Sync_Group**: 一组已互相授权共享剪贴板的设备集合
- **Clip_Payload**: 通过网络传输的剪贴板数据包，包含内容类型和实际数据
- **Clipboard_Monitor**: 本地剪贴板轮询线程，检测剪贴板内容变化
- **Receiver_Module**: 接收端处理模块，负责接收远程剪贴板数据并写入本地剪贴板
- **Sync_Toggle**: 用户界面中的共享剪贴板开关控件
- **Echo_Guard**: 回声抑制机制，防止设备将刚收到的远程内容再次广播出去

## 需求

### 需求 1：剪贴板变化检测与广播

**用户故事：** 作为用户，我希望在一台设备上复制内容后，局域网内其他设备能自动收到该内容，这样我可以在任意设备上直接粘贴。

#### 验收标准

1. WHILE Clipboard_Sync_Service 处于启用状态, WHEN Clipboard_Monitor 检测到本地剪贴板内容发生变化, THE Clipboard_Sync_Service SHALL 将新内容封装为 Clip_Payload 并发送给 Sync_Group 中所有在线设备
2. THE Clipboard_Sync_Service SHALL 在检测到剪贴板变化后 2 秒内完成向所有在线设备的发送
3. WHEN Clip_Payload 的内容类型为文本, THE Clipboard_Sync_Service SHALL 使用 Kind::Clipboard 传输类型通过现有加密通道发送数据
4. WHEN Clip_Payload 的内容类型为图片, THE Clipboard_Sync_Service SHALL 将图片编码为 PNG 格式后通过现有加密通道发送数据
5. THE Echo_Guard SHALL 在收到远程剪贴板内容写入本地后的 3 秒内抑制对该内容的重复广播

### 需求 2：剪贴板内容接收与写入

**用户故事：** 作为用户，我希望其他设备复制的内容能自动出现在我的剪贴板中，这样我无需手动操作即可粘贴。

#### 验收标准

1. WHEN Receiver_Module 收到来自 Sync_Group 中设备的 Clip_Payload, THE Receiver_Module SHALL 将内容写入本地剪贴板
2. WHEN Receiver_Module 收到文本类型的 Clip_Payload, THE Receiver_Module SHALL 将文本内容直接写入本地剪贴板
3. WHEN Receiver_Module 收到图片类型的 Clip_Payload, THE Receiver_Module SHALL 将 PNG 数据解码为 RGBA 图像并写入本地剪贴板
4. IF Receiver_Module 接收到的 Clip_Payload 数据校验失败, THEN THE Receiver_Module SHALL 丢弃该数据并记录错误日志
5. WHEN Receiver_Module 成功写入本地剪贴板, THE Receiver_Module SHALL 通过事件通知前端更新剪贴板历史记录

### 需求 3：设备配对与同步组管理

**用户故事：** 作为用户，我希望能选择哪些设备参与剪贴板共享，这样我可以控制数据的流向。

#### 验收标准

1. THE Clipboard_Sync_Service SHALL 复用现有 mDNS-SD 设备发现机制来发现局域网内的可用设备
2. WHEN 用户在设备列表中选择一台设备并点击"共享剪贴板", THE Clipboard_Sync_Service SHALL 将该设备添加到 Sync_Group
3. WHEN 用户从 Sync_Group 中移除一台设备, THE Clipboard_Sync_Service SHALL 停止向该设备发送剪贴板内容
4. THE Clipboard_Sync_Service SHALL 将 Sync_Group 配置持久化到本地存储，在应用重启后自动恢复
5. IF Sync_Group 中的设备离线超过 30 秒, THEN THE Clipboard_Sync_Service SHALL 将该设备标记为离线并跳过发送

### 需求 4：同步开关与用户控制

**用户故事：** 作为用户，我希望能随时开启或关闭剪贴板共享功能，这样我可以在需要隐私时暂停同步。

#### 验收标准

1. THE Sync_Toggle SHALL 在侧边栏或设置页面中提供一个全局开关来启用或禁用剪贴板共享
2. WHEN 用户关闭 Sync_Toggle, THE Clipboard_Sync_Service SHALL 立即停止监控剪贴板变化和接收远程内容
3. WHEN 用户开启 Sync_Toggle, THE Clipboard_Sync_Service SHALL 恢复监控剪贴板变化和接收远程内容
4. THE Clipboard_Sync_Service SHALL 在应用启动时根据上次保存的开关状态自动启用或禁用同步
5. WHILE Clipboard_Sync_Service 处于禁用状态, THE Receiver_Module SHALL 拒绝所有传入的 Clip_Payload

### 需求 5：传输安全

**用户故事：** 作为用户，我希望共享的剪贴板内容在传输过程中是加密的，这样局域网内的其他人无法窃取我的数据。

#### 验收标准

1. THE Clipboard_Sync_Service SHALL 使用现有的 ChaCha20-Poly1305 加密协议传输所有 Clip_Payload 数据
2. THE Clipboard_Sync_Service SHALL 为每次剪贴板传输生成独立的一次性密钥
3. IF 传输过程中加密握手失败, THEN THE Clipboard_Sync_Service SHALL 中止该次传输并记录错误

### 需求 6：内容大小限制

**用户故事：** 作为用户，我希望系统对同步的内容大小有合理限制，这样不会因为意外复制大量数据而阻塞网络。

#### 验收标准

1. THE Clipboard_Sync_Service SHALL 对文本类型的 Clip_Payload 设置 10 MB 的最大传输限制
2. THE Clipboard_Sync_Service SHALL 对图片类型的 Clip_Payload 设置 50 MB 的最大传输限制
3. IF 剪贴板内容超过对应类型的大小限制, THEN THE Clipboard_Sync_Service SHALL 跳过该次同步并在前端显示提示信息

### 需求 7：前端界面集成

**用户故事：** 作为用户，我希望在 rust-air 界面中能看到剪贴板共享的状态和历史，这样我知道同步是否正常工作。

#### 验收标准

1. THE Sync_Toggle SHALL 在设备列表页面中为每个设备显示剪贴板共享状态图标
2. WHEN Clipboard_Sync_Service 成功发送或接收一条 Clip_Payload, THE 前端 SHALL 在剪贴板历史中标注该条目的来源设备名称
3. WHEN Clipboard_Sync_Service 发送或接收失败, THE 前端 SHALL 显示一条短暂的错误提示通知
