# 实现计划：TodoList + 日历功能

## 概述

基于 Tauri (Rust 后端 + Vue 3 前端) 架构，在 rust-air 应用侧边栏新增"待办"Tab，集成月历视图与待办事项 CRUD 功能。后端使用 `todo_commands.rs` 模块管理 JSON 持久化，前端内联在 `App.vue` 中。

## 任务

- [x] 1. 实现 Rust 后端 TodoStore 核心模块
  - [x] 1.1 创建 `tauri-app/src-tauri/src/todo_commands.rs` 模块，定义 `TodoItem` 结构体（id, title, date, completed）和 `TodoStore` 结构体（items, path）
    - 实现 `TodoStore::load()` 从 `todos.json` 加载数据，文件不存在或损坏时创建空列表
    - 实现 `TodoStore::save()` 将数据持久化到 `todos.json`
    - 使用 `dirs::data_dir()` 获取平台无关的应用数据目录
    - ID 生成使用时间戳 + 随机数的 u64 方式（与 `clip_history_commands.rs` 一致）
    - _需求：1.1, 1.2, 1.4, 1.5, 1.6, 7.1_

  - [x] 1.2 实现 Tauri IPC 命令：`get_todos`、`add_todo`、`toggle_todo`、`delete_todo`、`get_todo_dates`
    - `get_todos(date)` 按日期过滤并返回排序后的列表（未完成在前）
    - `add_todo(title, date)` 校验标题非空白、日期格式有效，创建新项
    - `toggle_todo(id)` 切换完成状态
    - `delete_todo(id)` 删除指定项
    - `get_todo_dates(year, month)` 返回该月有未完成待办的日期列表
    - 所有命令返回 `Result<T, String>`，错误通过 `.map_err(|e| e.to_string())` 转换
    - _需求：8.1, 8.2, 8.3, 8.4, 8.5, 8.6, 4.4_

  - [x] 1.3 在 `tauri-app/src-tauri/src/lib.rs` 中注册 TodoStore 状态和 IPC 命令
    - 添加 `mod todo_commands;`
    - 在 `tauri::Builder` 中 `.manage(Mutex<TodoStore>)` 注入状态
    - 在 `invoke_handler` 中注册所有 todo 命令
    - _需求：1.2, 1.3, 8.1-8.5_

- [x] 2. 检查点 - 确保后端编译通过
  - 确保所有代码编译通过，如有问题请询问用户。

- [ ] 3. 实现后端属性测试与单元测试
  - [ ]* 3.1 编写属性测试：序列化往返一致性
    - **属性 1：序列化往返一致性**
    - 使用 `proptest` 生成随机 `Vec<TodoItem>`，验证 JSON 序列化后反序列化结果等价
    - **验证需求：1.6**

  - [ ]* 3.2 编写属性测试：ID 唯一性
    - **属性 2：ID 唯一性**
    - 批量调用 add_todo，验证所有生成的 ID 互不相同
    - **验证需求：1.5**

  - [ ]* 3.3 编写属性测试：日期过滤正确性
    - **属性 3：日期过滤正确性**
    - 生成跨多日期的随机 TodoItem，验证 `get_todos` 返回的所有项 date 字段匹配且无遗漏
    - **验证需求：3.1, 8.1**

  - [ ]* 3.4 编写属性测试：添加待办正确性
    - **属性 4：添加待办正确性**
    - 生成随机有效标题和日期，验证返回值 completed=false、title/date 匹配、出现在列表顶部
    - **验证需求：4.2, 4.5, 8.2**

  - [ ]* 3.5 编写属性测试：空白标题拒绝
    - **属性 5：空白标题拒绝**
    - 生成随机空白字符串，验证 `add_todo` 拒绝且列表不变
    - **验证需求：4.4**

  - [ ]* 3.6 编写属性测试：完成状态切换对合映射
    - **属性 6：完成状态切换是对合映射**
    - 生成随机 TodoItem，验证 toggle 一次翻转、两次恢复原状
    - **验证需求：3.3, 8.3**

  - [ ]* 3.7 编写属性测试：列表排序不变量
    - **属性 7：列表排序不变量**
    - 生成混合完成状态的列表，验证 `get_todos` 返回未完成项在已完成项之前
    - **验证需求：3.4**

  - [ ]* 3.8 编写属性测试：删除移除正确性
    - **属性 8：删除移除正确性**
    - 生成随机列表并删除随机项，验证该 ID 不再出现且其他项不受影响
    - **验证需求：5.2, 8.4**

  - [ ]* 3.9 编写属性测试：未完成待办日期标记正确性
    - **属性 9：未完成待办日期标记正确性**
    - 生成跨月的随机 TodoItem，验证 `get_todo_dates` 返回的日期集合正确
    - **验证需求：2.5, 5.3, 8.5**

  - [ ]* 3.10 编写属性测试：月份导航正确性
    - **属性 10：月份导航正确性**
    - 生成随机年月，验证前进/后退逻辑（12月→次年1月，1月→上年12月）
    - **验证需求：2.4**

  - [ ]* 3.11 编写单元测试：TodoStore 边界场景
    - 测试 `todos.json` 不存在时的初始化
    - 测试 `todos.json` 内容损坏时的恢复
    - 测试文件读写后重新加载的持久化一致性
    - _需求：1.2, 1.4_

