# 实施计划：Android 构建支持

## 概述

将 rust-air（Tauri 2 LAN 文件传输应用）改造为支持 Android 构建。核心策略是通过 Cargo feature `desktop` 和 `#[cfg()]` 条件编译隔离桌面专用依赖和代码，为 Android 提供 UDP 广播设备发现替代实现，并配置 Android 项目结构、权限和签名。用户不需要在本机编译，只需确保代码和配置就绪。

## 任务

- [x] 1. Core 库条件编译改造
  - [x] 1.1 改造 `core/Cargo.toml`，添加 `desktop` feature 并将桌面专用依赖设为可选
    - 新增 `[features]` 段，定义 `desktop = ["arboard", "notify", "walkdir", "mdns-sd", "if-addrs", "axum", "qrcode", "indicatif", "rayon"]`
    - 将 `arboard`、`notify`、`walkdir`、`mdns-sd`、`if-addrs`、`axum`、`qrcode`、`indicatif`、`rayon` 改为 `optional = true`
    - 保持 `tokio`、`anyhow`、`serde`、`chacha20poly1305`、`rand`、`base64`、`sha2`、`tar`、`zstd`、`os_pipe`、`serde_json`、`chrono`、`dirs`、`unicode-normalization`、`hex` 为始终可用依赖
    - _需求: 2.1, 2.2_

  - [x] 1.2 改造 `core/src/lib.rs`，对桌面专用模块添加条件编译属性
    - 对 `clipboard`、`clipboard_history`、`sync_vault`、`http_qr` 模块添加 `#[cfg(feature = "desktop")]`
    - 对 `discovery` 模块添加 `#[cfg(feature = "desktop")]`，非桌面时使用 `discovery_udp` 并 `pub use discovery_udp as discovery`
    - 对桌面专用的 `pub use` 导出（`SyncConfig`、`SyncEvent`、`SyncStore`、`ClipContent`、`ClipEntry`、`HistoryStore`、`send_clipboard` 等）添加 `#[cfg(feature = "desktop")]`
    - 非桌面平台引入 `stubs` 模块
    - 确保 `archive`、`crypto`、`proto`、`transfer` 模块和 `DeviceInfo`、`DeviceStatus`、`TransferEvent` 导出在所有平台可用
    - _需求: 2.3, 2.4, 2.5_

  - [x] 1.3 创建 `core/src/stubs.rs` 存根模块
    - 提供 `SyncConfig`、`SyncEvent`、`SyncStore`、`ExcludeSet`、`ClipContent`、`ClipEntry`、`HistoryStore` 空存根类型
    - 提供 `fmt_bytes()`、`default_excludes()` 存根函数
    - 确保依赖 Core 的 crate 在无 `desktop` feature 时仍可编译
    - _需求: 2.4_

  - [x] 1.4 创建 `core/src/discovery_udp.rs` UDP 广播设备发现模块
    - 实现 `ServiceHandle` 和 `BrowseHandle` 类型
    - 实现 `register_self(port, device_name)` 函数，周期性向 `255.255.255.255:51820` 发送 UDP 广播
    - 实现 `browse_devices_sync(tx)` 函数，监听 UDP 广播并解析设备信息
    - 实现 `local_lan_ip()`、`safe_device_name()`、`lan_ipv4_addrs()` 函数
    - 广播包格式：`MAGIC(8B "RUSTAIR1") + port(2B LE) + name_len(1B) + name(UTF-8)`
    - 确保公共接口与 `discovery.rs` 兼容，使调用方无需修改
    - _需求: 4.1, 4.2, 4.3, 4.4_

  - [ ]* 1.5 为 `discovery_udp.rs` 编写单元测试
    - 测试广播包的序列化/反序列化
    - 测试 `safe_device_name()` 函数
    - 测试 `local_lan_ip()` 返回值格式
    - _需求: 4.1, 4.2_

