//! File sync/backup engine — ported from sync-vault.
//!
//! Provides:
//! - `SyncConfig` / `SyncStore`  — persisted config and state
//! - `full_sync`                 — one-shot incremental sync (SHA-256 based)
//! - `start_watcher`             — file-system watcher with debounce
//! - `SyncEvent`                 — progress/result events

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    time::Instant,
};
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;

/// NFD → NFC: macOS HFS+ stores paths in NFD; compose to NFC for correct display.
#[inline]
fn nfc(s: &str) -> String {
    if s.is_ascii() { return s.to_owned(); }
    s.nfc().collect()
}

// ── Public event type ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SyncEvent {
    Copied   { rel: String, bytes: u64 },
    Deleted  { rel: String },
    Error    { rel: String, err: String },
    Progress { scanned: usize, total: usize },
    Done     { total_files: u64, total_bytes: u64 },
}

// ── Persisted state ───────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub hash:     String,
    pub size:     u64,
    pub modified: DateTime<Local>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct SyncState {
    pub files:         HashMap<String, FileRecord>,
    pub last_sync:     Option<DateTime<Local>>,
    pub total_synced:  u64,
    pub total_bytes:   u64,
}

/// User-facing configuration (persisted separately from state).
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub src:            String,
    pub dst:            String,
    pub delete_removed: bool,
    pub excludes:       Vec<String>,
    pub auto_watch:     bool,
}

impl SyncConfig {
    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        if let Some(p) = config_path() {
            if let Some(parent) = p.parent() { let _ = std::fs::create_dir_all(parent); }
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, json);
            }
        }
    }
}

pub struct SyncStore {
    pub state:     SyncState,
    path:          PathBuf,
    dirty:         bool,
    last_save:     Instant,
}

impl SyncStore {
    pub fn load() -> Self {
        let path  = state_path();
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { state, path, dirty: false, last_save: Instant::now() }
    }

    pub fn mark_dirty(&mut self) { self.dirty = true; }

    pub fn flush_if_needed(&mut self) {
        if self.dirty && self.last_save.elapsed().as_secs() >= 3 {
            self.flush_now();
        }
    }

