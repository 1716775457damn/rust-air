<div align="center">

# ✈️ rust-air

**局域网极速文件传输 — 跨平台 CLI + 桌面 GUI**

[![Release](https://img.shields.io/github/v/release/1716775457damn/rust-air?style=flat-square&color=22d3ee)](https://github.com/1716775457damn/rust-air/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/1716775457damn/rust-air/release.yml?style=flat-square&label=CI)](https://github.com/1716775457damn/rust-air/actions)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?style=flat-square)](https://www.rust-lang.org)

同一局域网内，文件/文件夹/剪贴板一键传输，全程端到端加密，无需账号、无需云服务。

[下载](#-下载) · [快速上手](#-快速上手) · [功能](#-功能) · [协议设计](#-协议设计) · [本地构建](#-本地构建)

</div>

---

## 📦 下载

前往 [Releases](https://github.com/1716775457damn/rust-air/releases) 下载最新版本：

| 平台 | 文件 | 说明 |
|------|------|------|
| Windows | `rust-air_x64_en-US.msi` | 桌面 GUI，带安装向导 |
| Windows CLI | `rust-air-cli-x86_64-pc-windows-msvc.exe` | 免安装命令行版 |
| macOS (Apple Silicon) | `rust-air_aarch64.dmg` | M 系列芯片 |
| macOS (Intel) | `rust-air_x64.dmg` | x86_64 |
| macOS CLI | `rust-air-cli-aarch64-apple-darwin` | 命令行版 |
| Linux | `rust-air_amd64.deb` | Debian / Ubuntu |
| Linux | `rust-air_amd64.AppImage` | 免安装，所有发行版通用 |
| Linux CLI | `rust-air-cli-x86_64-unknown-linux-gnu` | 命令行版 |

> **macOS 首次打开提示"未验证的开发者"**：右键 → 打开 → 仍然打开

---

## 🚀 快速上手

### GUI

打开安装好的 **rust-air** 桌面应用，支持深色 / 浅色主题切换（右上角 ☀️/🌙）。

| 标签 | 功能 |
|------|------|
| 📤 发送 | 拖拽或点击选择文件/文件夹，点击局域网设备卡片即发送 |
| 📥 接收 | 自动监听，有人发送时自动弹出进度，完成后点击路径打开目录 |
| 🔍 设备 | 扫描局域网内所有在线的 rust-air 实例 |
| 📂 搜索 | 按文件名或文本内容搜索本机文件，支持正则 |
| 🔄 同步 | 增量同步两个目录，支持实时监听 |

### CLI

```bash
# 发送文件（自动扫描局域网，交互式选择目标）
rust-air send photo.jpg

# 发送文件夹（流式 tar+zstd 压缩，无临时文件）
rust-air send ./my_project

# 发送到指定地址
rust-air send video.mp4 --to 192.168.1.5:49821

# 生成二维码，手机浏览器直接下载
rust-air send video.mp4 --qr

# 接收文件（自动监听，Ctrl-C 停止）
rust-air receive --out ~/Downloads

# 发送剪贴板内容到另一台机器
rust-air send-clip --to 192.168.1.5:49821

# 扫描局域网内所有在线设备
rust-air scan
```

---

## ✨ 功能

### 🔒 安全

- **端到端加密**：每次传输随机生成 32 字节一次性密钥，ChaCha20-Poly1305 AEAD 全程加密，密钥不经过任何服务器
- **SHA-256 完整性校验**：发送方在流式传输过程中实时计算哈希，EOF 后追加到流尾；接收方验证失败自动删除文件
- **协议版本校验**：握手头包含 4 字节 Magic（`RAR4`），版本不匹配立即断开

### ⚡ 传输性能

- **流式传输**：边读边发、边收边写，传输数十 GB 大文件时内存占用恒定在几十 MB
- **零临时文件压缩**：发送文件夹时，tar + zstd 在内存管道中实时压缩并传输，磁盘无中间文件
- **断点续传**：单文件传输中断后重新运行，自动从已完成的 256 KB 块边界继续，无需重传
- **256 KB AEAD 分块**：加密与 I/O 并行，最大化吞吐量

### 🌐 自动发现

- **mDNS-SD**：基于标准 `_rustair._tcp.local.` 服务类型，启动即广播，无需手动输入 IP
- **多网卡支持**：同时注册所有非回环 IPv4 地址，Wi-Fi + 有线同时在线时均可被发现
- **唯一实例名**：主机名后附加 IP 派生的 4 位十六进制后缀，防止同名设备冲突

### 📂 文件搜索

- **文件名搜索**：正则匹配，实时流式返回结果
- **文本内容搜索**：支持 UTF-8 / GBK 双编码，自动跳过二进制文件，高亮匹配行
- **结果过滤**：搜索完成后可在结果中二次过滤，最多返回 2000 条

### 🔄 文件同步

- **增量同步**：先比对文件大小和 mtime 快速跳过未变更文件，再并行 SHA-256 哈希确认
- **原子写入**：先写 `.svtmp` 临时文件再重命名，防止写入中断导致目标文件损坏
- **实时监听**：`notify` 文件系统事件 + 300 ms 防抖，自动同步变更
- **排除规则**：支持精确名称（`node_modules`）和通配符（`*.tmp`），内置常用默认规则

### 📱 手机下载

```bash
rust-air send video.mp4 --qr
```

终端打印 ASCII 二维码，手机扫码后在浏览器直接下载，无需安装任何 App。

---

## 🏗 项目结构

```
rust-air/
├── core/                    # rust-air-core — 核心引擎库
│   └── src/
│       ├── proto.rs         # 协议常量、Kind、DeviceInfo、TransferEvent
│       ├── crypto.rs        # ChaCha20-Poly1305 AEAD 流式加解密
│       ├── archive.rs       # tar + zstd 零临时文件流式压缩/解压
│       ├── transfer.rs      # 发送/接收引擎，SHA-256 校验，断点续传
│       ├── discovery.rs     # mDNS-SD 设备广播与发现，多网卡支持
│       ├── http_qr.rs       # axum HTTP 文件服务 + 终端二维码
│       ├── clipboard.rs     # arboard 跨平台剪贴板读写
│       └── sync_vault.rs    # 增量文件同步引擎，文件系统监听
│
├── cli/                     # rust-air CLI 二进制
│   └── src/main.rs          # send / receive / scan / send-clip
│
├── tauri-app/               # Tauri v2 桌面 GUI
│   ├── src/                 # Vue 3 + Tailwind CSS v4 前端
│   │   ├── App.vue          # 全部 UI：发送、接收、设备、搜索、同步
│   │   └── style.css        # CSS 变量主题（深色/浅色）
│   └── src-tauri/           # Tauri Rust 后端
│       └── src/
│           ├── commands.rs       # 文件传输 IPC 命令
│           ├── search_commands.rs # 文件搜索命令
│           └── sync_commands.rs  # 文件同步命令
│
└── .github/workflows/
    └── release.yml          # CI/CD：四平台矩阵构建 + 自动发布
```

---

## 🔬 协议设计

### 传输协议（v4）

```
TCP 连接建立后：

发送方 → 接收方（明文握手头）：
  [4B MAGIC "RAR4"][32B 一次性密钥][1B kind]
  [2B name_len][name bytes][8B total_size]

接收方 → 发送方（断点续传协商）：
  [8B already_have]   ← 已完成字节数，0 = 全新传输

发送方 → 接收方（加密数据流）：
  重复: [4B chunk_len][16B AEAD tag][ciphertext]
  结束: [4B = 0x00000000]   ← EOF 哨兵

发送方 → 接收方（完整性校验）：
  [32B SHA-256]   ← 发送方流式计算，EOF 后追加，无需二次读文件
```

传输类型（`kind` 字节）：

| 值 | 类型 | 说明 |
|----|------|------|
| `0x01` | File | 单文件，支持断点续传 |
| `0x02` | Archive | 文件夹，tar+zstd 流式压缩 |
| `0x03` | Clipboard | 剪贴板文本，接收后自动写入系统剪贴板 |

### 加密设计

| 项目 | 值 |
|------|----|
| 算法 | ChaCha20-Poly1305（AEAD） |
| 密钥 | 每次传输随机生成 32 字节，明文嵌入握手头（仅局域网传输） |
| Nonce | 8 字节帧计数器（小端序）+ 4 字节零填充，单调递增，永不重用 |
| 分块 | 256 KB，每块独立加密，支持流式处理 |

---

## 🔧 本地构建

### 环境要求

- Rust 1.75+（`rustup update stable`）
- Node.js 22+ 和 pnpm 9+（GUI 构建需要）
- Linux 额外依赖：

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libavahi-compat-libdnssd-dev \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

### 构建 CLI

```bash
git clone https://github.com/1716775457damn/rust-air.git
cd rust-air

cargo build -p rust-air-cli --release

# 产物
./target/release/rust-air        # Linux / macOS
./target/release/rust-air.exe    # Windows
```

### 构建桌面 GUI

```bash
cd tauri-app
pnpm install

# 开发模式（热重载）
pnpm tauri dev

# 生产构建（生成安装包）
pnpm tauri build
```

### 快速测试（同一台机器，两个终端）

```bash
echo "hello world" > test.txt

# 终端 1 — 接收方
./target/release/rust-air receive --out /tmp

# 终端 2 — 发送方（选择终端 1 显示的设备）
./target/release/rust-air send test.txt
```

---

## 🤖 CI/CD

推送 `v*` 格式的 tag 触发四平台并行构建：

```bash
git tag v1.2.0
git push origin v1.2.0
```

GitHub Actions 启动 4 台虚拟机（Windows / macOS ARM / macOS Intel / Ubuntu），约 10 分钟后在 [Releases](https://github.com/1716775457damn/rust-air/releases) 生成全部安装包和 CLI 二进制。

---

## 📋 技术栈

| 层 | 技术 |
|----|------|
| 异步运行时 | tokio |
| 加密 | chacha20poly1305 |
| 压缩 | zstd + tar |
| 网络发现 | mdns-sd |
| 剪贴板 | arboard |
| HTTP / QR | axum + qrcode |
| 文件搜索 | regex + ignore + memmap2 + encoding_rs |
| 文件同步 | notify + walkdir + sha2 + rayon |
| 桌面 GUI | Tauri v2 |
| 前端 | Vue 3 + Tailwind CSS v4 |
| CLI 解析 | clap |

---

## 📄 License

MIT © 2024 rust-air contributors
