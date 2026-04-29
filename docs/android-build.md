# Android 构建指南

本文档说明如何在配置了 Android SDK/NDK 的机器上构建 rust-air Android APK。

## 环境依赖

### 1. Android SDK

- 安装 [Android Studio](https://developer.android.com/studio) 或通过命令行工具安装 Android SDK
- 最低 API Level: **24** (Android 7.0)
- 推荐安装 SDK Platform: **API 33** 或更高

### 2. Android NDK

- 版本要求: **NDK r25** 或更高（推荐 r25c）
- 通过 Android Studio SDK Manager 安装，或从 [NDK 下载页](https://developer.android.com/ndk/downloads) 手动下载

### 3. JDK

- 版本要求: **JDK 17**
- 推荐使用 [Eclipse Temurin](https://adoptium.net/) 或 Android Studio 自带的 JDK

### 4. Rust 工具链

安装 Rust Android 交叉编译目标：

```bash
rustup target add aarch64-linux-android
rustup target add armv7-linux-androideabi
rustup target add x86_64-linux-android
rustup target add i686-linux-android
```

### 5. Node.js

- 版本要求: Node.js 18+
- 需要 npm 或 pnpm 包管理器

## 环境变量配置

设置以下环境变量（根据实际安装路径调整）：

### Linux / macOS

```bash
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/25.2.9519653"
export PATH="$ANDROID_HOME/platform-tools:$PATH"
```

### Windows

```powershell
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"
$env:NDK_HOME = "$env:ANDROID_HOME\ndk\25.2.9519653"
```

建议将这些变量添加到 shell 配置文件（`~/.bashrc`、`~/.zshrc`）或系统环境变量中。

## 完整构建步骤

### 1. 克隆仓库并安装依赖

```bash
git clone <repo-url> rust-air
cd rust-air/tauri-app
npm install
```

### 2. 初始化 Android 项目

```bash
npx tauri android init
```

此命令会在 `src-tauri/gen/android/` 下生成 Android Gradle 项目。

### 3. 配置 Android 权限

打开 `src-tauri/gen/android/app/src/main/AndroidManifest.xml`，参考
`src-tauri/gen/android/manifest-permissions.xml` 模板添加所需权限：

- `INTERNET` — 网络通信
- `ACCESS_NETWORK_STATE` — 网络状态检测
- `ACCESS_WIFI_STATE` — Wi-Fi 状态（获取本机 IP）
- `READ_EXTERNAL_STORAGE` / `WRITE_EXTERNAL_STORAGE` — 文件读写
- `CHANGE_WIFI_MULTICAST_STATE` — UDP 广播（设备发现）

在 `<application>` 标签上添加 `android:usesCleartextTraffic="true"`。

### 4. 配置签名密钥

#### 生成签名密钥

```bash
keytool -genkey -v \
  -keystore rust-air-release.keystore \
  -alias rust-air \
  -keyalg RSA \
  -keysize 2048 \
  -validity 10000 \
  -storepass <your_store_password> \
  -keypass <your_key_password> \
  -dname "CN=rust-air, O=rustair, L=Unknown, ST=Unknown, C=CN"
```

#### 配置 keystore.properties

在 `src-tauri/gen/android/` 目录下创建 `keystore.properties` 文件：

```properties
storeFile=../../../rust-air-release.keystore
storePassword=your_store_password
keyAlias=rust-air
keyPassword=your_key_password
```

参考 `src-tauri/gen/android/signing-config.gradle.kts` 将签名配置合并到
`app/build.gradle.kts` 的 `android {}` 块中。

### 5. 构建 APK

#### Debug 构建

```bash
npx tauri android build --debug
```

#### Release 构建

```bash
npx tauri android build
```

构建产物位于 `src-tauri/gen/android/app/build/outputs/apk/`。

## 常见问题排查

### NDK 路径错误

**症状**: `NDK not found` 或 `linker not found` 错误

**解决**: 确认 `NDK_HOME` 环境变量指向正确的 NDK 目录，例如：
```
$ANDROID_HOME/ndk/25.2.9519653
```

### SDK 版本不匹配

**症状**: `SDK platform not found` 错误

**解决**: 通过 Android Studio SDK Manager 安装对应 API Level 的 SDK Platform，
或运行：
```bash
sdkmanager "platforms;android-33"
```

### Rust 编译错误：找不到链接器

**症状**: `error: linker 'aarch64-linux-android-clang' not found`

**解决**: 确认已安装 NDK 并正确设置 `NDK_HOME`。Tauri CLI 会自动配置
Cargo 的链接器路径，但需要 NDK 可用。

### Gradle 构建失败：keystore.properties not found

**症状**: `GradleException: keystore.properties not found`

**解决**: 按照上方"配置签名密钥"步骤创建 `keystore.properties` 文件。
Debug 构建可以跳过签名配置。

### 前端构建失败

**症状**: Vite 构建错误

**解决**: 确认在 `tauri-app` 目录下已运行 `npm install`，且 Node.js 版本 >= 18。

### JDK 版本不兼容

**症状**: `Unsupported class file major version` 错误

**解决**: 确认使用 JDK 17。可通过 `java -version` 检查当前版本。
如果安装了多个 JDK，设置 `JAVA_HOME` 环境变量指向 JDK 17。
