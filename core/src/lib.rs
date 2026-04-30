// ── Always-available modules ───────────────────────────────────────────────────
pub mod archive;
pub mod crypto;
pub mod proto;
pub mod transfer;

// ── Desktop-only modules ──────────────────────────────────────────────────────
#[cfg(feature = "desktop")]
pub mod clipboard;
#[cfg(feature = "desktop")]
pub mod clipboard_history;
#[cfg(feature = "desktop")]
pub mod clipboard_sync;
#[cfg(feature = "desktop")]
pub mod sync_vault;
#[cfg(feature = "desktop")]
pub mod http_qr;
#[cfg(feature = "desktop")]
pub mod whiteboard;

// ── Discovery: mDNS on desktop, UDP broadcast otherwise ───────────────────────
#[cfg(feature = "desktop")]
pub mod discovery;
#[cfg(not(feature = "desktop"))]
pub mod discovery_udp;
#[cfg(not(feature = "desktop"))]
pub use discovery_udp as discovery;

// ── Stubs for non-desktop platforms ───────────────────────────────────────────
#[cfg(not(feature = "desktop"))]
pub mod stubs;

// ── Always-available re-exports ───────────────────────────────────────────────
pub use proto::{DeviceInfo, DeviceStatus, TransferEvent};

// ── Desktop-only re-exports ───────────────────────────────────────────────────
#[cfg(feature = "desktop")]
pub use sync_vault::{SyncConfig, SyncEvent, SyncStore, full_sync, start_watcher, fmt_bytes, default_excludes};
#[cfg(feature = "desktop")]
pub use sync_vault::ExcludeSet;
#[cfg(feature = "desktop")]
pub use clipboard_history::{ClipContent, ClipEntry, HistoryStore, start_monitor};
#[cfg(feature = "desktop")]
pub use transfer::{send_clipboard, send_clipboard_image};