- [x] 4. 实现 Vue 前端待办 Tab 界面
  - [x] 4.1 在 `App.vue` 中扩展类型定义和状态
    - 扩展 `Tab` 类型添加 `"todo"`
    - 添加 `TodoItem` 接口定义
    - 添加日历和待办相关的 ref 状态（selectedDate, calendarYear, calendarMonth, todos, todoDates, newTodoTitle）
    - 扩展 `TAB_KEYS` 添加 `"6": "todo"` 快捷键
    - _需求：6.1, 6.4_

  - [x] 4.2 实现日历渲染逻辑
    - 编写 `calendarDays` computed 属性，计算当月天数、第一天星期、生成 6×7 网格
    - 包含上月末尾和下月开头的灰色日期填充
    - 实现 `prevMonth()` 和 `nextMonth()` 月份导航函数
    - 实现 `todayStr()` 辅助函数返回 YYYY-MM-DD 格式当天日期
    - _需求：2.1, 2.4_

  - [x] 4.3 实现前端与后端的 IPC 交互函数
    - `loadTodos(date)` 调用 `get_todos` 获取指定日期待办列表
    - `loadTodoDates(year, month)` 调用 `get_todo_dates` 获取当月有待办的日期
    - `addTodo()` 调用 `add_todo` 添加新待办，成功后清空输入框并刷新列表
    - `toggleTodo(id)` 调用 `toggle_todo` 切换完成状态
    - `deleteTodo(id)` 调用 `delete_todo` 删除待办
    - 所有 invoke 调用使用 try/catch，错误通过临时提示展示
    - 添加 watch 监听 selectedDate 变化自动加载待办列表
    - 添加 watch 监听 calendarYear/calendarMonth 变化自动加载日期标记
    - _需求：3.1, 3.3, 4.2, 4.3, 5.2, 5.3_

  - [x] 4.4 实现侧边栏导航和待办 Tab 模板
    - 在侧边栏 `<nav>` 中添加待办 Tab 按钮（📋 图标，快捷键 6）
    - 实现 `v-if="tab === 'todo'"` 的主内容区域模板
    - 日历区域：月份导航按钮、星期标题行、日期网格（当天高亮、选中高亮、未完成圆点标记）
    - 待办列表区域：每项显示复选框、标题（已完成显示删除线）、删除按钮
    - 空状态显示"暂无待办"提示
    - 底部输入框：回车提交新待办，空白标题忽略
    - 使用与现有 Tab 一致的视觉风格（CSS 变量、圆角、间距）
    - _需求：2.1, 2.2, 2.3, 2.5, 2.6, 3.2, 3.4, 3.5, 4.1, 4.4, 4.5, 5.1, 6.1, 6.2, 6.3, 7.2, 7.3_

- [x] 5. 检查点 - 确保前后端编译通过并功能可用
  - 确保 Rust 后端编译通过，前端无 TypeScript 错误，如有问题请询问用户。

- [x] 6. 最终检查点
  - 确保所有测试通过，如有问题请询问用户。

## 备注

- 标记 `*` 的任务为可选任务，可跳过以加速 MVP 开发
- 每个任务引用了具体的需求编号以确保可追溯性
- 属性测试使用 Rust `proptest` 库，每个属性至少运行 100 次迭代
- 前端逻辑全部内联在 `App.vue` 中，与现有 Tab 保持一致
- 检查点确保增量验证
