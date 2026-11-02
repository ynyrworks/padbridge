//! PadBridge wire protocol.
//!
//! Every message travels in a frame: `[len: u32 LE][kind: u8][body]`, where
//! `len` counts the kind byte plus the body. All integers and floats are
//! little-endian. The byte layout of each body is documented in
//! `docs/DESIGN.md` and must stay in sync with the Swift implementation.

use std::fmt;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAGIC: [u8; 4] = *b"PDBR";
/// Upper bound for `len`; anything larger is treated as a corrupt stream.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

pub const DISPLAY_PORT: u16 = 47800;
pub const MIRROR_PORT: u16 = 47801;
pub const BONJOUR_SERVICE: &str = "_padbridge._tcp";

mod kind {
    pub const HELLO: u8 = 0x01;
    pub const DISPLAY_INFO: u8 = 0x02;
    pub const STREAM_CONFIG: u8 = 0x03;
    pub const CODEC_PARAMS: u8 = 0x10;
    pub const VIDEO_FRAME: u8 = 0x11;
    pub const REQUEST_KEYFRAME: u8 = 0x12;
    pub const POINTER: u8 = 0x20;
    pub const PING: u8 = 0x30;
    pub const PONG: u8 = 0x31;
    pub const LOG: u8 = 0x40;
    pub const BYE: u8 = 0x7F;
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $value:expr),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum $name {
            $($variant = $value),+
        }

        impl TryFrom<u8> for $name {
            type Error = Error;

            fn try_from(value: u8) -> Result<Self, Error> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(Error::BadEnum { field: stringify!($name), value }),
                }
            }
        }
    };
}

wire_enum!(Role { Host = 0, DisplaySink = 1, MirrorSource = 2 });
wire_enum!(Codec { H264 = 1, Hevc = 2 });
wire_enum!(PointerKind { Touch = 0, Pen = 1, Mouse = 2 });
wire_enum!(Phase { Down = 0, Move = 1, Up = 2, Cancel = 3, Hover = 4 });

impl Codec {
    /// Bit used for this codec in [`Message::DisplayInfo::codecs`].
    pub fn mask_bit(self) -> u8 {
        1 << (self as u8 - 1)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Hello {
        version: u16,
        role: Role,
        name: String,
    },
    DisplayInfo {
        width_px: u32,
        height_px: u32,
        /// Points-to-pixels scale × 1000 (e.g. 2000 for a 2× Retina screen).
        scale_milli: u16,
        max_fps: u16,
        /// Bitmask of [`Codec::mask_bit`] values the sender can decode.
        codecs: u8,
    },
    StreamConfig {
        codec: Codec,
        width: u32,
        height: u32,
        fps: u16,
        bitrate_kbps: u32,
    },
    /// Parameter sets (VPS/SPS/PPS), each prefixed by a big-endian u32 length.
    CodecParams { codec: Codec, data: Vec<u8> },
    /// AVCC NAL units (big-endian u32 length prefixes).
    VideoFrame {
        pts_us: u64,
        keyframe: bool,
        data: Vec<u8>,
    },
    RequestKeyframe,
    /// `x`/`y` are normalized to the video frame (0..1); angles are radians.
    Pointer {
        id: u32,
        kind: PointerKind,
        phase: Phase,
        x: f32,
        y: f32,
        pressure: f32,
        altitude: f32,
        azimuth: f32,
    },
    Ping { nonce: u64, t_us: u64 },
    Pong { nonce: u64, t_us: u64 },
    Log { level: u8, text: String },
    Bye,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated,
    TrailingBytes,
    FrameTooLarge(usize),
    UnknownKind(u8),
    BadMagic,
    BadUtf8,
    StringTooLong(usize),
    BadEnum { field: &'static str, value: u8 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => write!(f, "message body is truncated"),
            Error::TrailingBytes => write!(f, "message body has trailing bytes"),
            Error::FrameTooLarge(len) => write!(f, "frame length {len} exceeds {MAX_FRAME_LEN}"),
            Error::UnknownKind(kind) => write!(f, "unknown message kind 0x{kind:02X}"),
            Error::BadMagic => write!(f, "hello magic mismatch"),
            Error::BadUtf8 => write!(f, "string is not valid UTF-8"),
            Error::StringTooLong(len) => write!(f, "string of {len} bytes exceeds u16 length"),
            Error::BadEnum { field, value } => write!(f, "invalid {field} value {value}"),
        }
    }
}

impl std::error::Error for Error {}

impl Message {
    /// Appends this message as one complete frame to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let start = out.len();
        out.extend_from_slice(&[0; 4]);
        out.push(self.kind());
        let mut w = Writer(out);
        match self {
            Message::Hello { version, role, name } => {
                w.bytes(&MAGIC);
                w.u16(*version);
                w.u8(*role as u8);
                w.str(name)?;
            }
            Message::DisplayInfo { width_px, height_px, scale_milli, max_fps, codecs } => {
                w.u32(*width_px);
                w.u32(*height_px);
                w.u16(*scale_milli);
                w.u16(*max_fps);
                w.u8(*codecs);
            }
            Message::StreamConfig { codec, width, height, fps, bitrate_kbps } => {
                w.u8(*codec as u8);
                w.u32(*width);
                w.u32(*height);
                w.u16(*fps);
                w.u32(*bitrate_kbps);
            }
            Message::CodecParams { codec, data } => {
                w.u8(*codec as u8);
                w.bytes(data);
            }
            Message::VideoFrame { pts_us, keyframe, data } => {
                w.u64(*pts_us);
                w.u8(*keyframe as u8);
                w.bytes(data);
            }
            Message::Pointer { id, kind, phase, x, y, pressure, altitude, azimuth } => {
                w.u32(*id);
                w.u8(*kind as u8);
                w.u8(*phase as u8);
                for v in [x, y, pressure, altitude, azimuth] {
                    w.f32(*v);
                }
            }
            Message::Ping { nonce, t_us } | Message::Pong { nonce, t_us } => {
                w.u64(*nonce);
                w.u64(*t_us);
            }
            Message::Log { level, text } => {
                w.u8(*level);
                w.str(text)?;
            }
            Message::RequestKeyframe | Message::Bye => {}
        }

