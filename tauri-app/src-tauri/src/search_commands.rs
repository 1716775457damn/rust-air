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
            .hidden(false)   // skip hidden files/dirs (e.g. .git internals)
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
                    if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                        return ignore::WalkState::Continue;
                    }
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
    let ranges = byte_ranges_to_char_ranges(name.as_ref(), re);
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
    let display = normalize_path(path);

    // Use mmap above threshold, direct read for small files.
    // Process each branch separately so the backing buffer lives long enough.
    if len >= MMAP_THRESHOLD {
        let mmap = unsafe { Mmap::map(&File::open(path)?)? };
        if is_binary(&mmap) { return Ok(None); }
        let content = decode_bytes(&mmap);
        Ok(collect_matches(&content, re, &display))
    } else {
        let bytes = std::fs::read(path)?;
        if is_binary(&bytes) { return Ok(None); }
        let content = decode_bytes(&bytes);
        Ok(collect_matches(&content, re, &display))
    }
}

/// Collect matching lines from decoded text content.
fn collect_matches(content: &str, re: &regex::Regex, display: &str) -> Option<FileResult> {
    const MAX_MATCHES_PER_FILE: usize = 100;
    let mut matches: Vec<MatchLine> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        // Single find_iter pass: collect ranges and check for match simultaneously.
        let ranges = byte_ranges_to_char_ranges(line, re);
        if ranges.is_empty() { continue; }
        let line_str = if line.len() <= MAX_LINE_LEN { line.to_owned() } else { truncate_line(line) };
        matches.push(MatchLine { line_num: i + 1, line: line_str, ranges });
        if matches.len() >= MAX_MATCHES_PER_FILE { break; }
    }
    if matches.is_empty() { return None; }
    Some(FileResult { icon: file_icon(display).to_string(), path: display.to_string(), matches })
}

/// Convert byte-offset regex matches to char-offset [start, end) ranges in one O(n) pass.
/// Avoids the O(n*m) cost of calling `line[..m.start()].chars().count()` per match.
fn byte_ranges_to_char_ranges(line: &str, re: &regex::Regex) -> Vec<(usize, usize)> {
    let byte_matches: Vec<_> = re.find_iter(line).collect();
    if byte_matches.is_empty() { return vec![]; }

    let mut ranges = Vec::with_capacity(byte_matches.len());
    let mut mi = 0usize;          // index into byte_matches
    let mut char_idx = 0usize;    // current char position

    for (byte_idx, ch) in line.char_indices() {
        while mi < byte_matches.len() && byte_matches[mi].start() == byte_idx {
            let m = &byte_matches[mi];
            let start_char = char_idx;
            let end_char   = start_char + line[m.start()..m.end()].chars().count();
            ranges.push((start_char, end_char));
            mi += 1;
        }
        if mi >= byte_matches.len() { break; }
        char_idx += 1;
        let _ = ch;
    }
    ranges
}

/// Normalise path separators to forward-slash for cross-platform display.
#[inline]
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Truncate a line to `MAX_LINE_LEN` chars, appending `…`.
fn truncate_line(line: &str) -> String {
    let end = line.char_indices()
        .nth(MAX_LINE_LEN)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    format!("{}…", &line[..end])
}

/// Heuristic binary detection on the first chunk.
/// Treats the file as binary if it contains null bytes OR if more than 30% of
/// sampled bytes are non-printable non-whitespace ASCII (common in compiled files).
#[inline]
fn is_binary(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(BINARY_CHECK_LEN)];
    if sample.contains(&0) { return true; }
    let non_text = sample.iter().filter(|&&b| b < 0x09 || (b > 0x0d && b < 0x20) || b == 0x7f).count();
    non_text * 10 > sample.len() * 3  // > 30%
}

/// Decode bytes as UTF-8; fall back to GBK. Avoids double-copy by borrowing
/// the input slice directly when it is valid UTF-8.
fn decode_bytes(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(s)  => std::borrow::Cow::Borrowed(s),
        Err(_) => std::borrow::Cow::Owned(GBK.decode(bytes).0.into_owned()),
    }
}

fn file_icon(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    if ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("msi") { return "⚙"; }
    if ext.eq_ignore_ascii_case("rs")   || ext.eq_ignore_ascii_case("py")
    || ext.eq_ignore_ascii_case("js")   || ext.eq_ignore_ascii_case("ts")
    || ext.eq_ignore_ascii_case("go")   || ext.eq_ignore_ascii_case("c")
    || ext.eq_ignore_ascii_case("cpp")  || ext.eq_ignore_ascii_case("java")
    || ext.eq_ignore_ascii_case("cs")   || ext.eq_ignore_ascii_case("rb")
    || ext.eq_ignore_ascii_case("swift")                                   { return "📝"; }
    if ext.eq_ignore_ascii_case("toml") || ext.eq_ignore_ascii_case("json")
    || ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
    || ext.eq_ignore_ascii_case("xml")  || ext.eq_ignore_ascii_case("ini")
    || ext.eq_ignore_ascii_case("cfg")  || ext.eq_ignore_ascii_case("env") { return "🔧"; }
    if ext.eq_ignore_ascii_case("md")   || ext.eq_ignore_ascii_case("txt")
    || ext.eq_ignore_ascii_case("log")                                     { return "📄"; }
    if ext.eq_ignore_ascii_case("png")  || ext.eq_ignore_ascii_case("jpg")
    || ext.eq_ignore_ascii_case("jpeg") || ext.eq_ignore_ascii_case("gif")
    || ext.eq_ignore_ascii_case("svg")  || ext.eq_ignore_ascii_case("ico")
    || ext.eq_ignore_ascii_case("bmp")  || ext.eq_ignore_ascii_case("webp"){ return "🖼"; }
    if ext.eq_ignore_ascii_case("mp4")  || ext.eq_ignore_ascii_case("mkv")
    || ext.eq_ignore_ascii_case("avi")  || ext.eq_ignore_ascii_case("mov") { return "🎬"; }
    if ext.eq_ignore_ascii_case("mp3")  || ext.eq_ignore_ascii_case("wav")
    || ext.eq_ignore_ascii_case("flac") || ext.eq_ignore_ascii_case("ogg") { return "🎵"; }
    if ext.eq_ignore_ascii_case("zip")  || ext.eq_ignore_ascii_case("rar")
    || ext.eq_ignore_ascii_case("7z")   || ext.eq_ignore_ascii_case("tar")
    || ext.eq_ignore_ascii_case("gz")   || ext.eq_ignore_ascii_case("xz")  { return "📦"; }
    if ext.eq_ignore_ascii_case("pdf")                                     { return "📕"; }
    if ext.eq_ignore_ascii_case("db")   || ext.eq_ignore_ascii_case("sqlite")
    || ext.eq_ignore_ascii_case("sql")                                     { return "🗄"; }
    "📄"
}
