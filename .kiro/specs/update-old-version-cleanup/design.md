# 更新旧版本清理 Bugfix 设计

## 概述

rust-air 应用在 Windows 端执行自动更新后，不会清理旧版本相关文件。`download_and_install()` 函数将安装包下载到系统临时目录后，启动安装程序并直接退出应用，没有任何文件清理逻辑。这导致每次更新都会在 temp 目录中累积残留的安装包文件（.msi / .exe），浪费磁盘空间。此外，便携版用户更新后旧的可执行文件也不会被删除。

修复策略：在应用启动时（`setup` 阶段）执行一次旧文件清理，删除 temp 目录中 rust-air 相关的旧安装包文件，并在 `download_and_install()` 中记录当前下载的安装包路径以便下次启动时清理。

## 术语表

- **Bug_Condition (C)**: 应用完成更新流程后（安装包已下载、安装程序已启动），旧文件未被清理的条件
- **Property (P)**: 应用启动时应自动清理上一次更新遗留的安装包文件
- **Preservation**: 现有的下载、安装、进度报告等更新流程行为必须保持不变
- **download_and_install()**: `tauri-app/src-tauri/src/update_commands.rs` 中负责下载安装包并启动安装程序的函数
- **cleanup_old_update_files()**: 新增函数，在应用启动时清理旧版本安装包文件
- **update-cleanup.json**: 新增的持久化文件，记录上次下载的安装包路径，存储在 `data_local_dir/rust-air/` 目录下

## Bug 详情

### Bug 条件

当用户通过 `download_and_install()` 完成更新后，安装包文件被下载到 `std::env::temp_dir()` 但从未被清理。应用退出后，这些文件永久残留在 temp 目录中。多次更新后，temp 目录中会累积多个旧版本安装包文件。

**形式化规约：**
```
FUNCTION isBugCondition(input)
  INPUT: input of type AppState
  OUTPUT: boolean
  
  RETURN input.previousUpdateCompleted = true
         AND fileExists(input.tempDir, input.previousInstallerFilename)
         AND NOT fileCleanedUp(input.previousInstallerFilename)
END FUNCTION
```

### 示例

- 用户从 v0.3.30 更新到 v0.3.31：temp 目录中残留 `rust-air_0.3.31_x64_en-US.msi`，下次启动时未被清理
- 用户连续从 v0.3.28 → v0.3.29 → v0.3.30 → v0.3.31 更新：temp 目录中累积 3 个旧安装包文件
- 便携版用户更新后：旧版本可执行文件仍然存在于原目录
- 边界情况：temp 目录中的安装包文件被其他进程占用时，清理应静默失败而不影响应用运行

## 期望行为

### 保持性需求

**不变行为：**
- `download_and_install()` 必须继续正确下载安装包到 temp 目录并启动安装程序
- 下载过程中必须继续通过 "update-progress" 事件正确报告进度
- `check_update()` 在没有新版本时必须继续返回 `None` 且不执行任何文件操作
- `is_newer()` 版本比较逻辑必须保持不变
- `pick_asset()` 平台资产选择逻辑必须保持不变
- `UpdateSettings` 的加载和保存逻辑必须保持不变

**范围：**
所有不涉及旧文件清理的输入和操作应完全不受此修复影响。包括：
- 正常的更新检查流程
- 安装包下载和安装流程
- 更新设置的读写
- 非更新相关的应用功能（文件传输、同步、搜索等）

## 假设的根本原因

基于 bug 分析，最可能的原因如下：

1. **缺少清理逻辑**: `download_and_install()` 函数在下载安装包并启动安装程序后，直接调用 `app.exit(0)` 退出应用，完全没有文件清理代码。由于安装程序正在使用该文件，退出前也无法删除。

2. **缺少文件追踪机制**: 没有任何机制记录已下载的安装包路径，因此即使想在下次启动时清理，也无法知道需要清理哪些文件。

3. **缺少启动时清理钩子**: `lib.rs` 中的 `setup` 阶段没有任何旧文件清理逻辑，应用启动时不会检查和清理之前更新遗留的文件。

4. **便携版无清理策略**: 对于便携版用户，没有任何机制来检测和删除旧版本的可执行文件。

## 正确性属性

Property 1: Bug Condition - 启动时清理旧安装包文件

_For any_ 应用启动状态，其中上一次更新已完成且 temp 目录中存在已记录的旧安装包文件（isBugCondition 返回 true），修复后的启动清理函数 SHALL 删除这些旧安装包文件，使得清理后 temp 目录中不再存在已记录的 rust-air 旧安装包。

**Validates: Requirements 2.1, 2.2**

Property 2: Preservation - 更新流程行为不变

_For any_ 更新操作输入，其中 bug 条件不成立（isBugCondition 返回 false），修复后的代码 SHALL 产生与原始代码完全相同的结果，保持下载、安装、进度报告等所有更新流程行为不变。

**Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5**

## 修复实现

### 所需变更

假设我们的根本原因分析正确：

**文件**: `tauri-app/src-tauri/src/update_commands.rs`

**函数**: `download_and_install()` 及新增 `cleanup_old_update_files()`

**具体变更**:

1. **新增清理记录结构体**: 定义 `CleanupRecord` 结构体，包含 `installer_path: String` 字段，用于记录上次下载的安装包路径。持久化到 `data_local_dir/rust-air/update-cleanup.json`。

