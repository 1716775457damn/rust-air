//! Auto-update: check GitHub Releases, download installer, run silently.
//!
//! Flow:
//!   `check_update()`  → returns `UpdateInfo` { version, url, size } or None
//!   `download_and_install(url)` → streams download, emits "update-progress",
//!                               then launches installer and exits the app.
//!
//! Settings (persisted in data_local_dir/rust-air/update-settings.json):
//!   `auto_check`:   check on startup (default true)
//!   `auto_install`: download+install silently when update found (default false)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_API: &str =
    "https://api.github.com/repos/1716775457damn/rust-air/releases/latest";
const GITHUB_RELEASES_LATEST: &str =
    "https://github.com/1716775457damn/rust-air/releases/latest";

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
    let Some(record_path) = cleanup_record_path() else { return };

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

#[tauri::command]
pub fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}

/// Check GitHub for a newer release. Returns Some(UpdateInfo) if one exists.
#[tauri::command]
pub async fn check_update() -> Result<Option<UpdateInfo>, String> {
    let release = fetch_latest_release().await.map_err(|e| {
        format!(
            "检查更新失败：无法获取 GitHub Release 信息（可能是网络、代理或 GitHub API 限制）。原始错误：{}",
            e
        )
    })?;

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

    let resp = client
        .get(GITHUB_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?;

    let status = resp.status();
    let body = resp.text().await?;
    if status.is_success() {
        match serde_json::from_str::<GhRelease>(&body) {
            Ok(release) => return Ok(release),
            Err(err) => {
                eprintln!(
                    "warn: GitHub API decode failed, falling back to HTML release page: {err}"
                );
            }
        }
    } else {
        eprintln!(
            "warn: GitHub API returned status {}, falling back to HTML release page",
            status
        );
    }

    fetch_latest_release_from_html(&client).await
}

async fn fetch_latest_release_from_html(client: &reqwest::Client) -> anyhow::Result<GhRelease> {
    use anyhow::{anyhow, Context};
    use regex::Regex;

    let resp = client.get(GITHUB_RELEASES_LATEST).send().await?;
    let final_url = resp.url().clone();
    let html = resp.text().await?;

    let tag_name = final_url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .or_else(|| {
            Regex::new(r#"/releases/tag/([^\"?#]+)"#)
                .ok()
                .and_then(|re| re.captures(&html))
                .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        })
        .ok_or_else(|| anyhow!("unable to determine latest release tag from GitHub HTML page"))?;

    let href_re = Regex::new(r#"href=\"([^\"]+/releases/download/[^\"]+)\""#)
        .context("compile asset href regex")?;
    let body_re = Regex::new(r#"(?s)<div[^>]*data-test-selector=\"body-content\"[^>]*>(.*?)</div>"#)
        .context("compile release body regex")?;
    let strip_tags_re = Regex::new(r"<[^>]+>").context("compile strip tags regex")?;

    let mut assets = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for caps in href_re.captures_iter(&html) {
        let Some(raw_href) = caps.get(1).map(|m| m.as_str()) else { continue };
        let href = raw_href.replace("&amp;", "&");
        let absolute = if href.starts_with("http://") || href.starts_with("https://") {
            href.clone()
        } else {
            format!("https://github.com{href}")
        };
        if !seen.insert(absolute.clone()) {
            continue;
        }
        let name = absolute.rsplit('/').next().unwrap_or_default().to_string();
        if name.is_empty() {
            continue;
        }
        assets.push(GhAsset {
            name,
            browser_download_url: absolute,
            size: 0,
        });
    }

    let release_notes = body_re
        .captures(&html)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        .map(|body| strip_tags_re.replace_all(&body, " ").to_string())
        .map(|body| {
            body.replace("&amp;", "&")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&nbsp;", " ")
        })
        .map(|body| body.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();

    Ok(GhRelease {
        tag_name,
        body: Some(release_notes),
        assets,
    })
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

pub fn apply_download_proxy(url: &str) -> String {
    format!("https://xgn.io/{url}")
}

pub fn expected_download_size(total: u64, content_length: Option<u64>) -> u64 {
    content_length.unwrap_or(total)
}

#[cfg(target_os = "windows")]
pub fn windows_installer_command(path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let path_str = path.to_string_lossy();
    if ext.eq_ignore_ascii_case("msi") {
        format!(
            "ping -n 5 127.0.0.1 >nul & msiexec /i \"{}\" /qb REINSTALL=ALL REINSTALLMODE=vomus",
            path_str
        )
    } else {
        format!("ping -n 3 127.0.0.1 >nul & \"{}\"", path_str)
    }
}

#[cfg(not(target_os = "windows"))]
pub fn windows_installer_command(path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let path_str = path.to_string_lossy();
    if ext.eq_ignore_ascii_case("msi") {
        format!(
            "ping -n 5 127.0.0.1 >nul & msiexec /i \"{}\" /qb REINSTALL=ALL REINSTALLMODE=vomus",
            path_str
        )
    } else {
        format!("ping -n 3 127.0.0.1 >nul & \"{}\"", path_str)
    }
}

async fn download_installer(url: &str, total: u64, app: &AppHandle) -> anyhow::Result<PathBuf> {
    let tmp = std::env::temp_dir().join(
        url.rsplit('/').next().unwrap_or("rust-air-update.msi")
    );

    // Use xgn.io proxy to accelerate GitHub release downloads (especially for CN users).
    // Original: https://github.com/user/repo/releases/download/vX/file.msi
    // Proxied:  https://xgn.io/https://github.com/user/repo/releases/download/vX/file.msi
    let proxied_url = apply_download_proxy(url);

    let client = reqwest::Client::builder()
        .user_agent(format!("rust-air/{CURRENT_VERSION}"))
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    // Try proxied URL first, fall back to direct GitHub URL on failure.
    let mut resp = match client.get(&proxied_url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => {
            eprintln!("info: xgn.io proxy unavailable, falling back to direct download");
            client.get(url).send().await?
        }
    };

    anyhow::ensure!(
        resp.status().is_success(),
        "download request failed with status {}",
        resp.status()
    );

    let expected_total = expected_download_size(total, resp.content_length());
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut downloaded = 0u64;
    let mut last_emit = std::time::Instant::now();

    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if last_emit.elapsed().as_millis() >= 100 {
            app.emit("update-progress", UpdateProgress {
                downloaded, total: expected_total, done: false,
            }).ok();
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().await?;

    // Verify download completeness — reject truncated files
    if expected_total > 0 && downloaded != expected_total {
        let _ = tokio::fs::remove_file(&tmp).await;
        anyhow::bail!(
            "download incomplete: got {} bytes, expected {} bytes",
            downloaded, expected_total
        );
    }
    // Sanity check: MSI files must be at least 1 KB
    if downloaded < 1024 {
        let _ = tokio::fs::remove_file(&tmp).await;
        anyhow::bail!(
            "downloaded file too small ({} bytes) — likely not a valid installer",
            downloaded
        );
    }

    app.emit("update-progress", UpdateProgress { downloaded, total: expected_total, done: true }).ok();
    Ok(tmp)
}

#[cfg(target_os = "windows")]
fn spawn_windows_detached(command_line: &str) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new("cmd")
        .args(["/C", command_line])
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()?;

    Ok(())
}

fn launch_installer(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let command_line = windows_installer_command(path);
        if ext.eq_ignore_ascii_case("msi") {
            let msi_path = path.to_string_lossy().to_string();
            eprintln!(
                "info: Launching MSI installer with version rollback support: {}",
                msi_path
            );
            spawn_windows_detached(&command_line)?;
        } else {
            spawn_windows_detached(&command_line)?;
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
