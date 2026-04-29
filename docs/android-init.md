# Android 项目初始化说明

## 前提条件

确保已安装以下工具：
- Android SDK (API Level 24+)
- Android NDK r25+
- JDK 17
- Rust Android 目标工具链

## 初始化步骤

### 1. 生成 Android 项目骨架

在 `tauri-app` 目录下运行：

```bash
npx tauri android init
```

此命令会在 `tauri-app/src-tauri/gen/android/` 下生成完整的 Android Gradle 项目结构。

### 2. 修改 AndroidManifest.xml

生成后，需要手动将权限声明添加到 `AndroidManifest.xml` 中。
参考模板文件：`tauri-app/src-tauri/gen/android/manifest-permissions.xml`

在 `<manifest>` 标签内、`<application>` 标签之前添加所有 `<uses-permission>` 声明。
在 `<application>` 标签上添加 `android:usesCleartextTraffic="true"` 属性。

### 3. 配置 Gradle 签名

将 `tauri-app/src-tauri/gen/android/signing-config.gradle.kts` 中的签名配置
合并到生成的 `app/build.gradle.kts` 的 `android {}` 块中。

同时，将 `keystore.properties.example` 复制为 `keystore.properties` 并填入实际的签名密钥信息。

### 4. 构建

```bash
npx tauri android build
```

详细构建指南请参阅 [docs/android-build.md](./android-build.md)。
