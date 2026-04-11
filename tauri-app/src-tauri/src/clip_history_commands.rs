//! Tauri IPC commands for clipboard history.
//!
//! The `HistoryState` is managed by Tauri and shared across all commands.
//! The background monitor thread sends new entries via an mpsc channel;
//! `tick_history` drains the channel and should be called periodically
//! (triggered by the frontend via a setInterval).

use rust_air_core::{ClipContent, ClipEntry, HistoryStore, start_monitor};
use serde::{Deserialize, Serialize};
use std::sync::{mpsc, Mutex};
use tauri::State;

// ── State ─────────────────────────────────────────────────────────────────────

pub struct HistoryState {
    store:   Mutex<HistoryStore>,
    rx:      Mutex<mpsc::Receiver<ClipContent>>,
    paused:  Mutex<bool>,
}

impl HistoryState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        start_monitor(tx);
        Self {
            store:  Mutex::new(HistoryStore::load()),
            rx:     Mutex::new(rx),
            paused: Mutex::new(false),
        }
    }
}

// ── Serialisable view sent to frontend ───────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct ClipEntryView {
    pub id:         u64,
    pub kind:       String,   // "text" | "image"
    pub preview:    String,
    pub stats:      String,
    pub time_str:   String,
    pub pinned:     bool,
    pub char_count: usize,
    /// Only present for image entries (base64-encoded PNG)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_b64:  Option<String>,
}

impl From<&ClipEntry> for ClipEntryView {
    fn from(e: &ClipEntry) -> Self {
        let (kind, image_b64) = match &e.content {
            ClipContent::Text { .. } => ("text".to_string(), None),
            ClipContent::Image { width, height, rgba } => {
                // Encode image as base64 PNG for display in the frontend
                let b64 = encode_rgba_to_b64_png(*width, *height, rgba);
                ("image".to_string(), Some(b64))
            }
        };
        Self {
            id: e.id, kind, preview: e.preview.clone(), stats: e.stats.clone(),
            time_str: e.time_str.clone(), pinned: e.pinned,
            char_count: e.char_count, image_b64,
        }
    }
}

fn encode_rgba_to_b64_png(width: u32, height: u32, rgba: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    // Encode as raw PNG via the `image` crate if available, else return empty
    let mut buf = Vec::new();
    if let Some(img) = image::RgbaImage::from_raw(width, height, rgba.to_vec()) {
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut cursor = std::io::Cursor::new(&mut buf);
        if dyn_img.write_to(&mut cursor, image::ImageFormat::Png).is_ok() {
            return STANDARD.encode(&buf);
        }
    }
    String::new()
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Drain the monitor channel and push new entries into the store.
/// Frontend should call this every ~500 ms via setInterval.
/// Returns the updated entry list (filtered by optional query).
#[tauri::command]
pub fn tick_history(
    query: String,
    state: State<'_, HistoryState>,
) -> Vec<ClipEntryView> {
    let paused = *state.paused.lock().unwrap_or_else(|e| e.into_inner());
    // Drain channel and push into store — acquire locks separately to avoid deadlock
    {
        let rx = state.rx.lock().unwrap_or_else(|e| e.into_inner());
        let pending: Vec<ClipContent> = rx.try_iter().collect();
        drop(rx); // release rx lock before acquiring store lock
        if !pending.is_empty() {
            let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
            for content in pending {
                if !paused { store.push(content); }
            }
            store.flush_if_needed();
        } else {
            state.store.lock().unwrap_or_else(|e| e.into_inner()).flush_if_needed();
        }
    }
    get_entries_filtered(&state, &query)
}

/// Return all entries matching `query` (empty = all).
#[tauri::command]
pub fn get_history(
    query: String,
    state: State<'_, HistoryState>,
) -> Vec<ClipEntryView> {
    get_entries_filtered(&state, &query)
}

/// Copy an entry's content back to the system clipboard and bump it to the top.
#[tauri::command]
pub fn copy_history_entry(id: u64, state: State<'_, HistoryState>) -> Result<(), String> {
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    let entry = store.entries.iter().find(|e| e.id == id).cloned()
        .ok_or_else(|| format!("entry {id} not found"))?;
    // Write to clipboard
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match &entry.content {
        ClipContent::Text { text } => cb.set_text(text.clone()).map_err(|e| e.to_string())?,
        ClipContent::Image { width, height, rgba } => {
            cb.set_image(arboard::ImageData {
                width:  *width  as usize,
                height: *height as usize,
                bytes:  std::borrow::Cow::Borrowed(rgba),
            }).map_err(|e| e.to_string())?;
        }
    }
    // Bump to top
    store.push(entry.content);
    Ok(())
}

#[tauri::command]
pub fn delete_history_entry(id: u64, state: State<'_, HistoryState>) {
    state.store.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

#[tauri::command]
pub fn toggle_pin_entry(id: u64, state: State<'_, HistoryState>) {
    state.store.lock().unwrap_or_else(|e| e.into_inner()).toggle_pin(id);
}

#[tauri::command]
pub fn clear_history(state: State<'_, HistoryState>) {
    state.store.lock().unwrap_or_else(|e| e.into_inner()).clear_unpinned();
}

#[tauri::command]
pub fn set_history_paused(paused: bool, state: State<'_, HistoryState>) {
    *state.paused.lock().unwrap_or_else(|e| e.into_inner()) = paused;
}

#[tauri::command]
pub fn get_history_paused(state: State<'_, HistoryState>) -> bool {
    *state.paused.lock().unwrap_or_else(|e| e.into_inner())
}

/// Flush history to disk immediately (call on app exit).
#[tauri::command]
pub fn flush_history(state: State<'_, HistoryState>) {
    state.store.lock().unwrap_or_else(|e| e.into_inner()).flush_now();
}

// ── Helper ────────────────────────────────────────────────────────────────────

fn get_entries_filtered(state: &HistoryState, query: &str) -> Vec<ClipEntryView> {
    let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    let q = query.trim().to_lowercase();
    store.entries.iter()
        .filter(|e| {
            if q.is_empty() { return true; }
            match &e.content {
                ClipContent::Text { .. } => e.text_lc.contains(&q),
                ClipContent::Image { .. } => false,
            }
        })
        .map(ClipEntryView::from)
        .collect()
}
