use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use minifb::{Key, ScaleMode, Window, WindowOptions};
use std::io::Write;
use std::sync::atomic::AtomicBool;

use padbridge_host::capture::list_monitors;
use padbridge_host::display::{run_display, DisplayEvent, DisplaySettings};
use padbridge_host::gpu::list_gpus;
use padbridge_host::mirror::{run_mirror, DecoderChoice, Frame, MirrorEvent};
use padbridge_host::pairing::HostSecurity;
use padbridge_host::secure::SecureLink;
use padbridge_host::target::{describe_device, resolve_target};
use padbridge_host::{host_name, ping_session, tr, vdd, BoxError, Event};
use padbridge_protocol::{DISPLAY_PORT, MIRROR_PORT};
use padbridge_usbmux::{DeviceEvent, Usbmux};

const USAGE_ZH: &str = "\
用法：
  padbridge-cli devices                     列出透過 USB 連接的 iPad
  padbridge-cli watch                       持續顯示 iPad 插入 / 拔除
  padbridge-cli ping <目標> [次數]          握手並量測 RTT（預設埠 47800）
  padbridge-cli mirror <目標>               在視窗中顯示 iPad 畫面（預設埠 47801）
  padbridge-cli monitors                    列出可傳送到 iPad 的螢幕
  padbridge-cli gpus                        列出可用來編碼 / 解碼的 GPU
  padbridge-cli paired                      列出已配對的 iPad
  padbridge-cli unpair <金鑰開頭|名稱>       取消與某台 iPad 的配對
  padbridge-cli vdd on|off                  開啟 / 移除 Virtual Display Driver 的虛擬螢幕
  padbridge-cli display <目標> [螢幕編號]   把 Windows 螢幕顯示在 iPad 上（預設埠 47800）

<目標>：
  usb[:埠]          第一台 USB 連接的 iPad（usbmux）
  usb:<UDID>[:埠]   指定 UDID 的 iPad
  <IP>[:埠]         Wi-Fi / 區網

mirror 需要先在 iPad 的 PadBridge App 按下廣播按鈕。
USB 需要 Apple Mobile Device Service（「Apple 裝置」App 或 iTunes）。
圖形介面版本：PadBridge.exe（crates/app）。
語言：設定環境變數 PADBRIDGE_LANG=en 或 zh。";

const USAGE_EN: &str = "\
Usage:
  padbridge-cli devices                     list iPads connected over USB
  padbridge-cli watch                       show iPads being plugged in / out
  padbridge-cli ping <target> [count]       handshake and measure RTT (default port 47800)
  padbridge-cli mirror <target>             show the iPad screen in a window (default port 47801)
  padbridge-cli monitors                    list monitors that can be shown on the iPad
  padbridge-cli gpus                        list GPUs for encoding / decoding
  padbridge-cli paired                      list paired iPads
  padbridge-cli unpair <key prefix|name>    unpair an iPad
  padbridge-cli vdd on|off                  turn the Virtual Display Driver's display on / remove it
  padbridge-cli display <target> [monitor]  show a Windows monitor on the iPad (default port 47800)

<target>:
  usb[:port]          the first iPad connected over USB (usbmux)
  usb:<UDID>[:port]   the iPad with this UDID
  <IP>[:port]         Wi-Fi / local network

mirror needs a broadcast started from the PadBridge app on the iPad first.
USB needs Apple Mobile Device Service (the Apple Devices app or iTunes).
Desktop app: PadBridge.exe (crates/app).
Language: set PADBRIDGE_LANG=en or zh.";

