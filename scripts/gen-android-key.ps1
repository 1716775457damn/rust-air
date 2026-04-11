# gen-android-key.ps1
# Reusable Android signing keystore generator
#
# Usage:
#   .\scripts\gen-android-key.ps1 -AppName "my-app"
#   .\scripts\gen-android-key.ps1 -AppName "my-app" -Alias "my-alias" -Password "mypass123"
#
# Output:
#   <AppName>-release.keystore    binary keystore  (BACK THIS UP!)
#   <AppName>-keystore-b64.txt    base64 string    (paste into GitHub Secrets)
#   <AppName>-secrets.txt         summary of all secret values

param(
    [Parameter(Mandatory)]
    [string]$AppName,

    [string]$Alias    = "",
    [string]$Password = "",
    [string]$OutDir   = (Get-Location).Path,
    [string]$Country  = "CN",
    [string]$State    = "Beijing",
    [string]$City     = "Beijing",
    [string]$Org      = "",
    [int]$Validity    = 10000
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if (-not $Alias) { $Alias = $AppName }
if (-not $Org)   { $Org   = $AppName }

# ── 1. Locate keytool ────────────────────────────────────────────────────────

function Find-Keytool {
    # 1. Bundled jdk-keytool/ next to this script (works wherever the folder is copied)
    $scriptDir = Split-Path -Parent $MyInvocation.ScriptName
    $bundled   = Join-Path $scriptDir "jdk-keytool\bin\keytool.exe"
    if (Test-Path $bundled) { return $bundled }

    # 2. System PATH
    $kt = Get-Command keytool -ErrorAction SilentlyContinue
    if ($kt) { return $kt.Source }

    # 3. Common JDK install locations
    $candidates = @(
        "C:\Program Files\Java\*\bin\keytool.exe",
        "C:\Program Files\Eclipse Adoptium\*\bin\keytool.exe",
        "C:\Program Files\Microsoft\*\bin\keytool.exe",
        "$env:LOCALAPPDATA\Programs\Eclipse Adoptium\*\bin\keytool.exe"
    )
    foreach ($pattern in $candidates) {
        $found = Get-Item $pattern -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) { return $found.FullName }
    }

    # 4. JAVA_HOME env var
    if ($env:JAVA_HOME) {
        $kt = Join-Path $env:JAVA_HOME "bin\keytool.exe"
        if (Test-Path $kt) { return $kt }
    }
    return $null
}

$keytool = Find-Keytool
if (-not $keytool) {
    Write-Error "keytool not found. Install JDK from https://adoptium.net/"
    exit 1
}
Write-Host "[OK] keytool: $keytool" -ForegroundColor Green

# ── 2. Generate random password if not provided ──────────────────────────────

if (-not $Password) {
    $chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#%^&*"
    $rng   = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $bytes = New-Object byte[] 24
    $rng.GetBytes($bytes)
    $Password = -join ($bytes | ForEach-Object { $chars[$_ % $chars.Length] })
}

# ── 3. File paths ─────────────────────────────────────────────────────────────

$keystoreFile = Join-Path $OutDir "$AppName-release.keystore"
$b64File      = Join-Path $OutDir "$AppName-keystore-b64.txt"
$secretsFile  = Join-Path $OutDir "$AppName-secrets.txt"

if (Test-Path $keystoreFile) {
    Write-Warning "Overwriting existing $keystoreFile"
    Remove-Item $keystoreFile -Force
}

# ── 4. Generate keystore ──────────────────────────────────────────────────────

Write-Host "[..] Generating keystore..." -ForegroundColor Cyan

$dname = "CN=$AppName, OU=Dev, O=$Org, L=$City, ST=$State, C=$Country"

$oldPref = $ErrorActionPreference
$ErrorActionPreference = "Continue"
& $keytool -genkey -v `
    -keystore  $keystoreFile `
    -alias     $Alias `
    -keyalg    RSA `
    -keysize   2048 `
    -validity  $Validity `
    -dname     $dname `
    -storepass $Password `
    -keypass   $Password 2>&1 | Out-Null
$ErrorActionPreference = $oldPref

if (-not (Test-Path $keystoreFile)) {
    Write-Error "Keystore generation failed."
    exit 1
}

$size = (Get-Item $keystoreFile).Length
Write-Host "[OK] Keystore created: $keystoreFile ($size bytes)" -ForegroundColor Green

# ── 5. Convert to Base64 ──────────────────────────────────────────────────────

Write-Host "[..] Converting to Base64..." -ForegroundColor Cyan
$b64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($keystoreFile))
[IO.File]::WriteAllText($b64File, $b64)
Write-Host "[OK] Base64 saved: $b64File" -ForegroundColor Green

# ── 6. Write secrets summary file ────────────────────────────────────────────

$ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
$summary = @"
# GitHub Secrets summary for: $AppName
# Generated: $ts
#
# Go to: https://github.com/<user>/<repo>/settings/secrets/actions
# Click "New repository secret" and add each row below.
#
# WARNING: Do NOT commit this file to Git!

Secret Name                  Value
---------------------------  ----------------------------------------
ANDROID_KEYSTORE             (paste full content of $AppName-keystore-b64.txt)
ANDROID_KEY_ALIAS            $Alias
ANDROID_STORE_PASSWORD       $Password
ANDROID_KEY_PASSWORD         $Password

# Keystore details
File:      $keystoreFile
Alias:     $Alias
Password:  $Password
Validity:  $Validity days
Tool:      $keytool
"@

[IO.File]::WriteAllText($secretsFile, $summary)

# ── 7. Print summary ──────────────────────────────────────────────────────────

Write-Host ""
Write-Host "============================================================" -ForegroundColor Yellow
Write-Host "  Android signing key ready for: $AppName" -ForegroundColor Yellow
Write-Host "============================================================" -ForegroundColor Yellow
Write-Host ""
Write-Host "Files generated:" -ForegroundColor White
Write-Host "  $keystoreFile" -ForegroundColor Cyan
Write-Host "  $b64File" -ForegroundColor Cyan
Write-Host "  $secretsFile" -ForegroundColor Cyan
Write-Host ""
Write-Host "GitHub Secrets to configure:" -ForegroundColor White
Write-Host "  ANDROID_KEYSTORE        -> content of $AppName-keystore-b64.txt" -ForegroundColor Cyan
Write-Host "  ANDROID_KEY_ALIAS       -> $Alias" -ForegroundColor Cyan
Write-Host "  ANDROID_STORE_PASSWORD  -> $Password" -ForegroundColor Cyan
Write-Host "  ANDROID_KEY_PASSWORD    -> $Password" -ForegroundColor Cyan
Write-Host ""
Write-Host "IMPORTANT:" -ForegroundColor Red
Write-Host "  1. Back up $AppName-release.keystore immediately" -ForegroundColor Red
Write-Host "  2. Save the password in a password manager" -ForegroundColor Red
Write-Host "  3. These files are in .gitignore and will NOT be committed" -ForegroundColor Red
Write-Host ""
