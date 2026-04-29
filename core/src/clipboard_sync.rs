//! Clipboard sync — shared clipboard between LAN devices.
//!
//! Provides:
//! - `SyncPeer`        — device info within a sync group
//! - `SyncGroupConfig` — persistent sync group configuration
//! - `ClipPayload`     — clipboard data packet for network transfer
//! - `SizeError`       — content size validation errors
//! - `ClipSyncError`   — sync error event for frontend notification
//! - Size limit constants (`TEXT_MAX_BYTES`, `IMAGE_MAX_BYTES`)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::clipboard_history::{fnv1a, ClipContent};

// ── Size limit constants ──────────────────────────────────────────────────────

/// Maximum text content size: 10 MB
pub const TEXT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Maximum image content size: 50 MB
pub const IMAGE_MAX_BYTES: usize = 50 * 1024 * 1024;

// ── Data types ────────────────────────────────────────────────────────────────

/// A device within the sync group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncPeer {
    /// mDNS full name, e.g. "DESKTOP-ABC-a1b2._rustair._tcp.local."
    pub device_name: String,
    /// "ip:port" address
    pub addr: String,
    /// Last discovery timestamp (Unix seconds)
    pub last_seen: u64,
    /// Whether the device is currently online (last_seen < 30s ago)
    pub online: bool,
}

/// Sync group configuration (persisted to disk).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncGroupConfig {
    /// Whether clipboard sharing is enabled
    pub enabled: bool,
    /// Devices in the sync group
    pub peers: Vec<SyncPeer>,
}

/// Clipboard data packet transmitted over the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipPayload {
    /// Content type: "text" or "image"
    pub content_type: String,
    /// Text content (present when content_type == "text")
    pub text: Option<String>,
    /// PNG-encoded image data (present when content_type == "image")
    pub image_png: Option<Vec<u8>>,
    /// Sender device name
    pub source_device: String,
    /// Timestamp (Unix milliseconds)
    pub timestamp: u64,
}

/// Content size validation errors.
#[derive(Debug, Clone)]
pub enum SizeError {
    TextTooLarge { size: usize, limit: usize },
    ImageTooLarge { size: usize, limit: usize },
}

impl std::fmt::Display for SizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SizeError::TextTooLarge { size, limit } => {
                write!(f, "text too large: {} bytes exceeds {} byte limit", size, limit)
            }
            SizeError::ImageTooLarge { size, limit } => {
                write!(f, "image too large: {} bytes exceeds {} byte limit", size, limit)
            }
        }
    }
}

impl std::error::Error for SizeError {}

/// Sync error event pushed to the frontend via Tauri events.
#[derive(Debug, Clone, Serialize)]
pub struct ClipSyncError {
    /// Error kind: "size_limit" | "transfer_failed" | "checksum_failed"
    pub kind: String,
    /// Human-readable error description
    pub message: String,
    /// Related device name (if applicable)
    pub device: Option<String>,
}

impl Default for SyncGroupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            peers: Vec::new(),
        }
    }
}

/// Result of broadcasting clipboard content to a single peer.
#[derive(Debug, Clone, Serialize)]
pub struct BroadcastResult {
    /// Device name of the target peer
    pub device_name: String,
    /// Whether the send succeeded
    pub success: bool,
    /// Error message if the send failed
    pub error: Option<String>,
}

// ── SyncGroupConfig persistence ───────────────────────────────────────────────

/// Return the path to the sync config file:
/// `{data_local_dir}/rust-air/sync_clipboard.json`
fn sync_config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rust-air")
        .join("sync_clipboard.json")
}

impl SyncGroupConfig {
    /// Load config from disk. Returns default config (enabled=false, peers=[])
    /// if the file does not exist or is corrupted.
    pub fn load() -> Self {
        let path = sync_config_path();
        std::fs::read(&path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|s| serde_json::from_str::<SyncGroupConfig>(&s).ok())
            .unwrap_or_default()
    }

