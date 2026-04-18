//! mDNS-SD peer discovery — v4.
//!
//! Key design decisions:
//! - A single shared `ServiceDaemon` is used for both registration and browsing
//!   to avoid UDP 5353 port conflicts on Windows.
//! - `browse_devices` uses a dedicated short-lived daemon with an explicit
//!   shutdown signal so the blocking recv() thread exits cleanly.

use crate::proto::{DeviceInfo, DeviceStatus, MDNS_SERVICE};
use anyhow::Result;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

// ── Self-registration ─────────────────────────────────────────────────────────

pub struct ServiceHandle {
    daemon:   ServiceDaemon,
    fullname: String,
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        // Give mDNS a moment to send the goodbye packet before shutdown.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = self.daemon.shutdown();
    }
}

/// Register this device on the LAN so others can discover and connect to it.
pub fn register_self(port: u16, device_name: &str) -> Result<ServiceHandle> {
    let daemon   = ServiceDaemon::new()?;
    let hostname = safe_hostname();
    let local_ip = local_lan_ip().unwrap_or_default();

    let props: std::collections::HashMap<String, String> =
        [("v", "4")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();

    let svc = ServiceInfo::new(
        MDNS_SERVICE,
        device_name,
        &hostname,
        local_ip.as_str(),
        port,
        Some(props),
    )?;

    // mdns-sd fullname format: "<instance>.<service-type>" — no trailing dot here.
    // The library appends the trailing dot internally; we must NOT include it.
    let fullname = format!("{device_name}.{}", MDNS_SERVICE.trim_end_matches('.'));

    daemon.register(svc)?;

    Ok(ServiceHandle { daemon, fullname })
}

// ── Device browsing ───────────────────────────────────────────────────────────

/// Browse the LAN, streaming `DeviceInfo` events over `tx`.
/// Returns a `BrowseHandle` that shuts down the daemon when dropped.
/// The caller must drop the handle to stop browsing.
pub fn browse_devices_sync(tx: mpsc::Sender<DeviceInfo>) -> Result<BrowseHandle> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE)?;

    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let addr = best_addr(&info)
                        .map(|a| format!("{a}:{}", info.get_port()))
                        .unwrap_or_default();
                    if addr.is_empty() { continue; }
                    if tx.blocking_send(DeviceInfo {
                        name:   info.get_fullname().to_string(),
                        addr,
                        status: DeviceStatus::Idle,
                    }).is_err() {
                        break;
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    let _ = tx.blocking_send(DeviceInfo {
                        name:   fullname,
                        addr:   String::new(),
                        status: DeviceStatus::Idle,
                    });
                }
                _ => {}
            }
        }
        // daemon already shut down by BrowseHandle::drop; this is a no-op.
    });

    Ok(BrowseHandle { daemon })
}

/// Dropping this handle shuts down the browse daemon,
/// which causes `receiver.recv()` in the background thread to return `Err` and exit.
pub struct BrowseHandle {
    daemon: ServiceDaemon,
}

impl Drop for BrowseHandle {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

// Keep async version for CLI scan_once compatibility.
pub async fn browse_devices(tx: mpsc::Sender<DeviceInfo>) -> Result<()> {
    let _handle = browse_devices_sync(tx)?;
    // Block until the handle is dropped externally (tx drop causes thread to exit,
    // but we hold the handle here — so this version runs until tx is dropped).
    // For CLI use: caller drops tx via timeout, thread exits, then we drop handle.
    // We just park here; the thread will exit when tx is dropped by the caller.
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // BrowseHandle dropped when this future is cancelled/dropped by caller.
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn best_addr(info: &ResolvedService) -> Option<String> {
    let addrs = info.get_addresses();
    // Priority: routable IPv4 > any IPv4 > IPv6
    addrs.iter()
        .find(|a| a.is_ipv4() && !a.is_loopback() && !is_link_local_v4(&a.to_string()))
        .or_else(|| addrs.iter().find(|a| a.is_ipv4()))
        .or_else(|| addrs.iter().next())
        .map(|a| a.to_string())
}

fn is_link_local_v4(addr: &str) -> bool {
    addr.starts_with("169.254.")
}

/// Get the primary LAN IPv4 address by routing toward 8.8.8.8 (no packet sent).
pub fn local_lan_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

/// Safe ASCII device name for mDNS service instance label (no `.local.` suffix).
pub fn safe_device_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air".to_string());
    sanitize_label(&raw, "rust-air")
}

/// Safe hostname for mDNS, ends with `.local.`
fn safe_hostname() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air-host".to_string());
    let label = sanitize_label(&raw, "rust-air-host");
    let label = &label[..label.len().min(63)];
    format!("{label}.local.")
}

/// Replace non-ASCII-alphanumeric chars with `-`, collapse runs, enforce non-empty.
fn sanitize_label(raw: &str, fallback: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { fallback.to_string() } else { s }
}
