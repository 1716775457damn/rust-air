//! Tauri IPC commands for file sync/backup (sync-vault).

use rust_air_core::{
    default_excludes, fmt_bytes, full_sync, start_watcher,
    SyncConfig, SyncEvent, SyncStore,
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{mpsc, Mutex},
    thread,
};
use tauri::{AppHandle, Emitter, State};

// ── State ─────────────────────────────────────────────────────────────────────

pub struct SyncState {
    store:   Mutex<SyncStore>,
    config:  Mutex<SyncConfig>,
    /// Drop to stop the watcher
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    running: Mutex<bool>,
}

impl SyncState {
    pub fn new() -> Self {
        let config = SyncConfig::load();
        Self {
            store:   Mutex::new(SyncStore::load()),
            config:  Mutex::new(config),
            watcher: Mutex::new(None),
            running: Mutex::new(false),
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
    let running = *state.running.lock().unwrap_or_else(|e| e.into_inner());
    let watching = state.watcher.lock().unwrap_or_else(|e| e.into_inner()).is_some();
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

/// Run a full sync in a background thread.
/// Progress events are emitted as "sync-event" to the frontend.
#[tauri::command]
pub async fn start_sync(state: State<'_, SyncState>, app: AppHandle) -> Result<(), String> {
    {
        let mut running = state.running.lock().unwrap_or_else(|e| e.into_inner());
        if *running { return Err("sync already running".into()); }
        *running = true;
    }

    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() || config.dst.is_empty() {
        *state.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
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

    // Run sync in blocking thread
    let tx2 = tx.clone();
    let excludes = config.excludes.clone();
    let delete   = config.delete_removed;
    let app2     = app.clone();

    thread::spawn(move || {
        full_sync(&src, &dst, &mut store, delete, &excludes, &tx2);
        store.flush_now();
        drop(tx2);
        app2.emit("sync-done", ()).ok();
    });

    // Mark not-running after done event (frontend handles this via sync-event Done)
    let running_flag = state.running.lock().unwrap_or_else(|e| e.into_inner());
    drop(running_flag); // will be reset by stop_sync or on Done

    Ok(())
}

/// Reset the running flag (called by frontend when it receives sync-done).
#[tauri::command]
pub fn sync_done(state: State<'_, SyncState>) {
    *state.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
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
    let src = PathBuf::from(&config.src);
    let dst = PathBuf::from(&config.dst);
    let excludes = config.excludes.clone();

    let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
    let watcher  = start_watcher(src.clone(), tx).map_err(|e| e.to_string())?;
    *state.watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(watcher);

    // Sync changed files in background
    thread::spawn(move || {
        let (ev_tx, ev_rx) = mpsc::channel::<SyncEvent>();
        let app2 = app.clone();
        thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                app2.emit("sync-event", &ev).ok();
            }
        });
        let mut store = SyncStore::load();
        while let Ok(paths) = rx.recv() {
            for abs in paths {
                rust_air_core::sync_vault::sync_file(&abs, &src, &dst, &mut store, &excludes, &ev_tx);
            }
            store.flush_if_needed();
        }
    });

    Ok(())
}

/// Stop the file watcher.
#[tauri::command]
pub fn stop_watch(state: State<'_, SyncState>) {
    *state.watcher.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