        let len = out.len() - start - 4;
        if len > MAX_FRAME_LEN {
            out.truncate(start);
            return Err(Error::FrameTooLarge(len));
        }
        out[start..start + 4].copy_from_slice(&(len as u32).to_le_bytes());
        Ok(())
    }

    pub fn to_frame(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.encode(&mut out)?;
        Ok(out)
    }

    fn kind(&self) -> u8 {
        match self {
            Message::Hello { .. } => kind::HELLO,
            Message::DisplayInfo { .. } => kind::DISPLAY_INFO,
            Message::StreamConfig { .. } => kind::STREAM_CONFIG,
            Message::CodecParams { .. } => kind::CODEC_PARAMS,
            Message::VideoFrame { .. } => kind::VIDEO_FRAME,
            Message::RequestKeyframe => kind::REQUEST_KEYFRAME,
            Message::Pointer { .. } => kind::POINTER,
            Message::Ping { .. } => kind::PING,
            Message::Pong { .. } => kind::PONG,
            Message::Log { .. } => kind::LOG,
            Message::Bye => kind::BYE,
        }
    }

    /// Decodes a frame body (everything after `len`, starting at the kind byte).
    pub fn decode(kind: u8, body: &[u8]) -> Result<Message, Error> {
        let mut r = Reader { buf: body, pos: 0 };
        let msg = match kind {
            kind::HELLO => {
                if r.take(4)? != MAGIC {
                    return Err(Error::BadMagic);
                }
                Message::Hello { version: r.u16()?, role: r.u8()?.try_into()?, name: r.str()? }
            }
            kind::DISPLAY_INFO => Message::DisplayInfo {
                width_px: r.u32()?,
                height_px: r.u32()?,
                scale_milli: r.u16()?,
                max_fps: r.u16()?,
                codecs: r.u8()?,
            },
            kind::STREAM_CONFIG => Message::StreamConfig {
                codec: r.u8()?.try_into()?,
                width: r.u32()?,
                height: r.u32()?,
                fps: r.u16()?,
                bitrate_kbps: r.u32()?,
            },
            kind::CODEC_PARAMS => Message::CodecParams { codec: r.u8()?.try_into()?, data: r.rest() },
            kind::VIDEO_FRAME => Message::VideoFrame {
                pts_us: r.u64()?,
                keyframe: r.u8()? & 1 != 0,
                data: r.rest(),
            },
            kind::REQUEST_KEYFRAME => Message::RequestKeyframe,
            kind::POINTER => Message::Pointer {
                id: r.u32()?,
                kind: r.u8()?.try_into()?,
                phase: r.u8()?.try_into()?,
                x: r.f32()?,
                y: r.f32()?,
                pressure: r.f32()?,
                altitude: r.f32()?,
                azimuth: r.f32()?,
            },
            kind::PING => Message::Ping { nonce: r.u64()?, t_us: r.u64()? },
            kind::PONG => Message::Pong { nonce: r.u64()?, t_us: r.u64()? },
            kind::LOG => Message::Log { level: r.u8()?, text: r.str()? },
            kind::BYE => Message::Bye,
            other => return Err(Error::UnknownKind(other)),
        };
        if r.pos != body.len() {
            return Err(Error::TrailingBytes);
        }
        Ok(msg)
    }
}

/// Incremental decoder for a byte stream (TCP or usbmux tunnel).
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Returns the next complete message, or `Ok(None)` if more bytes are needed.
    ///
    /// Frames with an unknown kind are skipped so newer peers can add messages.
    /// Any other error leaves the stream unsynchronized; the caller should
    /// drop the connection.
    pub fn next_message(&mut self) -> Result<Option<Message>, Error> {
        loop {
            let Some(header) = self.buf.get(..4) else { return Ok(None) };
            let len = u32::from_le_bytes(header.try_into().unwrap()) as usize;
            if len == 0 || len > MAX_FRAME_LEN {
                return Err(Error::FrameTooLarge(len));
            }
            if self.buf.len() < 4 + len {
                return Ok(None);
            }
            let result = Message::decode(self.buf[4], &self.buf[5..4 + len]);
            self.buf.drain(..4 + len);
            match result {
                Err(Error::UnknownKind(_)) => continue,
                other => return other.map(Some),
            }
        }
    }
}

struct Writer<'a>(&'a mut Vec<u8>);

impl Writer<'_> {
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.bytes(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) -> Result<(), Error> {
        let len = u16::try_from(s.len()).map_err(|_| Error::StringTooLong(s.len()))?;
        self.u16(len);
        self.bytes(s.as_bytes());
        Ok(())
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len()).ok_or(Error::Truncated)?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        self.array().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        self.array().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Result<u64, Error> {
        self.array().map(u64::from_le_bytes)
    }
    fn f32(&mut self) -> Result<f32, Error> {
        self.array().map(f32::from_le_bytes)
    }
    fn str(&mut self) -> Result<String, Error> {
        let len = self.u16()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| Error::BadUtf8)
    }
    fn rest(&mut self) -> Vec<u8> {
        let out = self.buf[self.pos..].to_vec();
        self.pos = self.buf.len();
        out
    }
}
