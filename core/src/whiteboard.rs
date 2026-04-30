//! Shared whiteboard — multi-device collaborative text/image board.
//!
//! Provides:
//! - `WhiteboardContentType` — text or image content type
//! - `WhiteboardItem`        — a single whiteboard entry
//! - `SyncOp`                — sync operation type (Add/Delete/Clear/Snapshot)
//! - `WhiteboardSyncMessage` — sync message transmitted over the network
//! - `WhiteboardError`       — error event for frontend notification
//! - `WhiteboardStore`       — in-memory store with JSON persistence
//! - `apply_sync_message`    — apply a sync message to a store
//! - `handle_received_whiteboard` — parse received whiteboard data
//! - `broadcast_sync_message`     — broadcast sync message to peers

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

use crate::clipboard_sync::BroadcastResult;
use crate::proto::DeviceInfo;

// ── Data types ────────────────────────────────────────────────────────────────

/// 白板内容类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WhiteboardContentType {
    Text,
    Image,
}

/// 白板中的单个内容条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhiteboardItem {
    /// 全局唯一标识符
    pub id: String,
    /// 内容类型
    pub content_type: WhiteboardContentType,
    /// 文本内容（content_type == Text 时有值）
    pub text: Option<String>,
    /// Base64 编码的图片数据（content_type == Image 时有值）
    pub image_b64: Option<String>,
    /// 创建/修改时间戳（Unix 毫秒）
    pub timestamp: u64,
    /// 来源设备名称
    pub source_device: String,
}

/// 同步操作类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncOp {
    Add,
    Delete,
    Clear,
    Snapshot,
}

/// 白板同步消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhiteboardSyncMessage {
    /// 操作类型
    pub op: SyncOp,
    /// 发送设备名称
    pub source_device: String,
    /// 操作时间戳（Unix 毫秒）
    pub timestamp: u64,
    /// Add 操作时携带完整的 WhiteboardItem
    pub item: Option<WhiteboardItem>,
    /// Delete 操作时携带待删除条目的 UUID
    pub item_id: Option<String>,
    /// Snapshot 操作时携带完整的白板内容列表
    pub items: Option<Vec<WhiteboardItem>>,
}

/// 白板错误事件，通过 Tauri 事件推送到前端
#[derive(Debug, Clone, Serialize)]
pub struct WhiteboardError {
    /// 错误类型: "sync_failed" | "parse_failed" | "storage_failed"
    pub kind: String,
    /// 用户可读的错误描述
    pub message: String,
    /// 相关设备名（如适用）
    pub device: Option<String>,
}

// ── WhiteboardStore ───────────────────────────────────────────────────────────

/// Return the path to the whiteboard storage file:
/// `{data_local_dir}/rust-air/whiteboard.json`
fn whiteboard_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rust-air")
        .join("whiteboard.json")
}

/// 白板内容的本地持久化存储
pub struct WhiteboardStore {
    /// 内存中的白板条目列表（按 timestamp 升序排列）
    pub items: Vec<WhiteboardItem>,
    /// 存储文件路径
    path: PathBuf,
    /// 是否有未保存的变更
    dirty: bool,
    /// 上次保存时间
    last_save: Instant,
}

