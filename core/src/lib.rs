pub mod archive;
pub mod clipboard;
pub mod clipboard_history;
pub mod crypto;
pub mod discovery;
pub mod http_qr;
pub mod proto;
pub mod transfer;

pub use proto::{DeviceInfo, DeviceStatus, TransferEvent};
pub use clipboard_history::{ClipContent, ClipEntry, HistoryStore, start_monitor};