- [x] 2. 检查点 — 确保 Core 库编译通过
  - 运行 `cargo build -p rust-air-core --features desktop` 确保桌面编译不受影响
  - 运行 `cargo build -p rust-air-core` (无 desktop feature) 确保 Android 路径编译通过
  - 确保所有测试通过，如有问题请询问用户

- [x] 3. Tauri App 条件编译改造
  - [x] 3.1 改造 `tauri-app/src-tauri/Cargo.toml`，添加 `desktop` feature 并将桌面专用依赖设为可选
    - 新增 `[features]` 段，定义 `default = ["desktop"]` 和 `desktop = ["rust-air-core/desktop", "arboard", "notify", "ignore", "memmap2", "num_cpus", "encoding_rs"]`
    - 将 `arboard`、`notify`、`ignore`、`memmap2`、`num_cpus`、`encoding_rs` 改为 `optional = true`
    - 确保 `rust-air-core` 依赖在 `desktop` feature 启用时传递 `desktop` feature
    - _需求: 3.1, 3.6_

  - [x] 3.2 改造 `tauri-app/src-tauri/src/lib.rs`，对桌面专用模块和命令添加条件编译
    - 对 `sync_commands`、`search_commands`、`clip_history_commands`、`update_commands` 模块声明添加 `#[cfg(feature = "desktop")]`
    - 将 `tauri::generate_handler!` 拆分为桌面和非桌面两个版本
    - 非桌面版本仅注册 `start_listener`、`send_to`、`cancel_send`、`scan_devices`、`get_local_ips`
    - 桌面版本注册所有现有命令
    - 对剪贴板监控线程（`start_clip_monitor`）和自动更新检查的 `setup` 逻辑添加 `#[cfg(feature = "desktop")]`
    - 对桌面专用的 `.manage()` 调用添加 `#[cfg(feature = "desktop")]`
    - _需求: 3.2, 3.3, 3.4, 3.5_

  - [x] 3.3 改造 `tauri-app/src-tauri/src/commands.rs`，对桌面专用命令添加条件编译
    - 对 `read_clipboard`、`write_clipboard` 命令添加 `#[cfg(feature = "desktop")]`
    - 对 `open_path` 命令添加 `#[cfg(feature = "desktop")]`
    - 对 `use rust_air_core::clipboard` 导入添加 `#[cfg(feature = "desktop")]`
    - 确保 `start_listener`、`send_to`、`cancel_send`、`scan_devices`、`get_local_ips` 在所有平台可用
    - _需求: 3.2, 3.5_

- [x] 4. 检查点 — 确保 Tauri App 编译通过
  - 运行 `cargo build -p tauri-app` (默认启用 desktop) 确保桌面编译不受影响
  - 运行 `cargo build -p tauri-app --no-default-features` 确保无 desktop feature 时编译通过
  - 确保所有测试通过，如有问题请询问用户

- [x] 5. Android 项目初始化和配置
  - [x] 5.1 更新 `tauri-app/src-tauri/tauri.conf.json`，添加 Android 构建配置
    - 在 `bundle` 中添加 `"android": { "minSdkVersion": 24 }`
    - 确保 `identifier` 为 `dev.rustair.landrop`
    - _需求: 1.2, 1.3, 1.4_

  - [x] 5.2 创建 Android 项目初始化脚本说明
    - 在文档中说明需要运行 `npx tauri android init` 生成 Android 项目骨架
    - 说明生成后需要手动修改 AndroidManifest.xml 和 Gradle 配置
    - _需求: 1.1_

  - [x] 5.3 创建 AndroidManifest.xml 权限配置模板文件
    - 创建 `tauri-app/src-tauri/gen/android/manifest-permissions.xml` 模板，包含所有必需权限声明
    - 包含 `INTERNET`、`ACCESS_NETWORK_STATE`、`ACCESS_WIFI_STATE` 权限
    - 包含 `READ_EXTERNAL_STORAGE`、`WRITE_EXTERNAL_STORAGE` 权限
    - 包含 `CHANGE_WIFI_MULTICAST_STATE` 权限
    - 包含 `usesCleartextTraffic=true` 配置
    - _需求: 5.1, 5.2, 5.3, 5.4, 5.5, 5.6_

  - [x] 5.4 创建 Gradle 签名配置模板
    - 创建 `tauri-app/src-tauri/gen/android/signing-config.gradle.kts` 模板文件
    - 包含 release 签名配置，支持通过 `keystore.properties` 文件引用签名密钥
    - 密钥文件缺失时抛出 `GradleException` 并提示查看文档
    - 创建 `keystore.properties.example` 示例文件
    - _需求: 7.1, 7.2, 7.3_

