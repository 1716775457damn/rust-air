//! mDNS-SD peer discovery — v4.
//!
//! CRITICAL: On Windows, only ONE ServiceDaemon can own the UDP 5353 multicast
//! socket at a time. Using two daemons (one for register, one for browse) causes
//! the second daemon to silently fail to send multicast packets, making this
//! device invisible to others on the LAN.
//!
//! Solution: a single shared daemon for both registration and browsing.

use crate::proto::{DeviceInfo, DeviceStatus, MDNS_SERVICE};
use anyhow::Result;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// ── Shared daemon singleton ───────────────────────────────────────────────────

static SHARED_DAEMON: Mutex<Option<Arc<ServiceDaemon>>> = Mutex::new(None);

fn shared_daemon() -> Result<Arc<ServiceDaemon>> {
    let mut guard = SHARED_DAEMON.lock().unwrap();
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
        if let Ok(d) = shared_daemon() {
            let _ = d.unregister(&self.fullname);
        }
    }
}

/// Register this device on the LAN so others can discover and connect to it.
pub fn register_self(port: u16, device_name: &str) -> Result<ServiceHandle> {
    let daemon   = shared_daemon()?;
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

pub struct BrowseHandle {
    daemon:  Arc<ServiceDaemon>,
    service: String,
}

impl Drop for BrowseHandle {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(&self.service);
    }
}

/// Browse the LAN using the shared daemon.
/// Returns a `BrowseHandle` — drop it to stop browsing.
pub fn browse_devices_sync(tx: mpsc::Sender<DeviceInfo>) -> Result<BrowseHandle> {
    let daemon = shared_daemon()?;
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

    Ok(BrowseHandle {
        daemon,
        service: MDNS_SERVICE.to_string(),
    })
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
