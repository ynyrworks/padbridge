use std::net::TcpStream;
use std::process::ExitCode;
use std::time::Duration;

use padbridge_cli::{ping_session, Event};
use padbridge_protocol::DISPLAY_PORT;
use padbridge_usbmux::{ConnectionType, Device, DeviceEvent, Usbmux};

const USAGE: &str = "\
用法：
  padbridge devices                     列出透過 USB 連接的 iPad
  padbridge watch                       持續顯示 iPad 插入 / 拔除
  padbridge ping <目標> [次數]          握手並量測 RTT

<目標>：
  usb[:埠]          第一台 USB 連接的 iPad（usbmux）
  usb:<UDID>[:埠]   指定 UDID 的 iPad
  <IP>[:埠]         Wi‑Fi / 區網

埠預設 47800（Display 模式）；Mirror 模式的廣播擴充用 47801。
USB 需要 Apple Mobile Device Service（「Apple 裝置」App 或 iTunes）。";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["devices"] => devices(),
        ["watch"] => watch(),
        ["ping", target] => ping(target, 10),
        ["ping", target, count] => match count.parse() {
            Ok(n) => ping(target, n),
            Err(_) => return usage(),
        },
        _ => return usage(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("錯誤：{e}");
            ExitCode::FAILURE
        }
    }
}

type CliResult = Result<ExitCode, Box<dyn std::error::Error>>;

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn describe(d: &Device) -> String {
    let via = match &d.connection {
        ConnectionType::Usb => "USB".to_string(),
        ConnectionType::Network => "Wi‑Fi 同步".to_string(),
        ConnectionType::Other(s) => s.clone(),
    };
    format!("#{:<3} {:<8} {}", d.device_id, via, d.serial)
}

fn devices() -> CliResult {
    let devices = Usbmux::new().list_devices()?;
    if devices.is_empty() {
        println!("沒有裝置。請用 USB 接上 iPad，並在 iPad 上點「信任這部電腦」。");
    }
    for d in &devices {
        println!("{}", describe(d));
    }
    Ok(ExitCode::SUCCESS)
}

fn watch() -> CliResult {
    println!("監看中，按 Ctrl+C 結束");
    for event in Usbmux::new().listen()? {
        match event? {
            DeviceEvent::Attached(d) => println!("+ {}", describe(&d)),
            DeviceEvent::Detached { device_id } => println!("- #{device_id}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Parses `usb`, `usb:<port>`, `usb:<udid>` or `usb:<udid>:<port>`.
fn parse_usb_target(spec: &str) -> Option<(Option<&str>, u16)> {
    let rest = spec.strip_prefix("usb")?;
    let parts: Vec<&str> = rest.split(':').skip(1).collect();
    match parts.as_slice() {
        [] => Some((None, DISPLAY_PORT)),
        [one] => Some(match one.parse() {
            Ok(port) => (None, port),
            Err(_) => (Some(*one), DISPLAY_PORT),
        }),
        [udid, port] => Some((Some(*udid), port.parse().ok()?)),
        _ => None,
    }
}

fn connect_usb(udid: Option<&str>, port: u16) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let mux = Usbmux::new();
    let devices = mux.list_devices()?;
    let device = devices
        .iter()
        .filter(|d| d.connection == ConnectionType::Usb)
        .find(|d| udid.is_none_or(|u| d.serial.eq_ignore_ascii_case(u)))
        .ok_or("找不到 USB 連接的 iPad（執行 `padbridge devices` 檢查）")?;
    println!("經 USB 連線 {} 埠 {port}", describe(device));
    Ok(mux.connect(device.device_id, port)?)
}

fn ping(target: &str, count: u32) -> CliResult {
    let stream = if target == "usb" || target.starts_with("usb:") {
        let Some((udid, port)) = parse_usb_target(target) else { return Ok(usage()) };
        connect_usb(udid, port)?
    } else {
        let addr = if target.contains(':') { target.to_string() } else { format!("{target}:{DISPLAY_PORT}") };
        let stream = TcpStream::connect(&addr).map_err(|e| format!("無法連線 {addr}：{e}"))?;
        println!("已連線 {addr}");
        stream
    };

    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PadBridge host".into());
    let summary = ping_session(stream, &name, count, Duration::from_millis(200), |event| match event {
        Event::Rtt(rtt) => println!("RTT {:.2} ms", rtt.as_secs_f64() * 1000.0),
        Event::Received(msg) => println!("← {msg:?}"),
    })?;
    if summary.rtts.is_empty() {
        eprintln!("沒有收到任何 Pong");
        return Ok(ExitCode::FAILURE);
    }
    let mut ms: Vec<f64> = summary.rtts.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    let avg = ms.iter().sum::<f64>() / ms.len() as f64;
    println!(
        "{}/{} 個 Pong：最小 {:.2} / 平均 {:.2} / 最大 {:.2} ms",
        ms.len(),
        count,
        ms[0],
        avg,
        ms[ms.len() - 1]
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::parse_usb_target;

    #[test]
    fn usb_targets() {
        assert_eq!(parse_usb_target("usb"), Some((None, 47800)));
        assert_eq!(parse_usb_target("usb:47801"), Some((None, 47801)));
        assert_eq!(parse_usb_target("usb:00008103-ABC"), Some((Some("00008103-ABC"), 47800)));
        assert_eq!(parse_usb_target("usb:00008103-ABC:47801"), Some((Some("00008103-ABC"), 47801)));
        assert_eq!(parse_usb_target("usb:a:b:c"), None);
        assert_eq!(parse_usb_target("usb:abc:notaport"), None);
    }
}
