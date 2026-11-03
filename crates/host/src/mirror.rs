//! Mirror mode, host side: receives the iPad's video stream and turns it into
//! RGB frames.

use std::collections::VecDeque;
use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use padbridge_protocol::{Codec, FrameDecoder, Message, Role, PROTOCOL_VERSION};

/// What a mirror session reports besides frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorEvent {
    Connected { name: String },
    Stream { codec: Codec, width: u32, height: u32, fps: u16, bitrate_kbps: u32 },
    Log(String),
    /// Recoverable: a keyframe has been requested.
    DecodeError(String),
    Ended(String),
}

/// Runs a mirror session on the calling thread until the iPad disconnects,
/// `on_frame` returns `false`, or the stream is shut down from elsewhere
/// (`TcpStream::shutdown` on a clone is the way to stop it).
pub fn run_mirror(
    mut stream: TcpStream,
    host_name: &str,
    mut on_frame: impl FnMut(Frame) -> bool,
    mut on_event: impl FnMut(MirrorEvent),
) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => return on_event(MirrorEvent::Ended(format!("連線錯誤：{e}"))),
    };
    let mut send = |msg: Message| writer.write_all(&msg.to_frame().expect("small message"));
    if let Err(e) = send(Message::Hello { version: PROTOCOL_VERSION, role: Role::Host, name: host_name.into() }) {
        return on_event(MirrorEvent::Ended(format!("連線錯誤：{e}")));
    }
    let mut decoder = match MirrorDecoder::new() {
        Ok(d) => d,
        Err(e) => return on_event(MirrorEvent::Ended(format!("無法初始化解碼器：{e}"))),
    };

    // Reading runs on its own thread so a backlog is visible here: every frame
    // must be decoded (later ones reference it), but only the newest one in the
    // queue is converted and shown, which keeps latency flat when the viewer
    // is slower than the iPad's frame rate.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut frames = FrameDecoder::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) => return tx.send(Err("iPad 已中斷連線".to_string())),
                Ok(n) => n,
                Err(e) => return tx.send(Err(format!("連線結束：{e}"))),
            };
            frames.push(&buf[..n]);
            loop {
                match frames.next_message() {
                    Ok(Some(msg)) => {
                        if tx.send(Ok(msg)).is_err() {
                            return Ok(());
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return tx.send(Err(format!("協定錯誤：{e}"))),
                }
            }
        }
    });

    let mut queue = VecDeque::new();
    loop {
        if queue.is_empty() {
            match rx.recv() {
                Ok(item) => queue.push_back(item),
                Err(_) => return on_event(MirrorEvent::Ended("連線結束".into())),
            }
        }
        queue.extend(rx.try_iter());
        while let Some(item) = queue.pop_front() {
            let msg = match item {
                Ok(msg) => msg,
                Err(reason) => return on_event(MirrorEvent::Ended(reason)),
            };
            let newest = !queue.iter().any(|m| matches!(m, Ok(Message::VideoFrame { .. })));
            match &msg {
                Message::Hello { name, .. } => on_event(MirrorEvent::Connected { name: name.clone() }),
                Message::StreamConfig { codec, width, height, fps, bitrate_kbps } => on_event(MirrorEvent::Stream {
                    codec: *codec,
                    width: *width,
                    height: *height,
                    fps: *fps,
                    bitrate_kbps: *bitrate_kbps,
                }),
                Message::Log { text, .. } => on_event(MirrorEvent::Log(text.clone())),
                Message::Bye => return on_event(MirrorEvent::Ended("iPad 結束廣播".into())),
                _ => {}
            }
            match decoder.handle_frame(&msg, newest) {
                Ok(Some(frame)) => {
                    if !on_frame(frame) {
                        let _ = send(Message::Bye);
                        return on_event(MirrorEvent::Ended("已停止".into()));
                    }
                }
                Ok(None) => {}
                Err(e @ MirrorError::UnsupportedCodec(_)) => return on_event(MirrorEvent::Ended(e.to_string())),
                Err(e) => {
                    on_event(MirrorEvent::DecodeError(e.to_string()));
                    if send(Message::RequestKeyframe).is_err() {
                        return on_event(MirrorEvent::Ended("連線中斷".into()));
                    }
                }
            }
        }
    }
}

