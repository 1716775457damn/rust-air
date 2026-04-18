use encoding_rs::GBK;
use ignore::WalkBuilder;
use memmap2::Mmap;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};
use tauri::{AppHandle, Emitter, State};

const MAX_RESULTS: usize = 2000;
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024; // 50 MB
const MMAP_THRESHOLD: u64 = 32 * 1024;       // use mmap above 32 KB
const MAX_LINE_LEN: usize = 512;
const BINARY_CHECK_LEN: usize = 8 * 1024;    // check first 8 KB for binary

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct MatchLine {
    pub line_num: usize,
    pub line:     String,
    pub ranges:   Vec<(usize, usize)>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileResult {
    pub path:    String,
    pub icon:    String,
    pub matches: Vec<MatchLine>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind")]
pub enum SearchEvent {
    Result(FileResult),
    Done  { ms: u128, total: usize },
    Error { msg: String },
}

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct SearchState {
    cancel: Mutex<Option<Arc<AtomicBool>>>,
}

impl SearchState {
    pub fn new() -> Self { Self::default() }

    fn set_cancel(&self, flag: Arc<AtomicBool>) {
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(flag);
    }

    fn take_cancel(&self) -> Option<Arc<AtomicBool>> {
        self.cancel.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn start_search(
    pattern:      String,
    path:         String,
    ignore_case:  bool,
    fixed_string: bool,
    mode:         String, // "filename" | "text"
    app:          AppHandle,
    state:        State<'_, SearchState>,
) -> Result<(), String> {
    // Cancel any in-flight search before starting a new one
    if let Some(prev) = state.take_cancel() {
        prev.store(true, Ordering::Relaxed);
    }

    if pattern.is_empty() { return Ok(()); }
    if !Path::new(&path).exists() {
        return Err(format!("路径不存在: {path}"));
    }

    let pat = if fixed_string { regex::escape(&pattern) } else { pattern };
    let re = RegexBuilder::new(&pat)
        .case_insensitive(ignore_case)
        .unicode(true)
        .build()
        .map_err(|e| format!("正则错误: {e}"))?;

    let cancelled = Arc::new(AtomicBool::new(false));
    state.set_cancel(cancelled.clone());

    let is_text = mode == "text";
    let threads = num_cpus::get().min(8); // cap at 8 to avoid thrashing on large machines

    thread::spawn(move || {
        let start = std::time::Instant::now();
        let mut total = 0usize;

        let walker = WalkBuilder::new(&path)
            .hidden(true)
            .git_ignore(false)
            .ignore(false)
            .filter_entry(|e| {
                // Skip known large/irrelevant directories early.
                let name = e.file_name().to_string_lossy();
                !matches!(name.as_ref(),
                    "node_modules" | ".git" | ".svn" | "target" |
                    ".cache" | "__pycache__" | ".next" | "dist" | "build"
                )
            })
            .threads(threads)
            .build_parallel();

        let (tx, rx) = std::sync::mpsc::channel::<FileResult>();
        let cancelled2 = cancelled.clone();

        walker.run(|| {
            let tx        = tx.clone();
            let re        = re.clone();
            let cancelled = cancelled2.clone();
            Box::new(move |entry| {
                if cancelled.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                let entry = match entry {
                    Ok(e)  => e,
                    Err(_) => return ignore::WalkState::Continue,
                };
                let result = if is_text {
                    if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                        return ignore::WalkState::Continue;
                    }
                    search_file(entry.path(), &re).ok().flatten()
                } else {
                    search_filename(entry.path(), &re)
                };
                if let Some(r) = result {
                    let _ = tx.send(r);
                }
                ignore::WalkState::Continue
            })
        });
        drop(tx);

        // Batch results: emit up to 50 at a time to reduce IPC round-trips.
        let mut batch: Vec<FileResult> = Vec::with_capacity(50);
        while let Ok(r) = rx.recv() {
            if cancelled.load(Ordering::Relaxed) { break; }
            total += 1;
            batch.push(r);
            if batch.len() >= 50 || total >= MAX_RESULTS {
                let _ = app.emit("search-batch", &batch);
                batch.clear();
            }
            if total >= MAX_RESULTS { break; }
        }
        if !batch.is_empty() {
            let _ = app.emit("search-batch", &batch);
        }

        if !cancelled.load(Ordering::Relaxed) {
            let _ = app.emit("search-result", SearchEvent::Done {
                ms: start.elapsed().as_millis(),
                total,
            });
        }
    });

    Ok(())
}

#[tauri::command]
pub fn cancel_search(state: State<'_, SearchState>) {
    if let Some(c) = state.take_cancel() {
        c.store(true, Ordering::Relaxed);
    }
}

// ── Search helpers ────────────────────────────────────────────────────────────

fn search_filename(path: &Path, re: &regex::Regex) -> Option<FileResult> {
    let name = path.file_name()?.to_string_lossy();
    let ranges: Vec<(usize, usize)> = re.find_iter(name.as_ref())
        .map(|m| {
            let start = name[..m.start()].chars().count();
            let end   = start + name[m.start()..m.end()].chars().count();
            (start, end)
        })
        .collect();
    if ranges.is_empty() { return None; }
    let display = normalize_path(path);
    Some(FileResult {
        icon: file_icon(&display).to_string(),
        path: display,
        matches: vec![MatchLine { line_num: 0, line: name.into_owned(), ranges }],
    })
}

fn search_file(path: &Path, re: &regex::Regex) -> anyhow::Result<Option<FileResult>> {
    let len = std::fs::metadata(path)?.len();
    if len == 0 || len > MAX_FILE_SIZE { return Ok(None); }

    // Use mmap above threshold, direct read for small files.
    let content = if len >= MMAP_THRESHOLD {
        let mmap = unsafe { Mmap::map(&File::open(path)?)? };
        if is_binary(&mmap) { return Ok(None); }
        decode_bytes(&mmap)
    } else {
        let bytes = std::fs::read(path)?;
        if is_binary(&bytes) { return Ok(None); }
        decode_bytes(&bytes)
    };

    // Stream lines; stop early once we have enough matches per file.
    const MAX_MATCHES_PER_FILE: usize = 100;
    let mut matches: Vec<MatchLine> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let ranges: Vec<_> = re.find_iter(line)
            .map(|m| {
                let start = line[..m.start()].chars().count();
                let end   = start + line[m.start()..m.end()].chars().count();
                (start, end)
            })
            .collect();
        if ranges.is_empty() { continue; }
        matches.push(MatchLine { line_num: i + 1, line: truncate_line(line), ranges });
        if matches.len() >= MAX_MATCHES_PER_FILE { break; }
    }

    if matches.is_empty() { return Ok(None); }
    let display = normalize_path(path);
    Ok(Some(FileResult { icon: file_icon(&display).to_string(), path: display, matches }))
}

/// Normalise path separators to forward-slash for cross-platform display.
#[inline]
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Truncate a line at `MAX_LINE_LEN` chars, respecting char boundaries.
fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_LEN {
        return line.to_string();
    }
    let end = line.char_indices()
        .nth(MAX_LINE_LEN)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    format!("{}…", &line[..end])
}

/// Heuristic binary detection: look for null bytes in the first chunk.
#[inline]
fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(BINARY_CHECK_LEN)].contains(&0)
}

/// Decode bytes as UTF-8; fall back to GBK for Windows legacy files.
fn decode_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s)  => s.to_string(),
        Err(_) => GBK.decode(bytes).0.into_owned(),
    }
}

fn file_icon(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "exe" | "msi"                                    => "⚙",
        "rs" | "py" | "js" | "ts" | "go"
        | "c" | "cpp" | "java" | "cs" | "rb" | "swift"  => "📝",
        "toml" | "json" | "yaml" | "yml"
        | "xml" | "ini" | "cfg" | "env"                  => "🔧",
        "md" | "txt" | "log"                             => "📄",
        "png" | "jpg" | "jpeg" | "gif"
        | "svg" | "ico" | "bmp" | "webp"                 => "🖼",
        "mp4" | "mkv" | "avi" | "mov"                    => "🎬",
        "mp3" | "wav" | "flac" | "ogg"                   => "🎵",
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz"      => "📦",
        "pdf"                                            => "📕",
        "db" | "sqlite" | "sql"                          => "🗄",
        _                                                => "📄",
    }
}
