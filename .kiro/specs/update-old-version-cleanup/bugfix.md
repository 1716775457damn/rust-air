# Bugfix 需求文档

## 简介

rust-air 应用在 Windows 端执行自动更新后，不会清理旧版本相关文件。`download_and_install()` 函数将安装包下载到系统临时目录（`std::env::temp_dir()`），启动安装程序后直接调用 `app.exit(0)` 退出，没有任何文件清理逻辑。这导致每次更新都会在 temp 目录中累积残留的安装包文件（.msi / .exe），浪费磁盘空间。此外，如果用户使用便携版（非 MSI 安装），旧的可执行文件也不会被删除。

## Bug 分析

### 当前行为（缺陷）

1.1 WHEN 用户通过 `download_and_install()` 完成更新后 THEN 系统在 temp 目录中留下已下载的安装包文件（.msi 或 .exe），不会自动删除

1.2 WHEN 用户多次执行更新操作后 THEN 系统在 temp 目录中累积多个旧版本安装包文件，持续占用磁盘空间

1.3 WHEN 便携版用户更新到新版本后 THEN 系统不会删除旧版本的可执行文件，旧文件继续留在磁盘上

### 期望行为（正确）

2.1 WHEN 用户通过 `download_and_install()` 完成更新后 THEN 系统 SHALL 在下次启动时自动清理上一次更新遗留在 temp 目录中的安装包文件

2.2 WHEN 用户多次执行更新操作后 THEN 系统 SHALL 在每次启动时清理所有 rust-air 相关的旧安装包文件，避免 temp 目录中累积残留文件

2.3 WHEN 便携版用户更新到新版本后 THEN 系统 SHALL 在新版本首次启动时尝试删除旧版本的可执行文件

### 不变行为（回归预防）

3.1 WHEN 用户正常执行更新流程时 THEN 系统 SHALL CONTINUE TO 正确下载安装包到 temp 目录并启动安装程序

3.2 WHEN 更新正在下载中时 THEN 系统 SHALL CONTINUE TO 通过 "update-progress" 事件正确报告下载进度

3.3 WHEN 系统检查更新但没有新版本时 THEN 系统 SHALL CONTINUE TO 返回 None 且不执行任何文件操作

3.4 WHEN 清理旧文件失败时（如文件被占用或权限不足） THEN 系统 SHALL CONTINUE TO 正常运行，不因清理失败而崩溃或影响用户体验

3.5 WHEN 当前版本的安装包正在被安装程序使用时 THEN 系统 SHALL CONTINUE TO 不删除正在使用中的文件

---

## Bug 条件推导

### Bug 条件函数

```pascal
FUNCTION isBugCondition(X)
  INPUT: X of type AppUpdateEvent
  OUTPUT: boolean
  
  // 当应用完成更新流程后（安装包已下载、安装程序已启动），
  // 旧文件未被清理的条件成立
  RETURN X.updateCompleted = true AND X.oldFilesExist = true
END FUNCTION
```

### 属性规约 — 修复检查

```pascal
// Property: Fix Checking — 更新后旧文件应被清理
FOR ALL X WHERE isBugCondition(X) DO
  result ← appStartup'(X)
  ASSERT oldInstallerFiles(X.tempDir, X.previousVersions) = EMPTY
END FOR
```

### 保持性目标 — 保持检查

```pascal
// Property: Preservation Checking — 非 bug 输入行为不变
FOR ALL X WHERE NOT isBugCondition(X) DO
  ASSERT downloadAndInstall(X) = downloadAndInstall'(X)
END FOR
```