fn main() -> ExitCode {
    padbridge_host::i18n::set_english(match std::env::var("PADBRIDGE_LANG").as_deref() {
        Ok("zh") => false,
        Ok("en") => true,
        _ => padbridge_host::i18n::system_prefers_english(),
    });
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
        ["mirror", target] => mirror(target),
        ["monitors"] => monitors(),
        ["gpus"] => gpus(),
        ["paired"] => paired(),
        ["unpair", who] => unpair(who),
        ["vdd", "on"] => vdd::enable_monitor().map(|()| {
            println!("{}", tr!("虛擬螢幕已開啟", "Virtual display on"));
            ExitCode::SUCCESS
        }),
        ["vdd", "off"] => vdd::disable_monitor().map(|count| {
            println!("{}", tr!("已移除 {count} 個虛擬螢幕", "Removed {count} virtual display(s)"));
            ExitCode::SUCCESS
        }),
        ["display", target] => display(target, 0),
        ["display", target, monitor] => match monitor.parse() {
            Ok(n) => display(target, n),
            Err(_) => return usage(),
        },
        _ => return usage(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}", tr!("錯誤：{e}", "Error: {e}"));
            ExitCode::FAILURE
        }
    }
}

type CliResult = Result<ExitCode, BoxError>;

fn usage() -> ExitCode {
    eprintln!("{}", if padbridge_host::i18n::is_english() { USAGE_EN } else { USAGE_ZH });
    ExitCode::from(2)
}

fn devices() -> CliResult {
    let devices = Usbmux::new().list_devices()?;
    if devices.is_empty() {
        println!("{}", tr!("沒有裝置。請用 USB 接上 iPad，並在 iPad 上點「信任這部電腦」。", "No devices. Connect an iPad over USB and tap Trust This Computer on it."));
    }
    for d in &devices {
        println!("{}", describe_device(d));
    }
    Ok(ExitCode::SUCCESS)
}

