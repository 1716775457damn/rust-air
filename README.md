<div align="center">

# ✈️ rust-air

**局域网极速文件传输工具 — AirDrop 的命令行 + 桌面 GUI 版**

[![Release](https://img.shields.io/github/v/release/1716775457damn/rust-air?style=flat-square&color=cyan)](https://github.com/1716775457damn/rust-air/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/1716775457damn/rust-air/release.yml?style=flat-square&label=CI)](https://github.com/1716775457damn/rust-air/actions)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?style=flat-square)](https://www.rust-lang.org)

[下载安装包](#-下载安装) · [快速上手](#-快速上手) · [功能特性](#-功能特性) · [架构设计](#-架构设计) · [本地构建](#-本地构建)

</div>

---

## 📸 效果预览

```
# 发送方
$ rust-air send ./project.zip
📦 Sending  : project.zip
🔑 Name     : rust-air-aB3xYzQr
🔑 Key      : aB3xYzQr1234...（分享给接收方）
🔒 E2EE ChaCha20-Poly1305 + SHA-256 verify
⏳ Waiting for receiver…

🔗 Connected: 192.168.1.8:54321
Sending   [████████████████████████████████████████]  128.00 MB/128.00 MB   312.4 MB/s  ETA 0s

✅ Transfer complete!

# 接收方
$ rust-air receive rust-air-aB3xYzQr:aB3xYzQr1234...
🔍 Resolving 'rust-air-aB3xYzQr' via mDNS…
🔗 Found at 192.168.1.5:49821

Receiving [████████████████████████████████████████]  128.00 MB/128.00 MB   308.1 MB/s  ETA 0s

✅ Saved to: ./project.zip
```

---

## 📦 下载安装

前往 [Releases 页面](https://github.com/1716775457damn/rust-air/releases) 下载对应平台的安装包：

| 平台 | 文件 | 说明 |
|------|------|------|
| Windows | `rust-air_x64-setup.exe` | 推荐，带安装向导 |
| Windows | `rust-air_x64_en-US.msi` | MSI 安装包 |
| Windows CLI | `rust-air-cli-x86_64-pc-windows-msvc.exe` | 免安装命令行版 |
| macOS (M 系列) | `rust-air_aarch64.dmg` | Apple Silicon |
| macOS (Intel) | `rust-air_x64.dmg` | x86_64 |
| macOS CLI | `rust-air-cli-aarch64-apple-darwin` | 命令行版 |
| Linux | `rust-air_amd64.deb` | Debian / Ubuntu |
| Linux | `rust-air_amd64.AppImage` | 免安装，所有发行版通用 |
| Linux CLI | `rust-air-cli-x86_64-unknown-linux-gnu` | 命令行版 |

> **macOS 首次打开提示"未验证的开发者"**：右键点击 → 打开 → 仍然打开

---

## 🚀 快速上手

### CLI 模式

```bash
# ── 发送文件 ──────────────────────────────────────────────
rust-air send photo.jpg

# 发送整个文件夹（流式压缩，无临时文件）
rust-air send ./my_project

# 发送文件 + 同时生成二维码供手机扫码下载
rust-air send video.mp4 --qr

# ── 接收文件 ──────────────────────────────────────────────
# 将发送方显示的 Name 和 Key 拼接后输入
rust-air receive rust-air-aB3xYzQr:aB3xYzQr1234abcdefghijklmnopqr

# 接收到指定目录
rust-air receive rust-air-aB3xYzQr:KEY --out ~/Downloads

# ── 其他功能 ──────────────────────────────────────────────
# 扫描局域网内所有可用发送方
rust-air scan

# 发送剪贴板内容到另一台电脑
rust-air send-clip
```

### GUI 模式

直接打开安装好的 **rust-air** 桌面应用：

1. **发送**：拖拽文件/文件夹到发送区，或点击选择，界面显示分享码
2. **接收**：切换到接收标签，粘贴分享码，选择保存目录
3. **设备**：点击扫描，局域网内的发送方会自动出现在列表中

---

## ✨ 功能特性

### 🔒 安全性
- **端到端加密（E2EE）**：每次传输生成一次性 ChaCha20-Poly1305 密钥，密钥内嵌在分享码中，数据在 TCP 层全程加密，抓包无法还原
- **SHA-256 完整性校验**：接收完成后自动验证文件哈希，校验失败自动删除，杜绝静默数据损坏
- **零信任网络**：即使在公共 Wi-Fi 环境下也可安全使用

### ⚡ 传输性能
- **流式传输**：边读边发、边收边写，几十 GB 大文件内存占用恒定在几十 MB
- **零临时文件压缩**：发送文件夹时，tar + zstd 在内存管道中实时压缩并传输，不在磁盘生成中间文件
- **断点续传**：网络中断后重新运行命令，自动从断点字节处继续，无需重传已完成部分
- **64KB AEAD 分块**：加密与传输并行，最大化吞吐量

### 🔍 自动发现
- **mDNS-SD**：基于标准 DNS-SD 协议，局域网内设备自动广播与发现，无需手动输入 IP 地址
- **设备状态**：实时显示局域网内发送方的在线/忙碌状态

### 📱 跨设备
- **手机扫码下载**：`--qr` 参数在终端打印 ASCII 二维码，手机扫码直接在浏览器下载
- **剪贴板共享**：`send-clip` 将本机剪贴板内容发送到另一台电脑并自动写入其剪贴板
- **全平台 GUI**：Windows / macOS / Linux 原生桌面应用

---

## 🏗 架构设计

```
rust-air/
├── core/                    # rust-air-core — 核心引擎库
│   └── src/
│       ├── proto.rs         # 协议定义：MAGIC、Kind、DeviceInfo、TransferEvent
│       ├── crypto.rs        # ChaCha20-Poly1305 AEAD 流式加解密
│       ├── archive.rs       # tar + zstd 零临时文件流式压缩
│       ├── transfer.rs      # 发送/接收引擎，SHA-256 校验，断点续传
│       ├── discovery.rs     # mDNS-SD 设备广播与发现
│       ├── http_qr.rs       # axum HTTP server + 终端二维码
│       └── clipboard.rs     # arboard 跨平台剪贴板读写
│
├── cli/                     # rust-air CLI 二进制
│   └── src/main.rs          # send / receive / scan / send-clip
│
├── tauri-app/               # Tauri v2 桌面 GUI
│   ├── src/                 # Vue 3 + Tailwind CSS 前端
│   │   └── App.vue          # 拖拽发送、环形进度条、设备列表
│   └── src-tauri/           # Tauri Rust 后端
│       └── src/
│           ├── commands.rs  # 6 个 #[tauri::command] IPC 接口
│           └── lib.rs       # Tauri Builder 初始化
│
└── .github/workflows/
    └── release.yml          # CI/CD：三平台矩阵构建 + 自动发布
```

### 传输协议

```
TCP 连接建立后：

发送方 → 接收方（明文握手头）：
  [4B MAGIC "RAR2"][1B kind][2B name_len][name][8B total_size][32B SHA-256]

接收方 → 发送方（断点续传协商）：
  [8B already_have]   ← 已有字节数，0 表示全新传输

发送方 → 接收方（加密数据流）：
  重复: [4B plaintext_len][16B AEAD tag][ciphertext]
  结束: [4B = 0x00000000]   ← EOF 哨兵
```

### 加密设计

- 算法：ChaCha20-Poly1305（AEAD）
- 密钥：每次传输随机生成 32 字节，base64url 编码后内嵌在分享码中
- Nonce：8 字节帧计数器 + 4 字节零填充，单调递增，永不重用
- 分块：每 64KB 独立加密，支持流式处理

---

## 🔧 本地构建

### 环境要求

- Rust 1.75+（`rustup update stable`）
- Node.js 20+（GUI 构建需要）
- Linux 额外依赖：`libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libavahi-compat-libdnssd-dev`

### 构建 CLI

```bash
git clone https://github.com/1716775457damn/rust-air.git
cd rust-air

# 编译 CLI（推荐，体积小、无 GUI 依赖）
cargo build -p rust-air-cli --release

# 二进制位于
./target/release/rust-air        # Linux / macOS
./target/release/rust-air.exe    # Windows
```

### 构建桌面 GUI

```bash
cd tauri-app
npm install

# 开发模式（热重载）
npm run tauri dev

# 生产构建（生成安装包）
npm run tauri build
```

### 运行测试

```bash
# 在同一台机器上测试（两个终端）
echo "hello world" > test.txt

# 终端 1
./target/release/rust-air send test.txt

# 终端 2（粘贴终端 1 显示的 Name 和 Key）
./target/release/rust-air receive rust-air-XXXXXXXX:KEY --out /tmp
```

---

## 🤖 CI/CD 自动发布

推送 `v*` 格式的 tag 即可触发三平台并行构建：

```bash
git tag v1.0.0
git push origin v1.0.0
```

GitHub Actions 会自动启动 4 台虚拟机（Windows / macOS ARM / macOS Intel / Linux），约 15 分钟后在 [Releases](https://github.com/1716775457damn/rust-air/releases) 页面生成所有平台的安装包。

---

## 📋 技术栈

| 层 | 技术 |
|----|------|
| 异步运行时 | tokio |
| 加密 | chacha20poly1305 |
| 压缩 | zstd + tar |
| 网络发现 | mdns-sd |
| 进度条 | indicatif |
| 剪贴板 | arboard |
| HTTP / QR | axum + qrcode |
| 桌面 GUI | Tauri v2 |
| 前端 | Vue 3 + Tailwind CSS |
| CLI 解析 | clap |

---

## 📄 License

MIT © 2024 rust-air contributors
