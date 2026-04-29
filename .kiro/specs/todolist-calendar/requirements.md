# 需求文档：TodoList + 日历功能

## 简介

在 rust-air 应用首页集成 TodoList（待办事项）功能，与日历视图结合，用户可以按日期查看和管理待办事项。该功能作为侧边栏新 Tab 直接嵌入首页，无需深层导航，随时可用。数据通过 Rust 后端以本地 JSON 文件持久化存储，同时支持桌面和 Android 平台。

## 术语表

- **Todo_Item**：一条待办事项记录，包含标题、日期、完成状态等属性
- **Calendar_View**：日历视图组件，以月历形式展示日期，标记含有待办事项的日期
- **Todo_List_View**：待办事项列表视图，展示选中日期下的所有待办事项
- **Todo_Store**：Rust 后端的 JSON 文件存储模块，负责待办事项数据的读写和持久化
- **Todo_Panel**：首页侧边栏中的待办事项面板，包含 Calendar_View 和 Todo_List_View
- **Selected_Date**：用户在 Calendar_View 中当前选中的日期

## 需求

### 需求 1：待办事项数据持久化

**用户故事：** 作为用户，我希望待办事项数据保存在本地，以便关闭应用后重新打开时数据不会丢失。

#### 验收标准

1. THE Todo_Store SHALL 将所有 Todo_Item 数据以 JSON 格式存储在应用数据目录下的 `todos.json` 文件中
2. WHEN 应用启动时，THE Todo_Store SHALL 从 `todos.json` 文件加载已有的 Todo_Item 数据
3. WHEN 用户添加、编辑、删除或切换 Todo_Item 完成状态时，THE Todo_Store SHALL 在操作完成后将变更持久化到 `todos.json` 文件
4. IF `todos.json` 文件不存在或内容损坏，THEN THE Todo_Store SHALL 创建一个空的待办事项列表并生成新的 `todos.json` 文件
5. THE Todo_Store SHALL 为每个 Todo_Item 生成唯一的标识符（UUID）
6. FOR ALL 有效的 Todo_Item 列表，序列化为 JSON 后再反序列化 SHALL 产生等价的 Todo_Item 列表（往返一致性）

### 需求 2：日历视图展示

**用户故事：** 作为用户，我希望在首页看到一个月历视图，以便快速定位到某一天查看待办事项。

#### 验收标准

1. THE Calendar_View SHALL 以月历网格形式展示当前月份的所有日期
2. THE Calendar_View SHALL 默认将 Selected_Date 设置为当天日期
3. WHEN 用户点击 Calendar_View 中的某个日期时，THE Calendar_View SHALL 将该日期设置为 Selected_Date
4. WHEN 用户点击上一月或下一月导航按钮时，THE Calendar_View SHALL 切换到对应月份的日历视图
5. WHILE 某个日期存在至少一个未完成的 Todo_Item 时，THE Calendar_View SHALL 在该日期下方显示一个圆点标记
6. THE Calendar_View SHALL 以视觉高亮方式区分当天日期和 Selected_Date

### 需求 3：待办事项列表管理

**用户故事：** 作为用户，我希望查看和管理选中日期的待办事项，以便跟踪每天的任务完成情况。

#### 验收标准

1. WHEN Selected_Date 变更时，THE Todo_List_View SHALL 展示该日期下的所有 Todo_Item
2. THE Todo_List_View SHALL 为每个 Todo_Item 显示标题和完成状态复选框
3. WHEN 用户点击 Todo_Item 的复选框时，THE Todo_List_View SHALL 切换该 Todo_Item 的完成状态
4. THE Todo_List_View SHALL 将已完成的 Todo_Item 以删除线样式展示，并排列在未完成项之后
5. WHILE 选中日期没有任何 Todo_Item 时，THE Todo_List_View SHALL 显示"暂无待办"的空状态提示

### 需求 4：添加待办事项

**用户故事：** 作为用户，我希望快速添加新的待办事项到选中日期，以便记录需要完成的任务。

#### 验收标准

1. THE Todo_Panel SHALL 在 Todo_List_View 下方提供一个文本输入框用于输入新 Todo_Item 的标题
2. WHEN 用户在输入框中输入标题并按下回车键时，THE Todo_Panel SHALL 创建一个新的 Todo_Item，日期为当前 Selected_Date，完成状态为未完成
3. WHEN 新 Todo_Item 创建成功后，THE Todo_Panel SHALL 清空输入框内容
4. IF 用户提交的标题为空白字符串，THEN THE Todo_Panel SHALL 忽略该提交，不创建 Todo_Item
5. THE Todo_Panel SHALL 将新创建的 Todo_Item 添加到 Todo_List_View 列表的顶部

### 需求 5：删除待办事项

**用户故事：** 作为用户，我希望删除不需要的待办事项，以便保持列表整洁。

#### 验收标准

1. THE Todo_List_View SHALL 为每个 Todo_Item 提供一个删除按钮
2. WHEN 用户点击 Todo_Item 的删除按钮时，THE Todo_List_View SHALL 从列表和存储中移除该 Todo_Item
3. WHEN Todo_Item 被删除后，THE Calendar_View SHALL 更新对应日期的圆点标记状态

### 需求 6：首页集成与导航

**用户故事：** 作为用户，我希望在首页侧边栏直接访问待办事项功能，以便随时查看而无需深层导航。

#### 验收标准

1. THE Todo_Panel SHALL 作为一个新的 Tab 集成到 rust-air 首页的侧边栏导航中
2. THE Todo_Panel SHALL 使用与现有 Tab（发送、接收、设备、搜索、同步）一致的视觉风格和交互模式
3. WHEN 用户点击侧边栏中的待办 Tab 时，THE Todo_Panel SHALL 在主内容区域展示 Calendar_View 和 Todo_List_View
4. THE Todo_Panel SHALL 支持通过键盘快捷键（数字键 6）快速切换到待办 Tab

### 需求 7：跨平台兼容性

**用户故事：** 作为用户，我希望待办事项功能在桌面和 Android 平台上都能正常使用。

#### 验收标准

1. THE Todo_Store SHALL 使用平台无关的应用数据目录路径来存储 `todos.json` 文件
2. THE Calendar_View SHALL 在桌面端（最小宽度 640px）和移动端屏幕上均能正确布局
3. THE Todo_Panel SHALL 支持触摸操作和鼠标操作两种交互方式

### 需求 8：Rust 后端 Tauri 命令接口

**用户故事：** 作为开发者，我希望通过 Tauri IPC 命令与 Rust 后端交互，以便前端能安全地读写待办事项数据。

#### 验收标准

1. THE Todo_Store SHALL 提供 `get_todos` 命令，接收日期参数，返回该日期下的所有 Todo_Item
2. THE Todo_Store SHALL 提供 `add_todo` 命令，接收标题和日期参数，创建并返回新的 Todo_Item
3. THE Todo_Store SHALL 提供 `toggle_todo` 命令，接收 Todo_Item 的 UUID，切换其完成状态
4. THE Todo_Store SHALL 提供 `delete_todo` 命令，接收 Todo_Item 的 UUID，删除该 Todo_Item
5. THE Todo_Store SHALL 提供 `get_todo_dates` 命令，接收年月参数，返回该月中含有未完成 Todo_Item 的日期列表
6. IF 命令执行过程中发生文件读写错误，THEN THE Todo_Store SHALL 返回包含错误描述的错误响应