    pub fn flush_now(&mut self) {
        if !self.dirty { return; }
        if let Some(parent) = self.path.parent() { let _ = std::fs::create_dir_all(parent); }
        if let Ok(file) = std::fs::File::create(&self.path) {
            let _ = serde_json::to_writer(std::io::BufWriter::new(file), &self.state);
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }
}

// ── Sync engine ───────────────────────────────────────────────────────────────

/// Run a full incremental sync from `src` to `dst`.
/// Sends `SyncEvent`s over `tx` for progress reporting.
pub fn full_sync(
    src: &Path,
    dst: &Path,
    store: &mut SyncStore,
    delete_removed: bool,
    excludes: &[String],
    tx: &Sender<SyncEvent>,
) {
    let (to_copy, seen) = scan_needed(src, store, excludes, tx);

    for (rel, abs, size, hash) in to_copy {
        let dst_path = dst.join(&rel);
        atomic_copy(&abs, &dst_path, &rel, hash, size, store, tx);
    }

    if delete_removed {
        // Collect keys to remove before mutating the map.
        let removed: Vec<String> = store.state.files.keys()
            .filter(|k| !seen.contains(*k))
            .cloned()
            .collect();
        for rel in removed {
            let _ = std::fs::remove_file(dst.join(&rel));
            store.state.files.remove(&rel);
            store.mark_dirty();
            let _ = tx.send(SyncEvent::Deleted { rel });
        }
    }

    store.state.last_sync = Some(Local::now());
    store.mark_dirty();
    let _ = tx.send(SyncEvent::Done {
        total_files: store.state.total_synced,
        total_bytes: store.state.total_bytes,
    });
}

/// Sync a single file (called from the watcher on change events).
/// Accepts a pre-built `ExcludeSet` so callers that process a batch of files
/// don't rebuild it on every call.
pub fn sync_file(
    abs: &Path,
    src: &Path,
    dst: &Path,
    store: &mut SyncStore,
    excludes: &ExcludeSet,
    tx: &Sender<SyncEvent>,
) {
    let rel_path = match abs.strip_prefix(src) { Ok(r) => r, Err(_) => return };
    let rel = nfc(&rel_path.to_string_lossy().replace('\\', "/"));
    if excludes.matches(&rel) { return; }

    let dst_path = dst.join(rel_path);

    if !abs.exists() {
        let _ = std::fs::remove_file(&dst_path);
        store.state.files.remove(&rel);
        store.mark_dirty();
        let _ = tx.send(SyncEvent::Deleted { rel });
        return;
    }

    let size = match std::fs::metadata(abs) {
        Ok(m) => m.len(),
        Err(e) => { let _ = tx.send(SyncEvent::Error { rel, err: e.to_string() }); return; }
    };

    if let Some(rec) = store.state.files.get(&rel) {
        if rec.size == size {
            match hash_file(abs) {
                Ok(h) if h == rec.hash => return, // unchanged
                Ok(h) => { atomic_copy(abs, &dst_path, &rel, h, size, store, tx); return; }
                Err(e) => { let _ = tx.send(SyncEvent::Error { rel, err: e.to_string() }); return; }
            }
        }
    }

    match hash_file(abs) {
        Ok(h) => atomic_copy(abs, &dst_path, &rel, h, size, store, tx),
        Err(e) => { let _ = tx.send(SyncEvent::Error { rel, err: e.to_string() }); }
    }
}

// ── File watcher ──────────────────────────────────────────────────────────────

/// Start a debounced file-system watcher on `src`.
/// Changed paths are batched and sent over `tx` after a 300 ms quiet period.
/// Returns the watcher handle — drop it to stop watching.
pub fn start_watcher(
    src: PathBuf,
    tx: Sender<Vec<PathBuf>>,
) -> anyhow::Result<notify::RecommendedWatcher> {
    use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const DEBOUNCE_MS: u64 = 300;

    let pending: Arc<Mutex<HashMap<PathBuf, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let pending_flush = pending.clone();
    let tx_flush = tx.clone();
    // stop_tx is held by the watcher; dropping the watcher closes the channel,
    // which causes the debounce thread to exit.
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    std::thread::spawn(move || {
        loop {
            // Block for up to DEBOUNCE_MS waiting for a stop signal.
            // This replaces the 100ms busy-poll with a true sleep-until-event.
            match stop_rx.recv_timeout(Duration::from_millis(DEBOUNCE_MS)) {
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            let mut map = pending_flush.lock().unwrap();
            let ready: Vec<PathBuf> = map.iter()
                .filter(|(_, t)| t.elapsed() >= Duration::from_millis(DEBOUNCE_MS))
                .map(|(p, _)| p.clone())
                .collect();
            if !ready.is_empty() {
                for p in &ready { map.remove(p); }
                drop(map);
                if tx_flush.send(ready).is_err() { return; }
            }
        }
    });

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            // Keep stop_tx alive inside the closure so it's dropped with the watcher.
            let _keep = &stop_tx;
            if let Ok(event) = res {
                if !matches!(event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) { return; }
                let now = Instant::now();
                let mut map = pending.lock().unwrap();
                for path in event.paths { map.insert(path, now); }
            }
        },
        Config::default(),
    )?;
    watcher.watch(&src, RecursiveMode::Recursive)?;
    Ok(watcher)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn hash_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn fmt_bytes(b: u64) -> String {
    if b < 1024 { format!("{} B", b) }
    else if b < 1_048_576 { format!("{:.1} KB", b as f64 / 1024.0) }
    else if b < 1_073_741_824 { format!("{:.1} MB", b as f64 / 1_048_576.0) }
    else { format!("{:.2} GB", b as f64 / 1_073_741_824.0) }
}

pub fn default_excludes() -> Vec<String> {
    vec![
        ".git".into(), ".svn".into(), "node_modules".into(),
        "__pycache__".into(), "target".into(), ".DS_Store".into(),
        "Thumbs.db".into(), "*.tmp".into(), "*.swp".into(),
    ]
}

fn scan_needed(
    src: &Path,
    store: &SyncStore,
    excludes: &[String],
    tx: &Sender<SyncEvent>,
) -> (Vec<(String, PathBuf, u64, String)>, HashSet<String>) {
    let ex = ExcludeSet::new(excludes);
    let mut seen    = HashSet::new();
    let mut scanned = 0usize;
    // Candidates that passed the size+mtime fast-path and need hashing.
    let mut need_hash: Vec<(String, PathBuf, u64)> = Vec::new();

    for entry in WalkDir::new(src).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() { continue; }
        let abs = entry.path();
        let rel = match abs.strip_prefix(src) {
            Ok(r) => nfc(&r.to_string_lossy().replace('\\', "/")),
            Err(_) => continue,
        };
        scanned += 1;
        if scanned % 100 == 0 { let _ = tx.send(SyncEvent::Progress { scanned, total: 0 }); }
        if ex.matches(&rel) { continue; }
        seen.insert(rel.clone());

        let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
        let size = meta.len();

        // Fast path: skip hash if size AND mtime both match the record.
        if let Some(rec) = store.state.files.get(&rel) {
            if rec.size == size {
                let current_secs = meta.modified()
                    .ok()
                    .map(|t| chrono::DateTime::<Local>::from(t).timestamp())
                    .unwrap_or(0);
                if rec.modified.timestamp() == current_secs {
                    continue;
                }
            }
        }
        need_hash.push((rel, abs.to_path_buf(), size));
    }
    let _ = tx.send(SyncEvent::Progress { scanned, total: scanned });

