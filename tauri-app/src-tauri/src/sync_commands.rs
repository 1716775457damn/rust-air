//! Tauri IPC commands for file sync/backup (sync-vault).

use rust_air_core::{
    transfer,
    default_excludes, fmt_bytes, full_sync, start_watcher,
    SyncAction, SyncConfig, SyncEvent, SyncManifestEntry, SyncStore,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

// ── State ─────────────────────────────────────────────────────────────────────

pub struct SyncState {
    store:   Mutex<SyncStore>,
    config:  Mutex<SyncConfig>,
    watch_session: Mutex<Option<WatchSession>>,
    running: Arc<AtomicBool>,
    pending_remote_sync: Mutex<HashMap<String, oneshot::Sender<RemoteSyncResponse>>>,
    pending_remote_files: Mutex<HashMap<String, oneshot::Sender<Result<PathBuf, String>>>>,
}

struct WatchSession {
    watcher: notify::RecommendedWatcher,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl WatchSession {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        drop(self.watcher);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl SyncState {
    pub fn new() -> Self {
        let config = SyncConfig::load();
        Self {
            store:   Mutex::new(SyncStore::load()),
            config:  Mutex::new(config),
            watch_session: Mutex::new(None),
            running: Arc::new(AtomicBool::new(false)),
            pending_remote_sync: Mutex::new(HashMap::new()),
            pending_remote_files: Mutex::new(HashMap::new()),
        }
    }

    fn register_pending_remote_sync(&self, request_id: String) -> oneshot::Receiver<RemoteSyncResponse> {
        let (tx, rx) = oneshot::channel();
        self.pending_remote_sync
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        rx
    }

    fn resolve_pending_remote_sync(&self, response: RemoteSyncResponse) {
        if let Some(tx) = self.pending_remote_sync
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&response.request_id)
        {
            let _ = tx.send(response);
        }
    }

    fn register_pending_remote_file(&self, request_id: String) -> oneshot::Receiver<Result<PathBuf, String>> {
        let (tx, rx) = oneshot::channel();
        self.pending_remote_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        rx
    }

    pub(crate) fn resolve_pending_remote_file(&self, request_id: String, result: Result<PathBuf, String>) {
        if let Some(tx) = self.pending_remote_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id)
        {
            let _ = tx.send(result);
        }
    }
}

// ── View types ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct SyncStatus {
    pub last_sync:    Option<String>,
    pub total_files:  u64,
    pub total_bytes:  String,
    pub is_running:   bool,
    pub is_watching:  bool,
}

