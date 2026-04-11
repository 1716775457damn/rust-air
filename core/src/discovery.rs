//! mDNS-SD peer discovery.
//!
//! Sender registers `_rustair._tcp.local.` with TXT `status=idle|busy` and `v=2`.
//! Receiver browses the same service type and resolves by exact instance name.

use crate::proto::{DeviceInfo, DeviceStatus, MDNS_SERVICE};
use anyhow::Result;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

const RESOLVE_TIMEOUT_SECS: u64 = 15;
const BROWSE_POLL_MS: u64 = 500;

// ── Sender registration ───────────────────────────────────────────────────────

/// Register this node as a rust-air sender on the LAN.
/// The returned handle keeps the mDNS advertisement alive; dropping it unregisters.
pub fn register_sender(port: u16, instance_name: &str) -> Result<SenderHandle> {
    let daemon = ServiceDaemon::new()?;
    let hostname = gethostname();

    let props: std::collections::HashMap<String, String> =
        [("status", "idle"), ("v", "2")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let svc = ServiceInfo::new(
        MDNS_SERVICE,
        instance_name,
        &hostname,
        "",
        port,
        Some(props),
    )?;
    daemon.register(svc)?;

    Ok(SenderHandle {
        daemon,
        fullname: format!("{instance_name}.{MDNS_SERVICE}"),
    })
}

pub struct SenderHandle {
    daemon:   ServiceDaemon,
    fullname: String,
}

impl Drop for SenderHandle {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

// ── Device browsing ───────────────────────────────────────────────────────────

/// Browse the LAN continuously, streaming `DeviceInfo` events over `tx`.
/// Runs until the channel receiver is dropped.
pub async fn browse_devices(tx: mpsc::Sender<DeviceInfo>) -> Result<()> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE)?;

    tokio::task::spawn_blocking(move || {
        while let Ok(event) = receiver.recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let status = info
                        .get_property_val_str("status")
                        .map(|s| if s == "busy" { DeviceStatus::Busy } else { DeviceStatus::Idle })
                        .unwrap_or(DeviceStatus::Idle);

                    let addr = best_addr(&info)
                        .map(|a| format!("{a}:{}", info.get_port()))
                        .unwrap_or_default();

                    if tx.blocking_send(DeviceInfo {
                        name: info.get_fullname().to_string(),
                        addr,
                        status,
                    }).is_err() {
                        break;
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    // Empty addr signals removal to the UI.
                    let _ = tx.blocking_send(DeviceInfo {
                        name:   fullname,
                        addr:   String::new(),
                        status: DeviceStatus::Idle,
                    });
                }
                _ => {}
            }
        }
        let _ = daemon.shutdown();
    })
    .await?;
    Ok(())
}

// ── One-shot resolve ──────────────────────────────────────────────────────────

/// Resolve `instance_name` to `(ip, port)` via mDNS, timing out after 15 s.
pub async fn resolve_sender(instance_name: &str) -> Result<(String, u16)> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE)?;
    let target = instance_name.to_string();
    let timeout = std::time::Duration::from_secs(RESOLVE_TIMEOUT_SECS);
    let poll   = std::time::Duration::from_millis(BROWSE_POLL_MS);

    tokio::task::spawn_blocking(move || -> Result<(String, u16)> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            anyhow::ensure!(
                std::time::Instant::now() <= deadline,
                "mDNS: '{target}' not found within {RESOLVE_TIMEOUT_SECS} s"
            );
            if let Ok(ServiceEvent::ServiceResolved(info)) = receiver.recv_timeout(poll) {
                // Exact instance match: fullname = "<instance>.<service>"
                let instance_part = info.get_fullname().split('.').next().unwrap_or("");
                if instance_part == target {
                    let ip = best_addr(&info)
                        .ok_or_else(|| anyhow::anyhow!("no usable address in mDNS record"))?;
                    let _ = daemon.shutdown();
                    return Ok((ip, info.get_port()));
                }
            }
        }
    })
    .await?
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Prefer IPv4 over IPv6 when multiple addresses are advertised.
/// Returns the address as a string (e.g. "192.168.1.5").
fn best_addr(info: &ResolvedService) -> Option<String> {
    let addrs = info.get_addresses();
    // ScopedIp wraps IpAddr; prefer IPv4 for maximum compatibility.
    addrs.iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.iter().next())
        .map(|a| a.to_string())
}

fn gethostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air-host".to_string())
}