    /// Serialize config to JSON and write to disk.
    pub fn save(config: &SyncGroupConfig) {
        let path = sync_config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(config) {
            let _ = std::fs::write(&path, json);
        }
    }
}

// ── EchoGuard ─────────────────────────────────────────────────────────────────

/// Echo suppressor — prevents re-broadcasting content just received from a
/// remote device. Maintains a sliding window of `(content_hash, expiry)` pairs.
pub struct EchoGuard {
    /// (content_fnv1a_hash, expiry_instant)
    suppressed: Vec<(u64, Instant)>,
    /// Suppression window duration (default 3 seconds)
    window: Duration,
}

impl EchoGuard {
    /// Create a new EchoGuard with the given suppression window.
    pub fn new(window: Duration) -> Self {
        Self {
            suppressed: Vec::new(),
            window,
        }
    }

    /// Register a content hash to be suppressed for the duration of the window.
    pub fn register(&mut self, content_hash: u64) {
        self.suppressed.push((content_hash, Instant::now() + self.window));
    }

    /// Check whether the given content hash is currently suppressed.
    /// Cleans up expired entries first, then checks for a match.
    pub fn is_suppressed(&mut self, content_hash: u64) -> bool {
        self.cleanup();
        self.suppressed.iter().any(|(h, _)| *h == content_hash)
    }

    /// Remove all entries whose expiry time has passed.
    fn cleanup(&mut self) {
        let now = Instant::now();
        self.suppressed.retain(|(_, expiry)| *expiry > now);
    }
}

// ── Size validation ───────────────────────────────────────────────────────────

/// Check whether the given clipboard content is within the allowed size limits.
///
/// - Text: byte length must not exceed `TEXT_MAX_BYTES` (10 MB).
/// - Image: RGBA data length must not exceed `IMAGE_MAX_BYTES` (50 MB).
pub fn validate_size(content: &ClipContent) -> Result<(), SizeError> {
    match content {
        ClipContent::Text { text } => {
            let size = text.len();
            if size > TEXT_MAX_BYTES {
                Err(SizeError::TextTooLarge { size, limit: TEXT_MAX_BYTES })
            } else {
                Ok(())
            }
        }
        ClipContent::Image { rgba, .. } => {
            let size = rgba.len();
            if size > IMAGE_MAX_BYTES {
                Err(SizeError::ImageTooLarge { size, limit: IMAGE_MAX_BYTES })
            } else {
                Ok(())
            }
        }
    }
}

// ── PNG encoding helper ───────────────────────────────────────────────────────

/// Encode RGBA pixel data to PNG bytes for network transmission.
fn encode_rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;
    let mut png_buf = Vec::new();
    let encoder = PngEncoder::new(&mut png_buf);
    encoder.write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| anyhow::anyhow!("PNG encode failed: {e}"))?;
    Ok(png_buf)
}

// ── ClipboardSyncService ──────────────────────────────────────────────────────

/// Core sync service — manages config, echo guard, and broadcast decisions.
pub struct ClipboardSyncService {
    config: Arc<Mutex<SyncGroupConfig>>,
    echo_guard: Arc<Mutex<EchoGuard>>,
    enabled: Arc<AtomicBool>,
}

impl ClipboardSyncService {
    /// Create a new service by loading persisted config from disk.
    /// Initialises EchoGuard with a 3-second suppression window and sets the
    /// `enabled` flag from the loaded config.
    pub fn new() -> Self {
        let cfg = SyncGroupConfig::load();
        let enabled = cfg.enabled;
        Self {
            config: Arc::new(Mutex::new(cfg)),
            echo_guard: Arc::new(Mutex::new(EchoGuard::new(Duration::from_secs(3)))),
            enabled: Arc::new(AtomicBool::new(enabled)),
        }
    }

    /// Return a clone of the current in-memory config.
    pub fn config(&self) -> SyncGroupConfig {
        self.config.lock().unwrap().clone()
    }

