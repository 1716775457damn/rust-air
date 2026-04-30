//! Tauri IPC command handlers — v3 (scan-and-click, no pre-shared key).

#[cfg(feature = "desktop")]
use rust_air_core::clipboard;
#[cfg(feature = "desktop")]
use rust_air_core::clipboard_sync;
use rust_air_core::{
    discovery::{self, ServiceHandle},
    proto::DeviceInfo,
    transfer::{self, ReceiveOutcome},
};
#[cfg(feature = "desktop")]
use rust_air_core::ClipEntry;
use std::{path::PathBuf, sync::Mutex};
#[cfg(feature = "desktop")]
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::{net::{TcpListener, TcpStream}, sync::oneshot};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "desktop")]
use crate::clip_history_commands::{ClipEntryView, HistoryState};
#[cfg(feature = "desktop")]
use crate::clip_sync_commands::ClipSyncState;
#[cfg(feature = "desktop")]
use crate::whiteboard_commands::WhiteboardState;

// ── App state ─────────────────────────────────────────────────────────────────

pub struct AppState {
    /// mDNS / UDP self-registration handle (kept alive for the app lifetime).
    svc_handle:  Mutex<Option<ServiceHandle>>,
    /// Cancel token for the current outgoing send task.
    send_cancel: Mutex<Option<oneshot::Sender<()>>>,
    /// Cancel token for the receive-side reconnect loop.
    #[allow(dead_code)]
    recv_cancel: Mutex<Option<CancellationToken>>,
    /// Last send parameters (path, addr) for retry_send.
    last_send_params: Mutex<Option<(String, String)>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            svc_handle:       Mutex::new(None),
            send_cancel:      Mutex::new(None),
            recv_cancel:      Mutex::new(None),
            last_send_params: Mutex::new(None),
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
    #[allow(dead_code)]
    fn new_recv_cancel(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.recv_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.clone());
        token
    }
    fn save_send_params(&self, path: &str, addr: &str) {
        *self.last_send_params.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_string(), addr.to_string()));
    }
    fn take_send_params(&self) -> Option<(String, String)> {
        self.last_send_params.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

// ── Startup (desktop) ─────────────────────────────────────────────────────────

/// Desktop version: bind a listener, register on mDNS, start accepting.
/// Clipboard sync receives are post-processed: EchoGuard registration, history
/// update, and frontend event emission.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn start_listener(
    state: State<'_, AppState>,
    clip_sync: State<'_, ClipSyncState>,
    history: State<'_, Arc<HistoryState>>,
    _wb_state: State<'_, WhiteboardState>,
    app: AppHandle,
) -> Result<u16, String> {
    let listener = TcpListener::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
    let port     = listener.local_addr().map_err(|e| e.to_string())?.port();

    let device_name = device_name();
    let handle = discovery::register_self(port, &device_name).map_err(|e| e.to_string())?;
    state.set_svc(handle);

    let sync_svc = clip_sync.service.clone();
    let hist_state = history.inner().clone();

    // Accept loop — runs for the lifetime of the app.
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let app2 = app.clone();
                    let svc = sync_svc.clone();
                    let hs = hist_state.clone();
                    tokio::spawn(async move {
                        app2.emit("recv-peer-connected", peer.to_string()).ok();
                        let out = default_download_dir();
                        let app3 = app2.clone();
                        let cancel_token = CancellationToken::new();
                        match transfer::receive_with_reconnect(peer, &out, cancel_token, move |ev| {
                            app3.emit("recv-progress", &ev).ok();
                        }, Some(stream)).await {
                            Ok(ReceiveOutcome::File(p)) => {
                                app2.emit("recv-done", p.to_string_lossy().to_string()).ok();
                            }
                            Ok(ReceiveOutcome::Clipboard { name, data, .. }) => {
                                // Check if this is a whiteboard sync message
                                if name.starts_with("wb:") {
                                    match rust_air_core::whiteboard::handle_received_whiteboard(&name, &data) {
                                        Ok(msg) => {
                                            // Access the managed WhiteboardState
                                            if let Some(wb) = app2.try_state::<WhiteboardState>() {
                                                let items = {
                                                    let mut store = wb.store.lock().unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
                                                    rust_air_core::whiteboard::apply_sync_message(&mut store, msg);
                                                    store.flush_now();
                                                    store.snapshot()
                                                };
                                                let _ = app2.emit("whiteboard-update", &items);
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("warn: whiteboard sync receive failed: {e}");
                                            let wb_err = rust_air_core::whiteboard::WhiteboardError {
                                                kind: "parse_failed".to_string(),
                                                message: e.to_string(),
                                                device: None,
                                            };
                                            let _ = app2.emit("whiteboard-error", &wb_err);
                                        }
                                    }
                                } else {
                                    // Existing clipboard sync logic
                                    match svc.handle_received(&name, &data) {
                                        Ok((content, source_device)) => {
                                            let mut store = hs.store.lock().unwrap_or_else(|e| e.into_inner());
                                            let mut entry = ClipEntry::new(0, content);
                                            if !source_device.is_empty() {
                                                entry.source_device = Some(source_device.clone());
                                            }
                                            store.push(entry.content.clone());
                                            if !source_device.is_empty() {
                                                let first_unpinned = store.entries.iter().position(|e| !e.pinned).unwrap_or(0);
                                                if let Some(e) = store.entries.get_mut(first_unpinned) {
                                                    e.source_device = Some(source_device.clone());
                                                }
                                            }
                                            store.flush_if_needed();
                                            let entries: Vec<ClipEntryView> = store.entries.iter().map(ClipEntryView::from).collect();
                                            let _ = app2.emit("clip-update", &entries);

                                            if !source_device.is_empty() {
                                                let _ = app2.emit("clip-sync-received", &source_device);
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("warn: clipboard sync receive failed: {e}");
                                            let sync_err = clipboard_sync::ClipSyncError {
                                                kind: "transfer_failed".to_string(),
                                                message: e.to_string(),
                                                device: None,
                                            };
                                            let _ = app2.emit("clip-sync-error", &sync_err);
                                        }
                                    }
                                }
                            }
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

// ── Startup (non-desktop / Android) ───────────────────────────────────────────

/// Non-desktop version: bind a listener, register via UDP broadcast, start accepting.
/// No clipboard sync or history processing — just file transfers.
#[cfg(not(feature = "desktop"))]
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
                        match transfer::receive_with_reconnect(peer, &out, CancellationToken::new(), move |ev| {
                            app3.emit("recv-progress", &ev).ok();
                        }, Some(stream)).await {
                            Ok(ReceiveOutcome::File(p)) => {
                                app2.emit("recv-done", p.to_string_lossy().to_string()).ok();
                            }
                            Ok(ReceiveOutcome::Clipboard { .. }) => {
                                // Clipboard sync not available on non-desktop; ignore.
                            }
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
    state.save_send_params(&path, &addr);

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

/// Retry the last failed send using the same path and address.
#[tauri::command]
pub async fn retry_send(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let (path, addr) = state.take_send_params()
        .ok_or_else(|| "no previous send to retry".to_string())?;
    send_to(path, addr, state, app).await
}

// ── Scan (desktop) ────────────────────────────────────────────────────────────

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn scan_devices(
    clip_sync: State<'_, ClipSyncState>,
    _wb_state: State<'_, WhiteboardState>,
    app: AppHandle,
) -> Result<(), String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DeviceInfo>(32);
    let handle = discovery::browse_devices_sync(tx).map_err(|e| e.to_string())?;

    let svc = clip_sync.service.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(dev)) => {
                    // addr=="" means ServiceRemoved — skip, do NOT break
                    if dev.addr.is_empty() {
                        // Remove from whiteboard device list
                        if let Some(wb) = app.try_state::<WhiteboardState>() {
                            wb.remove_device(&dev.name);
                        }
                        continue;
                    }

                    // Update SyncPeer status if this device is in the sync group
                    svc.update_peer_status(&dev.name, &dev.addr);

                    // Update whiteboard device list for broadcasting
                    if let Some(wb) = app.try_state::<WhiteboardState>() {
                        wb.update_device(dev.clone());
                    }

                    app.emit("device-found", &dev).ok();
                }
                Ok(None) => break, // channel closed
                Err(_)   => break, // deadline reached
            }
        }
        drop(handle);
    });
    Ok(())
}

// ── Scan (non-desktop / Android) ──────────────────────────────────────────────

#[cfg(not(feature = "desktop"))]
#[tauri::command]
pub async fn scan_devices(
    app: AppHandle,
) -> Result<(), String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DeviceInfo>(32);
    let handle = discovery::browse_devices_sync(tx).map_err(|e| e.to_string())?;

    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(dev)) => {
                    if dev.addr.is_empty() { continue; }
                    app.emit("device-found", &dev).ok();
                }
                Ok(None) => break,
                Err(_)   => break,
            }
        }
        drop(handle);
    });
    Ok(())
}

// ── Clipboard (desktop only) ─────────────────────────────────────────────────

#[cfg(feature = "desktop")]
#[tauri::command]
pub fn read_clipboard() -> Result<String, String> {
    clipboard::read().map_err(|e| e.to_string())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub fn write_clipboard(text: String) -> Result<(), String> {
    clipboard::write(&text).map_err(|e| e.to_string())
}

// ── IP ────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_local_ips() -> Vec<String> {
    discovery::local_lan_ip().into_iter().collect()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn device_name() -> String {
    discovery::safe_device_name()
}

fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir())
        .unwrap_or_else(|| PathBuf::from("."))
}

// ── Shell open (desktop only) ───────────────────────────────────────────────

/// Open a file or folder in the system file manager.
#[cfg(feature = "desktop")]
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
