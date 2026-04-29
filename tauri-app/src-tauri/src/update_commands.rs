//! Auto-update: check GitHub Releases, download installer, run silently.
//!
//! Flow:
//!   check_update()  → returns UpdateInfo { version, url, size } or None
//!   download_and_install(url) → streams download, emits "update-progress",
//!                               then launches installer and exits the app.
//!
//! Settings (persisted in data_local_dir/rust-air/update-settings.json):
//!   auto_check:   check on startup (default true)
//!   auto_install: download+install silently when update found (default false)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_API: &str =
    "https://api.github.com/repos/1716775457damn/rust-air/releases/latest";

// ── Settings ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSettings {
    pub auto_check:   bool,
    pub auto_install: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { auto_check: true, auto_install: false }
    }
}

impl UpdateSettings {
    pub fn load() -> Self {
        settings_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        if let Some(p) = settings_path() {
            if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("rust-air").join("update-settings.json"))
}

// ── Cleanup record ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct CleanupRecord {
    installer_path: String,
}

fn cleanup_record_path() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("rust-air").join("update-cleanup.json"))
}

/// Clean up old update installer files left over from previous updates.
///
/// Called synchronously during `setup` — before the auto-update check.
/// Every file-system operation is wrapped in a silent ignore so that a
/// failure here can never prevent the application from starting.
pub fn cleanup_old_update_files() {
    // 1. Read the cleanup record (if any) and try to delete the recorded file.
    let record_path = match cleanup_record_path() {
        Some(p) => p,
        None => return,
    };

    if let Ok(json) = std::fs::read_to_string(&record_path) {
        if let Ok(record) = serde_json::from_str::<CleanupRecord>(&json) {
            let p = PathBuf::from(&record.installer_path);
            let _ = std::fs::remove_file(&p); // silent
        }
    }

    // 2. Scan temp dir for any rust-air installer files that are NOT the
    //    current version and delete them.
    let temp = std::env::temp_dir();
    if let Ok(entries) = std::fs::read_dir(&temp) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("rust-air") {
                continue;
            }
            let dominated = name_str.ends_with(".msi") || name_str.ends_with("-setup.exe");
            if !dominated {
                continue;
            }
            // Skip the installer for the currently running version so we
            // never delete a file that might still be in use.
            if name_str.contains(CURRENT_VERSION) {
                continue;
            }
            let _ = std::fs::remove_file(entry.path()); // silent
        }
    }

    // 3. Remove the record file itself (best-effort).
    let _ = std::fs::remove_file(&record_path);
}

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version:      String,
    pub url:          String,
    pub size:         u64,
    pub release_notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateProgress {
    pub downloaded: u64,
    pub total:      u64,
    pub done:       bool,
}

// ── GitHub API response (minimal) ─────────────────────────────────────────────

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    body:     Option<String>,
    assets:   Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name:                 String,
    browser_download_url: String,
    size:                 u64,
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_update_settings() -> UpdateSettings {
    UpdateSettings::load()
}

#[tauri::command]
pub fn save_update_settings(settings: UpdateSettings) {
    settings.save();
}

/// Check GitHub for a newer release. Returns Some(UpdateInfo) if one exists.
#[tauri::command]
pub async fn check_update() -> Result<Option<UpdateInfo>, String> {
    let release = fetch_latest_release().await.map_err(|e| e.to_string())?;

    let remote = release.tag_name.trim_start_matches('v');
    if !is_newer(remote, CURRENT_VERSION) {
        return Ok(None);
    }

    let asset = pick_asset(&release.assets).ok_or("no suitable installer found for this platform")?;

    Ok(Some(UpdateInfo {
        version:      release.tag_name.clone(),
        url:          asset.browser_download_url.clone(),
        size:         asset.size,
        release_notes: release.body.unwrap_or_default(),
    }))
}

