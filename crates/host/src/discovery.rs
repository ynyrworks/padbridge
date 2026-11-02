//! Finds iPads: attached over USB (usbmux notifications) and running the
//! PadBridge app on the local network (Bonjour `_padbridge._tcp`).

use std::net::IpAddr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use padbridge_protocol::Role;
use padbridge_usbmux::{ConnectionType, DeviceEvent, Usbmux};

use crate::target::Transport;

const SERVICE_TYPE: &str = "_padbridge._tcp.local.";

/// One place we can connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Stable identity for add/remove events.
    pub key: String,
    pub name: String,
    pub transport: Transport,
    /// Known for Bonjour services; for USB the caller picks the port per mode.
    pub port: Option<u16>,
    pub role: Option<Role>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    Added(Endpoint),
    Removed { key: String },
    /// A discovery source is unavailable (e.g. Apple Mobile Device Service not installed).
    Status { source: &'static str, message: String },
}

/// Keeps discovery running while alive.
pub struct Discovery {
    mdns: Option<ServiceDaemon>,
}

impl Discovery {
    pub fn start() -> (Discovery, Receiver<DiscoveryEvent>) {
        let (tx, rx) = mpsc::channel();
        spawn_usb(tx.clone());
        let mdns = match ServiceDaemon::new().and_then(|d| d.browse(SERVICE_TYPE).map(|r| (d, r))) {
            Ok((daemon, events)) => {
                thread::spawn(move || {
                    while let Ok(event) = events.recv() {
                        let msg = match event {
                            ServiceEvent::ServiceResolved(info) => {
                                let Some(ip) = info.get_addresses_v4().into_iter().next() else { continue };
                                Some(DiscoveryEvent::Added(Endpoint {
                                    key: info.get_fullname().to_string(),
                                    name: instance_name(info.get_fullname()),
                                    transport: Transport::Network(IpAddr::V4(ip)),
                                    port: Some(info.get_port()),
                                    role: info
                                        .get_property_val_str("role")
                                        .and_then(|r| r.parse::<u8>().ok())
                                        .and_then(|r| Role::try_from(r).ok()),
                                }))
                            }
                            ServiceEvent::ServiceRemoved(_, fullname) => Some(DiscoveryEvent::Removed { key: fullname }),
                            _ => None,
                        };
                        if let Some(msg) = msg {
                            if tx.send(msg).is_err() {
                                return;
                            }
                        }
                    }
                });
                Some(daemon)
            }
            Err(e) => {
                let _ = tx.send(DiscoveryEvent::Status { source: "Wi-Fi", message: format!("Bonjour 無法啟動：{e}") });
                None
            }
        };
        (Discovery { mdns }, rx)
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        if let Some(daemon) = &self.mdns {
            let _ = daemon.shutdown();
        }
    }
}

/// `My iPad._padbridge._tcp.local.` -> `My iPad`
fn instance_name(fullname: &str) -> String {
    fullname.strip_suffix(&format!(".{SERVICE_TYPE}")).unwrap_or(fullname).replace("\\032", " ")
}

fn usb_key(device_id: u64) -> String {
    format!("usb:{device_id}")
}

/// Follows usbmux attach/detach events, reconnecting if the service restarts.
fn spawn_usb(tx: Sender<DiscoveryEvent>) {
    thread::spawn(move || {
        let mut last_status = String::new();
        let mut attached: Vec<u64> = Vec::new();
        loop {
            let status = match Usbmux::new().listen() {
                Ok(events) => {
                    if tx.send(DiscoveryEvent::Status { source: "USB", message: String::new() }).is_err() {
                        return;
                    }
                    last_status.clear();
                    for event in events {
                        let msg = match event {
                            Ok(DeviceEvent::Attached(d)) if d.connection == ConnectionType::Usb => {
                                attached.push(d.device_id);
                                let short = d.serial.get(..8).unwrap_or(&d.serial);
                                DiscoveryEvent::Added(Endpoint {
                                    key: usb_key(d.device_id),
                                    name: format!("iPad ({short}…)"),
                                    transport: Transport::Usb { device_id: d.device_id },
                                    port: None,
                                    role: None,
                                })
                            }
                            Ok(DeviceEvent::Detached { device_id }) => {
                                attached.retain(|&id| id != device_id);
                                DiscoveryEvent::Removed { key: usb_key(device_id) }
                            }
                            Ok(_) => continue,
                            Err(_) => break,
                        };
                        if tx.send(msg).is_err() {
                            return;
                        }
                    }
                    "與 Apple Mobile Device Service 的連線中斷".to_string()
                }
                Err(e) => format!("無法使用 USB：{e}（需要「Apple 裝置」App 或 iTunes）"),
            };
            // The listen stream ended: whatever was attached is no longer known.
            for id in attached.drain(..) {
                if tx.send(DiscoveryEvent::Removed { key: usb_key(id) }).is_err() {
                    return;
                }
            }
            if status != last_status {
                if tx.send(DiscoveryEvent::Status { source: "USB", message: status.clone() }).is_err() {
                    return;
                }
                last_status = status;
            }
            thread::sleep(Duration::from_secs(3));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names() {
        assert_eq!(instance_name("Ynyr iPad._padbridge._tcp.local."), "Ynyr iPad");
        assert_eq!(instance_name("A\\032B._padbridge._tcp.local."), "A B");
        assert_eq!(instance_name("weird"), "weird");
    }
}
