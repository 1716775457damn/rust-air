#!/usr/bin/env pwsh
# rust-air 桌面版一键打包脚本 (Windows)
# 用法: .\scripts\build-desktop.ps1 [-Release] [-Debug] [-Verbose]

param(
    [switch]$Release,
    [switch]$Debug,
    [switch]$SkipFrontend
)

$ErrorActionPreference = "Stop"
$ROOT = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $ROOT) { $ROOT = (Get-Location).Path }

$TAURI_DIR = Join-Path $ROOT "tauri-app"

Write-Host "=== rust-air Desktop Build ===" -ForegroundColor Cyan
Write-Host "Project: $TAURI_DIR"

# 1. 检查依赖
Write-Host "`n[1/4] 检查构建依赖..." -ForegroundColor Yellow
$missing = @()
if (-not (Get-Command "cargo" -ErrorAction SilentlyContinue)) { $missing += "cargo (rustup)" }
if (-not (Get-Command "node" -ErrorAction SilentlyContinue)) { $missing += "node" }
if (-not (Get-Command "npm" -ErrorAction SilentlyContinue)) { $missing += "npm" }
if ($missing.Count -gt 0) {
    Write-Host "缺少依赖: $($missing -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host "  cargo: $(cargo --version)" -ForegroundColor Gray
Write-Host "  node:  $(node --version)" -ForegroundColor Gray

# 2. 安装前端依赖
Write-Host "`n[2/4] 安装前端依赖..." -ForegroundColor Yellow
Push-Location $TAURI_DIR
npm install
if ($LASTEXITCODE -ne 0) { Pop-Location; exit 1 }
Pop-Location

# 3. 构建
$buildArgs = @()
if ($Debug) {
    Write-Host "`n[3/4] 构建 Debug 版本..." -ForegroundColor Yellow
    $buildArgs += "--debug"
} else {
    Write-Host "`n[3/4] 构建 Release 版本..." -ForegroundColor Yellow
}

Push-Location $TAURI_DIR
npx tauri build @buildArgs
if ($LASTEXITCODE -ne 0) { Pop-Location; exit 1 }
Pop-Location

# 4. 输出结果
Write-Host "`n[4/4] 构建完成!" -ForegroundColor Green
$bundleDir = Join-Path $TAURI_DIR "src-tauri\target\release\bundle"
if ($Debug) { $bundleDir = Join-Path $TAURI_DIR "src-tauri\target\debug\bundle" }

if (Test-Path $bundleDir) {
    Write-Host "`n产物目录:" -ForegroundColor Cyan
    Get-ChildItem -Path $bundleDir -Recurse -File | Where-Object {
        $_.Extension -in ".exe", ".msi", ".dmg", ".AppImage", ".deb"
    } | ForEach-Object {
        $size = [math]::Round($_.Length / 1MB, 2)
        Write-Host "  $($_.FullName) ($size MB)" -ForegroundColor White
    }
}
