#!/usr/bin/env bash
# rust-air 桌面版一键打包脚本 (Linux / macOS)
# 用法: bash scripts/build-desktop.sh [--debug] [--release]
set -euo pipefail

DEBUG=false
for arg in "$@"; do
  case $arg in
    --debug) DEBUG=true ;;
    --release) DEBUG=false ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAURI_DIR="$ROOT/tauri-app"

echo "=== rust-air Desktop Build ==="
echo "Project: $TAURI_DIR"

# 1. 检查依赖
echo -e "\n[1/4] 检查构建依赖..."
for cmd in cargo node npm; do
  if ! command -v "$cmd" &>/dev/null; then
    echo "❌ 缺少: $cmd"
    exit 1
  fi
done
echo "  cargo: $(cargo --version)"
echo "  node:  $(node --version)"

# 2. 安装前端依赖
echo -e "\n[2/4] 安装前端依赖..."
cd "$TAURI_DIR"
npm install

# 3. 构建
if $DEBUG; then
  echo -e "\n[3/4] 构建 Debug 版本..."
  npx tauri build --debug
else
  echo -e "\n[3/4] 构建 Release 版本..."
  npx tauri build
fi

# 4. 输出结果
echo -e "\n[4/4] 构建完成! ✅"
BUNDLE_DIR="$TAURI_DIR/src-tauri/target/release/bundle"
$DEBUG && BUNDLE_DIR="$TAURI_DIR/src-tauri/target/debug/bundle"

if [ -d "$BUNDLE_DIR" ]; then
  echo -e "\n产物:"
  find "$BUNDLE_DIR" -type f \( -name "*.exe" -o -name "*.msi" -o -name "*.dmg" \
    -o -name "*.AppImage" -o -name "*.deb" -o -name "*.rpm" \) \
    -exec sh -c 'echo "  {} ($(du -h "{}" | cut -f1))"' \;
fi
