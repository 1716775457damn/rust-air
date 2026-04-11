/// mDNS-SD peer discovery.
///
/// Sender registers a service: _rustair._tcp.local. with TXT record port=<n>
/// Receiver browses for _rustair._tcp.local. and resolves the first match.
///
/// DeviceStatus is encoded in TXT: status=idle | status=busy

use crate::proto::{DeviceInfo, DeviceStatus, MDNS_SERVICE};
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Register this node as a rust-air sender on the LAN.
/// Returns a handle; dropping it unregisters the service.
pub fn register_sender(port: u16, instance_name: &str) -> Result<SenderHandle> {
    let daemon = ServiceDaemon::new()?;
    let mut props = HashMap::new();
    props.insert("status".to_string(), "idle".to_string());
    props.insert("v".to_string(), "2".to_string());

    let hostname = gethostname();
    let svc = ServiceInfo::new(
        MDNS_SERVICE,
        instance_name,
        &hostname,
        "",   // let mdns-sd resolve local IP
        port,
        Some(props),
    )?;
    daemon.register(svc)?;
    Ok(SenderHandle { daemon, fullname: format!("{instance_name}.{MDNS_SERVICE}") })
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

/// Browse the LAN and stream discovered devices over a channel.
/// Runs until the receiver is dropped.
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

                    let addr = info
                        .get_addresses()
                        .iter()
                        .next()
                        .map(|a| format!("{a}:{}", info.get_port()))
                        .unwrap_or_default();

                    let device = DeviceInfo {
                        name:   info.get_fullname().to_string(),
                        addr,
                        status,
                    };
                    if tx.blocking_send(device).is_err() { break; }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    // Signal removal with empty addr so UI can remove the card
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

/// One-shot: find the first sender advertising `instance_name` and return its address.
pub async fn resolve_sender(instance_name: &str) -> Result<(String, u16)> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE)?;
    let target = instance_name.to_string();

    let result = tokio::task::spawn_blocking(move || -> Result<(String, u16)> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("mDNS: sender '{target}' not found within 15 s");
            }
            match receiver.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    // Exact match: fullname is "<instance>.<service>" so split at first dot
                    let instance_part = info.get_fullname()
                        .split('.')
                        .next()
                        .unwrap_or("");
                    if instance_part == target {
                        let ip = info
                            .get_addresses()
                            .iter()
                            .next()
                            .map(|a| a.to_string())
                            .ok_or_else(|| anyhow::anyhow!("no address in mDNS record"))?;
                        let _ = daemon.shutdown();
                        return Ok((ip, info.get_port()));
                    }
                }
                _ => {}
            }
        }
    })
    .await??;
    Ok(result)
}

fn gethostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rust-air-host".to_string())
}
