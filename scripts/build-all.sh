#!/usr/bin/env bash
# rust-air 全平台一键打包脚本
# 用法: bash scripts/build-all.sh [--debug]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ARGS="$@"

echo "========================================="
echo "  rust-air 全平台打包"
echo "========================================="

echo -e "\n>>> 桌面版打包..."
bash "$SCRIPT_DIR/build-desktop.sh" $ARGS

if [ -n "${ANDROID_HOME:-}" ]; then
  echo -e "\n>>> Android 版打包..."
  bash "$SCRIPT_DIR/build-android.sh" $ARGS
else
  echo -e "\n>>> 跳过 Android (ANDROID_HOME 未设置)"
fi

echo -e "\n========================================="
echo "  全部完成! 🎉"
echo "========================================="