/// A decoded frame, upright, as `0RGB` pixels (the layout minifb expects).
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub pts_us: u64,
}

#[derive(Debug)]
pub enum MirrorError {
    UnsupportedCodec(Codec),
    BadAvcc,
    Decode(openh264::Error),
}

impl fmt::Display for MirrorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirrorError::UnsupportedCodec(c) => write!(f, "codec {c:?} is not supported by this viewer yet"),
            MirrorError::BadAvcc => write!(f, "malformed AVCC data"),
            MirrorError::Decode(e) => write!(f, "H.264 decode error: {e}"),
        }
    }
}

impl std::error::Error for MirrorError {}

pub struct MirrorDecoder {
    decoder: Decoder,
    /// SPS/PPS as Annex-B, fed ahead of every keyframe.
    params: Vec<u8>,
    /// Drop delta frames until a keyframe arrives (after start or an error).
    waiting_for_keyframe: bool,
}

impl MirrorDecoder {
    pub fn new() -> Result<Self, MirrorError> {
        Ok(Self {
            decoder: Decoder::new().map_err(MirrorError::Decode)?,
            params: Vec::new(),
            waiting_for_keyframe: true,
        })
    }

    /// Feeds one message. Returns a frame when one was decoded. After an
    /// error, the caller should send `RequestKeyframe`.
    pub fn handle(&mut self, msg: &Message) -> Result<Option<Frame>, MirrorError> {
        self.handle_frame(msg, true)
    }

    /// Like [`handle`](Self::handle), but with `want_image` false a video
    /// frame is only decoded (to keep the reference chain), not converted.
    pub fn handle_frame(&mut self, msg: &Message, want_image: bool) -> Result<Option<Frame>, MirrorError> {
        match msg {
            Message::StreamConfig { codec, .. } | Message::CodecParams { codec, .. } if *codec != Codec::H264 => {
                Err(MirrorError::UnsupportedCodec(*codec))
            }
            Message::CodecParams { data, .. } => {
                self.params = avcc_to_annex_b(data).ok_or(MirrorError::BadAvcc)?;
                Ok(None)
            }
            Message::VideoFrame { pts_us, keyframe, rotation, data } => {
                if !keyframe && self.waiting_for_keyframe {
                    return Ok(None);
                }
                let result = self.decode(*pts_us, *keyframe, *rotation, data, want_image);
                self.waiting_for_keyframe = result.is_err();
                result
            }
            _ => Ok(None),
        }
    }

    fn decode(
        &mut self,
        pts_us: u64,
        keyframe: bool,
        rotation: u8,
        data: &[u8],
        want_image: bool,
    ) -> Result<Option<Frame>, MirrorError> {
        let mut packet = if keyframe { self.params.clone() } else { Vec::new() };
        packet.extend(avcc_to_annex_b(data).ok_or(MirrorError::BadAvcc)?);
        let Some(yuv) = self.decoder.decode(&packet).map_err(MirrorError::Decode)? else { return Ok(None) };
        if !want_image {
            return Ok(None);
        }
        let (width, height, pixels) = yuv_to_xrgb_rotated(&yuv, rotation);
        Ok(Some(Frame { width, height, pixels, pts_us }))
    }
}

