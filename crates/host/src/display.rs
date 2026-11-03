//! Display mode, host side: capture a monitor, encode it and stream it to the
//! iPad, which shows it full screen.

use std::time::Instant;

use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use openh264::OpenH264API;
use padbridge_protocol::{Codec, Message};

use crate::mirror::annex_b_to_avcc;
use crate::{BoxError, GpuId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySettings {
    /// Index from `capture::list_monitors`.
    pub monitor: usize,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    /// Wider screens are scaled down to this width before encoding.
    pub max_width: usize,
    /// Inject the iPad's touches, Pencil and trackpad input into Windows.
    pub allow_input: bool,
    /// Which encoder to use; hardware falls back to software automatically.
    pub encoder: EncoderChoice,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            monitor: 0,
            max_fps: 60,
            bitrate_kbps: 10_000,
            max_width: 1920,
            allow_input: true,
            encoder: EncoderChoice::Auto,
        }
    }
}

/// Which encoder Display uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncoderChoice {
    /// Hardware on the GPU that renders the monitor (no copies), else software.
    #[default]
    Auto,
    /// OpenH264 on the CPU.
    Software,
    /// Hardware on this GPU. If the monitor is rendered by another GPU, each
    /// frame is copied across through system memory.
    Hardware(GpuId),
}

/// A desktop image as tightly packed BGRA.
pub struct ScreenImage<'a> {
    pub width: usize,
    pub height: usize,
    pub bgra: &'a [u8],
}

/// Scales and H.264-encodes desktop images into protocol messages (software path).
/// The cursor is not drawn in: the iPad draws it from `Cursor` messages.
pub struct VideoPipeline {
    settings: DisplaySettings,
    encoder: Encoder,
    scaled: Vec<u8>,
    yuv: Option<YUVBuffer>,
    start: Instant,
    keyframe_pending: bool,
}

impl VideoPipeline {
    pub fn new(settings: DisplaySettings) -> Result<Self, BoxError> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(settings.bitrate_kbps * 1000))
            .max_frame_rate(FrameRate::from_hz(settings.max_fps as f32))
            // Needed for the bitrate cap; skipped frames come back empty.
            .skip_frames(true)
            // Not supported for screen content; set explicitly to avoid warnings.
            .adaptive_quantization(false)
            .background_detection(false);
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)?;
        Ok(Self { settings, encoder, scaled: Vec::new(), yuv: None, start: Instant::now(), keyframe_pending: true })
    }

    /// The next emitted frame will be a keyframe (retried if rate control skips one).
    pub fn force_keyframe(&mut self) {
        self.keyframe_pending = true;
    }

    /// Size frames of `width`×`height` are encoded at.
    pub fn output_size(&self, width: usize, height: usize) -> (usize, usize) {
        let scale = (self.settings.max_width as f64 / width as f64).min(1.0);
        let even = |v: f64| ((v.round() as usize) & !1).max(2);
        (even(width as f64 * scale), even(height as f64 * scale))
    }

    /// Encodes one image. Keyframes are preceded by `StreamConfig` and `CodecParams`.
    pub fn encode(&mut self, image: &ScreenImage) -> Result<Vec<Message>, BoxError> {
        let (w, h) = self.output_size(image.width, image.height);
        resize_bgra(image.bgra, image.width, image.height, &mut self.scaled, w, h);
        let yuv = match &mut self.yuv {
            Some(yuv) if yuv_dimensions(yuv) == (w, h) => yuv,
            slot => slot.insert(YUVBuffer::new(w, h)),
        };
        yuv.read_bgra8(BgraSliceU8::new(&self.scaled, (w, h)));
        if self.keyframe_pending {
            self.encoder.force_intra_frame();
        }
        let annex_b = self.encoder.encode(yuv)?.to_vec();
        let (params, picture, keyframe) = split_access_unit(&annex_b_to_avcc(&annex_b));
        if picture.is_empty() {
            return Ok(Vec::new());
        }
        if keyframe {
            self.keyframe_pending = false;
        }

        let mut messages = Vec::with_capacity(3);
        if keyframe {
            messages.push(Message::StreamConfig {
                codec: Codec::H264,
                width: w as u32,
                height: h as u32,
                fps: self.settings.max_fps as u16,
                bitrate_kbps: self.settings.bitrate_kbps,
            });
            messages.push(Message::CodecParams { codec: Codec::H264, data: params });
        }
        let pts_us = self.start.elapsed().as_micros() as u64;
        messages.push(Message::VideoFrame { pts_us, keyframe, rotation: 0, data: picture });
        Ok(messages)
    }
}

