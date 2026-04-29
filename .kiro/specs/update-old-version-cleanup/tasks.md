# 实现计划

- [ ] 1. 编写 Bug Condition 探索性测试
  - **Property 1: Bug Condition** - 启动时旧安装包未被清理
  - **重要**: 此测试必须在实施修复之前编写
  - **目标**: 展示 bug 存在的反例，确认根本原因分析
  - **Scoped PBT 方法**: 将属性范围限定到具体的失败场景——temp 目录中存在 rust-air 旧安装包文件，但应用启动时不会清理
  - 测试场景：模拟 `download_and_install()` 完成后的状态，在 temp 目录中创建 rust-air 安装包文件（如 `rust-air_0.3.31_x64_en-US.msi`）
  - 调用应用启动逻辑（setup 阶段），验证旧安装包文件是否被清理
  - 断言：对于所有满足 `isBugCondition(input)` 的输入（`updateCompleted = true AND oldFilesExist = true`），启动后 temp 目录中的旧安装包文件应被删除
  - 在未修复代码上运行测试
  - **预期结果**: 测试失败（这是正确的——证明 bug 存在，因为当前代码完全没有清理逻辑）
  - 记录反例：`cleanup_old_update_files()` 函数不存在，temp 目录中的安装包文件在应用启动后仍然存在
  - 测试编写完成、运行并记录失败后标记任务完成
  - _Requirements: 1.1, 1.2, 2.1, 2.2_

- [ ] 2. 编写保持性属性测试（在实施修复之前）
  - **Property 2: Preservation** - 更新流程行为不变
  - **重要**: 遵循观察优先方法论
  - 观察：在未修复代码上运行 `is_newer("0.4.0", "0.3.31")` 返回 `true`
  - 观察：在未修复代码上运行 `is_newer("0.3.31", "0.3.31")` 返回 `false`
  - 观察：在未修复代码上运行 `pick_asset()` 对于包含正确后缀的资产列表返回匹配的资产
  - 观察：在未修复代码上运行 `UpdateSettings::load()` 和 `save()` 正确读写设置文件
  - 编写基于属性的测试：对于所有非 bug 条件输入（`NOT isBugCondition(X)`），验证：
    - `is_newer()` 版本比较逻辑在修复前后行为一致
    - `pick_asset()` 平台资产选择逻辑在修复前后行为一致
    - `UpdateSettings` 的序列化/反序列化在修复前后行为一致
    - `download_installer()` 仍然正确下载文件到 temp 目录
  - 在未修复代码上运行测试
  - **预期结果**: 测试通过（确认基线行为已被捕获）
  - 测试编写完成、运行并在未修复代码上通过后标记任务完成
  - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5_

- [ ] 3. 修复更新旧版本文件清理问题

  - [x] 3.1 实现修复
    - 在 `update_commands.rs` 中新增 `CleanupRecord` 结构体，包含 `installer_path: String` 字段
    - 新增 `cleanup_record_path()` 函数，返回 `data_local_dir/rust-air/update-cleanup.json` 路径
    - 在 `download_and_install()` 中，下载完成后、启动安装程序之前，将安装包完整路径写入 `update-cleanup.json`
    - 新增 `cleanup_old_update_files()` 公开函数：
      - 读取 `update-cleanup.json`，获取上次安装包路径
      - 尝试删除记录的安装包文件
      - 扫描 temp 目录中匹配 `rust-air*.msi` 和 `rust-air*-setup.exe` 模式的文件，删除非当前版本的旧文件
      - 删除成功后清除记录文件
      - 所有删除操作失败时静默忽略（文件不存在、被占用、权限不足）
    - 在 `lib.rs` 的 `setup` 闭包中，在自动更新检查之前调用 `cleanup_old_update_files()`
    - _Bug_Condition: isBugCondition(input) where updateCompleted = true AND oldFilesExist = true_
    - _Expected_Behavior: appStartup 后 oldInstallerFiles(tempDir, previousVersions) = EMPTY_
    - _Preservation: 下载、安装、进度报告、版本检查、设置读写等更新流程行为不变_
    - _Requirements: 1.1, 1.2, 1.3, 2.1, 2.2, 2.3, 3.1, 3.2, 3.3, 3.4, 3.5_

  - [x] 3.2 验证 Bug Condition 探索性测试现在通过
    - **Property 1: Expected Behavior** - 启动时旧安装包被正确清理
    - **重要**: 重新运行任务 1 中的同一测试，不要编写新测试
    - 任务 1 中的测试编码了期望行为
    - 当此测试通过时，确认期望行为已满足
    - 运行任务 1 中的 bug condition 探索性测试
    - **预期结果**: 测试通过（确认 bug 已修复）
    - _Requirements: 2.1, 2.2_

  - [x] 3.3 验证保持性测试仍然通过
    - **Property 2: Preservation** - 更新流程行为不变
    - **重要**: 重新运行任务 2 中的同一测试，不要编写新测试
    - 运行任务 2 中的保持性属性测试
    - **预期结果**: 测试通过（确认没有回归）
    - 确认修复后所有测试仍然通过（无回归）

- [x] 4. 检查点 - 确保所有测试通过
  - 运行完整测试套件，确保所有测试通过
  - 如有问题，询问用户
