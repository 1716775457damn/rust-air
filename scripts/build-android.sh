#!/usr/bin/env bash
# rust-air Android 一键打包脚本
# 用法: bash scripts/build-android.sh [--debug] [--release] [--init]
# 前提: 已安装 Android SDK, NDK r25+, JDK 17, Rust Android targets
set -euo pipefail

DEBUG=false
INIT=false
for arg in "$@"; do
  case $arg in
    --debug)   DEBUG=true ;;
    --release) DEBUG=false ;;
    --init)    INIT=true ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAURI_DIR="$ROOT/tauri-app"

echo "=== rust-air Android Build ==="

# 1. 环境检查
echo -e "\n[1/5] 检查 Android 构建环境..."
for cmd in cargo node npm java; do
  if ! command -v "$cmd" &>/dev/null; then
    echo "❌ 缺少: $cmd"; exit 1
  fi
done

if [ -z "${ANDROID_HOME:-}" ]; then
  echo "❌ ANDROID_HOME 未设置"; exit 1
fi
if [ -z "${NDK_HOME:-}" ] && [ -z "${ANDROID_NDK_HOME:-}" ]; then
  echo "⚠️  NDK_HOME 未设置，尝试自动检测..."
  NDK_HOME="$ANDROID_HOME/ndk/$(ls "$ANDROID_HOME/ndk/" 2>/dev/null | sort -V | tail -1)"
  if [ ! -d "$NDK_HOME" ]; then
    echo "❌ 未找到 NDK，请设置 NDK_HOME"; exit 1
  fi
  export NDK_HOME
  echo "  NDK: $NDK_HOME"
fi

# 检查 Rust Android targets
echo "  检查 Rust targets..."
TARGETS="aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android"
MISSING_TARGETS=""
for t in $TARGETS; do
  if ! rustup target list --installed | grep -q "$t"; then
    MISSING_TARGETS="$MISSING_TARGETS $t"
  fi
done
if [ -n "$MISSING_TARGETS" ]; then
  echo "  安装缺少的 targets:$MISSING_TARGETS"
  rustup target add $MISSING_TARGETS
fi

echo "  cargo: $(cargo --version)"
echo "  java:  $(java --version 2>&1 | head -1)"
echo "  ANDROID_HOME: $ANDROID_HOME"

# 2. 安装前端依赖
echo -e "\n[2/5] 安装前端依赖..."
cd "$TAURI_DIR"
npm install

# 3. 初始化 Android 项目 (首次)
if $INIT || [ ! -d "src-tauri/gen/android" ]; then
  echo -e "\n[3/5] 初始化 Android 项目..."
  npx tauri android init
else
  echo -e "\n[3/5] Android 项目已存在，跳过初始化"
fi

# 4. 构建
if $DEBUG; then
  echo -e "\n[4/5] 构建 Android Debug APK..."
  npx tauri android build --debug
else
  echo -e "\n[4/5] 构建 Android Release APK..."
  npx tauri android build
fi

# 5. 输出结果
echo -e "\n[5/5] 构建完成! ✅"
APK_DIR="$TAURI_DIR/src-tauri/gen/android/app/build/outputs/apk"
if [ -d "$APK_DIR" ]; then
  echo -e "\nAPK 产物:"
  find "$APK_DIR" -name "*.apk" -exec sh -c 'echo "  {} ($(du -h "{}" | cut -f1))"' \;
fi

AAB_DIR="$TAURI_DIR/src-tauri/gen/android/app/build/outputs/bundle"
if [ -d "$AAB_DIR" ]; then
  echo -e "\nAAB 产物:"
  find "$AAB_DIR" -name "*.aab" -exec sh -c 'echo "  {} ($(du -h "{}" | cut -f1))"' \;
fi