- [x] 6. 前端平台适配
  - [x] 6.1 安装 `@tauri-apps/plugin-os` 依赖并在 `App.vue` 中添加平台检测
    - 在 `package.json` 中添加 `@tauri-apps/plugin-os` 依赖
    - 在 `App.vue` 的 `onMounted` 中通过 `platform()` API 检测是否为 Android
    - 创建 `isAndroid` 响应式变量
    - _需求: 6.3_

  - [x] 6.2 在 Android 上隐藏桌面专用功能入口
    - 在侧边栏中隐藏"搜索"和"同步" Tab（当 `isAndroid` 为 true 时）
    - 在设置页面中隐藏自动更新相关选项
    - 跳过对 `get_sync_config`、`get_sync_status`、`get_default_excludes`、`get_update_settings` 等桌面专用命令的 `invoke` 调用
    - 隐藏剪贴板相关功能（`copyIp` 中的 `write_clipboard` 调用改为使用 `navigator.clipboard` 回退）
    - _需求: 6.3_

  - [x] 6.3 确保 `vite.config.ts` 兼容 Android 开发模式
    - 确认 `TAURI_DEV_HOST` 环境变量在 Android 开发模式下正确使用（现有配置已支持）
    - 确认前端构建输出路径 `../dist` 在 Android 构建流程中可被正确引用
    - _需求: 6.1, 6.2_

- [x] 7. 检查点 — 确保前端和整体编译通过
  - 运行 `npm run build`（在 `tauri-app` 目录）确保前端构建正常
  - 运行 `cargo build -p tauri-app` 确保桌面完整编译通过
  - 运行 `cargo build -p tauri-app --no-default-features` 确保无 desktop feature 编译通过
  - 确保所有测试通过，如有问题请询问用户

- [x] 8. 构建文档
  - [x] 8.1 创建 `docs/android-build.md` Android 构建指南
    - 说明 Android 构建所需环境依赖：Android SDK、NDK r25+、JDK 17
    - 说明 Rust Android 目标工具链安装：`rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android`
    - 说明环境变量配置：`ANDROID_HOME`、`NDK_HOME`
    - 说明从克隆仓库到生成 APK 的完整构建步骤（`npm install` → `npx tauri android init` → 配置权限和签名 → `npx tauri android build`）
    - 说明签名密钥的生成方法（`keytool` 命令）和 `keystore.properties` 配置
    - 说明常见问题排查（NDK 路径、SDK 版本、编译错误等）
    - _需求: 8.1, 8.2, 8.3, 8.4_

- [x] 9. 最终检查点 — 确保所有改动完整且一致
  - 确保所有测试通过，如有问题请询问用户
  - 确认桌面构建功能完全不受影响
  - 确认所有条件编译路径正确无遗漏

## 备注

- 标记 `*` 的任务为可选任务，可跳过以加快进度
- 每个任务引用了对应的需求编号以确保可追溯性
- 检查点任务确保增量验证，避免问题累积
- 本机不需要安装 Android SDK/NDK，条件编译验证可通过 `--no-default-features` 模拟
- 实际 Android 编译需要在配置了 Android NDK 的机器上进行
