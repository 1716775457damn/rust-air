//! mDNS-SD peer discovery — v3.
//!
//! Every running instance registers itself so others can find it.
//! No pre-shared key: the sender embeds the key in the transfer header.

use crate::proto::{DeviceInfo, DeviceStatus, MDNS_SERVICE};
use anyhow::Result;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

const _BROWSE_POLL_MS: u64 = 500;

// ── Self-registration ─────────────────────────────────────────────────────────

/// Register this device on the LAN so others can discover and connect to it.
/// `port` is the TCP port this device is listening on for incoming transfers.
/// `device_name` is the human-readable name shown in peer lists.
/// Drop the returned handle to unregister.
pub fn register_self(port: u16, device_name: &str) -> Result<ServiceHandle> {
    let daemon   = ServiceDaemon::new()?;
    let hostname = local_hostname();
    let props: std::collections::HashMap<String, String> =
        [("v", "3")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();

    let svc = ServiceInfo::new(MDNS_SERVICE, device_name, &hostname, "", port, Some(props))?;
    daemon.register(svc)?;

    Ok(ServiceHandle {
        daemon,
        fullname: format!("{device_name}.{MDNS_SERVICE}"),
    })
}

pub struct ServiceHandle {
    daemon:   ServiceDaemon,
    fullname: String,
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

// ── Device browsing ───────────────────────────────────────────────────────────

/// Browse the LAN continuously, streaming `DeviceInfo` events over `tx`.
/// Runs until the channel receiver is dropped.
pub async fn browse_devices(tx: mpsc::Sender<DeviceInfo>) -> Result<()> {
    let daemon   = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE)?;

    tokio::task::spawn_blocking(move || {
        while let Ok(event) = receiver.recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let addr = best_addr(&info)
                        .map(|a| format!("{a}:{}", info.get_port()))
                        .unwrap_or_default();
                    if tx.blocking_send(DeviceInfo {
                        name:   info.get_fullname().to_string(),
                        addr,
                        status: DeviceStatus::Idle,
                    }).is_err() { break; }
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
        let _ = daemon.shutdown();
    }).await?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn best_addr(info: &ResolvedService) -> Option<String> {
    let addrs = info.get_addresses();
    addrs.iter().find(|a| a.is_ipv4())
        .or_else(|| addrs.iter().next())
        .map(|a| a.to_string())
}

fn local_hostname() -> String {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air-host".to_string());
    if name.ends_with(".local.") { name }
    else if name.ends_with(".local") { format!("{name}.") }
    else { format!("{name}.local.") }
}
