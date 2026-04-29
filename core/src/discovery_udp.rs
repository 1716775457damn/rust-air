//! UDP broadcast device discovery — Android / non-desktop fallback.
//!
//! Replaces the mDNS-SD based `discovery` module when the `desktop` feature is
//! not enabled. The public interface mirrors `discovery.rs` so callers compile
//! without changes.
//!
//! Protocol:
//!   MAGIC (8 B "RUSTAIR1") + port (2 B LE) + name_len (1 B) + name (UTF-8)

use crate::proto::{DeviceInfo, DeviceStatus};
use anyhow::Result;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

const BROADCAST_PORT: u16 = 51820;
const MAGIC: &[u8; 8] = b"RUSTAIR1";
const BROADCAST_INTERVAL_MS: u64 = 2000;

// ── ServiceHandle (register) ──────────────────────────────────────────────────

/// Handle returned by `register_self`. Dropping it stops the periodic broadcast.
pub struct ServiceHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Register this device on the LAN by periodically broadcasting a UDP packet
/// to `255.255.255.255:BROADCAST_PORT`.
pub fn register_self(port: u16, device_name: &str) -> Result<ServiceHandle> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;

    let packet = build_packet(port, device_name);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    std::thread::spawn(move || {
        let dest = format!("255.255.255.255:{BROADCAST_PORT}");
        while !stop2.load(Ordering::SeqCst) {
            let _ = sock.send_to(&packet, &dest);
            std::thread::sleep(std::time::Duration::from_millis(BROADCAST_INTERVAL_MS));
        }
    });

    Ok(ServiceHandle { stop })
}

// ── BrowseHandle (browse) ─────────────────────────────────────────────────────

/// Handle returned by `browse_devices_sync`. Dropping it stops the listener.
pub struct BrowseHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for BrowseHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Listen for UDP broadcast packets and send discovered `DeviceInfo` through `tx`.
pub fn browse_devices_sync(tx: mpsc::Sender<DeviceInfo>) -> Result<BrowseHandle> {
    let sock = UdpSocket::bind(format!("0.0.0.0:{BROADCAST_PORT}"))?;
    sock.set_broadcast(true)?;
    // Non-blocking with a short timeout so we can check the stop flag.
    sock.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    // Collect our own IPs so we can skip self-broadcasts.
    let my_ips: std::collections::HashSet<String> =
        lan_ipv4_addrs().into_iter().collect();

    std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        while !stop2.load(Ordering::SeqCst) {
            let (n, src_addr) = match sock.recv_from(&mut buf) {
                Ok(v) => v,
                Err(_) => continue, // timeout or transient error
            };

            // Skip packets from ourselves.
            let src_ip = src_addr.ip().to_string();
            if my_ips.contains(&src_ip) {
                continue;
            }

            if let Some((port, name)) = parse_packet(&buf[..n]) {
                let addr = format!("{}:{}", src_ip, port);
                if tx.blocking_send(DeviceInfo {
                    name,
                    addr,
                    status: DeviceStatus::Idle,
                }).is_err() {
                    break;
                }
            }
        }
    });

    Ok(BrowseHandle { stop })
}

// ── Packet helpers ────────────────────────────────────────────────────────────

/// Build a broadcast packet: MAGIC(8) + port(2 LE) + name_len(1) + name(UTF-8)
fn build_packet(port: u16, name: &str) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(255) as u8;
    let mut pkt = Vec::with_capacity(8 + 2 + 1 + name_len as usize);
    pkt.extend_from_slice(MAGIC);
    pkt.extend_from_slice(&port.to_le_bytes());
    pkt.push(name_len);
    pkt.extend_from_slice(&name_bytes[..name_len as usize]);
    pkt
}

/// Parse a broadcast packet. Returns `(port, device_name)` on success.
fn parse_packet(data: &[u8]) -> Option<(u16, String)> {
    if data.len() < 11 {
        return None; // 8 magic + 2 port + 1 name_len minimum
    }
    if &data[..8] != MAGIC {
        return None;
    }
    let port = u16::from_le_bytes([data[8], data[9]]);
    let name_len = data[10] as usize;
    if data.len() < 11 + name_len {
        return None;
    }
    let name = String::from_utf8_lossy(&data[11..11 + name_len]).into_owned();
    Some((port, name))
}

// ── Network helpers (compatible with discovery.rs public API) ─────────────────

/// Get the primary LAN IPv4 via the routing trick (connect UDP to 8.8.8.8).
pub fn local_lan_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    match ip {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() => {
            Some(ip.to_string())
        }
        _ => None,
    }
}

/// Return all non-loopback, non-link-local IPv4 addresses on this machine.
/// Uses the routing trick with multiple probe targets since `if-addrs` is not
/// available without the `desktop` feature.
pub fn lan_ipv4_addrs() -> Vec<String> {
    const PROBES: &[&str] = &[
        "8.8.8.8:80",
        "1.1.1.1:80",
        "192.168.1.1:80",
        "10.0.0.1:80",
    ];
    let mut addrs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for probe in PROBES {
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(probe).is_ok() {
                if let Ok(local) = sock.local_addr() {
                    let s = local.ip().to_string();
                    if !s.starts_with("127.")
                        && !s.starts_with("169.254.")
                        && seen.insert(s.clone())
                    {
                        addrs.push(s);
                    }
                }
            }
        }
    }
    addrs
}

/// Safe ASCII device name for use in broadcast packets.
pub fn safe_device_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air".to_string());
    let s: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() {
        "rust-air".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_roundtrip() {
        let pkt = build_packet(12345, "my-device");
        let (port, name) = parse_packet(&pkt).unwrap();
        assert_eq!(port, 12345);
        assert_eq!(name, "my-device");
    }

    #[test]
    fn packet_rejects_short() {
        assert!(parse_packet(b"short").is_none());
    }

    #[test]
    fn packet_rejects_bad_magic() {
        let mut pkt = build_packet(80, "test");
        pkt[0] = b'X';
        assert!(parse_packet(&pkt).is_none());
    }

    #[test]
    fn safe_device_name_non_empty() {
        let name = safe_device_name();
        assert!(!name.is_empty());
    }

    #[test]
    fn local_lan_ip_format() {
        // May return None in CI environments without network
        if let Some(ip) = local_lan_ip() {
            assert!(!ip.starts_with("127."));
            assert!(!ip.starts_with("169.254."));
        }
    }
}
