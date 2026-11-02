use std::net::TcpStream;
use std::process::ExitCode;
use std::time::Duration;

use padbridge_cli::{ping_session, Event};
use padbridge_protocol::DISPLAY_PORT;

const USAGE: &str = "\
用法：
  padbridge ping <iPad IP>[:埠] [次數]

  埠預設 47800（Display 模式）；Mirror 模式的廣播擴充用 47801。
  USB 連線（usbmux）會在 M1 加入。";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, addr, rest @ ..] if cmd == "ping" && rest.len() <= 1 => {
            let count = match rest.first().map(|c| c.parse()) {
                None => 10,
                Some(Ok(n)) => n,
                Some(Err(_)) => return usage(),
            };
            ping(addr, count)
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn ping(addr: &str, count: u32) -> ExitCode {
    let addr = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{DISPLAY_PORT}") };
    let stream = match TcpStream::connect(&addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("無法連線 {addr}：{e}");
            return ExitCode::FAILURE;
        }
    };
    println!("已連線 {addr}");

    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PadBridge host".into());
    let result = ping_session(stream, &name, count, Duration::from_millis(200), |event| match event {
        Event::Rtt(rtt) => println!("RTT {:.2} ms", rtt.as_secs_f64() * 1000.0),
        Event::Received(msg) => println!("← {msg:?}"),
    });
    match result {
        Ok(summary) => {
            if summary.rtts.is_empty() {
                eprintln!("沒有收到任何 Pong");
                return ExitCode::FAILURE;
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
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("連線錯誤：{e}");
            ExitCode::FAILURE
        }
    }
}