fn yuv_dimensions(yuv: &YUVBuffer) -> (usize, usize) {
    openh264::formats::YUVSource::dimensions(yuv)
}

/// Splits an AVCC access unit into (SPS/PPS, picture NALs, contains IDR).
pub fn split_access_unit(avcc: &[u8]) -> (Vec<u8>, Vec<u8>, bool) {
    let (mut params, mut picture, mut idr) = (Vec::new(), Vec::new(), false);
    let mut rest = avcc;
    while rest.len() > 4 {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let Some(unit) = rest.get(..4 + len) else { break };
        let nal_type = unit[4] & 0x1F;
        idr |= nal_type == 5;
        if nal_type == 7 || nal_type == 8 { &mut params } else { &mut picture }.extend_from_slice(unit);
        rest = &rest[4 + len..];
    }
    (params, picture, idr)
}

/// BGRA resize: plain copy when the size is unchanged, bilinear for
/// reductions up to 2× (fast, and exact 2×2 averaging at 2×), area averaging
/// beyond that.
pub fn resize_bgra(src: &[u8], sw: usize, sh: usize, dst: &mut Vec<u8>, dw: usize, dh: usize) {
    dst.resize(dw * dh * 4, 0);
    if (sw, sh) == (dw, dh) {
        dst.copy_from_slice(&src[..dw * dh * 4]);
    } else if sw <= dw * 2 && sh <= dh * 2 {
        resize_bilinear(src, sw, sh, dst, dw, dh);
    } else {
        resize_area(src, sw, sh, dst, dw, dh);
    }
}

/// Source index and 8-bit weight of the next sample for each destination coordinate.
fn bilinear_taps(src_len: usize, dst_len: usize) -> Vec<(usize, u32)> {
    (0..dst_len)
        .map(|d| {
            let f = ((d as f64 + 0.5) * src_len as f64 / dst_len as f64 - 0.5).max(0.0);
            let i = (f as usize).min(src_len - 1);
            (i, ((f - i as f64) * 256.0) as u32)
        })
        .collect()
}

/// Interpolates two packed BGRA pixels, two channels per multiply (`w` in 0..=256).
#[inline]
fn lerp_pixel(a: u32, b: u32, w: u32) -> u32 {
    let iw = 256 - w;
    let rb = (((a & 0x00FF_00FF) * iw + (b & 0x00FF_00FF) * w + 0x0080_0080) >> 8) & 0x00FF_00FF;
    let ga = ((((a >> 8) & 0x00FF_00FF) * iw + ((b >> 8) & 0x00FF_00FF) * w + 0x0080_0080) >> 8) & 0x00FF_00FF;
    rb | (ga << 8)
}

fn resize_bilinear(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    let pixel = |row: &[u8], x: usize| u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap());
    let xs: Vec<(usize, usize, u32)> =
        bilinear_taps(sw, dw).into_iter().map(|(x0, w)| (x0, (x0 + 1).min(sw - 1), w)).collect();
    for (out, &(y0, wy)) in dst.chunks_exact_mut(dw * 4).zip(bilinear_taps(sh, dh).iter()) {
        let row0 = &src[y0 * sw * 4..][..sw * 4];
        let row1 = &src[(y0 + 1).min(sh - 1) * sw * 4..][..sw * 4];
        for (px, &(x0, x1, wx)) in out.as_chunks_mut::<4>().0.iter_mut().zip(&xs) {
            let top = lerp_pixel(pixel(row0, x0), pixel(row0, x1), wx);
            let bottom = lerp_pixel(pixel(row1, x0), pixel(row1, x1), wx);
            px.copy_from_slice(&lerp_pixel(top, bottom, wy).to_le_bytes());
        }
    }
}

