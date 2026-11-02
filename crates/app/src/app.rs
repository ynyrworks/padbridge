use std::collections::{BTreeMap, VecDeque};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};
use padbridge_host::capture::{list_monitors, Monitor};
use padbridge_host::discovery::{Discovery, DiscoveryEvent, Endpoint};
use padbridge_host::display::{run_display, DisplayEvent, DisplaySettings};
use padbridge_host::mirror::{run_mirror, Frame, MirrorEvent};
use padbridge_host::target::{connect, resolve_target, Transport};
use padbridge_host::{host_name, ping_session, Event};
use padbridge_protocol::{Role, DISPLAY_PORT, MIRROR_PORT};

const MAX_LOG_LINES: usize = 300;

pub struct PadBridgeApp {
    ctx: egui::Context,
    started: Instant,
    _discovery: Discovery,
    discovery_events: Receiver<DiscoveryEvent>,
    endpoints: Vec<Endpoint>,
    source_status: BTreeMap<&'static str, String>,
    manual_address: String,
    mirror: Option<MirrorView>,
    display: Option<DisplayView>,
    monitors: Vec<Monitor>,
    monitor_index: usize,
    allow_input: bool,
    log: VecDeque<String>,
    log_tx: Sender<String>,
    log_rx: Receiver<String>,
}

#[derive(Clone, Copy)]
enum Action {
    Mirror,
    Display,
    Ping,
}

/// State shared between a display worker thread and the UI.
#[derive(Default)]
struct DisplayShared {
    peer_name: Option<String>,
    screen: Option<String>,
    streaming: Option<String>,
    stats: Option<String>,
    ended: Option<String>,
}

struct DisplayView {
    title: String,
    shared: Arc<Mutex<DisplayShared>>,
    stop: Arc<AtomicBool>,
}

/// State shared between a mirror worker thread and the UI.
#[derive(Default)]
struct MirrorShared {
    frame: Option<ColorImage>,
    peer_name: Option<String>,
    stream_info: Option<String>,
    ended: Option<String>,
    /// Clone of the socket so the UI can stop the session.
    shutdown: Option<TcpStream>,
    stop_requested: bool,
}

struct MirrorView {
    title: String,
    shared: Arc<Mutex<MirrorShared>>,
    texture: Option<TextureHandle>,
    frames_in_window: u32,
    window_start: Instant,
    fps: f64,
}