fn watch() -> CliResult {
    println!("{}", tr!("監看中，按 Ctrl+C 結束", "Watching; press Ctrl+C to stop"));
    for event in Usbmux::new().listen()? {
        match event? {
            DeviceEvent::Attached(d) => println!("+ {}", describe_device(&d)),
            DeviceEvent::Detached { device_id } => println!("- #{device_id}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn open(target: &str, default_port: u16) -> Result<SecureLink, BoxError> {
    let (transport, port, description) = resolve_target(target, default_port)?;
    let security = HostSecurity::load()?;
    let link = padbridge_host::target::open(&transport, port, &security, |request| {
        println!("{}", tr!("要與「{}」配對嗎？請確認 iPad 上顯示的數字相同：", "Pair with \"{}\"? Check that the iPad shows the same number:", request.peer_name));
        println!("\n    {} {}\n", &request.code[..3], &request.code[3..]);
        print!("{}", tr!("相同請輸入 y 並在 iPad 上按「配對」：", "If it matches, type y and choose Pair on the iPad: "));
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("y")
    })?;
    if link.newly_paired {
        println!("{}", tr!("已與「{}」配對", "Paired with \"{}\"", link.peer_name));
    }
    println!("{}", tr!("已連線 {description}（加密）", "Connected to {description} (encrypted)"));
    Ok(link)
}

fn paired() -> CliResult {
    let devices = HostSecurity::load()?.devices();
    if devices.is_empty() {
        println!("{}", tr!("尚未與任何 iPad 配對。第一次連線時會要求確認配對碼。", "No iPad is paired yet. The first connection asks you to confirm a pairing code."));
    }
    for d in devices {
        println!("{}  {}", &d.key[..16], d.name);
    }
    Ok(ExitCode::SUCCESS)
}

fn unpair(prefix: &str) -> CliResult {
    let security = HostSecurity::load()?;
    let matches: Vec<_> = security.devices().into_iter().filter(|d| d.key.starts_with(prefix) || d.name == prefix).collect();
    let [device] = matches.as_slice() else {
        return Err(tr!(
            "找到 {} 台符合「{prefix}」的 iPad；請用 `padbridge-cli paired` 列出的金鑰開頭指定",
            "{} iPads match \"{prefix}\"; use a key prefix from `padbridge-cli paired`",
            matches.len()
        )
        .into());
    };
    security.forget(&device.key)?;
    println!("{}", tr!("已取消與「{}」的配對", "Unpaired \"{}\"", device.name));
    Ok(ExitCode::SUCCESS)
}

fn ping(target: &str, count: u32) -> CliResult {
    let stream = open(target, DISPLAY_PORT)?;
    let summary = ping_session(stream, &host_name(), count, Duration::from_millis(200), |event| match event {
        Event::Rtt(rtt) => println!("RTT {:.2} ms", rtt.as_secs_f64() * 1000.0),
        Event::Received(msg) => println!("← {msg:?}"),
    })?;
    if summary.rtts.is_empty() {
        eprintln!("{}", tr!("沒有收到任何 Pong", "No pong received"));
        return Ok(ExitCode::FAILURE);
    }
    let mut ms: Vec<f64> = summary.rtts.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    let avg = ms.iter().sum::<f64>() / ms.len() as f64;
    println!(
        "{}",
        tr!(
            "{}/{} 個 Pong：最小 {:.2} / 平均 {:.2} / 最大 {:.2} ms",
            "{}/{} pongs: min {:.2} / avg {:.2} / max {:.2} ms",
            ms.len(),
            count,
            ms[0],
            avg,
            ms[ms.len() - 1]
        )
    );
    Ok(ExitCode::SUCCESS)
}

fn monitors() -> CliResult {
    let monitors = list_monitors()?;
    for m in &monitors {
        println!("{}  {}  @({}, {})", m.index, m.label(), m.left, m.top);
    }
    let adapters = padbridge_host::vdisplay::virtual_adapters();
    if !adapters.is_empty() {
        println!("\n{}", tr!("已安裝的虛擬顯示卡：", "Installed virtual display adapters:"));
        for a in &adapters {
            println!("  {}  ({})", a.name, a.instance_id);
        }
    }
    if monitors.iter().all(|m| m.virtual_adapter.is_none()) {
        if adapters.is_empty() {
            println!("\n{}", tr!("要讓 iPad 當「延伸」螢幕，請先安裝虛擬顯示器驅動，再選它新增的螢幕。", "To extend the desktop onto the iPad, install a virtual display driver and choose the monitor it adds."));
        } else {
            println!("\n{}", tr!("虛擬顯示卡目前沒有開啟螢幕；由它所屬的軟體開啟後，再選它新增的螢幕。", "The virtual display adapter has no display on; turn one on with its software, then choose it."));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn gpus() -> CliResult {
    let monitors = list_monitors()?;
    for gpu in list_gpus() {
        let shows: Vec<String> = monitors.iter().filter(|m| m.gpu == gpu.id).map(|m| m.index.to_string()).collect();
        let shows = if shows.is_empty() { String::new() } else { tr!("  螢幕 {}", "  monitors {}", shows.join(", ")) };
        println!("{}  {} MB{shows}", gpu.label(), gpu.dedicated_mb);
    }
    Ok(ExitCode::SUCCESS)
}

fn display(target: &str, monitor: usize) -> CliResult {
    let label = list_monitors()?
        .into_iter()
        .find(|m| m.index == monitor)
        .ok_or_else(|| tr!("沒有第 {monitor} 個螢幕（執行 `padbridge-cli monitors` 檢查）", "There is no monitor {monitor} (check with `padbridge-cli monitors`)"))?
        .label();
    let stream = open(target, DISPLAY_PORT)?;
    println!("{}", tr!("傳送螢幕 {monitor}：{label}（Ctrl+C 結束）", "Sending monitor {monitor}: {label} (Ctrl+C to stop)"));
    let settings = DisplaySettings { monitor, ..DisplaySettings::default() };
    let stop = AtomicBool::new(false);
    run_display(stream, &host_name(), settings, &stop, |event| match event {
        DisplayEvent::Connected { name } => println!("{}", tr!("已連上 {name}", "Connected to {name}")),
        DisplayEvent::Screen { width_px, height_px, max_fps } => {
            println!("{}", tr!("iPad 螢幕 {width_px}×{height_px} @{max_fps} Hz", "iPad screen {width_px}×{height_px} @{max_fps} Hz"))
        }
        DisplayEvent::Streaming { width, height, encoder, .. } => {
            println!("{}", tr!("串流 {width}×{height}（{encoder}）", "Streaming {width}×{height} ({encoder})"))
        }
        DisplayEvent::Stats { fps, kbps, encode_ms } => {
            print!("\r{}", tr!("{fps:5.1} fps · {:6.2} Mbps · 編碼 {encode_ms:5.1} ms   ", "{fps:5.1} fps · {:6.2} Mbps · encode {encode_ms:5.1} ms   ", kbps / 1000.0));
            let _ = std::io::stdout().flush();
        }
        DisplayEvent::Log(text) => println!("\n[iPad] {text}"),
        DisplayEvent::Ended { reason, .. } => println!("\n{reason}"),
    });
    Ok(ExitCode::SUCCESS)
}

fn mirror(target: &str) -> CliResult {
    let stream = open(target, MIRROR_PORT)?;
    let shutdown = stream.shutdown_handle()?;

    // Small buffer: if the window falls behind, frames are dropped rather than queued.
    let (tx, rx) = mpsc::sync_channel::<Frame>(2);
    let name = host_name();
    thread::spawn(move || {
        run_mirror(
            stream,
            &name,
            DecoderChoice::Auto,
            |frame| !matches!(tx.try_send(frame), Err(TrySendError::Disconnected(_))),
            |event| match event {
                MirrorEvent::Connected { name } => println!("{}", tr!("已連上 {name}", "Connected to {name}")),
                MirrorEvent::Decoder(name) => println!("{}", tr!("解碼器：{name}", "Decoder: {name}")),
                MirrorEvent::Stream { codec, width, height, fps, bitrate_kbps } => {
                    println!("{}", tr!("串流 {codec:?} {width}×{height} @{fps} fps, {bitrate_kbps} kbps", "Stream {codec:?} {width}×{height} @{fps} fps, {bitrate_kbps} kbps"))
                }
                MirrorEvent::Log(text) => println!("[iPad] {text}"),
                MirrorEvent::Stats { received_fps, mbps, decode_ms } => {
                    print!("\r{}", tr!("收到 {received_fps:5.1} fps · {mbps:6.2} Mbps · 解碼 {decode_ms:5.1} ms   ", "received {received_fps:5.1} fps · {mbps:6.2} Mbps · decode {decode_ms:5.1} ms   "));
                    let _ = std::io::stdout().flush();
                }
                MirrorEvent::DecodeError(e) => eprintln!("{}", tr!("{e}；要求關鍵幀", "{e}; asking for a keyframe")),
                MirrorEvent::Ended { reason, .. } => println!("{reason}"),
            },
        );
    });

    let mut window: Option<Window> = None;
    let mut shown = 0u32;
    let mut last_title = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(mut frame) => {
                while let Ok(newer) = rx.try_recv() {
                    frame = newer;
                }
                let w = window.get_or_insert_with(|| open_window(frame.width, frame.height));
                w.update_with_buffer(&frame.pixels, frame.width, frame.height)?;
                shown += 1;
                if last_title.elapsed() >= Duration::from_secs(1) {
                    let fps = f64::from(shown) / last_title.elapsed().as_secs_f64();
                    w.set_title(&format!("PadBridge Mirror — {}×{} · {fps:.0} fps", frame.width, frame.height));
                    shown = 0;
                    last_title = Instant::now();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(w) = window.as_mut() {
                    w.update();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if window.as_ref().is_some_and(|w| !w.is_open() || w.is_key_down(Key::Escape)) {
            break;
        }
    }
    let _ = shutdown.shutdown(std::net::Shutdown::Both);
    println!("{}", tr!("鏡像結束", "Mirroring ended"));
    Ok(ExitCode::SUCCESS)
}

fn open_window(width: usize, height: usize) -> Window {
    // Fit within 1280×900 at first; the window is resizable afterwards.
    let scale = (1280.0 / width as f64).min(900.0 / height as f64).min(1.0);
    let options = WindowOptions { resize: true, scale_mode: ScaleMode::AspectRatioStretch, ..WindowOptions::default() };
    let mut window = Window::new(
        "PadBridge Mirror",
        (width as f64 * scale) as usize,
        (height as f64 * scale) as usize,
        options,
    )
    .expect("cannot create the window");
    // Frames are paced by the iPad; don't let minifb add its own delay.
    window.set_target_fps(0);
    window
}