/// Download the installer and launch it, then quit the app.
/// Emits "update-progress" events during download.
#[tauri::command]
pub async fn download_and_install(
    url:  String,
    size: u64,
    app:  AppHandle,
) -> Result<(), String> {
    let path = download_installer(&url, size, &app).await.map_err(|e| e.to_string())?;

    // Record the installer path so the next launch can clean it up.
    if let Some(rec_path) = cleanup_record_path() {
        let record = CleanupRecord {
            installer_path: path.to_string_lossy().to_string(),
        };
        if let Some(d) = rec_path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(json) = serde_json::to_string_pretty(&record) {
            let _ = std::fs::write(&rec_path, json);
        }
    }

    launch_installer(&path).map_err(|e| e.to_string())?;
    app.exit(0);
    Ok(())
}

// ── Internals ─────────────────────────────────────────────────────────────────

async fn fetch_latest_release() -> anyhow::Result<GhRelease> {
    let client = reqwest::Client::builder()
        .user_agent(format!("rust-air/{CURRENT_VERSION}"))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let release = client.get(GITHUB_API).send().await?.json::<GhRelease>().await?;
    Ok(release)
}

/// Pick the right installer asset for the current platform.
fn pick_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    #[cfg(target_os = "windows")]
    let preferred = &["_x64_en-US.msi", ".msi", "_x64-setup.exe"];
    #[cfg(target_os = "macos")]
    let preferred: &[&str] = if cfg!(target_arch = "aarch64") {
        &["_aarch64.dmg", ".dmg"]
    } else {
        &["_x64.dmg", ".dmg"]
    };
    #[cfg(target_os = "linux")]
    let preferred = &["_amd64.deb", ".AppImage"];

    for suffix in preferred {
        if let Some(a) = assets.iter().find(|a| a.name.ends_with(suffix)) {
            return Some(a);
        }
    }
    None
}

/// Simple semver comparison: returns true if `remote` > `local`.
/// Handles "1.2.3" format; ignores pre-release suffixes.
fn is_newer(remote: &str, local: &str) -> bool {
    fn parse(s: &str) -> (u32, u32, u32) {
        let mut p = s.splitn(4, '.').map(|x| x.parse::<u32>().unwrap_or(0));
        (p.next().unwrap_or(0), p.next().unwrap_or(0), p.next().unwrap_or(0))
    }
    parse(remote) > parse(local)
}

async fn download_installer(url: &str, total: u64, app: &AppHandle) -> anyhow::Result<PathBuf> {
    let tmp = std::env::temp_dir().join(
        url.rsplit('/').next().unwrap_or("rust-air-update.msi")
    );

    let client = reqwest::Client::builder()
        .user_agent(format!("rust-air/{CURRENT_VERSION}"))
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    let mut resp = client.get(url).send().await?;
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut downloaded = 0u64;
    let mut last_emit = std::time::Instant::now();

    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if last_emit.elapsed().as_millis() >= 100 {
            app.emit("update-progress", UpdateProgress {
                downloaded, total, done: false,
            }).ok();
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().await?;
    app.emit("update-progress", UpdateProgress { downloaded, total, done: true }).ok();
    Ok(tmp)
}

fn launch_installer(path: &PathBuf) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Use a small delay script so the app process fully exits before
        // the installer starts — avoids file-lock conflicts on Windows.
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("msi") {
            // msiexec /i <path> /qb REINSTALLMODE=omus
            // The REINSTALLMODE ensures all files are overwritten even if
            // the version looks the same to Windows Installer.
            // We use cmd /C with a ping delay to let the app exit first.
            let msi_path = path.to_string_lossy().to_string();
            std::process::Command::new("cmd")
                .args([
                    "/C",
                    &format!(
                        "ping -n 2 127.0.0.1 >nul && msiexec /i \"{}\" /qb REINSTALLMODE=omus",
                        msi_path
                    ),
                ])
                .spawn()?;
        } else {
            // exe installer: also delay slightly
            let exe_path = path.to_string_lossy().to_string();
            std::process::Command::new("cmd")
                .args([
                    "/C",
                    &format!("ping -n 2 127.0.0.1 >nul && \"{}\"", exe_path),
                ])
                .spawn()?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("deb") {
            std::process::Command::new("pkexec")
                .args(["dpkg", "-i", &path.to_string_lossy()])
                .spawn()?;
        } else {
            // AppImage: make executable and run
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
            std::process::Command::new(path).spawn()?;
        }
    }
    Ok(())
}
