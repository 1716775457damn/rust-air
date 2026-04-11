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

// ── State ─────────────────────────────────────────────────────────────────────

pub struct AppState {
    pub sender_handle: Mutex<Option<SenderHandle>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self { sender_handle: Mutex::new(None) }
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_send(
    path: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<SendSession, String> {
    let path = PathBuf::from(&path);
    if !path.exists() {
        return Err(format!("path not found: {}", path.display()));
    }
    let key = random_key();
    let listener = TcpListener::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let instance = format!("rust-air-{}", &encode_key(&key)[..8]);

    let handle = discovery::register_sender(port, &instance).map_err(|e| e.to_string())?;
    *state.sender_handle.lock().unwrap() = Some(handle);

    let key_clone = key;
    let app_clone = app.clone();
    tokio::spawn(async move {
        match listener.accept().await {
            Ok((stream, peer)) => {
                app_clone.emit("transfer-peer-connected", peer.to_string()).ok();
                let app2 = app_clone.clone();
                let result = transfer::send_path(stream, &path, &key_clone, move |ev| {
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

#[tauri::command]
pub fn cancel_send(state: State<'_, AppState>) {
    *state.sender_handle.lock().unwrap() = None;
}

#[tauri::command]
pub async fn start_receive(
    instance_name: String,
    key_b64: String,
    out_dir: String,
    app: AppHandle,
) -> Result<(), String> {
    let key_bytes = URL_SAFE_NO_PAD.decode(&key_b64).map_err(|e| e.to_string())?;
    if key_bytes.len() != 32 {
        return Err("key must be 32 bytes".into());
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&key_bytes);
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
    pub key_b64: String,
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
