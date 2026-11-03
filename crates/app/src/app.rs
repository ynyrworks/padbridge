use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};
use padbridge_host::capture::{list_monitors, Monitor};
use padbridge_host::vdd;
use padbridge_host::vdisplay::{virtual_adapters, VirtualAdapter};
use padbridge_host::discovery::{Discovery, DiscoveryEvent, Endpoint};
use padbridge_host::display::{run_display, DisplayEvent, DisplaySettings, EncoderChoice};
use padbridge_host::gpu::{list_gpus, Gpu};
use padbridge_host::mirror::{run_mirror, DecoderChoice, Frame, MirrorEvent};
use padbridge_host::{tr, GpuId};

use crate::settings::{CodecSetting, Settings};
use padbridge_host::pairing::HostSecurity;
use padbridge_host::secure::{device_id, PairingRequest, SecureLink};
use padbridge_host::target::{open, resolve_target, Transport};
use padbridge_host::{host_name, ping_session, Event};
use padbridge_protocol::{Role, DISPLAY_PORT, MIRROR_PORT};

const MAX_LOG_LINES: usize = 300;

pub struct PadBridgeApp {
    ctx: egui::Context,
    started: Instant,
    _discovery: Discovery,
    discovery_events: Receiver<DiscoveryEvent>,
    endpoints: Vec<Endpoint>,
    /// USB endpoint key → iPad fingerprint, learned from connections (USB
    /// doesn't advertise it the way Bonjour does).
    usb_devices: HashMap<String, String>,
    source_status: BTreeMap<&'static str, String>,
    manual_address: String,
    mirror: Option<MirrorView>,
    display: Option<DisplayView>,
    monitors: Vec<Monitor>,
    virtual_adapters: Vec<VirtualAdapter>,
    monitor_index: usize,
    allow_input: bool,
    gpus: Vec<Gpu>,
    encoder_choice: EncoderChoice,
    decoder_choice: DecoderChoice,
    /// What was last written to disk; compared each frame to save changes.
    saved_settings: Settings,
    /// Display quality and language (not edited elsewhere in the app state).
    settings: Settings,
    /// A Virtual Display Driver install or enable is running.
    vdd_busy: Arc<AtomicBool>,
    /// Set by background work that changed the monitors.
    monitors_changed: Arc<AtomicBool>,
    security: Option<Arc<HostSecurity>>,
    pairing_prompt: Arc<Mutex<Option<PairingPrompt>>>,
    log: VecDeque<String>,
    log_tx: Sender<String>,
    log_rx: Receiver<String>,
}

/// A pairing request waiting for the user's answer.
struct PairingPrompt {
    request: PairingRequest,
    answer: mpsc::Sender<bool>,
}

/// Opens secure connections from worker threads, asking the user (through
/// the UI thread) before pairing with a new iPad.
#[derive(Clone)]
struct Opener {
    security: Option<Arc<HostSecurity>>,
    prompt: Arc<Mutex<Option<PairingPrompt>>>,
    ctx: egui::Context,
}

impl Opener {
    fn open(&self, transport: &Transport, port: u16) -> Result<SecureLink, padbridge_host::BoxError> {
        let security = self.security.as_ref().ok_or_else(|| tr!("無法載入這台電腦的金鑰（見事件紀錄）", "This computer's key could not be loaded (see the event log)"))?;
        open(transport, port, security, |request| {
            let (answer, response) = mpsc::channel();
            *self.prompt.lock().unwrap() = Some(PairingPrompt { request: request.clone(), answer });
            self.ctx.request_repaint();
            // No answer within two minutes (or the dialog went away) counts as no.
            let accepted = response.recv_timeout(Duration::from_secs(120)).unwrap_or(false);
            self.prompt.lock().unwrap().take();
            self.ctx.request_repaint();
            accepted
        })
    }
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
    /// The session ended because the connection went away: try again.
    lost: bool,
    /// Identity key of the iPad reached, once connected.
    peer_key: Option<[u8; 32]>,
    /// The virtual monitor was switched to the iPad's resolution; reconnect.
    restart: bool,
}

/// Where a session connects to, kept so it can reconnect.
#[derive(Clone)]
struct Target {
    title: String,
    transport: Transport,
    port: u16,
    /// Discovery key of the device, to find it again (a re-plugged USB iPad
    /// gets a new usbmux id).
    key: Option<String>,
    /// Reconnection attempts so far; 0 for a fresh start.
    attempt: u32,
    /// The iPad's identity fingerprint once connected, to fail over to another
    /// route to the same iPad (USB ⇄ Wi‑Fi).
    device: Option<String>,
}

impl Target {
    fn manual(title: String, transport: Transport, port: u16) -> Self {
        Self { title, transport, port, key: None, attempt: 0, device: None }
    }
}

/// What a session's shared state says, for reconnection decisions.
struct SessionState {
    ended: bool,
    lost: bool,
    connected: bool,
    peer_key: Option<[u8; 32]>,
}

struct DisplayView {
    target: Target,
    /// When the next reconnection attempt is due.
    retry_at: Option<Instant>,
    shared: Arc<Mutex<DisplayShared>>,
    stop: Arc<AtomicBool>,
}

/// State shared between a mirror worker thread and the UI.
#[derive(Default)]
struct MirrorShared {
    frame: Option<ColorImage>,
    peer_name: Option<String>,
    stream_info: Option<String>,
    link_stats: Option<String>,
    ended: Option<String>,
    /// The session ended because the connection or broadcast went away: try again.
    lost: bool,
    /// Identity key of the iPad reached, once connected.
    peer_key: Option<[u8; 32]>,
    /// Clone of the socket so the UI can stop the session.
    shutdown: Option<TcpStream>,
    stop_requested: bool,
}

