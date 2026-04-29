use rust_air_core::{ClipContent, ClipEntry, HistoryStore};
use rust_air_core::clipboard_sync::ClipboardSyncService;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

pub struct HistoryState {
    pub store:  Mutex<HistoryStore>,
    pub paused: Mutex<bool>,
}

impl HistoryState {
    pub fn new() -> Self {
        Self {
            store:  Mutex::new(HistoryStore::load()),
            paused: Mutex::new(false),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClipEntryView {
    pub id:         u64,
    pub kind:       String,
    pub preview:    String,
    pub stats:      String,
    pub time_str:   String,
    pub pinned:     bool,
    pub char_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_b64:  Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_device: Option<String>,
}

impl From<&ClipEntry> for ClipEntryView {
    fn from(e: &ClipEntry) -> Self {
        let (kind, image_b64) = match &e.content {
            ClipContent::Text { .. } => ("text".to_string(), None),
            ClipContent::Image { width, height, rgba } => {
                use base64::{engine::general_purpose::STANDARD, Engine};
                let mut buf = Vec::new();
                let b64 = if let Some(img) = image::RgbaImage::from_raw(*width, *height, rgba.to_vec()) {
                    let mut c = std::io::Cursor::new(&mut buf);
                    if image::DynamicImage::ImageRgba8(img).write_to(&mut c, image::ImageFormat::Png).is_ok() {
                        STANDARD.encode(&buf)
                    } else { String::new() }
                } else { String::new() };
                ("image".to_string(), Some(b64))
            }
        };
        Self {
            id: e.id, kind, preview: e.preview.clone(), stats: e.stats.clone(),
            time_str: e.time_str.clone(), pinned: e.pinned,
            char_count: e.char_count, image_b64,
            source_device: e.source_device.clone(),
        }
    }
}

/// Start a background thread that polls clipboard and emits "clip-update" events directly.
/// No channel, no invoke — push-based.
/// When clipboard sync is enabled, new content is broadcast to online peers.
pub fn start_clip_monitor(
    app: AppHandle,
    state: std::sync::Arc<HistoryState>,
    sync_service: std::sync::Arc<ClipboardSyncService>,
) {
    std::thread::spawn(move || {
        // Wait for clipboard to be available
        let mut cb = None;
        for _ in 0..40 {
            if let Ok(c) = arboard::Clipboard::new() { cb = Some(c); break; }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let mut cb = match cb { Some(c) => c, None => return };

        let mut last_text = String::new();
        let mut last_img_hash = 0u64;

        // Build a tokio runtime for async broadcast calls.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok();

        // Delay initial emit so frontend listen() has time to register
        std::thread::sleep(std::time::Duration::from_millis(1000));

        // Emit initial load from disk immediately
        {
            let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
            let entries: Vec<ClipEntryView> = store.entries.iter().map(ClipEntryView::from).collect();
            let _ = app.emit("clip-update", &entries);
        }

        let device_name = rust_air_core::discovery::safe_device_name();

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));

            let paused = *state.paused.lock().unwrap_or_else(|e| e.into_inner());

            // Refresh clipboard handle each tick
            if let Ok(fresh) = arboard::Clipboard::new() { cb = fresh; }

            let new_content: Option<ClipContent> = if let Ok(text) = cb.get_text() {
                let text = text.trim().to_string();
                if !text.is_empty() && text != last_text {
                    last_text = text.clone();
                    last_img_hash = 0;
                    Some(ClipContent::Text { text })
                } else { None }
            } else if let Ok(img) = cb.get_image() {
                let hash = fnv1a(&img.bytes);
                if hash != last_img_hash {
                    last_img_hash = hash;
                    last_text.clear();
                    Some(ClipContent::Image {
                        width: img.width as u32, height: img.height as u32,
                        rgba: img.bytes.into_owned(),
                    })
                } else { None }
            } else { None };

            if let Some(content) = new_content {
                // Attempt clipboard sync broadcast (non-blocking, failures don't affect history)
                if sync_service.should_broadcast(&content) {
                    if let Some(ref rt) = rt {
                        let svc = sync_service.clone();
                        let c = content.clone();
                        let dn = device_name.clone();
                        let app2 = app.clone();
                        rt.block_on(async {
                            let results = svc.broadcast(&c, &dn).await;
                            for r in &results {
                                if !r.success {
                                    if let Some(ref err) = r.error {
                                        let sync_err = rust_air_core::clipboard_sync::ClipSyncError {
                                            kind: "transfer_failed".to_string(),
                                            message: err.clone(),
                                            device: Some(r.device_name.clone()),
                                        };
                                        let _ = app2.emit("clip-sync-error", &sync_err);
                                    }
                                }
                            }
                        });
                    }
                }

                let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
                if !paused { store.push(content); }
                store.flush_if_needed();
                let entries: Vec<ClipEntryView> = store.entries.iter().map(ClipEntryView::from).collect();
                let _ = app.emit("clip-update", &entries);
            }
        }
    });
}

fn fnv1a(data: &[u8]) -> u64 {
    let step = (data.len() / 512).max(1);
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data.iter().step_by(step) { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
    h
}

// ── Commands (still needed for mutations) ────────────────────────────────────

#[tauri::command]
pub fn get_history(state: State<'_, Arc<HistoryState>>) -> Vec<ClipEntryView> {
    state.store.lock().unwrap_or_else(|e| e.into_inner())
        .entries.iter().map(ClipEntryView::from).collect()
}

#[tauri::command]
pub fn copy_history_entry(id: u64, app: AppHandle, state: State<'_, Arc<HistoryState>>) -> Result<(), String> {
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    let entry = store.entries.iter().find(|e| e.id == id).cloned()
        .ok_or_else(|| format!("entry {id} not found"))?;
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match &entry.content {
        ClipContent::Text { text } => cb.set_text(text.clone()).map_err(|e| e.to_string())?,
        ClipContent::Image { width, height, rgba } => {
            cb.set_image(arboard::ImageData {
                width: *width as usize, height: *height as usize,
                bytes: std::borrow::Cow::Borrowed(rgba),
            }).map_err(|e| e.to_string())?;
        }
    }
    store.push(entry.content);
    let entries: Vec<ClipEntryView> = store.entries.iter().map(ClipEntryView::from).collect();
    let _ = app.emit("clip-update", &entries);
    Ok(())
}

#[tauri::command]
pub fn delete_history_entry(id: u64, app: AppHandle, state: State<'_, Arc<HistoryState>>) {
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    store.remove(id);
    let entries: Vec<ClipEntryView> = store.entries.iter().map(ClipEntryView::from).collect();
    let _ = app.emit("clip-update", &entries);
}

#[tauri::command]
pub fn toggle_pin_entry(id: u64, app: AppHandle, state: State<'_, Arc<HistoryState>>) {
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    store.toggle_pin(id);
    let entries: Vec<ClipEntryView> = store.entries.iter().map(ClipEntryView::from).collect();
    let _ = app.emit("clip-update", &entries);
}

#[tauri::command]
pub fn clear_history(app: AppHandle, state: State<'_, Arc<HistoryState>>) {
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    store.clear_unpinned();
    let entries: Vec<ClipEntryView> = store.entries.iter().map(ClipEntryView::from).collect();
    let _ = app.emit("clip-update", &entries);
}

#[tauri::command]
pub fn set_history_paused(paused: bool, state: State<'_, Arc<HistoryState>>) {
    *state.paused.lock().unwrap_or_else(|e| e.into_inner()) = paused;
}

#[tauri::command]
pub fn flush_history(state: State<'_, Arc<HistoryState>>) {
    state.store.lock().unwrap_or_else(|e| e.into_inner()).flush_now();
}

#[tauri::command]
pub fn tick_history(state: State<'_, Arc<HistoryState>>) -> Vec<ClipEntryView> {
    state.store.lock().unwrap_or_else(|e| e.into_inner())
        .entries.iter().map(ClipEntryView::from).collect()
}

#[tauri::command]
pub fn get_history_paused(state: State<'_, Arc<HistoryState>>) -> bool {
    *state.paused.lock().unwrap_or_else(|e| e.into_inner())
}
