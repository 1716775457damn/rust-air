//! Clipboard history engine — ported from clip-vault.
//!
//! Provides:
//! - `ClipEntry` / `ClipContent` — serialisable history item
//! - `HistoryStore`              — in-memory store with JSON persistence
//! - `start_monitor`             — background thread that polls the clipboard

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::PathBuf,
    time::Instant,
};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClipContent {
    Text { text: String },
    /// Images are not persisted (serde skip), only held in memory.
    #[serde(skip)]
    Image { width: u32, height: u32, rgba: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEntry {
    pub id:         u64,
    pub content:    ClipContent,
    pub time:       DateTime<Local>,
    pub pinned:     bool,
    pub preview:    String,
    pub stats:      String,
    pub char_count: usize,
    pub time_str:   String,
    #[serde(skip)]
    pub text_lc:    String,
    /// 来源设备名称（None 表示本地复制）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_device: Option<String>,
}

impl ClipEntry {
    pub fn new(id: u64, content: ClipContent) -> Self {
        let preview    = make_preview(&content);
        let (stats, char_count) = make_stats(&content);
        let text_lc    = match &content {
            ClipContent::Text { text } => text.to_lowercase(),
            ClipContent::Image { .. }  => String::new(),
        };
        let now      = Local::now();
        let time_str = now.format("%H:%M").to_string();
        Self { id, content, time: now, pinned: false, preview, stats, char_count, time_str, text_lc, source_device: None }
    }

    pub fn rebuild_time_str(&mut self, today: chrono::NaiveDate) {
        let date = self.time.date_naive();
        self.time_str = if date == today {
            self.time.format("%H:%M").to_string()
        } else {
            self.time.format("%m/%d %H:%M").to_string()
        };
    }
}

fn make_preview(content: &ClipContent) -> String {
    match content {
        ClipContent::Text { text } => {
            let s   = text.trim();
            let end = s.char_indices().nth(120).map(|(i, _)| i).unwrap_or(s.len());
            s[..end].to_string()
        }
        ClipContent::Image { .. } => "[图片]".to_string(),
    }
}

fn make_stats(content: &ClipContent) -> (String, usize) {
    match content {
        ClipContent::Text { text } => {
            let trimmed = text.trim();
            if trimmed.is_empty() { return ("0 字".to_string(), 0); }
            let mut chars = 0usize;
            let mut lines = 1usize;
            for ch in trimmed.chars() {
                chars += 1;
                if ch == '\n' { lines += 1; }
            }
            let stats = if lines > 1 {
                format!("{} 行 {} 字", lines, chars)
            } else {
                format!("{} 字", chars)
            };
            (stats, chars)
        }
        ClipContent::Image { width, height, .. } => (format!("{}×{}", width, height), 0),
    }
}

// ── HistoryStore ──────────────────────────────────────────────────────────────

pub struct HistoryStore {
    pub entries:  Vec<ClipEntry>,
    path:         PathBuf,
    next_id:      u64,
    dirty:        bool,
    last_save:    Instant,
    text_set:     HashSet<String>,
}

impl HistoryStore {
    pub fn load() -> Self {
        let path = data_path();
        let mut entries: Vec<ClipEntry> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| {
                // Ensure UTF-8; silently discard corrupt/legacy files
                let s = String::from_utf8(bytes).ok()?;
                serde_json::from_str(&s).ok()
            })
            .unwrap_or_default();
        // Drop any image entries that survived (shouldn't happen, but be safe)
        entries.retain(|e| matches!(&e.content, ClipContent::Text { .. }));
        let today = Local::now().date_naive();
        for e in &mut entries {
            if let ClipContent::Text { text } = &e.content {
                e.text_lc = text.to_lowercase();
            }
            e.rebuild_time_str(today);
        }
        let text_set: HashSet<String> = entries.iter()
            .filter_map(|e| if let ClipContent::Text { text } = &e.content { Some(text.clone()) } else { None })
            .collect();
        let next_id = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        Self { entries, path, next_id, dirty: false, last_save: Instant::now(), text_set }
    }

    /// Add a new entry, deduplicating and trimming to 500 unpinned items.
    pub fn push(&mut self, content: ClipContent) {
        // Dedup: remove existing entry with same text in O(n) retain, then re-insert at top.
        if let ClipContent::Text { text: ref t } = content {
            if self.text_set.contains(t.as_str()) {
                let t_clone = t.clone();
                self.entries.retain(|e| !matches!(&e.content, ClipContent::Text { text } if text == &t_clone));
                self.text_set.remove(t.as_str());
            }
        }
        let entry = ClipEntry::new(self.next_id, content);
        self.next_id += 1;
        if let ClipContent::Text { text: ref t } = entry.content {
            self.text_set.insert(t.clone());
        }
        // Insert after all pinned entries
        let first_unpinned = self.entries.iter().position(|e| !e.pinned).unwrap_or(self.entries.len());
        self.entries.insert(first_unpinned, entry);
        // Trim unpinned to 500
        let mut unpinned = 0usize;
        self.entries.retain(|e| {
            if e.pinned { return true; }
            unpinned += 1;
            if unpinned > 500 {
                if let ClipContent::Text { text } = &e.content { self.text_set.remove(text.as_str()); }
                return false;
            }
            true
        });
        self.dirty = true;
    }

    pub fn remove(&mut self, id: u64) {
        if let Some(e) = self.entries.iter().find(|e| e.id == id) {
            if let ClipContent::Text { text } = &e.content { self.text_set.remove(text.as_str()); }
        }
        self.entries.retain(|e| e.id != id);
        self.dirty = true;
    }

    pub fn toggle_pin(&mut self, id: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.pinned = !e.pinned;
        }
        self.dirty = true;
    }

    pub fn clear_unpinned(&mut self) {
        self.entries.retain(|e| {
            if e.pinned { return true; }
            if let ClipContent::Text { text } = &e.content { self.text_set.remove(text.as_str()); }
            false
        });
        self.dirty = true;
    }

    /// Call once per frame / tick — flushes to disk if dirty and ≥2 s elapsed.
    pub fn flush_if_needed(&mut self) {
        if !self.dirty || self.last_save.elapsed().as_secs() < 2 { return; }
        self.flush_now();
    }

    pub fn flush_now(&mut self) {
        if !self.dirty { return; }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::File::create(&self.path) {
            let refs: Vec<&ClipEntry> = self.entries.iter()
                .filter(|e| matches!(e.content, ClipContent::Text { .. }))
                .collect();
            let _ = serde_json::to_writer(std::io::BufWriter::new(file), &refs);
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }
}

// ── Background monitor ────────────────────────────────────────────────────────

/// Spawn a background thread that polls the clipboard every 500 ms and sends
/// new content over `tx`. Returns immediately.
pub fn start_monitor(tx: std::sync::mpsc::Sender<ClipContent>) {
    std::thread::spawn(move || {
        let mut cb = None;
        for _ in 0..20 {
            match arboard::Clipboard::new() {
                Ok(c)  => { cb = Some(c); break; }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
            }
        }
        let mut cb = match cb {
            Some(c) => c,
            None    => return,
        };
        let mut last_text = String::new();
        let mut last_img_hash = 0u64;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if let Ok(fresh) = arboard::Clipboard::new() { cb = fresh; }
            match cb.get_text() {
                Ok(text) => {
                    let text = text.trim().to_string();
                    if !text.is_empty() && text != last_text {
                        last_text = text.clone();
                        last_img_hash = 0;
                        if tx.send(ClipContent::Text { text }).is_err() { return; }
                    }
                    continue;
                }
                Err(_) => {}
            }
            if let Ok(img) = cb.get_image() {
                let hash = fnv1a(&img.bytes);
                if hash != last_img_hash {
                    last_img_hash = hash;
                    last_text.clear();
                    if tx.send(ClipContent::Image {
                        width: img.width as u32, height: img.height as u32,
                        rgba: img.bytes.into_owned(),
                    }).is_err() { return; }
                }
            }
        }
    });
}

/// Fast non-cryptographic hash for image change detection (samples every Nth byte).
pub fn fnv1a(data: &[u8]) -> u64 {
    let step = (data.len() / 512).max(1);
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data.iter().step_by(step) {
        h ^= b as u64;
        h  = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn data_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rust-air")
        .join("clipboard-history.json")
}
