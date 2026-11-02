//! Mirror mode, host side: turns the iPad's video messages into RGB frames.

use std::fmt;

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use padbridge_protocol::{Codec, Message};

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
    rgba: Vec<u8>,
}

impl MirrorDecoder {
    pub fn new() -> Result<Self, MirrorError> {
        Ok(Self {
            decoder: Decoder::new().map_err(MirrorError::Decode)?,
            params: Vec::new(),
            waiting_for_keyframe: true,
            rgba: Vec::new(),
        })
    }

    /// Feeds one message. Returns a frame when one was decoded. After an
    /// error, the caller should send `RequestKeyframe`.
    pub fn handle(&mut self, msg: &Message) -> Result<Option<Frame>, MirrorError> {
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
                let result = self.decode(*pts_us, *keyframe, *rotation, data);
                self.waiting_for_keyframe = result.is_err();
                result
            }
            _ => Ok(None),
        }
    }

    fn decode(&mut self, pts_us: u64, keyframe: bool, rotation: u8, data: &[u8]) -> Result<Option<Frame>, MirrorError> {
        let mut packet = if keyframe { self.params.clone() } else { Vec::new() };
        packet.extend(avcc_to_annex_b(data).ok_or(MirrorError::BadAvcc)?);
        let Some(yuv) = self.decoder.decode(&packet).map_err(MirrorError::Decode)? else { return Ok(None) };

        let (width, height) = yuv.dimensions();
        self.rgba.resize(width * height * 4, 0);
        yuv.write_rgba8(&mut self.rgba);
        let pixels: Vec<u32> = self
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
            .collect();
        let (width, height, pixels) = rotate(width, height, pixels, rotation);
        Ok(Some(Frame { width, height, pixels, pts_us }))
    }
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
