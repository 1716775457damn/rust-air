//! Tauri IPC commands for the shared whiteboard feature.

use rust_air_core::whiteboard::{
    self, WhiteboardContentType, WhiteboardItem, WhiteboardStore, WhiteboardSyncMessage, SyncOp,
};
use rust_air_core::proto::DeviceInfo;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

// ── App state ─────────────────────────────────────────────────────────────────

pub struct WhiteboardState {
    pub store: Mutex<WhiteboardStore>,
    /// Discovered device addresses for broadcasting sync messages.
    pub devices: Mutex<Vec<DeviceInfo>>,
}

impl WhiteboardState {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(WhiteboardStore::load()),
            devices: Mutex::new(Vec::new()),
        }
    }

    /// Update the list of discovered devices (called from scan_devices).
    pub fn update_device(&self, dev: DeviceInfo) {
        let mut devs = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = devs.iter_mut().find(|d| d.name == dev.name) {
            existing.addr = dev.addr;
        } else {
            devs.push(dev);
        }
    }

    /// Remove a device by name (when ServiceRemoved is received).
    pub fn remove_device(&self, name: &str) {
        let mut devs = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        devs.retain(|d| d.name != name);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn device_name() -> String {
    rust_air_core::discovery::safe_device_name()
}

/// Broadcast a sync message to all known devices and emit whiteboard-update.
async fn broadcast_and_emit(
    msg: &WhiteboardSyncMessage,
    devices: &[DeviceInfo],
    items: Vec<WhiteboardItem>,
    app: &AppHandle,
) {
    let local_name = device_name();
    // Broadcast to peers (fire-and-forget, errors are logged inside)
    let _ = whiteboard::broadcast_sync_message(msg, devices, &local_name).await;
    // Notify frontend
    let _ = app.emit("whiteboard-update", &items);
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Return all whiteboard items.
#[tauri::command]
pub fn get_whiteboard_items(state: State<'_, WhiteboardState>) -> Vec<WhiteboardItem> {
    let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    store.snapshot()
}

/// Add a text item to the whiteboard.
#[tauri::command]
pub async fn add_whiteboard_text(
    text: String,
    state: State<'_, WhiteboardState>,
    app: AppHandle,
) -> Result<WhiteboardItem, String> {
    let item = WhiteboardItem {
        id: uuid::Uuid::new_v4().to_string(),
        content_type: WhiteboardContentType::Text,
        text: Some(text),
        image_b64: None,
        timestamp: now_millis(),
        source_device: device_name(),
    };

    let (items, devices) = {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store.add(item.clone());
        store.flush_now();
        let items = store.snapshot();
        let devices = state.devices.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (items, devices)
    };

    let msg = WhiteboardSyncMessage {
        op: SyncOp::Add,
        source_device: device_name(),
        timestamp: item.timestamp,
        item: Some(item.clone()),
        item_id: None,
        items: None,
    };

    broadcast_and_emit(&msg, &devices, items, &app).await;
    Ok(item)
}

/// Add an image item to the whiteboard (Base64 encoded).
#[tauri::command]
pub async fn add_whiteboard_image(
    image_b64: String,
    state: State<'_, WhiteboardState>,
    app: AppHandle,
) -> Result<WhiteboardItem, String> {
    let item = WhiteboardItem {
        id: uuid::Uuid::new_v4().to_string(),
        content_type: WhiteboardContentType::Image,
        text: None,
        image_b64: Some(image_b64),
        timestamp: now_millis(),
        source_device: device_name(),
    };

    let (items, devices) = {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store.add(item.clone());
        store.flush_now();
        let items = store.snapshot();
        let devices = state.devices.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (items, devices)
    };

    let msg = WhiteboardSyncMessage {
        op: SyncOp::Add,
        source_device: device_name(),
        timestamp: item.timestamp,
        item: Some(item.clone()),
        item_id: None,
        items: None,
    };

    broadcast_and_emit(&msg, &devices, items, &app).await;
    Ok(item)
}

/// Delete a whiteboard item by UUID.
#[tauri::command]
pub async fn delete_whiteboard_item(
    id: String,
    state: State<'_, WhiteboardState>,
    app: AppHandle,
) -> Result<(), String> {
    let ts = now_millis();
    let (items, devices) = {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store.delete(&id);
        store.flush_now();
        let items = store.snapshot();
        let devices = state.devices.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (items, devices)
    };

    let msg = WhiteboardSyncMessage {
        op: SyncOp::Delete,
        source_device: device_name(),
        timestamp: ts,
        item: None,
        item_id: Some(id),
        items: None,
    };

    broadcast_and_emit(&msg, &devices, items, &app).await;
    Ok(())
}

/// Clear all whiteboard items.
#[tauri::command]
pub async fn clear_whiteboard(
    state: State<'_, WhiteboardState>,
    app: AppHandle,
) -> Result<(), String> {
    let ts = now_millis();
    let devices = {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store.clear();
        store.flush_now();
        let devices = state.devices.lock().unwrap_or_else(|e| e.into_inner()).clone();
        devices
    };

    let msg = WhiteboardSyncMessage {
        op: SyncOp::Clear,
        source_device: device_name(),
        timestamp: ts,
        item: None,
        item_id: None,
        items: None,
    };

    let _ = whiteboard::broadcast_sync_message(&msg, &devices, &device_name()).await;
    let _ = app.emit("whiteboard-update", &Vec::<WhiteboardItem>::new());
    Ok(())
}

/// Flush whiteboard store to disk if needed (called periodically by frontend).
#[tauri::command]
pub fn flush_whiteboard(state: State<'_, WhiteboardState>) {
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    store.flush_if_needed();
}

/// Send a full whiteboard snapshot to a specific device address.
#[tauri::command]
pub async fn send_whiteboard_snapshot(
    addr: String,
    state: State<'_, WhiteboardState>,
    _app: AppHandle,
) -> Result<(), String> {
    let items = {
        let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store.snapshot()
    };

    if items.is_empty() {
        return Ok(()); // Nothing to send
    }

    let msg = WhiteboardSyncMessage {
        op: SyncOp::Snapshot,
        source_device: device_name(),
        timestamp: now_millis(),
        item: None,
        item_id: None,
        items: Some(items),
    };

    let target = DeviceInfo {
        name: addr.clone(),
        addr: addr.clone(),
        status: rust_air_core::proto::DeviceStatus::Idle,
    };

    let results = whiteboard::broadcast_sync_message(&msg, &[target], &device_name()).await;
    if let Some(r) = results.first() {
        if !r.success {
            return Err(r.error.clone().unwrap_or_else(|| "snapshot send failed".to_string()));
        }
    }
    Ok(())
}