/// Converts I420 (BT.601, limited range) to upright `0RGB` in one pass,
/// applying `quarter_turns` clockwise rotations, split across threads.
/// Each output pixel reads its source position, so rotation costs nothing extra.
pub fn yuv_to_xrgb_rotated(yuv: &impl YUVSource, quarter_turns: u8) -> (usize, usize, Vec<u32>) {
    let (w, h) = yuv.dimensions();
    let (y_stride, u_stride, v_stride) = yuv.strides();
    let (y_plane, u_plane, v_plane) = (yuv.y(), yuv.u(), yuv.v());
    let turns = quarter_turns & 3;
    let (ow, oh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    // Output (ox, oy) → source (sx, sy).
    let source = move |ox: usize, oy: usize| match turns {
        0 => (ox, oy),
        1 => (oy, h - 1 - ox),
        2 => (w - 1 - ox, h - 1 - oy),
        _ => (w - 1 - oy, ox),
    };
    let convert = |sx: usize, sy: usize| {
        let c = 298 * (i32::from(y_plane[sy * y_stride + sx]) - 16);
        let d = i32::from(u_plane[(sy / 2) * u_stride + sx / 2]) - 128;
        let e = i32::from(v_plane[(sy / 2) * v_stride + sx / 2]) - 128;
        let r = ((c + 409 * e + 128) >> 8).clamp(0, 255) as u32;
        let g = ((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u32;
        let b = ((c + 516 * d + 128) >> 8).clamp(0, 255) as u32;
        r << 16 | g << 8 | b
    };

    let mut out = vec![0u32; ow * oh];
    if ow == 0 || oh == 0 {
        return (ow, oh, out);
    }
    let threads = thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let rows_per_band = oh.div_ceil(threads);
    thread::scope(|scope| {
        for (band, rows) in out.chunks_mut(rows_per_band * ow).enumerate() {
            let (source, convert) = (&source, &convert);
            scope.spawn(move || {
                for (i, row) in rows.chunks_mut(ow).enumerate() {
                    let oy = band * rows_per_band + i;
                    for (ox, pixel) in row.iter_mut().enumerate() {
                        let (sx, sy) = source(ox, oy);
                        *pixel = convert(sx, sy);
                    }
                }
            });
        }
    });
    (ow, oh, out)
}

/// Converts 4-byte big-endian length prefixes to Annex-B start codes.
pub fn avcc_to_annex_b(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut rest = data;
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let nal = rest.get(4..4 + len)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        rest = &rest[4 + len..];
    }
    Some(out)
}

/// Converts Annex-B to 4-byte big-endian length prefixes (AVCC).
pub fn annex_b_to_avcc(data: &[u8]) -> Vec<u8> {
    // Offsets just past each 00 00 01 start code.
    let starts: Vec<usize> = (0..data.len().saturating_sub(2))
        .filter(|&i| data[i..i + 3] == [0, 0, 1])
        .map(|i| i + 3)
        .collect();
    let mut out = Vec::with_capacity(data.len());
    for (k, &start) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |&next| next - 3);
        // NALs end in a non-zero stop byte; zeros before the next start code
        // belong to the start code (00 00 00 01) or are trailing padding.
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        out.extend_from_slice(&((end - start) as u32).to_be_bytes());
        out.extend_from_slice(&data[start..end]);
    }
    out
}

/// Rotates clockwise by `quarter_turns` × 90°.
pub fn rotate(width: usize, height: usize, pixels: Vec<u32>, quarter_turns: u8) -> (usize, usize, Vec<u32>) {
    match quarter_turns & 3 {
        0 => (width, height, pixels),
        2 => (width, height, pixels.into_iter().rev().collect()),
        turns => {
            let mut out = vec![0; pixels.len()];
            for y in 0..height {
                for x in 0..width {
                    // Clockwise: (x, y) -> (height-1-y, x). Counter-clockwise: (x, y) -> (y, width-1-x).
                    let (nx, ny) = if turns == 1 { (height - 1 - y, x) } else { (y, width - 1 - x) };
                    out[ny * height + nx] = pixels[y * width + x];
                }
            }
            (height, width, out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_annex_b_roundtrip() {
        let avcc = [0, 0, 0, 2, 0x67, 0x42, 0, 0, 0, 3, 0x68, 0xCE, 0x3C];
        let annex_b = avcc_to_annex_b(&avcc).unwrap();
        assert_eq!(annex_b, [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE, 0x3C]);
        assert_eq!(annex_b_to_avcc(&annex_b), avcc);
        assert_eq!(avcc_to_annex_b(&[0, 0, 0, 9, 1]), None);
    }

    #[test]
    fn rotations() {
        // 2x3 image:
        // 1 2
        // 3 4
        // 5 6
        let px = vec![1, 2, 3, 4, 5, 6];
        assert_eq!(rotate(2, 3, px.clone(), 0), (2, 3, px.clone()));
        assert_eq!(rotate(2, 3, px.clone(), 1), (3, 2, vec![5, 3, 1, 6, 4, 2]));
        assert_eq!(rotate(2, 3, px.clone(), 2), (2, 3, vec![6, 5, 4, 3, 2, 1]));
        assert_eq!(rotate(2, 3, px, 3), (3, 2, vec![2, 4, 6, 1, 3, 5]));
    }
}