impl WhiteboardStore {
    /// 从磁盘加载白板内容，文件不存在或损坏时返回空白板
    pub fn load() -> Self {
        let path = whiteboard_path();
        let mut items: Vec<WhiteboardItem> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| {
                if path.exists() {
                    eprintln!("warn: whiteboard storage corrupted, starting with empty whiteboard");
                }
                Vec::new()
            });
        // Ensure items are sorted by timestamp ascending
        items.sort_by_key(|item| item.timestamp);
        Self {
            items,
            path,
            dirty: false,
            last_save: Instant::now(),
        }
    }

    /// 添加条目，如果已存在相同 UUID 则按时间戳决定是否替换。
    /// 返回 true 表示条目被添加或替换，false 表示远程条目较旧被忽略。
    pub fn add(&mut self, item: WhiteboardItem) -> bool {
        // Check for existing item with same UUID
        if let Some(pos) = self.items.iter().position(|i| i.id == item.id) {
            if item.timestamp > self.items[pos].timestamp {
                // Remote is newer — replace
                self.items.remove(pos);
            } else {
                // Local is newer or equal — keep local
                return false;
            }
        }
        // Insert in timestamp-sorted position (ascending)
        let insert_pos = self.items.partition_point(|i| i.timestamp <= item.timestamp);
        self.items.insert(insert_pos, item);
        self.dirty = true;
        true
    }

    /// 按 UUID 删除条目，返回是否成功删除
    pub fn delete(&mut self, id: &str) -> bool {
        let len_before = self.items.len();
        self.items.retain(|i| i.id != id);
        if self.items.len() < len_before {
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// 清空所有条目
    pub fn clear(&mut self) {
        self.items.clear();
        self.dirty = true;
    }

    /// 用快照替换全部内容
    pub fn apply_snapshot(&mut self, mut items: Vec<WhiteboardItem>) {
        items.sort_by_key(|item| item.timestamp);
        self.items = items;
        self.dirty = true;
    }

    /// 获取所有条目的克隆（用于快照发送）
    pub fn snapshot(&self) -> Vec<WhiteboardItem> {
        self.items.clone()
    }

    /// 如果 dirty 且距上次保存 ≥2s，写入磁盘
    pub fn flush_if_needed(&mut self) {
        if !self.dirty || self.last_save.elapsed().as_secs() < 2 {
            return;
        }
        self.flush_now();
    }

    /// 立即写入磁盘
    pub fn flush_now(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::File::create(&self.path) {
            let _ = serde_json::to_writer_pretty(std::io::BufWriter::new(file), &self.items);
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }
}

// ── Sync message handling ─────────────────────────────────────────────────────

/// Apply a sync message to the whiteboard store.
///
/// Routes the message to the appropriate store method based on the operation type:
/// - Add: inserts or replaces the item (timestamp wins on UUID conflict)
/// - Delete: removes the item by UUID
/// - Clear: removes all items
/// - Snapshot: replaces all items with the snapshot contents
pub fn apply_sync_message(store: &mut WhiteboardStore, msg: WhiteboardSyncMessage) {
    match msg.op {
        SyncOp::Add => {
            if let Some(item) = msg.item {
                store.add(item);
            }
        }
        SyncOp::Delete => {
            if let Some(id) = msg.item_id {
                store.delete(&id);
            }
        }
        SyncOp::Clear => {
            store.clear();
        }
        SyncOp::Snapshot => {
            if let Some(items) = msg.items {
                store.apply_snapshot(items);
            }
        }
    }
}

/// Parse received whiteboard data from the network.
///
/// Accepts the `name` field (e.g. `wb:sync:DEVICE`) and raw `data` bytes
/// (JSON-encoded `WhiteboardSyncMessage`). Returns the parsed message on
/// success, or an error if JSON parsing fails.
pub fn handle_received_whiteboard(
    _name: &str,
    data: &[u8],
) -> anyhow::Result<WhiteboardSyncMessage> {
    let json_str = std::str::from_utf8(data)
        .map_err(|e| anyhow::anyhow!("whiteboard sync data is not valid UTF-8: {e}"))?;
    let msg: WhiteboardSyncMessage = serde_json::from_str(json_str)
        .map_err(|e| {
            eprintln!("warn: failed to parse whiteboard sync message: {e}");
            anyhow::anyhow!("whiteboard sync JSON parse failed: {e}")
        })?;
    Ok(msg)
}

/// Broadcast a whiteboard sync message to all discovered devices.
///
/// Serializes the message to JSON and sends it to each device using the
/// existing encrypted transfer protocol (`send_clipboard`). The name field
/// uses the format `wb:sync:{local_device_name}`.
///
/// Connection failures are logged and skipped — broadcast continues to
/// remaining devices. Returns a `BroadcastResult` per device.
pub async fn broadcast_sync_message(
    msg: &WhiteboardSyncMessage,
    devices: &[DeviceInfo],
    local_device_name: &str,
) -> Vec<BroadcastResult> {
    let json = match serde_json::to_string(msg) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("error: failed to serialize whiteboard sync message: {e}");
            return Vec::new();
        }
    };
    let name = format!("wb:sync:{}", local_device_name);
    let mut results = Vec::with_capacity(devices.len());

    for device in devices {
        let result = match tokio::net::TcpStream::connect(&device.addr).await {
            Ok(stream) => {
                match crate::transfer::send_clipboard(stream, &json, &name, |_| {}).await {
                    Ok(()) => BroadcastResult {
                        device_name: device.name.clone(),
                        success: true,
                        error: None,
                    },
                    Err(e) => {
                        eprintln!(
                            "warn: whiteboard sync send to {} failed: {}",
                            device.name, e
                        );
                        BroadcastResult {
                            device_name: device.name.clone(),
                            success: false,
                            error: Some(e.to_string()),
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "warn: TCP connect to {} ({}) failed: {}",
                    device.name, device.addr, e
                );
                BroadcastResult {
                    device_name: device.name.clone(),
                    success: false,
                    error: Some(e.to_string()),
                }
            }
        };
        results.push(result);
    }

    results
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_text_item(id: &str, text: &str, timestamp: u64) -> WhiteboardItem {
        WhiteboardItem {
            id: id.to_string(),
            content_type: WhiteboardContentType::Text,
            text: Some(text.to_string()),
            image_b64: None,
            timestamp,
            source_device: "test-device".to_string(),
        }
    }

    #[test]
    fn test_add_new_item() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        let item = make_text_item("id-1", "hello", 1000);
        assert!(store.add(item));
        assert_eq!(store.items.len(), 1);
        assert_eq!(store.items[0].id, "id-1");
    }

    #[test]
    fn test_add_duplicate_uuid_newer_wins() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-1", "old", 1000));
        // Newer timestamp should replace
        assert!(store.add(make_text_item("id-1", "new", 2000)));
        assert_eq!(store.items.len(), 1);
        assert_eq!(store.items[0].text.as_deref(), Some("new"));
    }

    #[test]
    fn test_add_duplicate_uuid_older_ignored() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-1", "current", 2000));
        // Older timestamp should be ignored
        assert!(!store.add(make_text_item("id-1", "old", 1000)));
        assert_eq!(store.items.len(), 1);
        assert_eq!(store.items[0].text.as_deref(), Some("current"));
    }

    #[test]
    fn test_items_sorted_by_timestamp() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-3", "c", 3000));
        store.add(make_text_item("id-1", "a", 1000));
        store.add(make_text_item("id-2", "b", 2000));
        assert_eq!(store.items[0].id, "id-1");
        assert_eq!(store.items[1].id, "id-2");
        assert_eq!(store.items[2].id, "id-3");
    }

    #[test]
    fn test_delete_existing() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-1", "hello", 1000));
        assert!(store.delete("id-1"));
        assert!(store.items.is_empty());
    }

    #[test]
    fn test_delete_nonexistent() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        assert!(!store.delete("no-such-id"));
    }

    #[test]
    fn test_clear() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-1", "a", 1000));
        store.add(make_text_item("id-2", "b", 2000));
        store.clear();
        assert!(store.items.is_empty());
    }

    #[test]
    fn test_apply_snapshot() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("old-1", "old", 500));
        let snapshot = vec![
            make_text_item("new-1", "x", 2000),
            make_text_item("new-2", "y", 1000),
        ];
        store.apply_snapshot(snapshot);
        assert_eq!(store.items.len(), 2);
        // Should be sorted by timestamp
        assert_eq!(store.items[0].id, "new-2");
        assert_eq!(store.items[1].id, "new-1");
    }

    #[test]
    fn test_snapshot_returns_clone() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-1", "hello", 1000));
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "id-1");
    }

    #[test]
    fn test_apply_sync_message_add() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        let msg = WhiteboardSyncMessage {
            op: SyncOp::Add,
            source_device: "remote".to_string(),
            timestamp: 1000,
            item: Some(make_text_item("id-1", "hello", 1000)),
            item_id: None,
            items: None,
        };
        apply_sync_message(&mut store, msg);
        assert_eq!(store.items.len(), 1);
    }

    #[test]
    fn test_apply_sync_message_delete() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-1", "hello", 1000));
        let msg = WhiteboardSyncMessage {
            op: SyncOp::Delete,
            source_device: "remote".to_string(),
            timestamp: 2000,
            item: None,
            item_id: Some("id-1".to_string()),
            items: None,
        };
        apply_sync_message(&mut store, msg);
        assert!(store.items.is_empty());
    }

    #[test]
    fn test_apply_sync_message_clear() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("id-1", "a", 1000));
        store.add(make_text_item("id-2", "b", 2000));
        let msg = WhiteboardSyncMessage {
            op: SyncOp::Clear,
            source_device: "remote".to_string(),
            timestamp: 3000,
            item: None,
            item_id: None,
            items: None,
        };
        apply_sync_message(&mut store, msg);
        assert!(store.items.is_empty());
    }

    #[test]
    fn test_apply_sync_message_snapshot() {
        let mut store = WhiteboardStore {
            items: Vec::new(),
            path: PathBuf::from("/tmp/test-wb.json"),
            dirty: false,
            last_save: Instant::now(),
        };
        store.add(make_text_item("old", "old", 500));
        let msg = WhiteboardSyncMessage {
            op: SyncOp::Snapshot,
            source_device: "remote".to_string(),
            timestamp: 3000,
            item: None,
            item_id: None,
            items: Some(vec![
                make_text_item("new-1", "x", 1000),
                make_text_item("new-2", "y", 2000),
            ]),
        };
        apply_sync_message(&mut store, msg);
        assert_eq!(store.items.len(), 2);
        assert_eq!(store.items[0].id, "new-1");
        assert_eq!(store.items[1].id, "new-2");
    }

    #[test]
    fn test_handle_received_whiteboard_valid() {
        let msg = WhiteboardSyncMessage {
            op: SyncOp::Add,
            source_device: "device-a".to_string(),
            timestamp: 1000,
            item: Some(make_text_item("id-1", "hello", 1000)),
            item_id: None,
            items: None,
        };
        let json = serde_json::to_vec(&msg).unwrap();
        let parsed = handle_received_whiteboard("wb:sync:device-a", &json).unwrap();
        assert_eq!(parsed.op, SyncOp::Add);
        assert_eq!(parsed.source_device, "device-a");
    }

    #[test]
    fn test_handle_received_whiteboard_invalid_json() {
        let result = handle_received_whiteboard("wb:sync:device-a", b"not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_serialization_roundtrip() {
        let item = make_text_item("id-1", "hello world", 1000);
        let json = serde_json::to_string(&item).unwrap();
        let parsed: WhiteboardItem = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, item.id);
        assert_eq!(parsed.content_type, item.content_type);
        assert_eq!(parsed.text, item.text);
        assert_eq!(parsed.timestamp, item.timestamp);
        assert_eq!(parsed.source_device, item.source_device);
    }
}