fn resize_area(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    for dy in 0..dh {
        let (y0, y1) = (dy * sh / dh, ((dy + 1) * sh / dh).max(dy * sh / dh + 1));
        for dx in 0..dw {
            let (x0, x1) = (dx * sw / dw, ((dx + 1) * sw / dw).max(dx * sw / dw + 1));
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = &src[(y * sw + x0) * 4..(y * sw + x1) * 4];
                for px in row.as_chunks::<4>().0 {
                    for c in 0..4 {
                        sum[c] += u32::from(px[c]);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let out = &mut dst[(dy * dw + dx) * 4..][..4];
            for c in 0..4 {
                out[c] = (sum[c] / n) as u8;
            }
        }
    }
}

#[cfg(windows)]
pub use session::{run_display, DisplayEvent};

#[cfg(windows)]
mod session {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use padbridge_protocol::{FrameDecoder, Message, Role, PROTOCOL_VERSION};

    use windows::Win32::Graphics::Direct3D11::{
        ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    use super::{DisplaySettings, EncoderChoice, VideoPipeline};
    use crate::capture::{list_monitors, Capturer};
    use crate::gpu::{open_video_device, Gpu};
    use crate::hwenc::HardwareEncoder;
    use crate::secure::{SecureLink, SecureReader, SecureWriter};
    use crate::GpuId;
    use crate::input::{Injector, InputMapper, ScreenRect};

    #[derive(Debug, Clone, PartialEq)]
    pub enum DisplayEvent {
        Connected { name: String },
        /// The iPad's screen.
        Screen { width_px: u32, height_px: u32, max_fps: u16 },
        Streaming { monitor: String, width: usize, height: usize, encoder: String },
        Stats { fps: f64, kbps: f64, encode_ms: f64 },
        Log(String),
        /// `lost`: the connection dropped (worth reconnecting), as opposed to
        /// being stopped or failing for another reason.
        Ended { reason: String, lost: bool },
    }

    enum Encoder {
        /// With `Upload` when the encoder's GPU isn't the one rendering the monitor.
        Hardware(Box<HardwareEncoder>, Option<Box<Upload>>),
        Software(Box<VideoPipeline>),
    }

    impl Encoder {
        fn name(&self) -> String {
            match self {
                Encoder::Hardware(e, None) => tr!("硬體：{}", "Hardware: {}", e.name()),
                Encoder::Hardware(e, Some(upload)) => {
                    tr!("硬體：{}（{}，跨 GPU 複製）", "Hardware: {} ({}, copied across GPUs)", e.name(), upload.gpu.label())
                }
                Encoder::Software(_) => tr!("軟體：OpenH264", "Software: OpenH264"),
            }
        }

        fn force_keyframe(&mut self) {
            match self {
                Encoder::Hardware(e, _) => e.force_keyframe(),
                Encoder::Software(p) => p.force_keyframe(),
            }
        }
    }

    /// Copies captured frames to another GPU's texture for its encoder.
    struct Upload {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        gpu: Gpu,
        texture: Option<(ID3D11Texture2D, (usize, usize))>,
    }

    impl Upload {
        fn new(device: ID3D11Device, gpu: Gpu) -> Result<Self, crate::BoxError> {
            let context = unsafe { device.GetImmediateContext()? };
            Ok(Self { device, context, gpu, texture: None })
        }

        fn write(&mut self, width: usize, height: usize, bgra: &[u8]) -> Result<&ID3D11Texture2D, crate::BoxError> {
            if self.texture.as_ref().is_none_or(|(_, size)| *size != (width, height)) {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: width as u32,
                    Height: height as u32,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut texture = None;
                unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture))? };
                self.texture = Some((texture.ok_or("upload texture was not created")?, (width, height)));
            }
            let (texture, _) = self.texture.as_ref().unwrap();
            unsafe { self.context.UpdateSubresource(texture, 0, None, bgra.as_ptr().cast(), (width * 4) as u32, 0) };
            Ok(texture)
        }
    }

    /// Streams a monitor to the iPad until it disconnects, an error occurs,
    /// or `stop` is set. Runs on the calling thread.
    pub fn run_display(
        link: SecureLink,
        host_name: &str,
        settings: DisplaySettings,
        stop: &AtomicBool,
        mut on_event: impl FnMut(DisplayEvent),
    ) {
        let (reason, lost) = match stream_display(link, host_name, settings, stop, &mut on_event) {
            Ok(end) => end,
            // Socket errors mean the link went away; anything else (capture, encoder) won't fix itself.
            Err(e) => (tr!("錯誤：{e}", "Error: {e}"), e.downcast_ref::<std::io::Error>().is_some()),
        };
        on_event(DisplayEvent::Ended { reason, lost });
    }

    /// Waits for the first desktop image so its size is known.
    fn first_image(capturer: &mut Capturer) -> Result<(), crate::BoxError> {
        for _ in 0..50 {
            if capturer.poll(100)?.image {
                return Ok(());
            }
        }
        Err(tr!("擷取不到螢幕畫面", "No image from the monitor").into())
    }

    /// Hardware encoding when chosen and available, otherwise software with CPU readback.
    fn open_encoder(
        settings: &DisplaySettings,
        on_event: &mut impl FnMut(DisplayEvent),
    ) -> Result<(Capturer, Encoder), crate::BoxError> {
        let gpu = match settings.encoder {
            EncoderChoice::Software => None,
            EncoderChoice::Auto => Some(None),
            EncoderChoice::Hardware(id) => Some(Some(id)),
        };
        if let Some(gpu) = gpu {
            match open_hardware(settings, gpu) {
                Ok(pair) => return Ok(pair),
                Err(e) => on_event(DisplayEvent::Log(tr!("無法使用硬體編碼，改用軟體編碼：{e}", "Hardware encoding unavailable, using software: {e}"))),
            }
        }
        let mut capturer = Capturer::new(settings.monitor, true)?;
        first_image(&mut capturer)?;
        Ok((capturer, Encoder::Software(Box::new(VideoPipeline::new(settings.clone())?))))
    }

    /// Hardware encoder on `gpu`, or on the GPU rendering the monitor when `None`.
    fn open_hardware(settings: &DisplaySettings, gpu: Option<GpuId>) -> Result<(Capturer, Encoder), crate::BoxError> {
        let monitor_gpu = list_monitors()?.get(settings.monitor).map(|m| m.gpu);
        let cross_gpu = gpu.is_some_and(|g| Some(g) != monitor_gpu);
        let mut capturer = Capturer::new(settings.monitor, cross_gpu)?;
        first_image(&mut capturer)?;
        if !cross_gpu {
            let encoder = HardwareEncoder::new(capturer.device(), capturer.size(), settings.clone())?;
            return Ok((capturer, Encoder::Hardware(Box::new(encoder), None)));
        }
        let (device, gpu) = open_video_device(gpu)?;
        let encoder = HardwareEncoder::new(&device, capturer.size(), settings.clone())?;
        Ok((capturer, Encoder::Hardware(Box::new(encoder), Some(Box::new(Upload::new(device, gpu)?)))))
    }

    fn stream_display(
        link: SecureLink,
        host_name: &str,
        settings: DisplaySettings,
        stop: &AtomicBool,
        on_event: &mut impl FnMut(DisplayEvent),
    ) -> Result<(String, bool), crate::BoxError> {
        let SecureLink { reader, mut writer, .. } = link;
        let send = |writer: &mut SecureWriter, message: &Message| -> Result<usize, crate::BoxError> {
            let frame = message.to_frame()?;
            writer.write_all(&frame)?;
            Ok(frame.len())
        };
        send(&mut writer, &Message::Hello { version: PROTOCOL_VERSION, role: Role::Host, name: host_name.into() })?;

        let (mut capturer, mut encoder) = open_encoder(&settings, on_event)?;
        let monitor = capturer.monitor().label();
        let input_rect = settings.allow_input.then(|| {
            let m = capturer.monitor();
            ScreenRect { left: m.left, top: m.top, width: m.width, height: m.height }
        });

        let want_keyframe = Arc::new(AtomicBool::new(true));
        let (events_tx, events_rx) = mpsc::channel();
        let reader_keyframe = want_keyframe.clone();
        thread::spawn(move || read_peer(reader, reader_keyframe, input_rect, events_tx));

        let interval = Duration::from_secs_f64(1.0 / f64::from(settings.max_fps.max(1)));
        let mut announced = None;
        let mut sent_cursor = None;
        let mut dirty = true;
        let mut next_send = Instant::now();
        let (mut frames, mut bytes, mut encode_time, mut window) = (0u32, 0usize, Duration::ZERO, Instant::now());

        loop {
            if stop.load(Ordering::Relaxed) {
                let _ = send(&mut writer, &Message::Bye);
                return Ok((tr!("已停止", "Stopped"), false));
            }
            while let Ok(event) = events_rx.try_recv() {
                match event {
                    Some(event) => on_event(event),
                    None => return Ok((tr!("iPad 已中斷連線", "The iPad disconnected"), true)),
                }
            }

            let wait = next_send.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            dirty |= capturer.poll(wait.as_millis().min(100) as u32)?.image;

            // The iPad draws the cursor itself, so moving it costs no video frame.
            let (width, height) = capturer.size();
            let cursor = capturer.cursor();
            if sent_cursor != Some(cursor) && width > 0 {
                let message = match cursor {
                    Some((x, y)) => Message::Cursor { x: x as f32 / width as f32, y: y as f32 / height as f32, visible: true },
                    None => Message::Cursor { x: 0.0, y: 0.0, visible: false },
                };
                if send(&mut writer, &message).is_err() {
                    return Ok((tr!("連線中斷", "Connection lost"), true));
                }
                sent_cursor = Some(cursor);
            }

            if want_keyframe.swap(false, Ordering::Relaxed) {
                encoder.force_keyframe();
                dirty = true;
            }
            if !dirty || Instant::now() < next_send {
                continue;
            }

            let started = Instant::now();
            let (messages, size) = match &mut encoder {
                Encoder::Hardware(hw, upload) => {
                    if hw.input_size() != capturer.size() {
                        // Resolution changed: rebuild the GPU pipeline for the new size.
                        let device = upload.as_ref().map_or(capturer.device(), |u| &u.device);
                        **hw = HardwareEncoder::new(device, capturer.size(), settings.clone())?;
                    }
                    let texture = match upload {
                        Some(upload) => {
                            let Some(image) = capturer.last_frame() else { continue };
                            upload.write(image.width, image.height, image.bgra)?
                        }
                        None => {
                            let Some(texture) = capturer.texture() else { continue };
                            texture
                        }
                    };
                    (hw.encode(texture)?, hw.output_size())
                }
                Encoder::Software(pipeline) => {
                    let Some(image) = capturer.last_frame() else { continue };
                    let image = super::ScreenImage { width: image.width, height: image.height, bgra: image.bgra };
                    (pipeline.encode(&image)?, pipeline.output_size(image.width, image.height))
                }
            };
            encode_time += started.elapsed();
            if announced != Some(size) {
                on_event(DisplayEvent::Streaming { monitor: monitor.clone(), width: size.0, height: size.1, encoder: encoder.name() });
                announced = Some(size);
            }
            for message in &messages {
                match send(&mut writer, message) {
                    Ok(n) => bytes += n,
                    Err(_) => return Ok((tr!("連線中斷", "Connection lost"), true)),
                }
            }
            frames += 1;
            dirty = false;
            next_send = started + interval;

            let elapsed = window.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let secs = elapsed.as_secs_f64();
                on_event(DisplayEvent::Stats {
                    fps: f64::from(frames) / secs,
                    kbps: bytes as f64 * 8.0 / 1000.0 / secs,
                    encode_ms: encode_time.as_secs_f64() * 1000.0 / f64::from(frames.max(1)),
                });
                (frames, bytes, encode_time, window) = (0, 0, Duration::ZERO, Instant::now());
            }
        }
    }

    /// Reads the iPad's messages and injects its input when `input_rect` is
    /// set. Sends `None` when the connection ends.
    fn read_peer(
        mut stream: SecureReader,
        want_keyframe: Arc<AtomicBool>,
        input_rect: Option<ScreenRect>,
        events: mpsc::Sender<Option<DisplayEvent>>,
    ) {
        let mut mapper = InputMapper::new();
        let injector = input_rect.and_then(|_| match Injector::new() {
            Ok(injector) => Some(injector),
            Err(e) => {
                let _ = events.send(Some(DisplayEvent::Log(tr!("無法啟用觸控輸入：{e}", "Touch input unavailable: {e}"))));
                None
            }
        });
        let mut reported_injection_error = false;
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            decoder.push(&buf[..n]);
            loop {
                let event = match decoder.next_message() {
                    Ok(Some(Message::Hello { name, .. })) => DisplayEvent::Connected { name },
                    Ok(Some(Message::DisplayInfo { width_px, height_px, max_fps, .. })) => {
                        DisplayEvent::Screen { width_px, height_px, max_fps }
                    }
                    Ok(Some(Message::Log { text, .. })) => DisplayEvent::Log(text),
                    Ok(Some(Message::RequestKeyframe)) => {
                        want_keyframe.store(true, Ordering::Relaxed);
                        continue;
                    }
                    Ok(Some(msg @ Message::Pointer { .. })) => {
                        let (Some(rect), Some(injector)) = (input_rect, &injector) else { continue };
                        for action in mapper.handle(&msg, rect) {
                            if let Err(e) = injector.apply(&action) {
                                if !reported_injection_error {
                                    reported_injection_error = true;
                                    // Typically: the target window runs as administrator.
                                    let _ = events.send(Some(DisplayEvent::Log(tr!("輸入注入失敗：{e}", "Input injection failed: {e}"))));
                                }
                            }
                        }
                        continue;
                    }
                    Ok(Some(Message::Bye)) | Err(_) => {
                        let _ = events.send(None);
                        return;
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => break,
                };
                if events.send(Some(event)).is_err() {
                    return;
                }
            }
        }
        let _ = events.send(None);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The cross-GPU path: frames uploaded from system memory to a separate
        /// device and encoded there. Needs a hardware encoder:
        /// `cargo test --release -p padbridge-host upload_then_encode -- --ignored`
        #[test]
        #[ignore]
        fn upload_then_encode() {
            let (device, gpu) = open_video_device(None).unwrap();
            let (w, h) = (640, 360);
            let mut upload = Upload::new(device.clone(), gpu).unwrap();
            let mut encoder = HardwareEncoder::new(&device, (w, h), DisplaySettings::default()).unwrap();
            let mut keyframes = 0;
            for shift in 0..10u8 {
                let bgra: Vec<u8> = (0..w * h).flat_map(|i| [(i as u8).wrapping_add(shift), 90, 200, 255]).collect();
                let texture = upload.write(w, h, &bgra).unwrap().clone();
                for message in encoder.encode(&texture).unwrap() {
                    keyframes += usize::from(matches!(message, Message::VideoFrame { keyframe: true, .. }));
                }
            }
            assert!(keyframes >= 1, "no keyframe from uploaded frames");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::MirrorDecoder;

    fn gradient(w: usize, h: usize, shift: usize) -> Vec<u8> {
        let mut bgra = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                bgra[i] = ((x + shift) % 256) as u8;
                bgra[i + 1] = (y % 256) as u8;
                bgra[i + 2] = 128;
                bgra[i + 3] = 255;
            }
        }
        bgra
    }

    /// `cargo test --release -p padbridge-host bench_pipeline -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_pipeline() {
        let (w, h) = (2560, 1440);
        let src = gradient(w, h, 0);
        let mut dst = Vec::new();
        let t = Instant::now();
        for _ in 0..10 {
            resize_bgra(&src, w, h, &mut dst, 1920, 1080);
        }
        println!("resize 2560x1440 -> 1920x1080: {:.1} ms", t.elapsed().as_secs_f64() * 100.0);

        let mut yuv = YUVBuffer::new(1920, 1080);
        let t = Instant::now();
        for _ in 0..10 {
            yuv.read_bgra8(BgraSliceU8::new(&dst, (1920, 1080)));
        }
        println!("bgra -> yuv 1920x1080: {:.1} ms", t.elapsed().as_secs_f64() * 100.0);

        let mut pipeline = VideoPipeline::new(DisplaySettings::default()).unwrap();
        let t = Instant::now();
        for i in 0..10 {
            let frame = gradient(1920, 1080, i);
            pipeline.encode(&ScreenImage { width: 1920, height: 1080, bgra: &frame }).unwrap();
        }
        println!("pipeline 1920x1080 (incl. test-frame generation): {:.1} ms", t.elapsed().as_secs_f64() * 100.0);
    }

    /// Viewer side of Mirror at the iPad's broadcast size (1334×1920, sent rotated).
    /// `cargo test --release -p padbridge-host bench_mirror_viewer -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_mirror_viewer() {
        use crate::mirror::rotate;
        let (w, h) = (1920, 1334);
        let settings = DisplaySettings { max_width: w, ..DisplaySettings::default() };
        let mut pipeline = VideoPipeline::new(settings).unwrap();
        let mut messages = Vec::new();
        for i in 0..30 {
            messages.extend(pipeline.encode(&ScreenImage { width: w, height: h, bgra: &gradient(w, h, i * 3) }).unwrap());
        }
        let mut decoder = MirrorDecoder::new().unwrap();
        let t = Instant::now();
        let mut frames = 0;
        for m in &messages {
            frames += usize::from(decoder.handle_frame(m, false).unwrap().is_none() && matches!(m, Message::VideoFrame { .. }));
        }
        println!("decode only: {:.1} ms/frame", t.elapsed().as_secs_f64() * 1000.0 / frames as f64);

        let mut decoder = MirrorDecoder::new().unwrap();
        let t = Instant::now();
        let mut last = None;
        for m in &messages {
            last = decoder.handle_frame(m, true).unwrap().or(last);
        }
        println!("decode + convert: {:.1} ms/frame", t.elapsed().as_secs_f64() * 1000.0 / frames as f64);

        let frame = last.unwrap();
        let t = Instant::now();
        for _ in 0..10 {
            rotate(frame.width, frame.height, frame.pixels.clone(), 1);
        }
        println!("separate rotate 90 (incl. clone): {:.1} ms", t.elapsed().as_secs_f64() * 100.0);

        let mut packet = Vec::new();
        for m in &messages[..3] {
            if let Message::CodecParams { data, .. } | Message::VideoFrame { data, .. } = m {
                packet.extend(crate::mirror::avcc_to_annex_b(data).unwrap());
            }
        }
        let mut raw = openh264::decoder::Decoder::new().unwrap();
        let yuv = raw.decode(&packet).unwrap().unwrap();
        for turns in [0, 1] {
            let t = Instant::now();
            for _ in 0..10 {
                crate::mirror::yuv_to_xrgb_rotated(&yuv, turns);
            }
            println!("fused convert, {turns} turns: {:.1} ms", t.elapsed().as_secs_f64() * 100.0);
        }
    }

    #[test]
    fn run_mirror_delivers_frames_and_ends() {
        mirror_session(crate::mirror::DecoderChoice::Software);
    }

    /// Needs a GPU with H.264 decoding:
    /// `cargo test --release -p padbridge-host run_mirror_hardware -- --ignored`
    #[test]
    #[ignore]
    fn run_mirror_hardware() {
        let events = mirror_session(crate::mirror::DecoderChoice::Auto);
        assert!(
            events.iter().any(|e| matches!(e, crate::mirror::MirrorEvent::Decoder(name) if name.contains("MFT"))),
            "fell back to software: {events:?}"
        );
    }

    fn mirror_session(choice: crate::mirror::DecoderChoice) -> Vec<crate::mirror::MirrorEvent> {
        use crate::mirror::{run_mirror, MirrorEvent};
        use crate::secure::{accept_as_device, connect_as_host, Identity, Verdict};
        use padbridge_protocol::{Role, PROTOCOL_VERSION};
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ipad = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let identity = Identity::generate().unwrap();
            let mut link = accept_as_device(socket, &identity, "test iPad", |_| false, |_| Verdict::Accepted).unwrap();
            let settings = DisplaySettings { max_width: 320, ..DisplaySettings::default() };
            let mut pipeline = VideoPipeline::new(settings).unwrap();
            let hello = Message::Hello { version: PROTOCOL_VERSION, role: Role::Host, name: "test iPad".into() };
            link.writer.write_all(&hello.to_frame().unwrap()).unwrap();
            for i in 0..10 {
                for m in pipeline.encode(&ScreenImage { width: 320, height: 200, bgra: &gradient(320, 200, i) }).unwrap() {
                    link.writer.write_all(&m.to_frame().unwrap()).unwrap();
                }
                thread::sleep(Duration::from_millis(5));
            }
            link.writer.write_all(&Message::Bye.to_frame().unwrap()).unwrap();
            // Closing with the host's Hello unread would reset the connection on Windows.
            link.shutdown_handle().unwrap().shutdown(std::net::Shutdown::Write).unwrap();
            let _ = std::io::copy(&mut link.reader, &mut std::io::sink());
        });

        let host = Identity::generate().unwrap();
        let stream = connect_as_host(TcpStream::connect(addr).unwrap(), &host, "test host", |_| false, |_| true).unwrap();
        let shown = AtomicUsize::new(0);
        let mut events = Vec::new();
        run_mirror(
            stream,
            "test host",
            choice,
            |frame| {
                assert_eq!((frame.width, frame.height), (320, 200));
                shown.fetch_add(1, Ordering::Relaxed);
                true
            },
            |event| events.push(event),
        );
        ipad.join().unwrap();
        assert!(shown.load(Ordering::Relaxed) >= 1, "no frame shown: {events:?}");
        assert!(events.contains(&MirrorEvent::Connected { name: "test iPad".into() }));
        assert!(matches!(events.last(), Some(MirrorEvent::Ended { lost: true, .. })), "{events:?}");
        events
    }

    #[test]
    fn fused_conversion_matches_reference() {
        use crate::mirror::{avcc_to_annex_b, rotate, yuv_to_xrgb_rotated};
        use openh264::decoder::Decoder;
        use openh264::formats::YUVSource;

        let settings = DisplaySettings { max_width: 96, ..DisplaySettings::default() };
        let mut pipeline = VideoPipeline::new(settings).unwrap();
        let messages = pipeline.encode(&ScreenImage { width: 96, height: 64, bgra: &gradient(96, 64, 5) }).unwrap();
        let mut packet = Vec::new();
        for m in &messages {
            match m {
                Message::CodecParams { data, .. } | Message::VideoFrame { data, .. } => {
                    packet.extend(avcc_to_annex_b(data).unwrap());
                }
                _ => {}
            }
        }
        let mut decoder = Decoder::new().unwrap();
        let yuv = decoder.decode(&packet).unwrap().expect("frame");
        let (w, h) = yuv.dimensions();
        let mut rgba = vec![0u8; w * h * 4];
        yuv.write_rgba8(&mut rgba);
        let xrgb: Vec<u32> =
            rgba.as_chunks::<4>().0.iter().map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])).collect();
        for turns in 0..4 {
            let expected = rotate(w, h, xrgb.clone(), turns);
            let got = yuv_to_xrgb_rotated(&yuv, turns);
            assert_eq!((got.0, got.1), (expected.0, expected.1), "size, {turns} turns");
            let worst = got.2.iter().zip(&expected.2).flat_map(|(a, b)| {
                (0..3).map(move |c| ((a >> (c * 8)) & 255).abs_diff((b >> (c * 8)) & 255))
            });
            assert!(worst.max().unwrap() <= 3, "colors differ, {turns} turns");
        }
    }

    fn gray(values: &[u8]) -> Vec<u8> {
        values.iter().flat_map(|&v| [v, v, v, 255]).collect()
    }

    #[test]
    fn resize_halves_by_averaging_blocks() {
        // 4x2 -> 2x1 (bilinear at exactly 2x): each output pixel averages a 2x2 block.
        let src = gray(&[0, 100, 200, 250, 50, 150, 210, 230]);
        let mut dst = Vec::new();
        resize_bgra(&src, 4, 2, &mut dst, 2, 1);
        assert_eq!(dst, gray(&[75, 223]));
    }

    #[test]
    fn resize_area_for_large_reductions() {
        // 8x1 -> 2x1 (4x): area averaging of 4 pixels each.
        let src = gray(&[0, 40, 80, 120, 200, 200, 200, 200]);
        let mut dst = Vec::new();
        resize_bgra(&src, 8, 1, &mut dst, 2, 1);
        assert_eq!(dst, gray(&[60, 200]));
    }

    #[test]
    fn resize_bilinear_keeps_flat_color() {
        let src = gray(&[90; 2560 * 4 / 4]);
        let mut dst = Vec::new();
        resize_bgra(&src, 64, 40, &mut dst, 48, 30);
        assert!(dst.as_chunks::<4>().0.iter().all(|p| *p == [90, 90, 90, 255]));
    }

    #[test]
    fn encodes_scaled_stream_the_viewer_can_decode() {
        let settings = DisplaySettings { max_width: 640, ..DisplaySettings::default() };
        let mut pipeline = VideoPipeline::new(settings).unwrap();
        let mut decoder = MirrorDecoder::new().unwrap();
        let (w, h) = (1280, 720);

        let first = gradient(w, h, 0);
        let messages = pipeline.encode(&ScreenImage { width: w, height: h, bgra: &first }).unwrap();
        assert!(matches!(messages[0], Message::StreamConfig { width: 640, height: 360, .. }));
        assert!(matches!(messages[1], Message::CodecParams { .. }));
        assert!(matches!(messages[2], Message::VideoFrame { keyframe: true, .. }));
        let mut decoded = None;
        for m in &messages {
            decoded = decoder.handle(m).unwrap().or(decoded);
        }
        let frame = decoded.expect("decodable keyframe");
        assert_eq!((frame.width, frame.height), (640, 360));

        let second = gradient(w, h, 7);
        let messages = pipeline.encode(&ScreenImage { width: w, height: h, bgra: &second }).unwrap();
        assert!(matches!(messages.as_slice(), [Message::VideoFrame { keyframe: false, .. }]));
        assert!(decoder.handle(&messages[0]).unwrap().is_some());

        pipeline.force_keyframe();
        let messages = pipeline.encode(&ScreenImage { width: w, height: h, bgra: &second }).unwrap();
        assert!(matches!(messages.last(), Some(Message::VideoFrame { keyframe: true, .. })));
    }
}
