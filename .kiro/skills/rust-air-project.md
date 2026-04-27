---
name: rust-air-project
description: "rust-air 项目总览。当需要了解项目结构、模块职责、依赖关系时使用此 skill。适用于任何涉及项目架构决策的场景。"
inclusion: manual
---

# rust-air 项目总览

## 项目定位
LAN 文件传输工具，类似 AirDrop，支持文件/文件夹/剪贴板传输，E2E 加密。

## Workspace 结构

```
rust-air/
├── core/          # rust-air-core 核心库
│   ├── src/
│   │   ├── archive.rs          # tar+zstd 流式归档/解包
│   │   ├── clipboard.rs        # 剪贴板读写 (arboard)
│   │   ├── clipboard_history.rs # 剪贴板历史
│   │   ├── crypto.rs           # ChaCha20-Poly1305 AEAD 加解密
│   │   ├── discovery.rs        # mDNS-SD 设备发现
│   │   ├── http_qr.rs          # HTTP 下载 + QR 码
│   │   ├── proto.rs            # 协议常量和类型定义
│   │   ├── sync_vault.rs       # 文件同步引擎
│   │   └── transfer.rs         # 传输引擎（发送/接收/进度）
│   └── tests/                  # 集成测试
├── cli/           # rust-air-cli 命令行工具
│   └── src/main.rs             # send/receive/scan/send-clip
├── tauri-app/     # Tauri 桌面应用
│   ├── src/                    # Vue 前端
│   └── src-tauri/src/
│       ├── commands.rs         # Tauri IPC 命令
│       ├── lib.rs              # Tauri 插件注册
│       └── main.rs             # 入口
└── Cargo.toml     # Workspace 定义
```

## 核心依赖

| 用途 | Crate | 版本 |
|------|-------|------|
| 异步运行时 | tokio (full) | 1 |
| AEAD 加密 | chacha20poly1305 | 0.10 |
| 哈希 | sha2 | 0.10 |
| 压缩 | zstd | 0.13 |
| 归档 | tar | 0.4 |
| 并行 | rayon | 1 |
| 设备发现 | mdns-sd | 0.19 |
| 文件监控 | notify | 6 |
| 桌面框架 | tauri | 2 |

## 数据流

```
发送文件:
  用户选择文件 → send_path() → [header] → [resume handshake] → [AEAD chunks] → [EOF] → [SHA-256]

发送文件夹:
  walk_dir() → compress_entries → ChannelWriter → mpsc → ErrorAwareReader → stream_encrypted_hash → TCP

接收:
  TCP → recv_header() → Decryptor → write to disk / unpack archive → verify SHA-256

设备发现:
  mDNS-SD register_self() ←→ browse_devices_sync()
```

## 构建命令

```bash
cargo build                              # 全部
cargo build --package rust-air-core      # 仅核心库
cargo build --package rust-air-cli       # 仅 CLI
cargo test --package rust-air-core --tests  # 测试（排除 examples）
```