    // Parallel hash of all candidates.
    // Pre-extract cached records to avoid borrowing store inside par_iter.
    use rayon::prelude::*;
    let cached: std::collections::HashMap<String, (u64, String)> = need_hash.iter()
        .filter_map(|(rel, _, size)| {
            store.state.files.get(rel)
                .filter(|rec| rec.size == *size)
                .map(|rec| (rel.clone(), (*size, rec.hash.clone())))
        })
        .collect();

    let to_copy: Vec<(String, PathBuf, u64, String)> = need_hash
        .into_par_iter()
        .filter_map(|(rel, abs, size)| {
            let hash = hash_file(&abs).ok()?;
            if let Some((_, cached_hash)) = cached.get(&rel) {
                if *cached_hash == hash { return None; }
            }
            Some((rel, abs, size, hash))
        })
        .collect();

    (to_copy, seen)
}

fn atomic_copy(
    src: &Path, dst: &Path, rel: &str,
    hash: String, size: u64,
    store: &mut SyncStore, tx: &Sender<SyncEvent>,
) {
    if let Some(p) = dst.parent() { let _ = std::fs::create_dir_all(p); }
    let tmp = dst.with_file_name(format!(
        "{}.svtmp", dst.file_name().unwrap_or_default().to_string_lossy()
    ));
    match std::fs::copy(src, &tmp) {
        Ok(bytes) => {
            if let Err(e) = std::fs::rename(&tmp, dst) {
                let _ = std::fs::remove_file(&tmp);
                let _ = tx.send(SyncEvent::Error { rel: rel.to_string(), err: e.to_string() });
                return;
            }
            store.state.files.insert(rel.to_string(), FileRecord {
                hash, size, modified: Local::now(),
            });
            store.state.total_synced += 1;
            store.state.total_bytes  += bytes;
            store.mark_dirty();
            let _ = tx.send(SyncEvent::Copied { rel: rel.to_string(), bytes });
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            let _ = tx.send(SyncEvent::Error { rel: rel.to_string(), err: e.to_string() });
        }
    }
}

pub struct ExcludeSet {
    exact: HashSet<String>,
    exts:  HashSet<String>,
}

impl ExcludeSet {
    fn new(excludes: &[String]) -> Self {
        Self {
            exact: excludes.iter().filter(|p| !p.starts_with("*.")).cloned().collect(),
            exts:  excludes.iter().filter_map(|p| p.strip_prefix("*.").map(|s| s.to_string())).collect(),
        }
    }
    fn matches(&self, rel: &str) -> bool {
        rel.split('/').any(|seg| {
            if self.exact.contains(seg) { return true; }
            // Extract extension from segment and check the set.
            if let Some(dot) = seg.rfind('.') {
                if dot > 0 { return self.exts.contains(&seg[dot + 1..]); }
            }
            false
        })
    }
}

fn state_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rust-air").join("sync-state.json")
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("rust-air").join("sync-config.json"))
}