struct MirrorView {
    target: Target,
    /// When the next reconnection attempt is due.
    retry_at: Option<Instant>,
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
        let settings = Settings::load();
        apply_language(&settings);
        let (discovery, discovery_events) = Discovery::start();
        let (log_tx, log_rx) = mpsc::channel();
        let mut app = Self {
            ctx,
            started: Instant::now(),
            _discovery: discovery,
            discovery_events,
            endpoints: Vec::new(),
            usb_devices: HashMap::new(),
            source_status: BTreeMap::new(),
            manual_address: String::new(),
            mirror: None,
            display: None,
            monitors: Vec::new(),
            virtual_adapters: Vec::new(),
            monitor_index: 0,
            allow_input: true,
            gpus: list_gpus(),
            encoder_choice: EncoderChoice::Auto,
            decoder_choice: DecoderChoice::Auto,
            saved_settings: Settings::default(),
            settings,
            vdd_busy: Arc::new(AtomicBool::new(false)),
            monitors_changed: Arc::new(AtomicBool::new(false)),
            security: None,
            pairing_prompt: Arc::new(Mutex::new(None)),
            log: VecDeque::new(),
            log_tx,
            log_rx,
        };
        match HostSecurity::load() {
            Ok(security) => app.security = Some(Arc::new(security)),
            Err(e) => app.push_log(tr!("無法載入這台電腦的金鑰，將無法連線：{e}", "Could not load this computer's key; connecting won't work: {e}")),
        }
        app.refresh_monitors();
        app.apply_settings();
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
                        Action::Display => app.start_display(Target::manual(description, transport, port)),
                        _ => app.start_mirror(Target::manual(description, transport, port)),
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
                        self.push_log(tr!("發現 {}（{}）", "Found {} ({})", endpoint.name, endpoint.transport.label()));
                    }
                    self.endpoints.retain(|e| e.key != endpoint.key);
                    self.endpoints.push(endpoint);
                    self.endpoints.sort_by(|a, b| a.name.cmp(&b.name));
                }
                DiscoveryEvent::Removed { key } => {
                    if let Some(e) = self.endpoints.iter().find(|e| e.key == key) {
                        let line = tr!("移除 {}", "Gone: {}", e.name);
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
        if self.monitors_changed.swap(false, Ordering::Relaxed) {
            self.refresh_monitors();
        }
        let restart = self.display.as_ref().is_some_and(|view| std::mem::take(&mut view.shared.lock().unwrap().restart));
        if restart {
            if let Some(view) = self.display.take() {
                self.refresh_monitors();
                self.start_display_with(view.target, false);
            }
        }
        self.reconnect_if_due();
    }

    /// Where the target's iPad can be reached now, with the endpoint key: its
    /// own endpoint while it exists, otherwise another route to the same iPad
    /// (identified by fingerprint), USB first.
    fn route(&self, target: &Target, display: bool) -> Option<(Transport, u16, String)> {
        let default_port = if display { DISPLAY_PORT } else { MIRROR_PORT };
        let via = |e: &Endpoint| (e.transport.clone(), e.port.unwrap_or(default_port), e.key.clone());
        if let Some(e) = target.key.as_ref().and_then(|key| self.endpoints.iter().find(|e| &e.key == key)) {
            return Some((e.transport.clone(), e.port.unwrap_or(target.port), e.key.clone()));
        }
        let device = target.device.as_ref()?;
        let role = if display { Role::DisplaySink } else { Role::MirrorSource };
        let usb = self.endpoints.iter().find(|e| self.usb_devices.get(&e.key) == Some(device));
        let wifi = self.endpoints.iter().find(|e| e.device_id.as_ref() == Some(device) && e.role == Some(role));
        usb.or(wifi).map(via)
    }

    /// A session whose connection dropped keeps trying (USB unplugged, Wi‑Fi
    /// change, sleep, the iPad app backgrounded, the broadcast stopped) until
    /// it reconnects or the user closes it. If the same iPad is reachable
    /// another way (USB unplugged but on Wi‑Fi), it switches right away, and
    /// it moves back to USB when that is plugged in again.
    fn reconnect_if_due(&mut self) {
        let now = Instant::now();
        let mut starts = Vec::new();
        for display in [true, false] {
            let Some((target, retry_at, state)) = self.session_snapshot(display) else { continue };
            let mut target = target;
            if state.connected {
                target.attempt = 0;
            }
            if let Some(key) = state.peer_key {
                let device = device_id(&key);
                // Remember which iPad sits behind a USB endpoint, to recognise it later.
                if let Some(usb_key) = target.key.as_ref().filter(|k| k.starts_with("usb:")) {
                    self.usb_devices.insert(usb_key.clone(), device.clone());
                }
                target.device = Some(device);
            }
            let mut route = self.route(&target, display);
            let other_route = route.as_ref().is_some_and(|(_, _, key)| Some(key) != target.key.as_ref());
            let mut retry_at = retry_at;
            let mut start = false;
            if state.ended && state.lost {
                match retry_at {
                    // Another way to the same iPad: take it now. Otherwise back off 2–10 s.
                    None => {
                        let delay = if other_route {
                            Duration::from_millis(300)
                        } else {
                            Duration::from_secs(u64::from(2 * (target.attempt + 1)).min(10))
                        };
                        retry_at = Some(now + delay);
                    }
                    Some(at) => start = at <= now,
                }
            } else if !state.ended && state.connected && matches!(target.transport, Transport::Network(_)) {
                // Back to USB once the same iPad is plugged in again.
                let default_port = if display { DISPLAY_PORT } else { MIRROR_PORT };
                let usb = target.device.as_ref().and_then(|device| {
                    self.endpoints.iter().find(|e| self.usb_devices.get(&e.key) == Some(device))
                });
                if let Some(e) = usb {
                    route = Some((e.transport.clone(), default_port, e.key.clone()));
                    start = true;
                    self.push_log(tr!("USB 已接上，改用 USB 連線", "USB is back, switching to USB"));
                }
            }
            if let Some((transport, port, key)) = route.filter(|_| start) {
                if Some(&key) != target.key.as_ref() && state.ended {
                    self.push_log(tr!("改用 {} 連線", "Switching to {}", transport.label()));
                }
                target.transport = transport;
                target.port = port;
                target.key = Some(key);
            }
            self.store_session(display, target.clone(), retry_at);
            if start {
                target.attempt += 1;
                starts.push((target, display));
            }
        }
        for (target, display) in starts {
            if display {
                self.start_display(target);
            } else {
                self.start_mirror(target);
            }
        }
        if self.display.as_ref().is_some_and(|v| v.retry_at.is_some()) || self.mirror.as_ref().is_some_and(|v| v.retry_at.is_some()) {
            self.ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn session_snapshot(&self, display: bool) -> Option<(Target, Option<Instant>, SessionState)> {
        if display {
            let view = self.display.as_ref()?;
            let s = view.shared.lock().unwrap();
            let state = SessionState { ended: s.ended.is_some(), lost: s.lost, connected: s.peer_name.is_some(), peer_key: s.peer_key };
            Some((view.target.clone(), view.retry_at, state))
        } else {
            let view = self.mirror.as_ref()?;
            let s = view.shared.lock().unwrap();
            let state = SessionState { ended: s.ended.is_some(), lost: s.lost, connected: s.peer_name.is_some(), peer_key: s.peer_key };
            Some((view.target.clone(), view.retry_at, state))
        }
    }

    fn store_session(&mut self, display: bool, target: Target, retry_at: Option<Instant>) {
        if display {
            if let Some(view) = &mut self.display {
                (view.target, view.retry_at) = (target, retry_at);
            }
        } else if let Some(view) = &mut self.mirror {
            (view.target, view.retry_at) = (target, retry_at);
        }
    }
    /// Restores the saved choices, where they still make sense on this machine.
    fn apply_settings(&mut self) {
        let s = self.settings.clone();
        self.allow_input = s.allow_input;
        let gpu = |name: &str| self.gpus.iter().find(|g| g.name == name).map(|g| g.id);
        self.encoder_choice = match &s.encoder {
            CodecSetting::Auto => EncoderChoice::Auto,
            CodecSetting::Cpu => EncoderChoice::Software,
            CodecSetting::Gpu(name) => gpu(name).map_or(EncoderChoice::Auto, EncoderChoice::Hardware),
        };
        self.decoder_choice = match &s.decoder {
            CodecSetting::Auto => DecoderChoice::Auto,
            CodecSetting::Cpu => DecoderChoice::Software,
            CodecSetting::Gpu(name) => gpu(name).map_or(DecoderChoice::Auto, DecoderChoice::Hardware),
        };
        if let Some(monitor) = s.monitor.as_ref().and_then(|name| self.monitors.iter().find(|m| &m.device_name == name)) {
            self.monitor_index = monitor.index;
        }
        self.saved_settings = self.current_settings();
    }

    fn current_settings(&self) -> Settings {
        let gpu_name = |id: GpuId| self.gpus.iter().find(|g| g.id == id).map(|g| g.name.clone());
        Settings {
            monitor: self.monitors.iter().find(|m| m.index == self.monitor_index).map(|m| m.device_name.clone()),
            allow_input: self.allow_input,
            encoder: match self.encoder_choice {
                EncoderChoice::Auto => CodecSetting::Auto,
                EncoderChoice::Software => CodecSetting::Cpu,
                EncoderChoice::Hardware(id) => gpu_name(id).map_or(CodecSetting::Auto, CodecSetting::Gpu),
            },
            decoder: match self.decoder_choice {
                DecoderChoice::Auto => CodecSetting::Auto,
                DecoderChoice::Software => CodecSetting::Cpu,
                DecoderChoice::Hardware(id) => gpu_name(id).map_or(CodecSetting::Auto, CodecSetting::Gpu),
            },
            ..self.settings.clone()
        }
    }

    /// Writes the settings when the user changed something.
    fn persist_settings(&mut self) {
        let current = self.current_settings();
        if current != self.saved_settings {
            if let Err(e) = current.save() {
                self.push_log(tr!("無法儲存設定：{e}", "Could not save settings: {e}"));
            }
            self.saved_settings = current;
        }
    }

    fn opener(&self) -> Opener {
        Opener { security: self.security.clone(), prompt: self.pairing_prompt.clone(), ctx: self.ctx.clone() }
    }

    /// The pairing confirmation, shown on top of everything while a worker waits.
    fn pairing_dialog(&mut self, ctx: &egui::Context) {
        let mut slot = self.pairing_prompt.lock().unwrap();
        let Some(prompt) = slot.as_ref() else { return };
        let mut answer = None;
        egui::Modal::new(egui::Id::new("pairing")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(tr!("與「{}」配對？", "Pair with \"{}\"?", prompt.request.peer_name));
            ui.add_space(8.0);
            ui.label(tr!("確認 iPad 上顯示的是同一組數字，再兩邊都按「配對」：", "Check that the iPad shows the same number, then choose Pair on both:"));
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                let code = &prompt.request.code;
                ui.label(RichText::new(format!("{} {}", &code[..3], &code[3..])).size(40.0).monospace().strong());
            });
            ui.add_space(8.0);
            ui.label(RichText::new(tr!("數字不同就按取消：可能有人冒充你的 iPad。", "If the numbers differ, cancel: someone may be impersonating your iPad.")).small().weak());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(tr!("配對", "Pair")).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr!("取消", "Cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(answer) = answer {
            if let Some(prompt) = slot.take() {
                let _ = prompt.answer.send(answer);
            }
        }
    }

    /// Paired iPads, each with a way to forget it.
    fn paired_list(&mut self, ui: &mut egui::Ui) {
        let Some(security) = self.security.clone() else { return };
        let devices = security.devices();
        ui.label(RichText::new(tr!("已配對的 iPad", "Paired iPads")).strong());
        if devices.is_empty() {
            ui.label(
                RichText::new(tr!(
                    "尚未配對。第一次連線時，兩邊會顯示同一組 6 位數字讓你確認。",
                    "None yet. The first connection shows the same 6-digit code on both screens for you to confirm."
                ))
                .small()
                .weak(),
            );
        }
        for device in devices {
            ui.horizontal(|ui| {
                ui.label(&device.name).on_hover_text(tr!("金鑰 {}…", "Key {}…", &device.key[..16]));
                if ui.small_button(tr!("取消配對", "Unpair")).clicked() {
                    match security.forget(&device.key) {
                        Ok(()) => self.push_log(tr!("已取消與「{}」的配對", "Unpaired \"{}\"", device.name)),
                        Err(e) => self.push_log(tr!("無法取消配對：{e}", "Could not unpair: {e}")),
                    }
                }
            });
        }
    }

    fn start_mirror(&mut self, target: Target) {
        self.stop_sessions();
        if target.attempt == 0 {
            self.push_log(tr!("開始鏡像：{}", "Mirroring: {}", target.title));
        }
        let shared = Arc::new(Mutex::new(MirrorShared::default()));
        let worker_shared = shared.clone();
        let ctx = self.ctx.clone();
        let log = self.log_tx.clone();
        let decoder = self.decoder_choice;
        let opener = self.opener();
        let (transport, port, quiet) = (target.transport.clone(), target.port, target.attempt > 0);
        thread::spawn(move || {
            let stream = match opener.open(&transport, port) {
                Ok(s) => s,
                Err(e) => {
                    let reason = tr!("連線失敗：{e}", "Connection failed: {e}");
                    if !quiet {
                        let _ = log.send(tr!("鏡像結束：{reason}", "Mirroring ended: {reason}"));
                    }
                    let mut s = worker_shared.lock().unwrap();
                    s.lost = is_retryable(&e);
                    s.ended = Some(reason);
                    ctx.request_repaint();
                    return;
                }
            };
            if quiet {
                let _ = log.send(tr!("已重新連線", "Reconnected"));
            }
            {
                let mut s = worker_shared.lock().unwrap();
                if s.stop_requested {
                    return;
                }
                s.shutdown = stream.shutdown_handle().ok();
                s.peer_key = Some(stream.peer_key);
            }
            let frame_shared = worker_shared.clone();
            let frame_ctx = ctx.clone();
            run_mirror(
                stream,
                &host_name(),
                decoder,
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
                        MirrorEvent::Decoder(name) => {
                            let _ = log.send(tr!("鏡像解碼：{name}", "Mirror decoder: {name}"));
                        }
                        MirrorEvent::Stats { received_fps, mbps, decode_ms } => {
                            s.link_stats =
                                Some(tr!(
                                "· 收到 {received_fps:.0} fps · {mbps:.1} Mbps · 解碼 {decode_ms:.1} ms",
                                "· received {received_fps:.0} fps · {mbps:.1} Mbps · decode {decode_ms:.1} ms"
                            ));
                        }
                        MirrorEvent::DecodeError(e) => {
                            let _ = log.send(tr!("解碼錯誤，已要求關鍵幀：{e}", "Decode error, asked for a keyframe: {e}"));
                        }
                        MirrorEvent::Ended { reason, lost } => {
                            let _ = log.send(tr!("鏡像結束：{reason}", "Mirroring ended: {reason}"));
                            s.lost = lost && !s.stop_requested;
                            s.ended = Some(reason);
                        }
                    }
                    ctx.request_repaint();
                },
            );
        });
        self.mirror = Some(MirrorView {
            target,
            retry_at: None,
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
        let _ = log.send(tr!("測試連線：{title}", "Testing connection: {title}"));
        let opener = self.opener();
        thread::spawn(move || {
            let result = opener.open(&transport, port).and_then(|stream| {
                ping_session(stream, &host_name(), 5, Duration::from_millis(200), |event| {
                    if let Event::Received(padbridge_protocol::Message::Hello { name, .. }) = event {
                        let _ = log.send(tr!("已連上 {name}", "Connected to {name}"));
                    }
                })
                .map_err(Into::into)
            });
            let line = match result {
                Ok(summary) if !summary.rtts.is_empty() => {
                    let ms: Vec<f64> = summary.rtts.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
                    let avg = ms.iter().sum::<f64>() / ms.len() as f64;
                    let max = ms.iter().copied().fold(0.0, f64::max);
                    tr!("{title}：{}/5 個回應，平均 {avg:.1} ms，最大 {max:.1} ms", "{title}: {}/5 replies, average {avg:.1} ms, max {max:.1} ms", ms.len())
                }
                Ok(_) => tr!("{title}：沒有收到回應", "{title}: no reply"),
                Err(e) => format!("{title}：{e}"),
            };
            let _ = log.send(line);
            ctx.request_repaint();
        });
    }

    fn device_list(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr!("裝置", "Devices"));
        ui.add_space(4.0);
        for (source, message) in &self.source_status {
            if !message.is_empty() {
                ui.label(RichText::new(format!("⚠ {source}：{message}")).color(ui.visuals().warn_fg_color).small());
            }
        }
        if self.endpoints.is_empty() {
            ui.label(
                RichText::new(tr!(
                    "尚未發現裝置。\n用 USB 接上 iPad（並點「信任」），\n或在同一個網路的 iPad 上開啟 PadBridge App。",
                    "No devices yet.\nConnect an iPad over USB (and tap Trust),\nor open the PadBridge app on an iPad on this network."
                ))
                .weak(),
            );
        }

        let mut action: Option<(Action, Target)> = None;
        for endpoint in &self.endpoints {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&endpoint.name).strong());
                let detail = match (&endpoint.transport, endpoint.port) {
                    (Transport::Network(ip), Some(port)) => format!("Wi-Fi · {ip}:{port}"),
                    (transport, _) => transport.label().to_string(),
                };
                let role = match endpoint.role {
                    Some(Role::MirrorSource) => tr!(" · 廣播中", " · broadcasting"),
                    Some(Role::DisplaySink) => tr!(" · App 開啟中", " · app open"),
                    _ => String::new(),
                };
                ui.label(RichText::new(format!("{detail}{role}")).small().weak());
                ui.horizontal(|ui| {
                    let can_mirror = matches!(endpoint.role, None | Some(Role::MirrorSource));
                    let can_ping = matches!(endpoint.role, None | Some(Role::DisplaySink));
                    let target = |default: u16| Target {
                        title: endpoint.name.clone(),
                        transport: endpoint.transport.clone(),
                        port: endpoint.port.unwrap_or(default),
                        key: Some(endpoint.key.clone()),
                        attempt: 0,
                        device: endpoint.device_id.clone().or_else(|| self.usb_devices.get(&endpoint.key).cloned()),
                    };
                    if can_ping
                        && ui
                            .button(tr!("🖥 延伸螢幕", "🖥 Extend"))
                            .on_hover_text(tr!("把下方選擇的螢幕顯示在 iPad 上", "Show the monitor chosen below on the iPad"))
                            .clicked()
                    {
                        action = Some((Action::Display, target(DISPLAY_PORT)));
                    }
                    if can_mirror
                        && ui
                            .button(tr!("📺 鏡像", "📺 Mirror"))
                            .on_hover_text(tr!(
                                "在這裡顯示 iPad 畫面（iPad 需先開始廣播）",
                                "Show the iPad screen here (start broadcasting on the iPad first)"
                            ))
                            .clicked()
                    {
                        action = Some((Action::Mirror, target(MIRROR_PORT)));
                    }
                    if can_ping && ui.button("⏱").on_hover_text(tr!("測試連線", "Test connection")).clicked() {
                        action = Some((Action::Ping, target(DISPLAY_PORT)));
                    }
                });
            });
        }

        ui.add_space(12.0);
        ui.separator();
        self.monitor_picker(ui);

        ui.add_space(12.0);
        ui.separator();
        self.codec_picker(ui);

        ui.add_space(12.0);
        ui.separator();
        self.paired_list(ui);

        ui.add_space(12.0);
        ui.separator();
        ui.label(RichText::new(tr!("手動連線", "Connect manually")).strong());
        ui.add(egui::TextEdit::singleline(&mut self.manual_address).hint_text(tr!("iPad 的 IP 或 usb", "iPad IP address or usb")).desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            let address = self.manual_address.trim().to_string();
            ui.add_enabled_ui(!address.is_empty(), |ui| {
                let buttons = [
                    (Action::Display, tr!("🖥 延伸螢幕", "🖥 Extend"), DISPLAY_PORT),
                    (Action::Mirror, tr!("📺 鏡像", "📺 Mirror"), MIRROR_PORT),
                    (Action::Ping, "⏱".to_string(), DISPLAY_PORT),
                ];
                for (kind, label, default_port) in buttons {
                    if ui.button(label).clicked() {
                        match resolve_target(&address, default_port) {
                            Ok((transport, port, description)) => {
                                action = Some((kind, Target::manual(description, transport, port)));
                            }
                            Err(e) => {
                                let _ = self.log_tx.send(format!("{address}：{e}"));
                            }
                        }
                    }
                }
            });
        });

        if let Some((kind, target)) = action {
            match kind {
                Action::Mirror => self.start_mirror(target),
                Action::Display => self.start_display(target),
                Action::Ping => self.start_ping(target.title, target.transport, target.port),
            }
        }
    }

    fn monitor_picker(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(tr!("傳送到 iPad 的螢幕", "Monitor to show on the iPad")).strong());
        ui.horizontal(|ui| {
            let selected = self.monitors.iter().find(|m| m.index == self.monitor_index).map(|m| m.label());
            egui::ComboBox::from_id_salt("monitor")
                .width(ui.available_width() - 36.0)
                .selected_text(selected.unwrap_or_else(|| tr!("（沒有螢幕）", "(no monitor)")))
                .show_ui(ui, |ui| {
                    for m in &self.monitors {
                        ui.selectable_value(&mut self.monitor_index, m.index, m.label());
                    }
                });
            if ui.button("🔄").on_hover_text(tr!("重新整理螢幕清單（新增虛擬螢幕後）", "Refresh the list (after adding a virtual display)")).clicked() {
                self.refresh_monitors();
            }
        });
        ui.checkbox(&mut self.allow_input, tr!("允許 iPad 操作（觸控、Apple Pencil、觸控板）", "Let the iPad control this PC (touch, Apple Pencil, trackpad)"));
        let selected_virtual =
            self.monitors.iter().any(|m| m.index == self.monitor_index && m.virtual_adapter.is_some());
        let any_virtual_monitor = self.monitors.iter().any(|m| m.virtual_adapter.is_some());
        let vdd_installed = self.virtual_adapters.iter().any(vdd::is_vdd);
        let hint = if selected_virtual {
            None
        } else if any_virtual_monitor {
            Some(tr!(
                "目前選的是實體螢幕，iPad 會鏡像它。清單中標「虛擬」的螢幕才是延伸桌面。",
                "A physical monitor is selected, so the iPad mirrors it. Monitors marked virtual extend the desktop."
            ))
        } else if vdd_installed {
            Some(tr!(
                "Virtual Display Driver 已安裝，但目前沒有開啟虛擬螢幕。",
                "Virtual Display Driver is installed but has no display turned on."
            ))
        } else if !self.virtual_adapters.is_empty() {
            let names: Vec<_> = self.virtual_adapters.iter().map(|a| a.name.as_str()).collect();
            Some(tr!(
                "偵測到虛擬顯示卡：{}，但目前沒有開啟虛擬螢幕（要由它所屬的軟體開啟）。也可以改裝下面的開源驅動。",
                "Found virtual display adapters: {}, but none has a display on (their own software turns it on). You can also install the open-source driver below.",
                names.join(", ")
            ))
        } else {
            Some(tr!(
                "目前只會鏡像既有螢幕。要讓 iPad 成為「延伸」螢幕，需要一個虛擬螢幕。",
                "Only existing monitors can be mirrored. To extend the desktop onto the iPad, a virtual display is needed."
            ))
        };
        if let Some(hint) = hint {
            ui.label(RichText::new(hint).small().weak());
        }
        if any_virtual_monitor {
            return;
        }
        let busy = self.vdd_busy.load(Ordering::Relaxed);
        if vdd_installed {
            let button = ui.add_enabled(!busy, egui::Button::new(tr!("🖥 開啟虛擬螢幕", "🖥 Turn on the virtual display")));
            if button.clicked() {
                self.run_vdd_task(tr!("開啟虛擬螢幕", "Turning on the virtual display"), |_| vdd::enable_monitor());
            }
        } else {
            let button = ui
                .add_enabled(!busy, egui::Button::new(tr!("⬇ 安裝虛擬螢幕（Virtual Display Driver）", "⬇ Install a virtual display (Virtual Display Driver)")))
                .on_hover_text(tr!(
                    "用 winget 下載開源的 Virtual Display Driver（MIT）控制程式並開啟它；\n在控制程式按 Install Driver，Windows 會要求系統管理員權限。\n裝好後按 🔄。",
                    "Downloads the open-source Virtual Display Driver (MIT) control app with winget and opens it.\nPress Install Driver there; Windows asks for administrator rights.\nThen press 🔄."
                ));
            if button.clicked() {
                self.run_vdd_task(tr!("安裝虛擬螢幕", "Installing the virtual display"), |mut log| vdd::install(&mut log));
            }
        }
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(tr!("處理中…", "Working…")).small());
            });
        }
    }

    /// Which GPU (or the CPU) encodes for Display and decodes for Mirror.
    fn codec_picker(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(tr!("編碼 / 解碼", "Encoding / decoding")).strong());
        let gpu_label = |id: GpuId, gpus: &[Gpu]| {
            gpus.iter().find(|g| g.id == id).map_or_else(|| tr!("（已不存在的 GPU）", "(GPU no longer present)"), Gpu::label)
        };
        let monitor_gpu = self.monitors.iter().find(|m| m.index == self.monitor_index).map(|m| m.gpu);

        let encoder_text = |choice: EncoderChoice, gpus: &[Gpu]| match choice {
            EncoderChoice::Auto => tr!("自動（螢幕所在的 GPU）", "Auto (the monitor's GPU)"),
            EncoderChoice::Software => tr!("CPU（軟體）", "CPU (software)"),
            EncoderChoice::Hardware(id) => {
                let copy = if Some(id) != monitor_gpu { tr!("・跨 GPU 複製", " · copied across GPUs") } else { String::new() };
                format!("{}{copy}", gpu_label(id, gpus))
            }
        };
        ui.horizontal(|ui| {
            ui.label(tr!("延伸螢幕編碼", "Extend encoder"));
            egui::ComboBox::from_id_salt("encoder")
                .width(ui.available_width())
                .selected_text(encoder_text(self.encoder_choice, &self.gpus))
                .show_ui(ui, |ui| {
                    let mut options = vec![EncoderChoice::Auto];
                    options.extend(self.gpus.iter().map(|g| EncoderChoice::Hardware(g.id)));
                    options.push(EncoderChoice::Software);
                    for option in options {
                        ui.selectable_value(&mut self.encoder_choice, option, encoder_text(option, &self.gpus));
                    }
                })
                .response
                .on_hover_text(tr!(
                    "螢幕由哪張顯示卡繪製，就用那張編碼最快；選另一張會多一次經過記憶體的複製。",
                    "Encoding is fastest on the GPU that draws the monitor; another GPU adds a copy through memory."
                ));
        });

        let decoder_text = |choice: DecoderChoice, gpus: &[Gpu]| match choice {
            DecoderChoice::Auto => tr!("自動（獨顯優先）", "Auto (discrete GPU first)"),
            DecoderChoice::Software => tr!("CPU（軟體）", "CPU (software)"),
            DecoderChoice::Hardware(id) => gpu_label(id, gpus),
        };
        ui.horizontal(|ui| {
            ui.label(tr!("鏡像解碼", "Mirror decoder"));
            egui::ComboBox::from_id_salt("decoder")
                .width(ui.available_width())
                .selected_text(decoder_text(self.decoder_choice, &self.gpus))
                .show_ui(ui, |ui| {
                    let mut options = vec![DecoderChoice::Auto];
                    options.extend(self.gpus.iter().map(|g| DecoderChoice::Hardware(g.id)));
                    options.push(DecoderChoice::Software);
                    for option in options {
                        ui.selectable_value(&mut self.decoder_choice, option, decoder_text(option, &self.gpus));
                    }
                });
        });

        ui.add_space(8.0);
        ui.label(RichText::new(tr!("延伸螢幕畫質", "Extend quality")).strong());
        ui.horizontal(|ui| {
            ui.label(tr!("最大寬度", "Max width"));
            egui::ComboBox::from_id_salt("max_width")
                .selected_text(format!("{} px", self.settings.max_width))
                .show_ui(ui, |ui| {
                    for width in [1280, 1600, 1920, 2560, 3840] {
                        ui.selectable_value(&mut self.settings.max_width, width, format!("{width} px"));
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label(tr!("位元率", "Bitrate"));
            let mut mbps = self.settings.bitrate_kbps / 1000;
            if ui.add(egui::Slider::new(&mut mbps, 2..=50).suffix(" Mbps")).changed() {
                self.settings.bitrate_kbps = mbps * 1000;
            }
        });
        ui.horizontal(|ui| {
            ui.label(tr!("最高 fps", "Max fps"));
            for fps in [30, 60] {
                ui.selectable_value(&mut self.settings.max_fps, fps, fps.to_string());
            }
        });
        ui.label(RichText::new(tr!("下次開始延伸螢幕 / 鏡像時套用。", "Applies the next time Extend or Mirror starts.")).small().weak());

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("語言 / Language");
            let name = |language: Option<&str>| match language {
                Some("zh") => "中文".to_string(),
                Some("en") => "English".to_string(),
                _ => tr!("跟隨 Windows", "Same as Windows"),
            };
            let before = self.settings.language.clone();
            egui::ComboBox::from_id_salt("language").selected_text(name(before.as_deref())).show_ui(ui, |ui| {
                for option in [None, Some("zh".to_string()), Some("en".to_string())] {
                    let label = name(option.as_deref());
                    ui.selectable_value(&mut self.settings.language, option, label);
                }
            });
            if self.settings.language != before {
                apply_language(&self.settings);
            }
        });
    }

    /// Runs Virtual Display Driver work off the UI thread, logging progress.
    fn run_vdd_task(
        &self,
        what: String,
        task: impl FnOnce(&mut dyn FnMut(String)) -> Result<(), padbridge_host::BoxError> + Send + 'static,
    ) {
        let (busy, changed, log, ctx) =
            (self.vdd_busy.clone(), self.monitors_changed.clone(), self.log_tx.clone(), self.ctx.clone());
        busy.store(true, Ordering::Relaxed);
        thread::spawn(move || {
            let mut progress = |line: String| {
                let _ = log.send(line);
                ctx.request_repaint();
            };
            match task(&mut progress) {
                Ok(()) => progress(tr!("{what}：完成", "{what}: done")),
                Err(e) => progress(tr!("{what}失敗：{e}", "{what} failed: {e}")),
            }
            busy.store(false, Ordering::Relaxed);
            changed.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    fn refresh_monitors(&mut self) {
        self.virtual_adapters = virtual_adapters();
        match list_monitors() {
            Ok(monitors) => {
                // Prefer a virtual display if one exists, since that's the extended-desktop case;
                // switch to one that just appeared unless a virtual one is already chosen.
                let current = monitors.iter().find(|m| m.index == self.monitor_index);
                let virtual_available = monitors.iter().any(|m| m.virtual_adapter.is_some());
                if current.is_none()
                    || self.monitors.is_empty()
                    || (virtual_available && current.is_some_and(|m| m.virtual_adapter.is_none()))
                {
                    self.monitor_index = monitors
                        .iter()
                        .find(|m| m.virtual_adapter.is_some())
                        .or_else(|| monitors.first())
                        .map_or(0, |m| m.index);
                }
                self.monitors = monitors;
            }
            Err(e) => self.push_log(tr!("無法列出螢幕：{e}", "Could not list monitors: {e}")),
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

    fn start_display(&mut self, target: Target) {
        self.start_display_with(target, true);
    }

    /// With `allow_refit`, a Virtual Display Driver monitor whose size differs
    /// from the iPad's is switched to the iPad's resolution and the session restarted.
    fn start_display_with(&mut self, target: Target, allow_refit: bool) {
        self.stop_sessions();
        let settings = DisplaySettings {
            monitor: self.monitor_index,
            allow_input: self.allow_input,
            encoder: self.encoder_choice,
            max_width: self.settings.max_width,
            bitrate_kbps: self.settings.bitrate_kbps,
            max_fps: self.settings.max_fps,
        };
        let selected = self.monitors.iter().find(|m| m.index == self.monitor_index);
        let monitor = selected.map(|m| m.label()).unwrap_or_default();
        let fit_target = selected
            .filter(|m| allow_refit && m.virtual_adapter.as_deref().is_some_and(|a| a.contains("Virtual Display Driver")))
            .map(|m| (m.device_name.clone(), (m.width, m.height)));
        if target.attempt == 0 {
            self.push_log(tr!("延伸螢幕：{monitor} → {}", "Extending: {monitor} → {}", target.title));
        }
        let shared = Arc::new(Mutex::new(DisplayShared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_shared, worker_stop, ctx, log) = (shared.clone(), stop.clone(), self.ctx.clone(), self.log_tx.clone());
        let (transport, port, quiet) = (target.transport.clone(), target.port, target.attempt > 0);
        let opener = self.opener();
        thread::spawn(move || {
            let mut refit = None;
            let stream = match opener.open(&transport, port) {
                Ok(s) => s,
                Err(e) => {
                    let reason = tr!("連線失敗：{e}", "Connection failed: {e}");
                    if !quiet {
                        let _ = log.send(tr!("延伸螢幕結束：{reason}", "Extend ended: {reason}"));
                    }
                    let mut s = worker_shared.lock().unwrap();
                    s.lost = is_retryable(&e);
                    s.ended = Some(reason);
                    ctx.request_repaint();
                    return;
                }
            };
            if quiet {
                let _ = log.send(tr!("已重新連線", "Reconnected"));
            }
            worker_shared.lock().unwrap().peer_key = Some(stream.peer_key);
            run_display(stream, &host_name(), settings, &worker_stop, |event| {
                let mut s = worker_shared.lock().unwrap();
                match event {
                    DisplayEvent::Connected { name } => s.peer_name = Some(name),
                    DisplayEvent::Screen { width_px, height_px, max_fps } => {
                        s.screen = Some(format!("iPad {width_px}×{height_px} · {max_fps} Hz"));
                        if let Some((device, size)) = &fit_target {
                            if *size != (width_px, height_px) {
                                refit = Some((device.clone(), width_px, height_px));
                                worker_stop.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                    DisplayEvent::Streaming { monitor, width, height, encoder } => {
                        s.streaming = Some(tr!("{monitor}\n傳送解析度 {width}×{height} · {encoder}", "{monitor}\nSending {width}×{height} · {encoder}"));
                    }
                    DisplayEvent::Stats { fps, kbps, encode_ms } => {
                        s.stats = Some(tr!("{fps:.0} fps · {:.1} Mbps · 編碼 {encode_ms:.1} ms", "{fps:.0} fps · {:.1} Mbps · encode {encode_ms:.1} ms", kbps / 1000.0));
                    }
                    DisplayEvent::Log(text) => {
                        let _ = log.send(format!("[iPad] {text}"));
                    }
                    DisplayEvent::Ended { reason, lost } if refit.is_none() => {
                        let _ = log.send(tr!("延伸螢幕結束：{reason}", "Extend ended: {reason}"));
                        s.lost = lost;
                        s.ended = Some(reason);
                    }
                    DisplayEvent::Ended { .. } => {}
                }
                ctx.request_repaint();
            });
            if let Some((device, width, height)) = refit {
                let mut progress = |line: String| {
                    let _ = log.send(line);
                    ctx.request_repaint();
                };
                progress(tr!("把虛擬螢幕調成 iPad 的解析度 {width}×{height}…", "Setting the virtual display to the iPad's {width}×{height}…"));
                if let Err(e) = vdd::fit_monitor(&device, width, height, &mut progress) {
                    progress(tr!("無法調整虛擬螢幕解析度：{e}；以目前解析度繼續", "Could not change the virtual display's resolution: {e}; continuing as is"));
                }
                // Reconnect either way; the restarted session doesn't try again.
                worker_shared.lock().unwrap().restart = true;
                ctx.request_repaint();
            }
        });
        self.display = Some(DisplayView { target, retry_at: None, shared, stop });
    }

    fn display_view(&mut self, ui: &mut egui::Ui) {
        let Some(view) = &self.display else { return };
        let s = view.shared.lock().unwrap();
        let mut stop = false;
        ui.horizontal(|ui| {
            ui.heading(format!("🖥 {}", s.peer_name.as_deref().unwrap_or(&view.target.title)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                stop = ui.button(if s.ended.is_some() { tr!("關閉", "Close") } else { tr!("⏹ 停止", "⏹ Stop") }).clicked();
            });
        });
        ui.separator();
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            match &s.ended {
                Some(reason) => {
                    ui.label(RichText::new(reason).color(ui.visuals().warn_fg_color).size(16.0));
                    if let Some(at) = view.retry_at {
                        ui.add_space(8.0);
                        ui.label(retry_text(at));
                    }
                }
                None if s.streaming.is_none() => {
                    ui.spinner();
                    ui.label(connecting_text(view.target.attempt));
                }
                None => {
                    ui.label(RichText::new(tr!("正在把螢幕顯示在 iPad 上", "Showing the monitor on the iPad")).size(18.0));
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
        let (frame, peer, info, link, ended) = {
            let mut s = view.shared.lock().unwrap();
            (s.frame.take(), s.peer_name.clone(), s.stream_info.clone(), s.link_stats.clone(), s.ended.clone())
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
            ui.heading(peer.as_deref().unwrap_or(&view.target.title));
            if let Some(info) = &info {
                ui.label(RichText::new(info).weak());
            }
            if ended.is_none() && view.texture.is_some() {
                ui.label(RichText::new(tr!("顯示 {:.0} fps", "showing {:.0} fps", view.fps)).weak());
                if let Some(link) = &link {
                    ui.label(RichText::new(link).weak())
                        .on_hover_text(tr!("iPad 實際送來的畫格數與流量，以及每格解碼所花的時間", "Frames and data the iPad actually sends, and decode time per frame"));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                stop = ui.button(if ended.is_some() { tr!("關閉", "Close") } else { tr!("⏹ 停止", "⏹ Stop") }).clicked();
            });
        });
        if let Some(reason) = &ended {
            ui.horizontal(|ui| {
                ui.label(RichText::new(reason).color(ui.visuals().warn_fg_color));
                if let Some(at) = view.retry_at {
                    ui.label(retry_text(at));
                }
            });
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
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.4);
                    ui.spinner();
                    ui.label(connecting_text(view.target.attempt));
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
        self.pairing_dialog(&ui.ctx().clone());
        self.persist_settings();

        egui::Panel::left("devices").resizable(true).default_size(290.0).min_size(230.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.device_list(ui));
        });
        egui::Panel::bottom("log").resizable(true).default_size(130.0).min_size(60.0).show(ui, |ui| {
            ui.label(RichText::new(tr!("事件", "Events")).strong());
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

/// Whether a failed connection is worth retrying: not when someone declined
/// pairing or the other side runs an incompatible version.
fn is_retryable(error: &padbridge_host::BoxError) -> bool {
    use padbridge_host::secure::SecureError;
    !matches!(
        error.downcast_ref::<SecureError>(),
        Some(SecureError::Refused(_) | SecureError::DeclinedHere | SecureError::BadMagic)
    )
}

fn apply_language(settings: &Settings) {
    padbridge_host::i18n::set_english(match settings.language.as_deref() {
        Some("zh") => false,
        Some("en") => true,
        _ => padbridge_host::i18n::system_prefers_english(),
    });
}

fn connecting_text(attempt: u32) -> String {
    if attempt == 0 { tr!("連線中…", "Connecting…") } else { tr!("重新連線中（第 {attempt} 次）…", "Reconnecting (attempt {attempt})…") }
}

fn retry_text(at: Instant) -> String {
    let secs = at.saturating_duration_since(Instant::now()).as_secs_f32().ceil();
    tr!("{secs:.0} 秒後自動重新連線（按「關閉」取消）", "Reconnecting in {secs:.0} s (Close to cancel)")
}

fn welcome(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.25);
        ui.heading("PadBridge");
        ui.add_space(8.0);
        ui.label(tr!("iPad 與 Windows 畫面共享，USB 與 Wi-Fi 皆可", "iPad and Windows screen sharing over USB or Wi-Fi"));
        ui.add_space(16.0);
        ui.label(
            RichText::new(tr!(
                "鏡像 iPad 畫面：\n1. 在 iPad 開啟 PadBridge App，按廣播按鈕並選「PadBridge Mirror」\n2. 在左側裝置清單按「📺 鏡像」",
                "To mirror the iPad:\n1. In the PadBridge app on the iPad, start a broadcast and choose PadBridge Mirror\n2. Choose 📺 Mirror in the device list on the left"
            ))
            .weak(),
        );
    });
}

/// `0RGB` pixels from the decoder to an egui image.
fn to_color_image(frame: &Frame) -> ColorImage {
    // A few million pixels per frame: split across threads to keep up with 60 fps.
    let mut pixels = vec![Color32::BLACK; frame.pixels.len()];
    let band = frame.pixels.len().div_ceil(4).max(1);
    thread::scope(|scope| {
        for (out, src) in pixels.chunks_mut(band).zip(frame.pixels.chunks(band)) {
            scope.spawn(move || {
                for (o, &p) in out.iter_mut().zip(src) {
                    *o = Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8);
                }
            });
        }
    });
    ColorImage::new([frame.width, frame.height], pixels)
}
