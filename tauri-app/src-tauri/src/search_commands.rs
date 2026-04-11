use encoding_rs::GBK;
use ignore::WalkBuilder;
use memmap2::Mmap;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::{fs::File, path::Path, sync::{atomic::{AtomicBool, Ordering}, Arc}, thread};
use tauri::{AppHandle, Emitter, State};
use std::sync::Mutex;

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
    Done { ms: u128, total: usize },
    Error { msg: String },
}

// ── State ─────────────────────────────────────────────────────────────────────

pub struct SearchState {
    pub cancel: Mutex<Option<Arc<AtomicBool>>>,
}

impl SearchState {
    pub fn new() -> Self { Self { cancel: Mutex::new(None) } }
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn start_search(
    pattern: String,
    path: String,
    ignore_case: bool,
    fixed_string: bool,
    mode: String,          // "filename" | "text"
    app: AppHandle,
    state: State<'_, SearchState>,
) -> Result<(), String> {
    // Cancel any running search
    if let Some(c) = state.cancel.lock().unwrap().take() {
        c.store(true, Ordering::Relaxed);
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
    *state.cancel.lock().unwrap() = Some(cancelled.clone());

    let is_text = mode == "text";
    let threads = num_cpus::get();

    thread::spawn(move || {
        let start = std::time::Instant::now();
        let mut total = 0usize;

        let walker = WalkBuilder::new(&path)
            .hidden(true)
            .git_ignore(false)
            .ignore(false)
            .threads(threads)
            .build_parallel();

        // Use a channel to collect results from parallel walker
        let (tx, rx) = std::sync::mpsc::channel::<FileResult>();
        let cancelled2 = cancelled.clone();

        walker.run(|| {
            let tx = tx.clone();
            let re = re.clone();
            let cancelled = cancelled2.clone();
            Box::new(move |entry| {
                if cancelled.load(Ordering::Relaxed) { return ignore::WalkState::Quit; }
                let entry = match entry { Ok(e) => e, Err(_) => return ignore::WalkState::Continue };
                let result = if is_text {
                    if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                        return ignore::WalkState::Continue;
                    }
                    search_file(entry.path(), &re, 10 * 1024 * 1024).ok().flatten()
                } else {
                    search_filename(entry.path(), &re)
                };
                if let Some(r) = result {
                    let _ = tx.send(r);
                }
                ignore::WalkState::Continue
            })
        });
        drop(tx); // signal end

        // Forward results to frontend
        while let Ok(r) = rx.recv() {
            if cancelled.load(Ordering::Relaxed) { break; }
            total += 1;
            let _ = app.emit("search-result", SearchEvent::Result(r));
            if total >= 2000 { break; }
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
    if let Some(c) = state.cancel.lock().unwrap().take() {
        c.store(true, Ordering::Relaxed);
    }
}

// ── Search logic (from rust-seek) ─────────────────────────────────────────────

fn search_filename(path: &Path, re: &regex::Regex) -> Option<FileResult> {
    let name = path.file_name()?.to_string_lossy();
    let ranges: Vec<(usize, usize)> = re.find_iter(name.as_ref())
        .map(|m| (m.start(), m.end())).collect();
    if ranges.is_empty() { return None; }
    let display = path.to_string_lossy().replace('\\', "/");
    Some(FileResult {
        icon: file_icon(&display).to_string(),
        path: display,
        matches: vec![MatchLine { line_num: 0, line: name.into_owned(), ranges }],
    })
}

fn search_file(path: &Path, re: &regex::Regex, max_size: u64) -> anyhow::Result<Option<FileResult>> {
    let meta = std::fs::metadata(path)?;
    let len = meta.len();
    if len == 0 || len > max_size { return Ok(None); }

    let content = if len < 4096 {
        let bytes = std::fs::read(path)?;
        if is_binary(&bytes) { return Ok(None); }
        decode_str(&bytes)
    } else {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        if is_binary(&mmap) { return Ok(None); }
        decode_str(&mmap)
    };

    let matches: Vec<MatchLine> = content.lines().enumerate()
        .filter_map(|(i, line)| {
            let ranges: Vec<_> = re.find_iter(line).map(|m| (m.start(), m.end())).collect();
            if ranges.is_empty() { return None; }
            let line = if line.len() > 512 { line[..512].to_string() + "…" } else { line.to_string() };
            Some(MatchLine { line_num: i + 1, line, ranges })
        })
        .collect();

    if matches.is_empty() { return Ok(None); }
    let display = path.to_string_lossy().replace('\\', "/");
    Ok(Some(FileResult { icon: file_icon(&display).to_string(), path: display, matches }))
}

fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(1024)].contains(&0)
}

fn decode_str(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => { let (cow, _, _) = GBK.decode(bytes); cow.into_owned() }
    }
}

fn file_icon(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "exe" | "msi"                         => "⚙",
        "rs" | "py" | "js" | "ts" | "go"
        | "c" | "cpp" | "java" | "cs"         => "📝",
        "toml" | "json" | "yaml" | "yml"
        | "xml" | "ini" | "cfg"               => "🔧",
        "md" | "txt" | "log"                  => "📄",
        "png" | "jpg" | "jpeg" | "gif"
        | "svg" | "ico" | "bmp"               => "🖼",
        "mp4" | "mkv" | "avi"                 => "🎬",
        "mp3" | "wav" | "flac"                => "🎵",
        "zip" | "rar" | "7z" | "tar" | "gz"  => "📦",
        "pdf"                                 => "📕",
        _                                     => "📄",
    }
}
