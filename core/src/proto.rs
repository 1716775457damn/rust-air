/// Wire protocol v4 — key-in-header, checksum sent AFTER data stream.
///
/// After TCP connect the SENDER writes:
///   [4B MAGIC "RAR4"][32B one-time key][1B kind]
///   [2B name_len][name bytes][8B total_size]
///
/// RECEIVER replies:
///   [8B already_have]   (0 = fresh)
///
/// SENDER streams AEAD-encrypted chunks, then appends:
///   [32B sha256]   ← computed on-the-fly while streaming, sent after EOF sentinel
///
/// This eliminates the double-read of large files (no pre-scan for checksum).

use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 4] = b"RAR4";
pub const MDNS_SERVICE: &str = "_rustair._tcp.local.";
pub const CHUNK: usize = 1024 * 1024; // 1 MB: better throughput on fast LANs
pub const MAX_NAME_LEN: usize = 512;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File      = 0x01,
    Archive   = 0x02,
    Clipboard = 0x03,
}

impl TryFrom<u8> for Kind {
    type Error = anyhow::Error;
    fn try_from(v: u8) -> anyhow::Result<Self> {
        match v {
            0x01 => Ok(Kind::File),
            0x02 => Ok(Kind::Archive),
            0x03 => Ok(Kind::Clipboard),
            _    => anyhow::bail!("unknown kind byte {v:#x}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name:   String,
    /// "ip:port"
    pub addr:   String,
    pub status: DeviceStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceStatus {
    Idle,
    Busy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEvent {
    pub bytes_done:      u64,
    pub total_bytes:     u64,
    pub bytes_per_sec:   u64,
    pub done:            bool,
    pub error:           Option<String>,
    /// Whether this transfer is in resume mode.
    pub resumed:         bool,
    /// Number of bytes skipped via resume.
    pub resume_offset:   u64,
    /// Reconnection status (only present during reconnect attempts).
    pub reconnect_info:  Option<ReconnectInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectInfo {
    /// Current reconnect attempt (1-based).
    pub attempt:      u32,
    /// Maximum number of reconnect attempts.
    pub max_attempts: u32,
}

/// Transfer session metadata, persisted as JSON for resume validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionManifest {
    /// Name of the file or directory being transferred.
    pub name:        String,
    /// Total transfer size in bytes.
    pub total_size:  u64,
    /// Transfer type (File / Archive / Clipboard).
    pub kind:        Kind,
    /// Sender address "ip:port".
    pub sender_addr: String,
    /// Creation timestamp (Unix epoch seconds).
    pub created_at:  u64,
}
