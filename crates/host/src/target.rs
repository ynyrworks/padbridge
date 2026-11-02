//! Opening a connection to an iPad, over USB (usbmux) or the network.

use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use padbridge_usbmux::{ConnectionType, Device, Usbmux};

use crate::BoxError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Transport {
    Usb { device_id: u64 },
    Network(IpAddr),
}

impl Transport {
    pub fn label(&self) -> &'static str {
        match self {
            Transport::Usb { .. } => "USB",
            Transport::Network(_) => "Wi-Fi",
        }
    }
}

/// Opens a TCP stream to `port` on the iPad. Over USB this is a usbmux
/// tunnel; the iPad app must be listening on that port.
pub fn connect(transport: &Transport, port: u16) -> Result<TcpStream, BoxError> {
    let stream = match transport {
        Transport::Usb { device_id } => Usbmux::new().connect(*device_id, port)?,
        Transport::Network(ip) => TcpStream::connect_timeout(&SocketAddr::new(*ip, port), Duration::from_secs(3))
            .map_err(|e| format!("無法連線 {ip}:{port}：{e}"))?,
    };
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Parses `usb`, `usb:<port>`, `usb:<udid>` or `usb:<udid>:<port>`.
pub fn parse_usb_target(spec: &str, default_port: u16) -> Option<(Option<&str>, u16)> {
    let rest = spec.strip_prefix("usb")?;
    let parts: Vec<&str> = rest.split(':').skip(1).collect();
    match parts.as_slice() {
        [] => Some((None, default_port)),
        [one] => Some(match one.parse() {
            Ok(port) => (None, port),
            Err(_) => (Some(*one), default_port),
        }),
        [udid, port] => Some((Some(*udid), port.parse().ok()?)),
        _ => None,
    }
}

pub fn describe_device(d: &Device) -> String {
    let via = match &d.connection {
        ConnectionType::Usb => "USB".to_string(),
        ConnectionType::Network => "Wi-Fi 同步".to_string(),
        ConnectionType::Other(s) => s.clone(),
    };
    format!("#{:<3} {:<8} {}", d.device_id, via, d.serial)
}

/// Resolves a command-line target (`usb…`, `host` or `host:port`) to a
/// transport and port, plus a human-readable description.
pub fn resolve_target(spec: &str, default_port: u16) -> Result<(Transport, u16, String), BoxError> {
    if spec == "usb" || spec.starts_with("usb:") {
        let (udid, port) = parse_usb_target(spec, default_port).ok_or("無效的 USB 目標")?;
        let devices = Usbmux::new().list_devices()?;
        let device = devices
            .iter()
            .filter(|d| d.connection == ConnectionType::Usb)
            .find(|d| udid.is_none_or(|u| d.serial.eq_ignore_ascii_case(u)))
            .ok_or("找不到 USB 連接的 iPad（執行 `padbridge-cli devices` 檢查）")?;
        let description = format!("USB {} 埠 {port}", describe_device(device));
        Ok((Transport::Usb { device_id: device.device_id }, port, description))
    } else {
        let with_port = if spec.contains(':') { spec.to_string() } else { format!("{spec}:{default_port}") };
        let addr = with_port
            .to_socket_addrs()
            .map_err(|e| format!("無法解析 {with_port}：{e}"))?
            .next()
            .ok_or_else(|| format!("無法解析 {with_port}"))?;
        Ok((Transport::Network(addr.ip()), addr.port(), addr.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_targets() {
        assert_eq!(parse_usb_target("usb", 47800), Some((None, 47800)));
        assert_eq!(parse_usb_target("usb", 47801), Some((None, 47801)));
        assert_eq!(parse_usb_target("usb:47801", 47800), Some((None, 47801)));
        assert_eq!(parse_usb_target("usb:00008103-ABC", 47800), Some((Some("00008103-ABC"), 47800)));
        assert_eq!(parse_usb_target("usb:00008103-ABC:47801", 47800), Some((Some("00008103-ABC"), 47801)));
        assert_eq!(parse_usb_target("usb:a:b:c", 47800), None);
        assert_eq!(parse_usb_target("usb:abc:notaport", 47800), None);
    }

    #[test]
    fn network_targets() {
        let (t, port, _) = resolve_target("127.0.0.1", 47801).unwrap();
        assert_eq!((t, port), (Transport::Network("127.0.0.1".parse().unwrap()), 47801));
        let (_, port, _) = resolve_target("127.0.0.1:5000", 47801).unwrap();
        assert_eq!(port, 5000);
    }
}
