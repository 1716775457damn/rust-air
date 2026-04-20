//! mDNS-SD peer discovery — v5.
//!
//! Architecture:
//!   - REGISTER daemon: a long-lived singleton used exclusively for
//!     `register_self`. Stays alive for the entire app lifetime so our
//!     mDNS advertisement is never interrupted.
//!
//!   - BROWSE daemon: a fresh `ServiceDaemon` created per scan and
//!     shutdown when the `BrowseHandle` is dropped. Completely isolated
//!     from the register daemon so stopping a scan never affects our own
//!     advertisement.
//!
//! Why two daemons instead of one?
//!   On Windows a single daemon CAN do both, but `stop_browse` on the
//!   shared daemon was silently clearing internal browse state and making
//!   subsequent scans return zero results. Separating concerns is simpler
//!   and more robust.

use crate::proto::{DeviceInfo, DeviceStatus, MDNS_SERVICE};
use anyhow::Result;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// ── Register daemon singleton (never stopped) ─────────────────────────────────

static REG_DAEMON: Mutex<Option<Arc<ServiceDaemon>>> = Mutex::new(None);

fn reg_daemon() -> Result<Arc<ServiceDaemon>> {
    let mut guard = REG_DAEMON.lock().unwrap();
    if let Some(ref d) = *guard {
        return Ok(d.clone());
    }
    let d = Arc::new(ServiceDaemon::new()?);
    *guard = Some(d.clone());
    Ok(d)
}

// ── Self-registration ─────────────────────────────────────────────────────────

pub struct ServiceHandle {
    fullname: String,
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        if let Ok(d) = reg_daemon() {
            let _ = d.unregister(&self.fullname);
        }
    }
}

/// Register this device on the LAN so others can discover and connect to it.
pub fn register_self(port: u16, device_name: &str) -> Result<ServiceHandle> {
    let daemon   = reg_daemon()?;
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

    let fullname = format!("{device_name}.{}", MDNS_SERVICE.trim_end_matches('.'));
    daemon.register(svc)?;

    Ok(ServiceHandle { fullname })
}

// ── Device browsing ───────────────────────────────────────────────────────────

/// Owns the per-scan daemon. Dropping this stops the browse and shuts down
/// the temporary daemon — the register daemon is completely unaffected.
pub struct BrowseHandle {
    // Keep the daemon alive until the handle is dropped.
    _daemon: Arc<ServiceDaemon>,
}

impl Drop for BrowseHandle {
    fn drop(&mut self) {
        // stop_browse on the *browse* daemon only — register daemon untouched.
        let _ = self._daemon.stop_browse(MDNS_SERVICE);
        // Arc refcount drops to zero here → daemon shuts down.
    }
}

/// Browse the LAN using a **fresh, dedicated daemon** (never shared with
/// registration). Returns a `BrowseHandle` — drop it to stop browsing.
pub fn browse_devices_sync(tx: mpsc::Sender<DeviceInfo>) -> Result<BrowseHandle> {
    // Always create a new daemon for browsing so stop_browse never touches
    // the registration daemon.
    let daemon = Arc::new(ServiceDaemon::new()?);
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
                ServiceEvent::SearchStopped(_) => break,
                _ => {}
            }
        }
    });

    Ok(BrowseHandle { _daemon: daemon })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn best_addr(info: &ResolvedService) -> Option<String> {
    let addrs = info.get_addresses();
    // Single pass: prefer non-loopback non-link-local IPv4, then any IPv4, then any.
    let mut any_v4: Option<String> = None;
    let mut any_addr: Option<String> = None;
    for a in addrs.iter() {
        let s = a.to_string();
        if a.is_ipv4() {
            if !a.is_loopback() && !is_link_local_v4(&s) {
                return Some(s);
            }
            if any_v4.is_none() { any_v4 = Some(s.clone()); }
        }
        if any_addr.is_none() { any_addr = Some(s); }
    }
    any_v4.or(any_addr)
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

fn safe_hostname() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air-host".to_string());
    let label = sanitize_label(&raw, "rust-air-host");
    let label = &label[..label.len().min(63)];
    format!("{label}.local.")
}

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
