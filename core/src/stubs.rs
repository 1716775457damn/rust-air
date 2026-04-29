//! Stub types for non-desktop platforms.
//!
//! These minimal definitions allow downstream crates to compile without the
//! `desktop` feature while still referencing core types by name.

pub struct SyncConfig;
pub struct SyncEvent;
pub struct SyncStore;
pub struct ExcludeSet;
pub struct ClipContent;
pub struct ClipEntry;
pub struct HistoryStore;

pub fn fmt_bytes(_bytes: u64) -> String {
    String::new()
}

pub fn default_excludes() -> Vec<String> {
    vec![]
}
