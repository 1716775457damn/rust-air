//! Tauri IPC command handlers.
//!
//! All `#[tauri::command]` functions return `Result<T, String>` as required by
//! Tauri's serialisation layer. Internal logic uses `anyhow::Result` and is
//! converted at the boundary with `.map_err(|e| e.to_string())`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use rust_air_core::{
    clipboard,
    discovery::{self, SenderHandle},
    proto::DeviceInfo,
    transfer,
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Emitter, State};
use tokio::net::{TcpListener, TcpStream};

// ── App state ─────────────────────────────────────────────────────────────────

pub struct AppState {
    sender_handle: Mutex<Option<SenderHandle>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self { sender_handle: Mutex::new(None) }
    }
}

impl AppState {
    /// Take the current handle (unregisters mDNS on drop), poison-safe.
    pub fn take_handle(&self) -> Option<SenderHandle> {
        self.sender_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Store a new handle, poison-safe.
    pub fn set_handle(&self, h: SenderHandle) {
        *self.sender_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Start advertising a file/folder for sending.
/// Returns the session info (instance name + key) for the frontend to display.
#[tauri::command]
pub async fn start_send(
    path: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<SendSession, String> {
    inner_start_send(path, state, app).await.map_err(|e| e.to_string())
}

async fn inner_start_send(
    path: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> anyhow::Result<SendSession> {
    let path = PathBuf::from(&path);
    anyhow::ensure!(path.exists(), "path not found: {}", path.display());

    let key      = random_key();
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port     = listener.local_addr()?.port();
    let instance = format!("rust-air-{}", &encode_key(&key)[..8]);

    let handle = discovery::register_sender(port, &instance)?;
    state.set_handle(handle);

    // Spawn background task: wait for one connection, then transfer.
    let app_clone = app.clone();
    tokio::spawn(async move {
        match listener.accept().await {
            Ok((stream, peer)) => {
                app_clone.emit("transfer-peer-connected", peer.to_string()).ok();
                let app2 = app_clone.clone();
                let result = transfer::send_path(stream, &path, &key, move |ev| {
                    app2.emit("transfer-progress", &ev).ok();
                })
                .await;
                match result {
                    Ok(_)  => { app_clone.emit("transfer-done", "").ok(); }
                    Err(e) => { app_clone.emit("transfer-error", e.to_string()).ok(); }
                }
            }
            Err(e) => { app_clone.emit("transfer-error", e.to_string()).ok(); }
        }
    });

    Ok(SendSession { instance_name: instance, key_b64: encode_key(&key) })
}

/// Cancel the current send session and unregister mDNS.
#[tauri::command]
pub fn cancel_send(state: State<'_, AppState>) {
    state.take_handle(); // Drop unregisters the mDNS service.
}

/// Resolve a sender by instance name and receive a file/folder.
#[tauri::command]
pub async fn start_receive(
    instance_name: String,
    key_b64: String,
    out_dir: String,
    app: AppHandle,
) -> Result<(), String> {
    let key = decode_key(&key_b64).map_err(|e| e.to_string())?;
    let out = PathBuf::from(out_dir);

    tokio::spawn(async move {
        let result: anyhow::Result<()> = async {
            let (ip, port) = discovery::resolve_sender(&instance_name).await?;
            app.emit("transfer-peer-connected", format!("{ip}:{port}")).ok();
            let stream = TcpStream::connect((ip.as_str(), port)).await?;
            tokio::fs::create_dir_all(&out).await?;
            let app2 = app.clone();
            let saved = transfer::receive_to_disk(stream, &key, &out, move |ev| {
                app2.emit("transfer-progress", &ev).ok();
            })
            .await?;
            app.emit("transfer-done", saved.to_string_lossy().to_string()).ok();
            Ok(())
        }
        .await;
        if let Err(e) = result {
            app.emit("transfer-error", e.to_string()).ok();
        }
    });
    Ok(())
}

/// Start continuous LAN device scanning; results arrive as "device-found" events.
#[tauri::command]
pub async fn scan_devices(app: AppHandle) -> Result<(), String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DeviceInfo>(32);
    tokio::spawn(discovery::browse_devices(tx));
    tokio::spawn(async move {
        while let Some(dev) = rx.recv().await {
            app.emit("device-found", &dev).ok();
        }
    });
    Ok(())
}

#[tauri::command]
pub fn read_clipboard() -> Result<String, String> {
    clipboard::read().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn write_clipboard(text: String) -> Result<(), String> {
    clipboard::write(&text).map_err(|e| e.to_string())
}

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct SendSession {
    pub instance_name: String,
    pub key_b64:       String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

fn encode_key(key: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

/// Decode a base64url key and validate its length.
fn decode_key(b64: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD.decode(b64)?;
    anyhow::ensure!(bytes.len() == 32, "key must be exactly 32 bytes, got {}", bytes.len());
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}
