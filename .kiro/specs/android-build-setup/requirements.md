# 需求文档：Android 构建支持

## 简介

为 rust-air（Tauri 2 LAN 文件传输应用）配置 Android 构建支持。目标是完成所有项目配置和条件编译改造，使项目可以在配置了 Android NDK 的机器上成功编译出 Android APK。本机不需要编译，只需确保代码和配置就绪。

## 术语表

- **Build_System**: rust-air 项目的构建系统，包括 Cargo workspace、Tauri CLI 和 Vite 前端构建
- **Tauri_App**: 位于 `tauri-app/src-tauri` 的 Tauri 2 应用 crate（`tauri-app`）
- **Core_Lib**: 位于 `core` 的核心库 crate（`rust-air-core`），提供文件传输、发现、剪贴板、同步等功能
- **Android_Project**: 由 `tauri android init` 生成的 Android Gradle 项目，位于 `tauri-app/src-tauri/gen/android`
- **Desktop_Only_Crate**: 仅在桌面平台可用、在 Android 目标上无法编译的 Rust crate（如 `arboard`、`notify`、`mdns-sd`、`ignore`、`memmap2`）
- **Conditional_Compilation**: 使用 Rust 的 `#[cfg()]` 属性和 Cargo feature 在不同目标平台间选择性编译代码
- **Android_NDK**: Android Native Development Kit，用于将 Rust 代码交叉编译为 Android 原生库

## 需求

### 需求 1：初始化 Android 项目结构

**用户故事：** 作为开发者，我想要生成 Tauri Android 项目骨架，以便项目包含 Android 构建所需的 Gradle 配置和 Kotlin 入口文件。

#### 验收标准

1. WHEN `tauri android init` 命令执行后，THE Android_Project SHALL 在 `tauri-app/src-tauri/gen/android` 目录下生成完整的 Gradle 项目结构
2. THE Android_Project SHALL 包含正确的应用标识符 `dev.rustair.landrop`
3. THE Android_Project SHALL 包含正确的应用名称 `rust-air`
4. THE Build_System SHALL 在 `tauri.conf.json` 中包含 Android 构建所需的 bundle 配置

### 需求 2：Core 库条件编译 — 桌面专用依赖隔离

**用户故事：** 作为开发者，我想要将 Core_Lib 中不兼容 Android 的依赖通过条件编译隔离，以便 Core_Lib 可以在 Android 目标上成功编译。

#### 验收标准

1. THE Core_Lib SHALL 使用 Cargo feature `desktop` 将以下 Desktop_Only_Crate 设为可选依赖：`arboard`、`notify`、`walkdir`、`mdns-sd`、`if-addrs`、`axum`、`qrcode`、`indicatif`、`rayon`
2. WHEN 编译目标为 Android 时，THE Core_Lib SHALL 排除所有 Desktop_Only_Crate 的编译
3. THE Core_Lib SHALL 使用 `#[cfg(feature = "desktop")]` 属性对 `clipboard` 模块、`clipboard_history` 模块、`sync_vault` 模块、`discovery` 模块和 `http_qr` 模块进行条件编译
4. WHEN `desktop` feature 未启用时，THE Core_Lib SHALL 提供空的存根（stub）模块或类型定义，确保依赖 Core_Lib 的 crate 仍可编译
5. THE Core_Lib SHALL 保持 `archive`、`crypto`、`proto`、`transfer` 模块在所有平台上可用

### 需求 3：Tauri App 条件编译 — 桌面专用命令隔离

**用户故事：** 作为开发者，我想要将 Tauri_App 中不兼容 Android 的命令模块通过条件编译隔离，以便 Tauri_App 可以在 Android 目标上成功编译。

#### 验收标准

1. THE Tauri_App SHALL 使用 Cargo feature `desktop` 将以下 Desktop_Only_Crate 设为可选依赖：`arboard`、`notify`、`ignore`、`memmap2`、`num_cpus`、`encoding_rs`
2. WHEN 编译目标为 Android 时，THE Tauri_App SHALL 排除 `sync_commands`、`search_commands`、`clip_history_commands`、`update_commands` 模块的编译
3. WHEN 编译目标为 Android 时，THE Tauri_App SHALL 从 `tauri::generate_handler!` 宏中排除所有桌面专用命令的注册
4. WHEN 编译目标为 Android 时，THE Tauri_App SHALL 排除剪贴板监控线程（`start_clip_monitor`）和自动更新检查的启动逻辑
5. THE Tauri_App SHALL 在 Android 目标上保留核心文件传输功能（`start_listener`、`send_to`、`cancel_send`、`scan_devices`、`get_local_ips`）的注册
6. THE Tauri_App SHALL 在 `Cargo.toml` 中为桌面目标默认启用 `desktop` feature

