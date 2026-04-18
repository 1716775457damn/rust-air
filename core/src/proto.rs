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
pub const CHUNK: usize = 256 * 1024; // 256 KB: better throughput on fast LANs
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
    pub bytes_done:    u64,
    pub total_bytes:   u64,
    pub bytes_per_sec: u64,
    pub done:          bool,
    pub error:         Option<String>,
}