    /// Replace the in-memory config and persist it to disk.
    pub fn save_config(&self, config: SyncGroupConfig) {
        self.enabled.store(config.enabled, Ordering::SeqCst);
        let mut guard = self.config.lock().unwrap();
        *guard = config.clone();
        SyncGroupConfig::save(&config);
    }

    /// Add a peer to the sync group and persist.
    pub fn add_peer(&self, peer: SyncPeer) {
        let mut guard = self.config.lock().unwrap();
        guard.peers.push(peer);
        SyncGroupConfig::save(&guard);
    }

    /// Remove a peer by `device_name` and persist.
    pub fn remove_peer(&self, device_name: &str) {
        let mut guard = self.config.lock().unwrap();
        guard.peers.retain(|p| p.device_name != device_name);
        SyncGroupConfig::save(&guard);
    }

    /// Update a peer's address and last_seen timestamp, and set online=true.
    pub fn update_peer_status(&self, device_name: &str, addr: &str) {
        let mut guard = self.config.lock().unwrap();
        if let Some(peer) = guard.peers.iter_mut().find(|p| p.device_name == device_name) {
            peer.addr = addr.to_string();
            peer.last_seen = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            peer.online = true;
        }
        SyncGroupConfig::save(&guard);
    }

    /// Enable or disable sync. Updates the AtomicBool and persists the config.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        let mut guard = self.config.lock().unwrap();
        guard.enabled = enabled;
        SyncGroupConfig::save(&guard);
    }

    /// Return only the peers that are currently online.
    pub fn online_peers(&self) -> Vec<SyncPeer> {
        let guard = self.config.lock().unwrap();
        guard.peers.iter().filter(|p| p.online).cloned().collect()
    }

    /// Decide whether the given content should be broadcast to peers.
    ///
    /// Returns `false` if:
    /// - sync is disabled
    /// - the content hash is suppressed by EchoGuard (echo from a remote write)
    /// - the content exceeds size limits
    pub fn should_broadcast(&self, content: &ClipContent) -> bool {
        // 1. Check enabled
        if !self.enabled.load(Ordering::SeqCst) {
            return false;
        }

        // 2. Compute fnv1a hash and check EchoGuard
        let hash = match content {
            ClipContent::Text { text } => fnv1a(text.as_bytes()),
            ClipContent::Image { rgba, .. } => fnv1a(rgba),
        };
        {
            let mut eg = self.echo_guard.lock().unwrap();
            if eg.is_suppressed(hash) {
                return false;
            }
        }

        // 3. Validate size
        if validate_size(content).is_err() {
            return false;
        }

        true
    }

    /// Access the echo guard (e.g. to register a hash after receiving remote content).
    pub fn echo_guard(&self) -> &Arc<Mutex<EchoGuard>> {
        &self.echo_guard
    }

    /// Broadcast clipboard content to all online peers.
    ///
    /// For each online peer, establishes a TCP connection and sends the content
    /// using the existing encrypted transfer protocol. Text content uses
    /// `send_clipboard`, image content uses `send_clipboard_image`.
    ///
    /// Connection failures are logged (eprintln) and skipped — the broadcast
    /// continues to remaining peers. Returns a `BroadcastResult` per peer.
    pub async fn broadcast(
        &self,
        content: &ClipContent,
        local_device_name: &str,
    ) -> Vec<BroadcastResult> {
        let peers = self.online_peers();
        let mut results = Vec::with_capacity(peers.len());

        for peer in &peers {
            let result = match content {
                ClipContent::Text { text } => {
                    let name = format!("clip:text:{}", local_device_name);
                    match tokio::net::TcpStream::connect(&peer.addr).await {
                        Ok(stream) => {
                            match crate::transfer::send_clipboard(
                                stream,
                                text,
                                &name,
                                |_| {},
                            ).await {
                                Ok(()) => BroadcastResult {
                                    device_name: peer.device_name.clone(),
                                    success: true,
                                    error: None,
                                },
                                Err(e) => {
                                    eprintln!(
                                        "warn: clipboard send to {} failed: {}",
                                        peer.device_name, e
                                    );
                                    BroadcastResult {
                                        device_name: peer.device_name.clone(),
                                        success: false,
                                        error: Some(e.to_string()),
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "warn: TCP connect to {} ({}) failed: {}",
                                peer.device_name, peer.addr, e
                            );
                            BroadcastResult {
                                device_name: peer.device_name.clone(),
                                success: false,
                                error: Some(e.to_string()),
                            }
                        }
                    }
                }
                ClipContent::Image { width, height, rgba } => {
                    // Encode RGBA → PNG for transmission
                    let png_data = match encode_rgba_to_png(*width, *height, rgba) {
                        Ok(data) => data,
                        Err(e) => {
                            eprintln!(
                                "warn: PNG encode for {} failed: {}",
                                peer.device_name, e
                            );
                            results.push(BroadcastResult {
                                device_name: peer.device_name.clone(),
                                success: false,
                                error: Some(e.to_string()),
                            });
                            continue;
                        }
                    };
                    let name = format!("clip:image:{}", local_device_name);
                    match tokio::net::TcpStream::connect(&peer.addr).await {
                        Ok(stream) => {
                            match crate::transfer::send_clipboard_image(
                                stream,
                                &png_data,
                                &name,
                                |_| {},
                            ).await {
                                Ok(()) => BroadcastResult {
                                    device_name: peer.device_name.clone(),
                                    success: true,
                                    error: None,
                                },
                                Err(e) => {
                                    eprintln!(
                                        "warn: clipboard image send to {} failed: {}",
                                        peer.device_name, e
                                    );
                                    BroadcastResult {
                                        device_name: peer.device_name.clone(),
                                        success: false,
                                        error: Some(e.to_string()),
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "warn: TCP connect to {} ({}) failed: {}",
                                peer.device_name, peer.addr, e
                            );
                            BroadcastResult {
                                device_name: peer.device_name.clone(),
                                success: false,
                                error: Some(e.to_string()),
                            }
                        }
                    }
                }
            };
            results.push(result);
        }

        results
    }

    /// Handle received clipboard data from a remote device.
    ///
    /// Parses the `name` field (from the transfer header) to determine content
    /// type (`clip:text:DEVICE` or `clip:image:DEVICE`), decodes the payload,
    /// registers the content hash in EchoGuard to prevent echo, and returns
    /// the `ClipContent` along with the source device name.
    pub fn handle_received(
        &self,
        name: &str,
        data: &[u8],
    ) -> anyhow::Result<(ClipContent, String)> {
        let (content, source_device) = if let Some(device) = name.strip_prefix("clip:text:") {
            let text = String::from_utf8_lossy(data).into_owned();
            (ClipContent::Text { text }, device.to_string())
        } else if let Some(device) = name.strip_prefix("clip:image:") {
            // Decode PNG → RGBA
            let cursor = std::io::Cursor::new(data);
            let decoder = image::codecs::png::PngDecoder::new(cursor)
                .map_err(|e| anyhow::anyhow!("PNG decode failed: {e}"))?;
            use image::ImageDecoder;
            let (w, h) = decoder.dimensions();
            let mut rgba = vec![0u8; decoder.total_bytes() as usize];
            decoder.read_image(&mut rgba)
                .map_err(|e| anyhow::anyhow!("PNG read failed: {e}"))?;
            (
                ClipContent::Image { width: w, height: h, rgba },
                device.to_string(),
            )
        } else {
            // Legacy "clipboard" name — treat as text, no source device
            let text = String::from_utf8_lossy(data).into_owned();
            (ClipContent::Text { text }, String::new())
        };

        // Register content hash in EchoGuard to prevent re-broadcast
        let hash = match &content {
            ClipContent::Text { text } => fnv1a(text.as_bytes()),
            ClipContent::Image { rgba, .. } => fnv1a(rgba),
        };
        {
            let mut eg = self.echo_guard.lock().unwrap();
            eg.register(hash);
        }

        Ok((content, source_device))
    }
}