### 需求 4：设备发现功能 Android 适配

**用户故事：** 作为开发者，我想要在 Android 上提供替代的设备发现机制，以便 Android 版本仍能发现局域网内的其他设备。

#### 验收标准

1. WHEN 编译目标为 Android 时，THE Core_Lib SHALL 提供基于 UDP 广播的简化设备发现实现，替代 mDNS-SD
2. THE Core_Lib SHALL 确保 `DeviceInfo` 和 `DeviceStatus` 类型在所有平台上可用
3. WHEN 编译目标为 Android 时，THE Core_Lib SHALL 提供 `local_lan_ip()` 函数的 Android 兼容实现
4. IF mDNS-SD 依赖在 Android 上不可用，THEN THE Core_Lib SHALL 通过条件编译提供功能等价的替代实现

### 需求 5：Android 权限和清单配置

**用户故事：** 作为开发者，我想要配置 Android 应用所需的权限，以便应用在 Android 设备上可以正常进行网络通信和文件操作。

#### 验收标准

1. THE Android_Project SHALL 在 AndroidManifest.xml 中声明 `INTERNET` 权限
2. THE Android_Project SHALL 在 AndroidManifest.xml 中声明 `ACCESS_NETWORK_STATE` 权限
3. THE Android_Project SHALL 在 AndroidManifest.xml 中声明 `ACCESS_WIFI_STATE` 权限
4. THE Android_Project SHALL 在 AndroidManifest.xml 中声明 `READ_EXTERNAL_STORAGE` 和 `WRITE_EXTERNAL_STORAGE` 权限（用于文件传输）
5. THE Android_Project SHALL 在 AndroidManifest.xml 中声明 `CHANGE_WIFI_MULTICAST_STATE` 权限（用于设备发现广播）
6. THE Android_Project SHALL 配置 `usesCleartextTraffic=true` 以允许局域网 HTTP 通信

### 需求 6：前端构建适配

**用户故事：** 作为开发者，我想要确保前端构建配置兼容 Android 目标，以便 Vite 构建的前端资源可以正确打包到 Android APK 中。

#### 验收标准

1. THE Build_System SHALL 确保 `vite.config.ts` 中的 `TAURI_DEV_HOST` 环境变量在 Android 开发模式下正确配置
2. THE Build_System SHALL 确保前端构建输出路径（`../dist`）在 Android 构建流程中可被正确引用
3. WHEN 前端代码调用桌面专用的 Tauri 命令时，THE Build_System SHALL 通过平台检测避免在 Android 上调用不存在的命令

### 需求 7：Android 签名配置

**用户故事：** 作为开发者，我想要配置 Android APK 签名，以便构建出的 APK 可以安装到 Android 设备上。

#### 验收标准

1. THE Android_Project SHALL 在 Gradle 配置中包含 release 签名配置的模板
2. THE Android_Project SHALL 支持通过环境变量或 `keystore.properties` 文件引用签名密钥
3. IF 签名密钥文件不存在，THEN THE Build_System SHALL 在构建时给出明确的错误提示而非静默失败

### 需求 8：构建文档

**用户故事：** 作为开发者，我想要有清晰的 Android 构建指南，以便在另一台配置了 Android SDK/NDK 的电脑上可以顺利编译。

#### 验收标准

1. THE Build_System SHALL 提供文档说明 Android 构建所需的环境依赖（Android SDK、NDK、JDK 版本要求）
2. THE Build_System SHALL 提供文档说明 Rust Android 目标工具链的安装步骤（`rustup target add` 命令）
3. THE Build_System SHALL 提供文档说明从克隆仓库到生成 APK 的完整构建步骤
4. THE Build_System SHALL 提供文档说明签名密钥的生成和配置方法
