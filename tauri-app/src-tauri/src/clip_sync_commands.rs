//! Tauri IPC commands for shared clipboard sync.

use rust_air_core::clipboard_sync::{ClipboardSyncService, SyncGroupConfig, SyncPeer};
use std::sync::Arc;
use tauri::State;

// ── App state ─────────────────────────────────────────────────────────────────

pub struct ClipSyncState {
    pub service: Arc<ClipboardSyncService>,
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Return the current sync group configuration.
#[tauri::command]
pub fn get_sync_group(state: State<'_, ClipSyncState>) -> SyncGroupConfig {
    state.service.config()
}

/// Replace the entire sync group configuration and persist.
#[tauri::command]
pub fn save_sync_group(config: SyncGroupConfig, state: State<'_, ClipSyncState>) {
    state.service.save_config(config);
}

/// Add a device to the sync group.
#[tauri::command]
pub fn add_sync_peer(
    device_name: String,
    addr: String,
    state: State<'_, ClipSyncState>,
) {
    let peer = SyncPeer {
        device_name,
        addr,
        last_seen: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        online: true,
    };
    state.service.add_peer(peer);
}

/// Remove a device from the sync group by name.
#[tauri::command]
pub fn remove_sync_peer(device_name: String, state: State<'_, ClipSyncState>) {
    state.service.remove_peer(&device_name);
}

/// Enable or disable clipboard sync.
#[tauri::command]
pub fn set_clip_sync_enabled(enabled: bool, state: State<'_, ClipSyncState>) {
    state.service.set_enabled(enabled);
}

/// Return whether clipboard sync is currently enabled.
#[tauri::command]
pub fn get_clip_sync_enabled(state: State<'_, ClipSyncState>) -> bool {
    state.service.config().enabled
}