impl MirrorView {
    fn stop(&self) {
        let mut shared = self.shared.lock().unwrap();
        shared.stop_requested = true;
        if let Some(stream) = shared.shutdown.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

impl PadBridgeApp {
    pub fn new(ctx: egui::Context) -> Self {
        let (discovery, discovery_events) = Discovery::start();
        let (log_tx, log_rx) = mpsc::channel();
        let mut app = Self {
            ctx,
            started: Instant::now(),
            _discovery: discovery,
            discovery_events,
            endpoints: Vec::new(),
            source_status: BTreeMap::new(),
            manual_address: String::new(),
            mirror: None,
            display: None,
            monitors: Vec::new(),
            monitor_index: 0,
            allow_input: true,
            log: VecDeque::new(),
            log_tx,
            log_rx,
        };
        app.refresh_monitors();
        // `PadBridge.exe --mirror <target>` / `--display <target>` start right away (handy for shortcuts).
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let [flag, target] = args.as_slice() {
            let (kind, default_port) = match flag.as_str() {
                "--mirror" => (Some(Action::Mirror), MIRROR_PORT),
                "--display" => (Some(Action::Display), DISPLAY_PORT),
                _ => (None, 0),
            };
            if let Some(kind) = kind {
                match resolve_target(target, default_port) {
                    Ok((transport, port, description)) => match kind {
                        Action::Display => app.start_display(description, transport, port),
                        _ => app.start_mirror(description, transport, port),
                    },
                    Err(e) => app.push_log(format!("{target}：{e}")),
                }
            }
        }
        app
    }

    fn push_log(&mut self, line: String) {
        let t = self.started.elapsed().as_secs_f32();
        self.log.push_front(format!("[{t:7.1}s] {line}"));
        self.log.truncate(MAX_LOG_LINES);
    }

    fn poll_background(&mut self) {
        while let Ok(event) = self.discovery_events.try_recv() {
            match event {
                DiscoveryEvent::Added(endpoint) => {
                    // Bonjour resolves once per network interface; only log new devices.
                    if !self.endpoints.iter().any(|e| e.key == endpoint.key) {
                        self.push_log(format!("發現 {}（{}）", endpoint.name, endpoint.transport.label()));
                    }
                    self.endpoints.retain(|e| e.key != endpoint.key);
                    self.endpoints.push(endpoint);
                    self.endpoints.sort_by(|a, b| a.name.cmp(&b.name));
                }
                DiscoveryEvent::Removed { key } => {
                    if let Some(e) = self.endpoints.iter().find(|e| e.key == key) {
                        let line = format!("移除 {}", e.name);
                        self.push_log(line);
                    }
                    self.endpoints.retain(|e| e.key != key);
                }
                DiscoveryEvent::Status { source, message } => {
                    if !message.is_empty() {
                        self.push_log(format!("{source}：{message}"));
                    }
                    self.source_status.insert(source, message);
                }
            }
        }
        while let Ok(line) = self.log_rx.try_recv() {
            self.push_log(line);
        }
    }

    fn start_mirror(&mut self, title: String, transport: Transport, port: u16) {
        self.stop_sessions();
        self.push_log(format!("開始鏡像：{title}"));
        let shared = Arc::new(Mutex::new(MirrorShared::default()));
        let worker_shared = shared.clone();
        let ctx = self.ctx.clone();
        let log = self.log_tx.clone();
        thread::spawn(move || {
            let stream = match connect(&transport, port) {
                Ok(s) => s,
                Err(e) => {
                    worker_shared.lock().unwrap().ended = Some(format!("連線失敗：{e}"));
                    ctx.request_repaint();
                    return;
                }
            };
            {
                let mut s = worker_shared.lock().unwrap();
                if s.stop_requested {
                    return;
                }
                s.shutdown = stream.try_clone().ok();
            }
            let frame_shared = worker_shared.clone();
            let frame_ctx = ctx.clone();
            run_mirror(
                stream,
                &host_name(),
                move |frame| {
                    let image = to_color_image(&frame);
                    let mut s = frame_shared.lock().unwrap();
                    s.frame = Some(image);
                    frame_ctx.request_repaint();
                    !s.stop_requested
                },
                |event| {
                    let mut s = worker_shared.lock().unwrap();
                    match event {
                        MirrorEvent::Connected { name } => s.peer_name = Some(name),
                        MirrorEvent::Stream { codec, width, height, fps, bitrate_kbps } => {
                            s.stream_info = Some(format!("{codec:?} · {width}×{height} · {fps} fps · {bitrate_kbps} kbps"));
                        }
                        MirrorEvent::Log(text) => {
                            let _ = log.send(format!("[iPad] {text}"));
                        }
                        MirrorEvent::DecodeError(e) => {
                            let _ = log.send(format!("解碼錯誤，已要求關鍵幀：{e}"));
                        }
                        MirrorEvent::Ended(reason) => s.ended = Some(reason),
                    }
                    ctx.request_repaint();
                },
            );
        });
        self.mirror = Some(MirrorView {
            title,
            shared,
            texture: None,
            frames_in_window: 0,
            window_start: Instant::now(),
            fps: 0.0,
        });
    }

    fn start_ping(&self, title: String, transport: Transport, port: u16) {
        let log = self.log_tx.clone();
        let ctx = self.ctx.clone();
        let _ = log.send(format!("測試連線：{title}"));
        thread::spawn(move || {
            let result = connect(&transport, port).and_then(|stream| {
                ping_session(stream, &host_name(), 5, Duration::from_millis(200), |event| {
                    if let Event::Received(padbridge_protocol::Message::Hello { name, .. }) = event {
                        let _ = log.send(format!("已連上 {name}"));
                    }
                })
                .map_err(Into::into)
            });
            let line = match result {
                Ok(summary) if !summary.rtts.is_empty() => {
                    let ms: Vec<f64> = summary.rtts.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
                    let avg = ms.iter().sum::<f64>() / ms.len() as f64;
                    let max = ms.iter().copied().fold(0.0, f64::max);
                    format!("{title}：{}/5 個回應，平均 {avg:.1} ms，最大 {max:.1} ms", ms.len())
                }
                Ok(_) => format!("{title}：沒有收到回應"),
                Err(e) => format!("{title}：{e}"),
            };
            let _ = log.send(line);
            ctx.request_repaint();
        });
    }

    fn device_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("裝置");
        ui.add_space(4.0);
        for (source, message) in &self.source_status {
            if !message.is_empty() {
                ui.label(RichText::new(format!("⚠ {source}：{message}")).color(ui.visuals().warn_fg_color).small());
            }
        }
        if self.endpoints.is_empty() {
            ui.label(RichText::new("尚未發現裝置。\n用 USB 接上 iPad（並點「信任」），\n或在同一個網路的 iPad 上開啟 PadBridge App。").weak());
        }

        let mut action: Option<(Action, String, Transport, u16)> = None;
        for endpoint in &self.endpoints {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&endpoint.name).strong());
                let detail = match (&endpoint.transport, endpoint.port) {
                    (Transport::Network(ip), Some(port)) => format!("Wi-Fi · {ip}:{port}"),
                    (transport, _) => transport.label().to_string(),
                };
                let role = match endpoint.role {
                    Some(Role::MirrorSource) => " · 廣播中",
                    Some(Role::DisplaySink) => " · App 開啟中",
                    _ => "",
                };
                ui.label(RichText::new(format!("{detail}{role}")).small().weak());
                ui.horizontal(|ui| {
                    let can_mirror = matches!(endpoint.role, None | Some(Role::MirrorSource));
                    let can_ping = matches!(endpoint.role, None | Some(Role::DisplaySink));
                    let port_for = |default: u16| endpoint.port.unwrap_or(default);
                    if can_ping && ui.button("🖥 延伸螢幕").on_hover_text("把下方選擇的螢幕顯示在 iPad 上").clicked() {
                        action = Some((Action::Display, endpoint.name.clone(), endpoint.transport.clone(), port_for(DISPLAY_PORT)));
                    }
                    if can_mirror && ui.button("📺 鏡像").on_hover_text("在這裡顯示 iPad 畫面（iPad 需先開始廣播）").clicked() {
                        action = Some((Action::Mirror, endpoint.name.clone(), endpoint.transport.clone(), port_for(MIRROR_PORT)));
                    }
                    if can_ping && ui.button("⏱").on_hover_text("測試連線").clicked() {
                        action = Some((Action::Ping, endpoint.name.clone(), endpoint.transport.clone(), port_for(DISPLAY_PORT)));
                    }
                });
            });
        }

        ui.add_space(12.0);
        ui.separator();
        self.monitor_picker(ui);

        ui.add_space(12.0);
        ui.separator();
        ui.label(RichText::new("手動連線").strong());
        ui.add(egui::TextEdit::singleline(&mut self.manual_address).hint_text("iPad 的 IP 或 usb").desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            let address = self.manual_address.trim().to_string();
            ui.add_enabled_ui(!address.is_empty(), |ui| {
                let buttons = [
                    (Action::Display, "🖥 延伸螢幕", DISPLAY_PORT),
                    (Action::Mirror, "📺 鏡像", MIRROR_PORT),
                    (Action::Ping, "⏱", DISPLAY_PORT),
                ];
                for (kind, label, default_port) in buttons {
                    if ui.button(label).clicked() {
                        match resolve_target(&address, default_port) {
                            Ok((transport, port, description)) => action = Some((kind, description, transport, port)),
                            Err(e) => {
                                let _ = self.log_tx.send(format!("{address}：{e}"));
                            }
                        }
                    }
                }
            });
        });

        if let Some((kind, title, transport, port)) = action {
            match kind {
                Action::Mirror => self.start_mirror(title, transport, port),
                Action::Display => self.start_display(title, transport, port),
                Action::Ping => self.start_ping(title, transport, port),
            }
        }
    }

    fn monitor_picker(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("傳送到 iPad 的螢幕").strong());
        ui.horizontal(|ui| {
            let selected = self.monitors.iter().find(|m| m.index == self.monitor_index).map(|m| m.label());
            egui::ComboBox::from_id_salt("monitor")
                .width(ui.available_width() - 36.0)
                .selected_text(selected.unwrap_or_else(|| "（沒有螢幕）".into()))
                .show_ui(ui, |ui| {
                    for m in &self.monitors {
                        ui.selectable_value(&mut self.monitor_index, m.index, m.label());
                    }
                });
            if ui.button("🔄").on_hover_text("重新整理螢幕清單（新增虛擬螢幕後）").clicked() {
                self.refresh_monitors();
            }
        });
        ui.checkbox(&mut self.allow_input, "允許 iPad 操作（觸控、Apple Pencil、觸控板）");
        let virtual_display = self.monitors.iter().any(|m| !m.primary && m.adapter.to_lowercase().contains("virtual"));
        if !virtual_display {
            ui.label(
                RichText::new("目前只會鏡像既有螢幕。要讓 iPad 成為「延伸」螢幕，請安裝虛擬顯示器驅動（例如 Virtual Display Driver），再按 🔄 並選它新增的螢幕。")
                    .small()
                    .weak(),
            );
        }
    }

    fn refresh_monitors(&mut self) {
        match list_monitors() {
            Ok(monitors) => {
                // Prefer a virtual display if one exists, since that's the extended-desktop case.
                if !monitors.iter().any(|m| m.index == self.monitor_index) || self.monitors.is_empty() {
                    self.monitor_index = monitors
                        .iter()
                        .find(|m| m.adapter.to_lowercase().contains("virtual"))
                        .or_else(|| monitors.first())
                        .map_or(0, |m| m.index);
                }
                self.monitors = monitors;
            }
            Err(e) => self.push_log(format!("無法列出螢幕：{e}")),
        }
    }

    fn stop_sessions(&mut self) {
        if let Some(view) = self.mirror.take() {
            view.stop();
        }
        if let Some(view) = self.display.take() {
            view.stop.store(true, Ordering::Relaxed);
        }
    }

    fn start_display(&mut self, title: String, transport: Transport, port: u16) {
        self.stop_sessions();
        let settings = DisplaySettings { monitor: self.monitor_index, allow_input: self.allow_input, ..DisplaySettings::default() };
        let monitor = self.monitors.iter().find(|m| m.index == self.monitor_index).map(|m| m.label()).unwrap_or_default();
        self.push_log(format!("延伸螢幕：{monitor} → {title}"));
        let shared = Arc::new(Mutex::new(DisplayShared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_shared, worker_stop, ctx, log) = (shared.clone(), stop.clone(), self.ctx.clone(), self.log_tx.clone());
        thread::spawn(move || {
            let stream = match connect(&transport, port) {
                Ok(s) => s,
                Err(e) => {
                    worker_shared.lock().unwrap().ended = Some(format!("連線失敗：{e}"));
                    ctx.request_repaint();
                    return;
                }
            };
            run_display(stream, &host_name(), settings, &worker_stop, |event| {
                let mut s = worker_shared.lock().unwrap();
                match event {
                    DisplayEvent::Connected { name } => s.peer_name = Some(name),
                    DisplayEvent::Screen { width_px, height_px, max_fps } => {
                        s.screen = Some(format!("iPad {width_px}×{height_px} · {max_fps} Hz"));
                    }
                    DisplayEvent::Streaming { monitor, width, height } => {
                        s.streaming = Some(format!("{monitor}\n傳送解析度 {width}×{height}"));
                    }
                    DisplayEvent::Stats { fps, kbps, encode_ms } => {
                        s.stats = Some(format!("{fps:.0} fps · {:.1} Mbps · 編碼 {encode_ms:.1} ms", kbps / 1000.0));
                    }
                    DisplayEvent::Log(text) => {
                        let _ = log.send(format!("[iPad] {text}"));
                    }
                    DisplayEvent::Ended(reason) => s.ended = Some(reason),
                }
                ctx.request_repaint();
            });
        });
        self.display = Some(DisplayView { title, shared, stop });
    }

    fn display_view(&mut self, ui: &mut egui::Ui) {
        let Some(view) = &self.display else { return };
        let s = view.shared.lock().unwrap();
        let mut stop = false;
        ui.horizontal(|ui| {
            ui.heading(format!("🖥 {}", s.peer_name.as_deref().unwrap_or(&view.title)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                stop = ui.button(if s.ended.is_some() { "關閉" } else { "⏹ 停止" }).clicked();
            });
        });
        ui.separator();
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            match &s.ended {
                Some(reason) => {
                    ui.label(RichText::new(reason).color(ui.visuals().warn_fg_color).size(16.0));
                }
                None if s.streaming.is_none() => {
                    ui.spinner();
                    ui.label("連線中…");
                }
                None => {
                    ui.label(RichText::new("正在把螢幕顯示在 iPad 上").size(18.0));
                }
            }
            ui.add_space(12.0);
            for line in [&s.streaming, &s.screen, &s.stats].into_iter().flatten() {
                ui.label(RichText::new(line).weak());
            }
        });
        drop(s);
        if stop {
            self.stop_sessions();
        }
    }

    fn mirror_view(&mut self, ui: &mut egui::Ui) {
        let Some(view) = self.mirror.as_mut() else {
            welcome(ui);
            return;
        };
        let (frame, peer, info, ended) = {
            let mut s = view.shared.lock().unwrap();
            (s.frame.take(), s.peer_name.clone(), s.stream_info.clone(), s.ended.clone())
        };
        if let Some(image) = frame {
            match view.texture.as_mut() {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => view.texture = Some(ui.ctx().load_texture("mirror", image, TextureOptions::LINEAR)),
            }
            view.frames_in_window += 1;
        }
        let elapsed = view.window_start.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            view.fps = f64::from(view.frames_in_window) / elapsed;
            view.frames_in_window = 0;
            view.window_start = Instant::now();
        }

        let mut stop = false;
        ui.horizontal(|ui| {
            ui.heading(peer.as_deref().unwrap_or(&view.title));
            if let Some(info) = &info {
                ui.label(RichText::new(info).weak());
            }
            if ended.is_none() && view.texture.is_some() {
                ui.label(RichText::new(format!("{:.0} fps", view.fps)).weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                stop = ui.button(if ended.is_some() { "關閉" } else { "⏹ 停止" }).clicked();
            });
        });
        if let Some(reason) = &ended {
            ui.label(RichText::new(reason).color(ui.visuals().warn_fg_color));
        }
        ui.separator();

        match &view.texture {
            Some(texture) => {
                let available = ui.available_size();
                let size = texture.size_vec2();
                let scale = (available.x / size.x).min(available.y / size.y);
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::from_texture(texture).fit_to_exact_size(size * scale));
                });
            }
            None if ended.is_none() => {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
            }
            None => {}
        }

        if stop {
            view.stop();
            self.mirror = None;
        }
    }

    fn log_panel(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for line in &self.log {
                ui.label(RichText::new(line).monospace().small());
            }
        });
    }
}

