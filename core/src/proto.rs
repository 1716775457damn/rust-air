/// Wire protocol v2.
///
/// Frame layout after TCP connect:
///   TX: [4B MAGIC][1B kind][2B name_len][name bytes][8B total_size][32B sha256]
///   RX: [8B already_have]
///   TX: AEAD-encrypted data chunks (see crypto.rs)
///       each chunk: [4B plaintext_len][16B tag][ciphertext]
///       sentinel:   [4B = 0x00000000]
///
/// Code format: "<port>-<base64url(32-byte key)>"

use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 4] = b"RAR2";
pub const MDNS_SERVICE: &str = "_rustair._tcp.local.";
/// AEAD frame size: 64 KiB plaintext per chunk.
pub const CHUNK: usize = 64 * 1024;
/// Maximum allowed filename length (prevents memory exhaustion on malformed headers).
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

/// Discovered peer on the LAN — serialised for Tauri IPC.
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

/// Real-time transfer progress pushed to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEvent {
    pub bytes_done:    u64,
    /// 0 = unknown (streaming archive)
    pub total_bytes:   u64,
    pub bytes_per_sec: u64,
    pub done:          bool,
    pub error:         Option<String>,
}