#[derive(Serialize, Deserialize)]
struct RemoteSyncRequest {
    request_id: String,
    manifest: Vec<SyncManifestEntry>,
    callback_addr: String,
    excludes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RemoteSyncResponse {
    request_id: String,
    manifest: Vec<SyncManifestEntry>,
}

#[derive(Serialize, Deserialize)]
struct RemoteSyncFileRequest {
    request_id: String,
    rel: String,
    callback_addr: String,
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_sync_config(state: State<'_, SyncState>) -> SyncConfig {
    state.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[tauri::command]
pub fn save_sync_config(config: SyncConfig, state: State<'_, SyncState>) {
    config.save();
    *state.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
}

#[tauri::command]
pub fn get_sync_status(state: State<'_, SyncState>) -> SyncStatus {
    let store   = state.store.lock().unwrap_or_else(|e| e.into_inner());
    let running = state.running.load(Ordering::Relaxed);
    let watching = state.watch_session.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    SyncStatus {
        last_sync:   store.state.last_sync.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
        total_files: store.state.total_synced,
        total_bytes: fmt_bytes(store.state.total_bytes),
        is_running:  running,
        is_watching: watching,
    }
}

#[tauri::command]
pub fn get_default_excludes() -> Vec<String> {
    default_excludes()
}

#[tauri::command]
pub async fn start_remote_sync(
    remote_addr: String,
    callback_addr: String,
    state: State<'_, SyncState>,
    app: AppHandle,
) -> Result<(), String> {
    if state.running.swap(true, Ordering::AcqRel) {
        return Err("sync already running".into());
    }

    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() {
        state.running.store(false, Ordering::Release);
        return Err("source directory must be set".into());
    }

    let src = PathBuf::from(&config.src);
    let excludes = config.excludes.clone();
    let local_manifest = rust_air_core::sync_vault::build_manifest(&src, &excludes);
    let request_id = uuid::Uuid::new_v4().to_string();
    let request = RemoteSyncRequest {
        request_id: request_id.clone(),
        manifest: local_manifest.clone(),
        callback_addr,
        excludes: excludes.clone(),
    };
    let response_rx = state.register_pending_remote_sync(request_id.clone());

    let json = serde_json::to_string(&request).map_err(|e| e.to_string())?;
    let stream = tokio::net::TcpStream::connect(&remote_addr).await.map_err(|e| e.to_string())?;
    transfer::send_clipboard(stream, &json, "sync:manifest-request", |_| {}).await.map_err(|e| e.to_string())?;

    let response = tokio::time::timeout(std::time::Duration::from_secs(20), response_rx)
        .await
        .map_err(|_| "timed out waiting for remote sync manifest response".to_string())?
        .map_err(|_| "remote sync response channel closed".to_string())?;

    let actions = rust_air_core::sync_vault::diff_manifests_latest_wins(&local_manifest, &response.manifest);
    let mut pull_waiters = Vec::new();
    for action in &actions {
        match action {
            SyncAction::PushToRemote(entry) => {
                let src_file = src.join(&entry.rel);
                let logical_name = format!("sync:file:push:{}", entry.rel);
                let stream = tokio::net::TcpStream::connect(&remote_addr).await.map_err(|e| e.to_string())?;
                transfer::send_path_as(stream, &src_file, Some(&logical_name), |_| {}).await.map_err(|e| e.to_string())?;
                app.emit("sync-event", SyncEvent::Copied { rel: entry.rel.clone(), bytes: entry.size }).ok();
            }
            SyncAction::PullFromRemote(entry) => {
                let file_request_id = uuid::Uuid::new_v4().to_string();
                let req = RemoteSyncFileRequest {
                    request_id: file_request_id.clone(),
                    rel: entry.rel.clone(),
                    callback_addr: request.callback_addr.clone(),
                };
                let waiter = state.register_pending_remote_file(file_request_id.clone());
                let json = serde_json::to_string(&req).map_err(|e| e.to_string())?;
                let stream = tokio::net::TcpStream::connect(&remote_addr).await.map_err(|e| e.to_string())?;
                transfer::send_clipboard(stream, &json, "sync:file-request", |_| {}).await.map_err(|e| e.to_string())?;
                pull_waiters.push((entry.rel.clone(), waiter));
            }
        }
    }

    for (rel, waiter) in pull_waiters {
        let received = tokio::time::timeout(std::time::Duration::from_secs(60), waiter)
            .await
            .map_err(|_| format!("timed out waiting for synced file: {rel}"))?
            .map_err(|_| format!("sync file completion channel closed: {rel}"))?;
        if let Err(e) = received {
            app.emit("sync-event", SyncEvent::Error { rel: rel.clone(), err: e }).ok();
        }
    }

    app.emit("sync-done", ()).ok();
    state.running.store(false, Ordering::Release);
    Ok(())
}

pub async fn handle_sync_manifest_request(
    data: &[u8],
    state: &SyncState,
) -> Result<(), String> {
    let req: RemoteSyncRequest = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() {
        return Err("local sync source directory not configured".to_string());
    }
    let src = PathBuf::from(&config.src);
    let manifest = rust_air_core::sync_vault::build_manifest(&src, &config.excludes);
    let resp = RemoteSyncResponse {
        request_id: req.request_id,
        manifest,
    };
    let json = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
    let stream = tokio::net::TcpStream::connect(&req.callback_addr).await.map_err(|e| e.to_string())?;
    transfer::send_clipboard(stream, &json, "sync:manifest-response", |_| {}).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn handle_sync_manifest_response(
    data: &[u8],
    state: &SyncState,
) -> Result<(), String> {
    let resp: RemoteSyncResponse = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    state.resolve_pending_remote_sync(resp);
    Ok(())
}

pub async fn handle_sync_file_request(
    data: &[u8],
    state: &SyncState,
) -> Result<(), String> {
    let req: RemoteSyncFileRequest = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() {
        return Err("local sync source directory not configured".to_string());
    }
    let src_file = PathBuf::from(&config.src).join(&req.rel);
    if !src_file.exists() {
        return Err(format!("sync source file not found: {}", src_file.display()));
    }
    let logical_name = format!("sync:file:{}:{}", req.request_id, req.rel);
    let stream = tokio::net::TcpStream::connect(&req.callback_addr).await.map_err(|e| e.to_string())?;
    transfer::send_path_as(stream, &src_file, Some(&logical_name), |_| {}).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn handle_received_sync_file(
    temp_path: &std::path::Path,
    logical_name: &str,
    state: &SyncState,
) -> Result<(String, String, PathBuf), String> {
    let payload = logical_name
        .strip_prefix("sync:file:")
        .ok_or_else(|| "invalid sync file logical name".to_string())?;
    let mut parts = payload.splitn(2, ':');
    let request_id = parts.next().unwrap_or_default().to_string();
    let rel = parts.next().ok_or_else(|| "missing sync file relative path".to_string())?;
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() {
        return Err("local sync source directory not configured".to_string());
    }
    let dst = PathBuf::from(&config.src).join(rel);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(temp_path, &dst).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(temp_path);
    Ok((request_id, rel.to_string(), dst))
}

/// Run a full sync in a background thread.
/// Progress events are emitted as "sync-event" to the frontend.
#[tauri::command]
pub async fn start_sync(state: State<'_, SyncState>, app: AppHandle) -> Result<(), String> {
    if state.running.swap(true, Ordering::AcqRel) {
        return Err("sync already running".into());
    }

    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() || config.dst.is_empty() {
        state.running.store(false, Ordering::Release);
        return Err("source and destination must be set".into());
    }

    let src = PathBuf::from(&config.src);
    let dst = PathBuf::from(&config.dst);

    // Load a fresh store snapshot for the thread
    let mut store = SyncStore::load();
    let (tx, rx) = mpsc::channel::<SyncEvent>();

    // Forward events to frontend
    let app_clone = app.clone();
    thread::spawn(move || {
        while let Ok(ev) = rx.recv() {
            app_clone.emit("sync-event", &ev).ok();
        }
    });

    // Run sync in blocking thread; reset running flag when done regardless of frontend state.
    let tx2      = tx.clone();
    let excludes = config.excludes.clone();
    let delete   = config.delete_removed;
    let app2     = app.clone();
    let running  = Arc::clone(&state.running);

    thread::spawn(move || {
        full_sync(&src, &dst, &mut store, delete, &excludes, &tx2);
        store.flush_now();
        drop(tx2);
        running.store(false, Ordering::Release);
        app2.emit("sync-done", ()).ok();
    });

    Ok(())
}

/// Reset the running flag (called by frontend when it receives sync-done).
#[tauri::command]
pub fn sync_done(state: State<'_, SyncState>) {
    state.running.store(false, Ordering::Release);
    // Reload store from disk to pick up changes made in the sync thread
    *state.store.lock().unwrap_or_else(|e| e.into_inner()) = SyncStore::load();
}

/// Start watching src for changes and auto-sync on modification.
#[tauri::command]
pub fn start_watch(state: State<'_, SyncState>, app: AppHandle) -> Result<(), String> {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() || config.dst.is_empty() {
        return Err("source and destination must be set".into());
    }

    if let Some(session) = state.watch_session.lock().unwrap_or_else(|e| e.into_inner()).take() {
        session.stop();
    }

    let src = PathBuf::from(&config.src);
    let dst = PathBuf::from(&config.dst);
    let excludes = config.excludes.clone();
    let stop = Arc::new(AtomicBool::new(false));

    let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
    let watcher  = start_watcher(src.clone(), tx).map_err(|e| e.to_string())?;

    // Sync changed files in background
    let stop_worker = stop.clone();
    let worker = thread::spawn(move || {
        let (ev_tx, ev_rx) = mpsc::channel::<SyncEvent>();
        let app2 = app.clone();
        let forward_stop = stop_worker.clone();
        let forwarder = thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                if forward_stop.load(Ordering::Acquire) {
                    break;
                }
                app2.emit("sync-event", &ev).ok();
            }
        });
        let mut store = SyncStore::load();
        // Build ExcludeSet once outside the loop — not per-file
        let ex = rust_air_core::sync_vault::ExcludeSet::new(&excludes);
        while let Ok(paths) = rx.recv() {
            if stop_worker.load(Ordering::Acquire) {
                break;
            }
            for abs in paths {
                if stop_worker.load(Ordering::Acquire) {
                    break;
                }
                rust_air_core::sync_vault::sync_file(&abs, &src, &dst, &mut store, &ex, &ev_tx);
            }
            store.flush_if_needed();
        }
        drop(ev_tx);
        let _ = forwarder.join();
    });

    *state.watch_session.lock().unwrap_or_else(|e| e.into_inner()) = Some(WatchSession {
        watcher,
        stop,
        worker: Some(worker),
    });

    Ok(())
}

/// Stop the file watcher.
#[tauri::command]
pub fn stop_watch(state: State<'_, SyncState>) {
    if let Some(session) = state.watch_session.lock().unwrap_or_else(|e| e.into_inner()).take() {
        session.stop();
    }
}