impl eframe::App for PadBridgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_background();
        // Discovery events arrive on background threads without waking the UI.
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        egui::Panel::left("devices").resizable(true).default_size(290.0).min_size(230.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.device_list(ui));
        });
        egui::Panel::bottom("log").resizable(true).default_size(130.0).min_size(60.0).show(ui, |ui| {
            ui.label(RichText::new("事件").strong());
            self.log_panel(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if self.display.is_some() {
                self.display_view(ui);
            } else {
                self.mirror_view(ui);
            }
        });
    }

    fn on_exit(&mut self) {
        self.stop_sessions();
    }
}

fn welcome(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.25);
        ui.heading("PadBridge");
        ui.add_space(8.0);
        ui.label("iPad 與 Windows 畫面共享，USB 與 Wi-Fi 皆可");
        ui.add_space(16.0);
        ui.label(RichText::new(
            "鏡像 iPad 畫面：\n\
             1. 在 iPad 開啟 PadBridge App，按廣播按鈕並選「PadBridge Mirror」\n\
             2. 在左側裝置清單按「📺 鏡像」",
        ).weak());
    });
}

/// `0RGB` pixels from the decoder to an egui image.
fn to_color_image(frame: &Frame) -> ColorImage {
    let pixels = frame
        .pixels
        .iter()
        .map(|&p| Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
        .collect();
    ColorImage::new([frame.width, frame.height], pixels)
}
