---
name: rust-air-testing
description: "rust-air 测试规范。当编写测试、运行 cargo test、或涉及测试策略时使用此 skill。"
inclusion: fileMatch
fileMatchPattern: "**/tests/**/*.rs,**/*_test.rs,**/*_tests.rs"
---

# rust-air 测试规范

## 项目结构

```
core/
  src/           # 库代码
  tests/         # 集成测试
  examples/      # 示例（有已知编译问题，忽略）
```

## 测试命令

```bash
# 运行 core 所有测试（排除 examples）
cargo test --package rust-air-core --tests

# 运行单个测试文件
cargo test --package rust-air-core --test archive_bug_test

# 运行匹配名称的测试
cargo test --package rust-air-core --tests -- test_name_pattern
```

## 测试编写规则

1. **异步测试** — 使用 `#[tokio::test]`，导入 `rust_air_core::archive` 等公开模块
2. **临时目录** — 每个测试创建独立 temp dir，测试结束后清理
3. **不要用 `#[test]` 测试 async 代码** — 会 panic
4. **Windows 兼容** — 不要依赖 Unix 权限 (chmod 0o000)，用删除文件模拟不可读
5. **包名** — `rust-air-core`（Cargo.toml 中的 name），Rust 模块名 `rust_air_core`

## 测试分类

### Bug 条件测试
- 验证 bug 存在：测试在未修复代码上应该 FAIL
- 验证修复有效：测试在修复后应该 PASS
- 不要在测试失败时尝试修复测试本身

### 保留性测试
- 验证修复不引入回归
- 在修复前后都应该 PASS
- 覆盖: 单文件归档、多文件归档、嵌套目录、空目录、walk_dir 大小计算

### 归档测试模式
```rust
// 创建 → 归档 → 解包 → 验证
let compressed = archive_to_bytes(&src_dir).await;
archive::unpack_archive_sync(Cursor::new(&compressed), &dest_dir)?;
// 验证文件内容一致
```

## 已知问题
- `core/examples/clip_test.rs` 有编译错误（`start_monitor` 未导出），与测试无关，忽略
- 运行 `cargo test --package rust-air-core`（不加 `--tests`）会尝试编译 examples 并失败
