//! Tauri IPC commands for file sync/backup (sync-vault).

use rust_air_core::{
    transfer,
    default_excludes, fmt_bytes, full_sync, start_watcher,
    SyncAction, SyncConfig, SyncEvent, SyncManifestEntry, SyncStore,
};
use rust_air_core::proto::{ArchiveStatus, ArchiveStatusCode, TransferEvent};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

// ── State ─────────────────────────────────────────────────────────────────────

pub struct SyncState {
    store:   Mutex<SyncStore>,
    config:  Mutex<SyncConfig>,
    watch_session: Mutex<Option<WatchSession>>,
    running: Arc<AtomicBool>,
    pending_remote_sync: Mutex<HashMap<String, oneshot::Sender<RemoteSyncResponse>>>,
    pending_remote_files: Mutex<HashMap<String, oneshot::Sender<Result<PathBuf, String>>>>,
}

struct WatchSession {
    watcher: notify::RecommendedWatcher,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl WatchSession {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        drop(self.watcher);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl SyncState {
    pub fn new() -> Self {
        let config = SyncConfig::load();
        Self {
            store:   Mutex::new(SyncStore::load()),
            config:  Mutex::new(config),
            watch_session: Mutex::new(None),
            running: Arc::new(AtomicBool::new(false)),
            pending_remote_sync: Mutex::new(HashMap::new()),
            pending_remote_files: Mutex::new(HashMap::new()),
        }
    }

    fn register_pending_remote_sync(&self, request_id: String) -> oneshot::Receiver<RemoteSyncResponse> {
        let (tx, rx) = oneshot::channel();
        self.pending_remote_sync
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        rx
    }

    fn resolve_pending_remote_sync(&self, response: RemoteSyncResponse) {
        if let Some(tx) = self.pending_remote_sync
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&response.request_id)
        {
            let _ = tx.send(response);
        }
    }

    fn register_pending_remote_file(&self, request_id: String) -> oneshot::Receiver<Result<PathBuf, String>> {
        let (tx, rx) = oneshot::channel();
        self.pending_remote_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        rx
    }

    pub(crate) fn resolve_pending_remote_file(&self, request_id: String, result: Result<PathBuf, String>) {
        if let Some(tx) = self.pending_remote_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id)
        {
            let _ = tx.send(result);
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

#[derive(Serialize, Deserialize)]
struct RemoteSyncRequest {
    request_id: String,
    manifest: Vec<SyncManifestEntry>,
    callback_addr: String,
    excludes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RemoteSyncResponse {
    request_id: String,
    manifest: Vec<SyncManifestEntry>,
    ready: bool,
    ready_reason: Option<String>,
    scanned_files: usize,
    hashed_files: usize,
    cached_files: usize,
}

#[derive(Serialize, Deserialize)]
struct RemoteSyncFileRequest {
    request_id: String,
    entry: SyncManifestEntry,
    callback_addr: String,
}

#[derive(Serialize, Deserialize)]
struct RemoteSyncDeleteRequest {
    tombstone: SyncManifestEntry,
}

#[derive(Serialize, Deserialize)]
struct RemoteSyncFileError {
    request_id: String,
    rel: String,
    err: String,
}

pub fn archive_status_detail_to_sync_event(status: &ArchiveStatus, rel: &str) -> Option<SyncEvent> {
    match status.code {
        ArchiveStatusCode::ResumeRejectedSafetyRestart => Some(SyncEvent::Info {
            msg: format!("⇆ 目录传输安全重启: {}", status.detail.clone().unwrap_or_else(|| rel.to_string())),
        }),
        ArchiveStatusCode::ParallelDisabledForResume => Some(SyncEvent::Info {
            msg: format!("⇆ 已切换为保守目录传输: {}", status.detail.clone().unwrap_or_else(|| rel.to_string())),
        }),
        ArchiveStatusCode::UnpackStarted => Some(SyncEvent::Info {
            msg: format!("⇠ 正在解包目录: {}", status.detail.clone().unwrap_or_else(|| rel.to_string())),
        }),
        ArchiveStatusCode::UnpackFinished => Some(SyncEvent::Info {
            msg: format!("⇠ 目录解包完成: {}", status.detail.clone().unwrap_or_else(|| rel.to_string())),
        }),
        ArchiveStatusCode::UnpackFailed => Some(SyncEvent::Error {
            rel: rel.to_string(),
            err: status.detail.clone().unwrap_or_else(|| "目录解包失败".to_string()),
        }),
    }
}

pub fn archive_status_to_sync_event(ev: &TransferEvent, rel: &str) -> Option<SyncEvent> {
    let status = ev.archive_status.as_ref()?;
    archive_status_detail_to_sync_event(status, rel)
}

fn encode_sync_file_logical_name(request_id: &str, entry: &SyncManifestEntry) -> String {
    format!(
        "sync:file:{request_id}:{}:{}:{}:{}",
        entry.modified_ts,
        entry.size,
        entry.hash,
        entry.rel
    )
}

fn build_entry_from_metadata(rel: &str, size: u64, modified_ts: i64, hash: String) -> SyncManifestEntry {
    SyncManifestEntry {
        rel: rel.to_string(),
        size,
        modified_ts,
        hash,
        deleted: false,
    }
}

fn decode_sync_file_logical_name(logical_name: &str) -> Result<(String, SyncManifestEntry), String> {
    let payload = logical_name
        .strip_prefix("sync:file:")
        .ok_or_else(|| "同步文件头格式无效".to_string())?;
    let mut parts = payload.splitn(5, ':');
    let request_id = parts.next().unwrap_or_default().to_string();
    let modified_ts = parts
        .next()
        .ok_or_else(|| "同步文件头缺少修改时间".to_string())?
        .parse::<i64>()
        .map_err(|_| "同步文件头中的修改时间无效".to_string())?;
    let size = parts
        .next()
        .ok_or_else(|| "同步文件头缺少文件大小".to_string())?
        .parse::<u64>()
        .map_err(|_| "同步文件头中的文件大小无效".to_string())?;
    let hash = parts
        .next()
        .ok_or_else(|| "同步文件头缺少文件摘要".to_string())?
        .to_string();
    let rel = parts
        .next()
        .ok_or_else(|| "同步文件头缺少相对路径".to_string())?
        .to_string();

    Ok((
        request_id,
        SyncManifestEntry {
            rel,
            size,
            modified_ts,
            hash,
            deleted: false,
        },
    ))
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
    let running = state.running.load(Ordering::Relaxed);
    let watching = state.watch_session.lock().unwrap_or_else(|e| e.into_inner()).is_some();
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

#[tauri::command]
pub async fn start_remote_sync(
    remote_addr: String,
    callback_addr: String,
    state: State<'_, SyncState>,
    app: AppHandle,
) -> Result<(), String> {
    if state.running.swap(true, Ordering::AcqRel) {
        return Err("同步任务正在进行中，请稍后再试".into());
    }

    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() {
        state.running.store(false, Ordering::Release);
        return Err("请先选择本机同步目录".into());
    }
    if remote_addr.trim().is_empty() {
        state.running.store(false, Ordering::Release);
        return Err("请先填写远端设备地址".into());
    }
    if callback_addr.trim().is_empty() {
        state.running.store(false, Ordering::Release);
        return Err("本机监听地址不可用，请稍后重试".into());
    }
    if remote_addr.trim() == callback_addr.trim() {
        state.running.store(false, Ordering::Release);
        return Err("远端设备地址不能与本机监听地址相同".into());
    }

    let src = PathBuf::from(&config.src);
    let excludes = config.excludes.clone();
    app.emit("sync-event", SyncEvent::Phase {
        phase: "scan".to_string(),
        detail: "扫描本机目录并构建同步清单".to_string(),
    }).ok();
    let (local_manifest, local_stats) = {
        let local_store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        rust_air_core::sync_vault::build_manifest_with_state_and_stats(
            &src,
            &local_store,
            &excludes,
            config.delete_removed,
        )
    };
    app.emit("sync-event", SyncEvent::Stats {
        label: "本机清单".to_string(),
        scanned_files: local_stats.scanned_files,
        hashed_files: local_stats.hashed_files,
        cached_files: local_stats.cached_files,
    }).ok();
    let request_id = uuid::Uuid::new_v4().to_string();
    let request = RemoteSyncRequest {
        request_id: request_id.clone(),
        manifest: local_manifest.clone(),
        callback_addr,
        excludes: excludes.clone(),
    };
    let response_rx = state.register_pending_remote_sync(request_id.clone());

    let json = serde_json::to_string(&request).map_err(|e| e.to_string())?;
    let stream = tokio::net::TcpStream::connect(&remote_addr).await.map_err(|e| e.to_string())?;
    transfer::send_clipboard(stream, &json, "sync:manifest-request", |_| {}).await.map_err(|e| e.to_string())?;

    let response = tokio::time::timeout(std::time::Duration::from_secs(20), response_rx)
        .await
        .map_err(|_| "等待远端同步清单响应超时".to_string())?
        .map_err(|_| "远端同步清单响应通道已关闭".to_string())?;

    if !response.ready {
        state.running.store(false, Ordering::Release);
        return Err(response
            .ready_reason
            .unwrap_or_else(|| "远端尚未完成双机同步准备，请先在对方设备选择同步目录".to_string()));
    }
    app.emit("sync-event", SyncEvent::Stats {
        label: "远端清单".to_string(),
        scanned_files: response.scanned_files,
        hashed_files: response.hashed_files,
        cached_files: response.cached_files,
    }).ok();

    app.emit("sync-event", SyncEvent::Phase {
        phase: "compare".to_string(),
        detail: "比较双端清单并生成同步计划".to_string(),
    }).ok();
    let actions = rust_air_core::sync_vault::diff_manifests_latest_wins(&local_manifest, &response.manifest);
    if actions.is_empty() {
        app.emit("sync-event", SyncEvent::Info {
            msg: "两端目录已一致，无需同步".to_string(),
        }).ok();
        app.emit("sync-done", ()).ok();
        state.running.store(false, Ordering::Release);
        return Ok(());
    }

    let mut pull_waiters = Vec::new();
    let mut push_count = 0usize;
    let mut pull_count = 0usize;
    let mut delete_count = 0usize;
    let mut had_error = false;
    let total_actions = actions.len();
    app.emit("sync-event", SyncEvent::Phase {
        phase: "transfer".to_string(),
        detail: format!("准备执行 {} 个同步动作", total_actions),
    }).ok();
    for (idx, action) in actions.iter().enumerate() {
        app.emit("sync-event", SyncEvent::Phase {
            phase: "transfer".to_string(),
            detail: format!("执行同步动作 {}/{}", idx + 1, total_actions),
        }).ok();
        app.emit("sync-event", SyncEvent::ActionProgress {
            current: idx + 1,
            total: total_actions,
            push_count,
            pull_count,
            delete_count,
        }).ok();
        match action {
            SyncAction::PushToRemote(entry) => {
                push_count += 1;
                app.emit("sync-event", SyncEvent::Info {
                    msg: format!("⇢ 推送到远端: {}", entry.rel),
                }).ok();
                let src_file = src.join(&entry.rel);
                let send_meta = std::fs::metadata(&src_file).map_err(|e| format!("读取同步文件元数据失败: {e}"))?;
                let send_modified_ts = send_meta
                    .modified()
                    .ok()
                    .map(|t| chrono::DateTime::<Local>::from(t).timestamp())
                    .unwrap_or(0);
                let header_entry = build_entry_from_metadata(&entry.rel, send_meta.len(), send_modified_ts, entry.hash.clone());
                // Push transfers do not wait for a response, but they still use the
                // same header format so the receiver can reconstruct metadata without
                // re-hashing the landed file.
                let logical_name = encode_sync_file_logical_name(&uuid::Uuid::new_v4().to_string(), &header_entry);
                let stream = tokio::net::TcpStream::connect(&remote_addr).await.map_err(|e| e.to_string())?;
                let send_outcome = transfer::send_path_as_with_outcome(stream, &src_file, Some(&logical_name), |_| {}).await.map_err(|e| e.to_string())?;
                let actual_entry = build_entry_from_metadata(&entry.rel, send_outcome.total_size, send_modified_ts, send_outcome.checksum_hex);
                {
                    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
                    let modified = chrono::DateTime::<Local>::from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(actual_entry.modified_ts.max(0) as u64));
                    let should_update = store.state.files.get(&actual_entry.rel)
                        .map(|rec| rec.hash != actual_entry.hash || rec.size != actual_entry.size || rec.modified.timestamp() != actual_entry.modified_ts)
                        .unwrap_or(true);
                    if should_update {
                        store.state.files.insert(actual_entry.rel.clone(), rust_air_core::sync_vault::FileRecord {
                            hash: actual_entry.hash.clone(),
                            size: actual_entry.size,
                            modified,
                        });
                    }
                    store.state.total_synced += 1;
                    store.state.total_bytes += actual_entry.size;
                    store.mark_dirty();
                }
                app.emit("sync-event", SyncEvent::Copied { rel: format!("⇢ 已推送: {}", actual_entry.rel), bytes: actual_entry.size }).ok();
            }
            SyncAction::PullFromRemote(entry) => {
                pull_count += 1;
                app.emit("sync-event", SyncEvent::Info {
                    msg: format!("⇠ 请求远端文件: {}", entry.rel),
                }).ok();
                let file_request_id = uuid::Uuid::new_v4().to_string();
                let req = RemoteSyncFileRequest {
                    request_id: file_request_id.clone(),
                    entry: entry.clone(),
                    callback_addr: request.callback_addr.clone(),
                };
                let waiter = state.register_pending_remote_file(file_request_id.clone());
                let json = serde_json::to_string(&req).map_err(|e| e.to_string())?;
                let stream = tokio::net::TcpStream::connect(&remote_addr).await.map_err(|e| e.to_string())?;
                transfer::send_clipboard(stream, &json, "sync:file-request", |_| {}).await.map_err(|e| e.to_string())?;
                pull_waiters.push((entry.rel.clone(), waiter));
            }
            SyncAction::DeleteRemote(entry) => {
                delete_count += 1;
                app.emit("sync-event", SyncEvent::Info {
                    msg: format!("⇢ 请求远端删除: {}", entry.rel),
                }).ok();
                if !should_apply_delete(&src.join(&entry.rel), entry) {
                    had_error = true;
                    app.emit("sync-event", SyncEvent::Error {
                        rel: entry.rel.clone(),
                        err: "文件在同步过程中已变化，已跳过远端删除，请重新执行一次双机同步".to_string(),
                    }).ok();
                    continue;
                }
                let req = RemoteSyncDeleteRequest { tombstone: entry.clone() };
                let json = serde_json::to_string(&req).map_err(|e| e.to_string())?;
                let stream = tokio::net::TcpStream::connect(&remote_addr).await.map_err(|e| e.to_string())?;
                transfer::send_clipboard(stream, &json, "sync:delete-request", |_| {}).await.map_err(|e| e.to_string())?;
                app.emit("sync-event", SyncEvent::Deleted { rel: format!("⇢ 已请求远端删除: {}", entry.rel) }).ok();
            }
            SyncAction::DeleteLocal(entry) => {
                delete_count += 1;
                app.emit("sync-event", SyncEvent::Info {
                    msg: format!("⇠ 校验后删除本地旧文件: {}", entry.rel),
                }).ok();
                let dst = src.join(&entry.rel);
                if !should_apply_delete(&dst, entry) {
                    app.emit("sync-event", SyncEvent::Info {
                        msg: format!("⇠ 跳过本地删除，文件已在本地更新: {}", entry.rel),
                    }).ok();
                    continue;
                }
                let _ = std::fs::remove_file(&dst);
                {
                    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
                    store.state.files.remove(&entry.rel);
                    store.state.deleted.insert(entry.rel.clone(), Local::now());
                    store.mark_dirty();
                }
                app.emit("sync-event", SyncEvent::Deleted { rel: format!("⇠ 已删除本地旧文件: {}", entry.rel) }).ok();
            }
        }
    }

    app.emit("sync-event", SyncEvent::ActionProgress {
        current: total_actions,
        total: total_actions,
        push_count,
        pull_count,
        delete_count,
    }).ok();

    for (rel, waiter) in pull_waiters {
        let received = tokio::time::timeout(std::time::Duration::from_secs(60), waiter)
            .await
            .map_err(|_| format!("等待同步文件超时: {rel}"))?
            .map_err(|_| format!("同步文件完成通道已关闭: {rel}"))?;
        match received {
            Ok(path) => {
                app.emit("sync-event", SyncEvent::Copied {
                    rel: format!("⇠ 已拉取到本地: {}", rel),
                    bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                }).ok();
            }
            Err(e) => {
                had_error = true;
                app.emit("sync-event", SyncEvent::Error { rel: rel.clone(), err: e }).ok();
            }
        }
    }

    {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store.state.last_sync = Some(Local::now());
        store.mark_dirty();
        store.flush_now();
    }

    app.emit("sync-event", SyncEvent::Phase {
        phase: "finalize".to_string(),
        detail: "写入双机同步结果".to_string(),
    }).ok();

    if !had_error {
        app.emit("sync-event", SyncEvent::Info {
            msg: format!("双端同步执行完成：推送 {} 个，拉取 {} 个", push_count, pull_count),
        }).ok();
    }
    app.emit("sync-done", ()).ok();
    state.running.store(false, Ordering::Release);
    Ok(())
}

pub async fn handle_sync_manifest_request(
    data: &[u8],
    state: &SyncState,
) -> Result<(), String> {
    let req: RemoteSyncRequest = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let not_ready_reason = if config.src.is_empty() {
        Some("远端未设置同步目录，请先选择同步目录".to_string())
    } else {
        None
    };

    if let Some(reason) = not_ready_reason {
        let resp = RemoteSyncResponse {
            request_id: req.request_id,
            manifest: Vec::new(),
            ready: false,
            ready_reason: Some(reason),
            scanned_files: 0,
            hashed_files: 0,
            cached_files: 0,
        };
        let json = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
        let stream = tokio::net::TcpStream::connect(&req.callback_addr).await.map_err(|e| e.to_string())?;
        transfer::send_clipboard(stream, &json, "sync:manifest-response", |_| {}).await.map_err(|e| e.to_string())?;
        return Ok(());
    }

    let src = PathBuf::from(&config.src);
    let (manifest, stats) = {
        let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        rust_air_core::sync_vault::build_manifest_with_state_and_stats(
            &src,
            &store,
            &config.excludes,
            config.delete_removed,
        )
    };
    eprintln!(
        "info: remote sync manifest stats scanned={} hashed={} cached={}",
        stats.scanned_files,
        stats.hashed_files,
        stats.cached_files
    );
    let resp = RemoteSyncResponse {
        request_id: req.request_id,
        manifest,
        ready: true,
        ready_reason: None,
        scanned_files: stats.scanned_files,
        hashed_files: stats.hashed_files,
        cached_files: stats.cached_files,
    };
    let json = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
    let stream = tokio::net::TcpStream::connect(&req.callback_addr).await.map_err(|e| e.to_string())?;
    transfer::send_clipboard(stream, &json, "sync:manifest-response", |_| {}).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn handle_sync_manifest_response(
    data: &[u8],
    state: &SyncState,
) -> Result<(), String> {
    let resp: RemoteSyncResponse = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    state.resolve_pending_remote_sync(resp);
    Ok(())
}

pub async fn handle_sync_file_request(
    data: &[u8],
    state: &SyncState,
) -> Result<(), String> {
    let req: RemoteSyncFileRequest = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let (src_file, logical_name, callback_addr) = {
        let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if config.src.is_empty() {
            return Err("远端未设置同步目录".to_string());
        }
        let src_file = PathBuf::from(&config.src).join(&req.entry.rel);
        if !src_file.exists() {
            let err = format!("sync source file not found: {}", src_file.display());
            let msg = RemoteSyncFileError {
                request_id: req.request_id,
                rel: req.entry.rel,
                err: err.clone(),
            };
            let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
            let stream = tokio::net::TcpStream::connect(&req.callback_addr).await.map_err(|e| e.to_string())?;
            transfer::send_clipboard(stream, &json, "sync:file-error", |_| {}).await.map_err(|e| e.to_string())?;
            return Err(err);
        }
        (
            src_file,
            encode_sync_file_logical_name(&req.request_id, &req.entry),
            req.callback_addr.clone(),
        )
    };
    let stream = tokio::net::TcpStream::connect(&callback_addr).await.map_err(|e| e.to_string())?;
    transfer::send_path_as(stream, &src_file, Some(&logical_name), |_| {}).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn handle_sync_delete_request(
    data: &[u8],
    state: &SyncState,
) -> Result<String, String> {
    let req: RemoteSyncDeleteRequest = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() {
        return Err("远端未设置同步目录".to_string());
    }
    let dst = PathBuf::from(&config.src).join(&req.tombstone.rel);
    if !should_apply_delete(&dst, &req.tombstone) {
        return Ok(format!("{}（已跳过，本地文件较新）", req.tombstone.rel));
    }
    let _ = std::fs::remove_file(&dst);
    {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        store.state.files.remove(&req.tombstone.rel);
        store.state.deleted.insert(req.tombstone.rel.clone(), Local::now());
        store.mark_dirty();
        store.flush_now();
    }
    Ok(req.tombstone.rel)
}

pub fn handle_received_sync_file(
    temp_path: &std::path::Path,
    logical_name: &str,
    state: &SyncState,
) -> Result<(String, String, PathBuf), String> {
    let (request_id, entry) = decode_sync_file_logical_name(logical_name)?;
    let rel = entry.rel.as_str();
    let dst = {
        let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if config.src.is_empty() {
            return Err("local sync source directory not configured".to_string());
        }
        PathBuf::from(&config.src).join(rel)
    };
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let copied = std::fs::copy(temp_path, &dst).map_err(|e| e.to_string())?;

    // Update persisted sync state so UI status reflects cross-device sync results.
    let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
    let modified = std::fs::metadata(&dst)
        .and_then(|m| m.modified())
        .map(chrono::DateTime::<Local>::from)
        .unwrap_or_else(|_| chrono::DateTime::<Local>::from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(entry.modified_ts.max(0) as u64)));
    store.state.files.insert(rel.to_string(), rust_air_core::sync_vault::FileRecord {
        hash: entry.hash,
        size: entry.size.max(copied),
        modified,
    });
    store.state.total_synced += 1;
    store.state.total_bytes += copied;
    store.state.last_sync = Some(Local::now());
    store.mark_dirty();
    store.flush_now();

    let _ = std::fs::remove_file(temp_path);
    Ok((request_id, rel.to_string(), dst))
}

pub fn handle_sync_file_error(
    data: &[u8],
    state: &SyncState,
) -> Result<(), String> {
    let msg: RemoteSyncFileError = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    state.resolve_pending_remote_file(msg.request_id, Err(format!("{}: {}", msg.rel, msg.err)));
    Ok(())
}

fn should_apply_delete(path: &std::path::Path, tombstone: &SyncManifestEntry) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    let modified_ts = meta
        .modified()
        .ok()
        .map(|t| chrono::DateTime::<Local>::from(t).timestamp())
        .unwrap_or(0);
    modified_ts <= tombstone.modified_ts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use rust_air_core::proto::{ArchiveStatus, ArchiveStatusCode};

    #[test]
    fn test_handle_received_sync_file_reconstructs_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let sync_root = tempfile::tempdir().unwrap();
        let incoming = tmp.path().join("incoming.bin");
        fs::write(&incoming, b"hello-sync").unwrap();

        let state = SyncState::new();
        {
            let mut cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            cfg.src = sync_root.path().to_string_lossy().to_string();
        }

        let (request_id, rel, final_path) = handle_received_sync_file(
            &incoming,
            "sync:file:req-123:1234:10:remotehash123:subdir/file.txt",
            &state,
        ).unwrap();

        assert_eq!(request_id, "req-123");
        assert_eq!(rel, "subdir/file.txt");
        assert_eq!(final_path, sync_root.path().join("subdir").join("file.txt"));
        assert!(final_path.exists());
        assert_eq!(fs::read(&final_path).unwrap(), b"hello-sync");
        let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        let rec = store.state.files.get("subdir/file.txt").unwrap();
        assert_eq!(rec.hash, "remotehash123");
    }

    #[test]
    fn test_sync_file_logical_name_round_trip() {
        let entry = SyncManifestEntry {
            rel: "folder/item.txt".to_string(),
            size: 42,
            modified_ts: 123456,
            hash: "abc123hash".to_string(),
            deleted: false,
        };

        let encoded = encode_sync_file_logical_name("req-42", &entry);
        let (request_id, decoded) = decode_sync_file_logical_name(&encoded).unwrap();

        assert_eq!(request_id, "req-42");
        assert_eq!(decoded, entry);
    }

    #[test]
    fn test_handle_sync_file_error_resolves_waiter() {
        let state = SyncState::new();
        let rx = state.register_pending_remote_file("req-err".to_string());

        let msg = RemoteSyncFileError {
            request_id: "req-err".to_string(),
            rel: "subdir/file.txt".to_string(),
            err: "not found".to_string(),
        };
        let bytes = serde_json::to_vec(&msg).unwrap();
        handle_sync_file_error(&bytes, &state).unwrap();

        let result = rx.blocking_recv().expect("pending file waiter should resolve");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("subdir/file.txt"));
    }

    #[test]
    fn test_handle_sync_delete_request_removes_local_file_and_records_tombstone() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("subdir").join("gone.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"delete-me").unwrap();

        let state = SyncState::new();
        {
            let mut cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            cfg.src = root.path().to_string_lossy().to_string();
        }

        let req = RemoteSyncDeleteRequest {
            tombstone: SyncManifestEntry {
                rel: "subdir/gone.txt".to_string(),
                size: 0,
                modified_ts: chrono::Local::now().timestamp(),
                hash: String::new(),
                deleted: true,
            },
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let rel = handle_sync_delete_request(&bytes, &state).unwrap();

        assert_eq!(rel, "subdir/gone.txt");
        assert!(!target.exists());
        let store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        assert!(store.state.deleted.contains_key("subdir/gone.txt"));
    }

    #[test]
    fn test_handle_sync_delete_request_skips_newer_local_file() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("subdir").join("keep.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"keep-me").unwrap();

        let state = SyncState::new();
        {
            let mut cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            cfg.src = root.path().to_string_lossy().to_string();
        }

        let req = RemoteSyncDeleteRequest {
            tombstone: SyncManifestEntry {
                rel: "subdir/keep.txt".to_string(),
                size: 0,
                modified_ts: 0,
                hash: String::new(),
                deleted: true,
            },
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let rel = handle_sync_delete_request(&bytes, &state).unwrap();

        assert!(rel.contains("已跳过"));
        assert!(target.exists());
    }

    #[test]
    fn test_archive_status_to_sync_event_maps_info_states() {
        let status = ArchiveStatus {
            code: ArchiveStatusCode::ResumeRejectedSafetyRestart,
            detail: Some("archive resume disabled for safety".to_string()),
        };

        let mapped = archive_status_detail_to_sync_event(&status, "folder-a").expect("should map archive info state");
        match mapped {
            SyncEvent::Info { msg } => assert!(msg.contains("安全重启")),
            other => panic!("expected info event, got {other:?}"),
        }
    }

    #[test]
    fn test_archive_status_to_sync_event_maps_error_state() {
        let status = ArchiveStatus {
            code: ArchiveStatusCode::UnpackFailed,
            detail: Some("archive unpack failed for incoming-transfer".to_string()),
        };

        let mapped = archive_status_detail_to_sync_event(&status, "folder-b").expect("should map archive error state");
        match mapped {
            SyncEvent::Error { rel, err } => {
                assert_eq!(rel, "folder-b");
                assert!(err.contains("archive unpack failed"));
            }
            other => panic!("expected error event, got {other:?}"),
        }
    }
}

/// Run a full sync in a background thread.
/// Progress events are emitted as "sync-event" to the frontend.
#[tauri::command]
pub async fn start_sync(state: State<'_, SyncState>, app: AppHandle) -> Result<(), String> {
    if state.running.swap(true, Ordering::AcqRel) {
        return Err("同步任务正在进行中，请稍后再试".into());
    }

    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() || config.dst.is_empty() {
        state.running.store(false, Ordering::Release);
        return Err("请先设置源目录和本地镜像目标目录".into());
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

    // Run sync in blocking thread; reset running flag when done regardless of frontend state.
    let tx2      = tx.clone();
    let excludes = config.excludes.clone();
    let delete   = config.delete_removed;
    let app2     = app.clone();
    let running  = Arc::clone(&state.running);

    thread::spawn(move || {
        full_sync(&src, &dst, &mut store, delete, &excludes, &tx2);
        store.flush_now();
        drop(tx2);
        running.store(false, Ordering::Release);
        app2.emit("sync-done", ()).ok();
    });

    Ok(())
}

/// Reset the running flag (called by frontend when it receives sync-done).
#[tauri::command]
pub fn sync_done(state: State<'_, SyncState>) {
    state.running.store(false, Ordering::Release);
    // Reload store from disk to pick up changes made in the sync thread
    *state.store.lock().unwrap_or_else(|e| e.into_inner()) = SyncStore::load();
}

/// Start watching src for changes and auto-sync on modification.
#[tauri::command]
pub fn start_watch(state: State<'_, SyncState>, app: AppHandle) -> Result<(), String> {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if config.src.is_empty() || config.dst.is_empty() {
        return Err("请先设置源目录和本地镜像目标目录".into());
    }

    if let Some(session) = state.watch_session.lock().unwrap_or_else(|e| e.into_inner()).take() {
        session.stop();
    }

    let src = PathBuf::from(&config.src);
    let dst = PathBuf::from(&config.dst);
    let excludes = config.excludes.clone();
    let stop = Arc::new(AtomicBool::new(false));

    let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
    let watcher  = start_watcher(src.clone(), tx).map_err(|e| e.to_string())?;

    // Sync changed files in background
    let stop_worker = stop.clone();
    let worker = thread::spawn(move || {
        let (ev_tx, ev_rx) = mpsc::channel::<SyncEvent>();
        let app2 = app.clone();
        let forward_stop = stop_worker.clone();
        let forwarder = thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                if forward_stop.load(Ordering::Acquire) {
                    break;
                }
                app2.emit("sync-event", &ev).ok();
            }
        });
        let mut store = SyncStore::load();
        // Build ExcludeSet once outside the loop — not per-file
        let ex = rust_air_core::sync_vault::ExcludeSet::new(&excludes);
        while let Ok(paths) = rx.recv() {
            if stop_worker.load(Ordering::Acquire) {
                break;
            }
            for abs in paths {
                if stop_worker.load(Ordering::Acquire) {
                    break;
                }
                rust_air_core::sync_vault::sync_file(&abs, &src, &dst, &mut store, &ex, &ev_tx);
            }
            store.flush_if_needed();
        }
        drop(ev_tx);
        let _ = forwarder.join();
    });

    *state.watch_session.lock().unwrap_or_else(|e| e.into_inner()) = Some(WatchSession {
        watcher,
        stop,
        worker: Some(worker),
    });

    Ok(())
}

/// Stop the file watcher.
#[tauri::command]
pub fn stop_watch(state: State<'_, SyncState>) {
    if let Some(session) = state.watch_session.lock().unwrap_or_else(|e| e.into_inner()).take() {
        session.stop();
    }
}
