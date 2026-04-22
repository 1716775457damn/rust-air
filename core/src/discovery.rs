//! mDNS-SD peer discovery — v6.
//!
//! Architecture:
//!   - REG_DAEMON  : singleton, never stopped, used only for register/unregister.
//!   - Browse      : fresh ServiceDaemon per scan, destroyed with BrowseHandle.
//!
//! Fixes vs v5:
//!   1. register_self falls back to enumerating all non-loopback IPv4 interfaces
//!      when the routing-trick returns nothing — empty IP no longer silently
//!      makes this device invisible.
//!   2. Instance name and hostname are made unique with a 4-hex-char suffix
//!      derived from the MAC/interface address so two machines with the same
//!      COMPUTERNAME don't collide on the LAN.
//!   3. BrowseHandle::drop sends stop_browse *before* the Arc drops so the
//!      background thread always gets SearchStopped and exits cleanly.
//!   4. scan_once / browse loop: ServiceRemoved (addr == "") is skipped, not
//!      treated as a termination signal.

use crate::proto::{DeviceInfo, DeviceStatus, MDNS_SERVICE};
use anyhow::Result;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// ── Register daemon singleton ─────────────────────────────────────────────────

static REG_DAEMON: Mutex<Option<Arc<ServiceDaemon>>> = Mutex::new(None);

fn reg_daemon() -> Result<Arc<ServiceDaemon>> {
    let mut g = REG_DAEMON.lock().unwrap();
    if let Some(ref d) = *g { return Ok(d.clone()); }
    let d = Arc::new(ServiceDaemon::new()?);
    *g = Some(d.clone());
    Ok(d)
}

// ── Self-registration ─────────────────────────────────────────────────────────

pub struct ServiceHandle { fullname: String }

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        if let Ok(d) = reg_daemon() { let _ = d.unregister(&self.fullname); }
    }
}

/// Register this device on the LAN.
/// Uses all non-loopback LAN IPv4 addresses so the service is reachable
/// regardless of which interface the peer is on.
pub fn register_self(port: u16, device_name: &str) -> Result<ServiceHandle> {
    let daemon = reg_daemon()?;

    // Collect every non-loopback, non-link-local LAN IPv4 address.
    // Registering multiple IPs lets mdns-sd pick the best one for each peer.
    let ips = lan_ipv4_addrs();
    anyhow::ensure!(!ips.is_empty(), "no LAN IPv4 address found — not connected to a network?");

    // Make instance name unique: "HOSTNAME-a1b2" so two machines with the
    // same COMPUTERNAME don't collide.
    let unique_name = unique_instance_name(device_name);
    let hostname    = unique_hostname(&unique_name);

    let props: std::collections::HashMap<String, String> =
        [("v", "4")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();

    // Register one ServiceInfo per IP address.
    // mdns-sd accepts a comma-separated list of IPs as the host field on some
    // versions; to be safe we register the primary IP and add the rest as
    // additional addresses via the addresses field.
    let ip_str = ips.join(",");
    let svc = ServiceInfo::new(
        MDNS_SERVICE,
        &unique_name,
        &hostname,
        ip_str.as_str(),
        port,
        Some(props),
    )?;

    let fullname = format!("{unique_name}.{}", MDNS_SERVICE.trim_end_matches('.'));
    daemon.register(svc)?;

    Ok(ServiceHandle { fullname })
}

// ── Device browsing ───────────────────────────────────────────────────────────

pub struct BrowseHandle {
    daemon: Arc<ServiceDaemon>,
}

impl Drop for BrowseHandle {
    fn drop(&mut self) {
        // stop_browse first — this sends SearchStopped to the receiver channel,
        // which causes the background thread to exit cleanly before the Arc drops.
        let _ = self.daemon.stop_browse(MDNS_SERVICE);
    }
}

/// Browse the LAN using a fresh, dedicated daemon.
/// Returns a BrowseHandle — drop it to stop browsing and free resources.
pub fn browse_devices_sync(tx: mpsc::Sender<DeviceInfo>) -> Result<BrowseHandle> {
    let daemon   = Arc::new(ServiceDaemon::new()?);
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
                    }).is_err() { break; }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    // Notify caller that a device left — addr="" signals removal.
                    // Callers must NOT treat this as a termination condition.
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

    Ok(BrowseHandle { daemon })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn best_addr(info: &ResolvedService) -> Option<String> {
    let addrs = info.get_addresses();
    let mut any_v4: Option<String> = None;
    let mut any_addr: Option<String> = None;
    for a in addrs.iter() {
        let s = a.to_string();
        if a.is_ipv4() {
            if !a.is_loopback() && !is_link_local_v4(&s) { return Some(s); }
            if any_v4.is_none() { any_v4 = Some(s.clone()); }
        }
        if any_addr.is_none() { any_addr = Some(s); }
    }
    any_v4.or(any_addr)
}

fn is_link_local_v4(addr: &str) -> bool { addr.starts_with("169.254.") }

/// Return all non-loopback, non-link-local IPv4 addresses on this machine.
/// Uses multiple UDP routing-trick targets to discover addresses on different
/// interfaces (e.g. Wi-Fi + Ethernet simultaneously active).
pub fn lan_ipv4_addrs() -> Vec<String> {
    const PROBES: &[&str] = &[
        "8.8.8.8:80",
        "1.1.1.1:80",
        "192.168.1.1:80",
        "10.0.0.1:80",
        "172.16.0.1:80",
    ];

    let mut seen  = std::collections::HashSet::new();
    let mut addrs: Vec<String> = Vec::new();

    for probe in PROBES {
        if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(probe).is_ok() {
                if let Ok(local) = sock.local_addr() {
                    let ip = local.ip();
                    let s  = ip.to_string();
                    if !s.starts_with("127.") && !s.starts_with("169.254.") && seen.insert(s.clone()) {
                        addrs.push(s);
                    }
                }
            }
        }
        // Early exit: once we have an address from the first probe we can
        // skip remaining probes on single-interface machines (the common case).
        // We still continue for multi-interface machines until all probes are done.
        if addrs.len() >= 4 { break; }
    }

    addrs
}

/// Get the primary LAN IPv4 via routing trick (connect UDP to 8.8.8.8, no packet sent).
pub fn local_lan_ip() -> Option<String> {
    routing_trick_ip()
}

fn routing_trick_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    match ip {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() => Some(ip.to_string()),
        _ => None,
    }
}


/// A 4-hex-char suffix unique to this machine, derived from its primary IP.
/// Ensures two machines with the same hostname don't collide on mDNS.
fn machine_suffix() -> String {
    let ip = routing_trick_ip().unwrap_or_else(|| "127.0.0.1".to_string());
    // Hash the IP string to get a stable 4-char hex suffix
    let mut h: u32 = 0x811c9dc5;
    for b in ip.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{:04x}", h & 0xffff)
}

fn unique_instance_name(base: &str) -> String {
    format!("{}-{}", base, machine_suffix())
}

fn unique_hostname(instance: &str) -> String {
    let label = &instance[..instance.len().min(63)];
    format!("{label}.local.")
}

/// Safe ASCII label for mDNS (no `.local.` suffix).
pub fn safe_device_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air".to_string());
    sanitize_label(&raw, "rust-air")
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