2. **在 download_and_install() 中记录安装包路径**: 在下载完成、启动安装程序之前，将安装包的完整路径写入 `update-cleanup.json`，以便下次启动时知道需要清理哪个文件。

3. **新增 cleanup_old_update_files() 函数**: 读取 `update-cleanup.json`，如果存在记录的安装包路径，尝试删除该文件。删除成功后清除记录。删除失败时（文件不存在、被占用、权限不足）静默忽略错误，不影响应用正常启动。

4. **在 lib.rs 的 setup 阶段调用清理函数**: 在应用启动的 `setup` 闭包中，在自动更新检查之前调用 `cleanup_old_update_files()`，确保每次启动时都尝试清理旧文件。

5. **清理 temp 目录中的 rust-air 相关旧文件**: 除了基于记录的精确清理外，还可以扫描 temp 目录中匹配 rust-air 安装包命名模式的文件（如 `rust-air*.msi`、`rust-air*-setup.exe`），删除非当前版本的旧文件，处理记录丢失的情况。

**文件**: `tauri-app/src-tauri/src/lib.rs`

**变更**: 在 `setup` 闭包中添加对 `cleanup_old_update_files()` 的调用。

## 测试策略

### 验证方法

测试策略采用两阶段方法：首先在未修复代码上展示 bug 的反例，然后验证修复后的代码正确工作且保持现有行为不变。

### 探索性 Bug 条件检查

**目标**: 在实施修复之前，展示 bug 存在的反例。确认或否定根本原因分析。如果否定，需要重新假设。

**测试计划**: 编写测试模拟更新完成后的状态，验证 temp 目录中的安装包文件是否被清理。在未修复代码上运行这些测试以观察失败并理解根本原因。

**测试用例**:
1. **单次更新残留测试**: 模拟一次 `download_and_install()` 完成后，检查 temp 目录中安装包文件是否存在（在未修复代码上将失败——文件未被清理）
2. **多次更新累积测试**: 模拟多次更新后，检查 temp 目录中是否累积了多个安装包文件（在未修复代码上将失败——文件持续累积）
3. **启动时清理测试**: 模拟应用启动，检查是否执行了旧文件清理（在未修复代码上将失败——没有清理逻辑）

**预期反例**:
- `download_and_install()` 完成后，temp 目录中的安装包文件仍然存在
- 应用启动时没有任何清理旧文件的行为
- 可能原因：完全缺少清理逻辑、缺少文件追踪机制

### 修复检查

**目标**: 验证对于所有 bug 条件成立的输入，修复后的函数产生期望行为。

**伪代码：**
```
FOR ALL input WHERE isBugCondition(input) DO
  result := appStartup_fixed(input)
  ASSERT oldInstallerFiles(input.tempDir, input.previousVersions) = EMPTY
  OR fileInUse(input.installerPath) = true  // 文件被占用时允许跳过
END FOR
```

### 保持性检查

**目标**: 验证对于所有 bug 条件不成立的输入，修复后的函数产生与原始函数相同的结果。

**伪代码：**
```
FOR ALL input WHERE NOT isBugCondition(input) DO
  ASSERT downloadAndInstall_original(input) = downloadAndInstall_fixed(input)
END FOR
```

**测试方法**: 推荐使用基于属性的测试进行保持性检查，因为：
- 它能自动生成大量跨输入域的测试用例
- 它能捕获手动单元测试可能遗漏的边界情况
- 它能提供强有力的保证，确保所有非 bug 输入的行为不变

**测试计划**: 先在未修复代码上观察正常更新流程的行为，然后编写基于属性的测试捕获该行为。

**测试用例**:
1. **下载流程保持测试**: 验证 `download_installer()` 在修复后仍然正确下载文件到 temp 目录
2. **进度报告保持测试**: 验证下载过程中 "update-progress" 事件仍然正确发出
3. **版本检查保持测试**: 验证 `check_update()` 和 `is_newer()` 的行为完全不变
4. **设置保持测试**: 验证 `UpdateSettings` 的加载和保存行为不变

### 单元测试

- 测试 `cleanup_old_update_files()` 在存在旧安装包文件时正确删除
- 测试 `cleanup_old_update_files()` 在文件不存在时静默处理
- 测试 `cleanup_old_update_files()` 在文件被占用时静默处理不崩溃
- 测试 `CleanupRecord` 的序列化和反序列化
- 测试 `download_and_install()` 在下载完成后正确写入清理记录
- 测试 temp 目录扫描匹配 rust-air 安装包命名模式

### 基于属性的测试

- 生成随机的安装包文件名和路径，验证清理函数能正确识别和删除 rust-air 相关文件
- 生成随机的版本号组合，验证 `is_newer()` 行为在修复前后完全一致
- 生成随机的更新设置配置，验证设置的加载和保存在修复前后完全一致

### 集成测试

- 测试完整的更新-重启-清理流程：模拟下载安装包 → 记录路径 → 重启应用 → 验证旧文件被清理
- 测试清理失败不影响应用启动：模拟清理文件时权限不足，验证应用仍然正常启动
- 测试多次更新后的累积清理：模拟多个旧安装包文件存在，验证启动时全部被清理
