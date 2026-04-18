//! Tauri IPC command handlers — v3 (scan-and-click, no pre-shared key).

use rust_air_core::{
    clipboard,
    discovery::{self, ServiceHandle},
    proto::DeviceInfo,
    transfer,
};
use std::{path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Emitter, State};
use tokio::{net::{TcpListener, TcpStream}, sync::oneshot};

// ── App state ─────────────────────────────────────────────────────────────────

pub struct AppState {
    /// mDNS self-registration handle (kept alive for the app lifetime).
    svc_handle:  Mutex<Option<ServiceHandle>>,
    /// Cancel token for the current outgoing send task.
    send_cancel: Mutex<Option<oneshot::Sender<()>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            svc_handle:  Mutex::new(None),
            send_cancel: Mutex::new(None),
        }
    }
}

impl AppState {
    pub fn set_svc(&self, h: ServiceHandle) {
        *self.svc_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
    }
    fn set_send_cancel(&self, tx: oneshot::Sender<()>) {
        *self.send_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }
    fn abort_send(&self) {
        if let Some(tx) = self.send_cancel.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
    }
}

// ── Startup ───────────────────────────────────────────────────────────────────

/// Called once on app start: bind a listener, register on mDNS, start accepting.
/// Incoming transfers are handled automatically; progress emitted as recv-* events.
#[tauri::command]
pub async fn start_listener(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<u16, String> {
    let listener = TcpListener::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
    let port     = listener.local_addr().map_err(|e| e.to_string())?.port();

    let device_name = device_name();
    let handle = discovery::register_self(port, &device_name).map_err(|e| e.to_string())?;
    state.set_svc(handle);

    // Accept loop — runs for the lifetime of the app.
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let app2 = app.clone();
                    tokio::spawn(async move {
                        app2.emit("recv-peer-connected", peer.to_string()).ok();
                        let out = default_download_dir();
                        let app3 = app2.clone();
                        match transfer::receive_to_disk(stream, &out, move |ev| {
                            app3.emit("recv-progress", &ev).ok();
                        }).await {
                            Ok(p)  => { app2.emit("recv-done", p.to_string_lossy().to_string()).ok(); }
                            Err(e) => { app2.emit("recv-error", e.to_string()).ok(); }
                        }
                    });
                }
                Err(_) => break,
            }
        }
    });

    Ok(port)
}

// ── Send ──────────────────────────────────────────────────────────────────────

/// Send a file/folder to a peer by its "ip:port" address string.
#[tauri::command]
pub async fn send_to(
    path: String,
    addr: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    state.abort_send();

    let path = PathBuf::from(&path);
    if !path.exists() {
        return Err(format!("path not found: {}", path.display()));
    }

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    state.set_send_cancel(cancel_tx);

    let app_clone = app.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = cancel_rx => {}
            result = do_send(path, addr, app_clone.clone()) => {
                match result {
                    Ok(_)  => { app_clone.emit("send-done", "").ok(); }
                    Err(e) => { app_clone.emit("send-error", e.to_string()).ok(); }
                }
            }
        }
    });
    Ok(())
}

async fn do_send(path: PathBuf, addr: String, app: AppHandle) -> anyhow::Result<()> {
    let stream = TcpStream::connect(&addr).await?;
    app.emit("send-peer-connected", &addr).ok();
    let app2 = app.clone();
    transfer::send_path(stream, &path, move |ev| {
        app2.emit("send-progress", &ev).ok();
    }).await
}

#[tauri::command]
pub fn cancel_send(state: State<'_, AppState>) {
    state.abort_send();
}

// ── Scan ──────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn scan_devices(app: AppHandle) -> Result<(), String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DeviceInfo>(32);

    // browse_devices_sync starts a background thread and returns a handle.
    // Dropping the handle shuts down the daemon, which unblocks receiver.recv()
    // in the thread immediately — no quiet-LAN deadlock.
    let handle = discovery::browse_devices_sync(tx).map_err(|e| e.to_string())?;

    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(dev)) => { app.emit("device-found", &dev).ok(); }
                _ => break,
            }
        }
        drop(handle); // shuts down daemon → thread exits cleanly
    });
    Ok(())
}

// ── Clipboard ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn read_clipboard() -> Result<String, String> {
    clipboard::read().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn write_clipboard(text: String) -> Result<(), String> {
    clipboard::write(&text).map_err(|e| e.to_string())
}

// ── IP ────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_local_ips() -> Vec<String> {
    discovery::local_lan_ip().into_iter().collect()
}

// ── Response types ────────────────────────────────────────────────────────────

// ── Helpers ───────────────────────────────────────────────────────────────────

fn device_name() -> String {
    // Use the same safe ASCII label as mDNS registration to avoid mismatches.
    discovery::safe_device_name()
}

fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir())
        .unwrap_or_else(|| PathBuf::from("."))
}

// ── Shell open ──────────────────────────────────────────────────────────────────

/// Open a file or folder in the system file manager.
#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    { std::process::Command::new("explorer").arg(path.replace('/', "\\")).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg(&path).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "linux")]
    { std::process::Command::new("xdg-open").arg(&path).spawn().map_err(|e| e.to_string())?; }
    Ok(())
}
