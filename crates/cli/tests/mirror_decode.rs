//! Encodes synthetic frames with OpenH264, packages them the way the iPad
//! does (CodecParams + AVCC VideoFrames), and checks the viewer decodes them.

use openh264::encoder::Encoder;
use openh264::formats::{RgbSliceU8, YUVBuffer};
use padbridge_cli::mirror::{annex_b_to_avcc, MirrorDecoder};
use padbridge_protocol::{Codec, Message};

const W: usize = 64;
const H: usize = 48;

/// Left half red, right half blue.
fn test_image() -> YUVBuffer {
    let mut rgb = vec![0u8; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 3;
            if x < W / 2 { rgb[i] = 255 } else { rgb[i + 2] = 255 }
        }
    }
    YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (W, H)))
}

/// Splits an encoded access unit into (parameter sets, picture NALs) in AVCC.
fn split(avcc: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut params, mut picture) = (Vec::new(), Vec::new());
    let mut rest = avcc;
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let unit = &rest[..4 + len];
        let nal_type = rest[4] & 0x1F;
        if nal_type == 7 || nal_type == 8 { &mut params } else { &mut picture }.extend_from_slice(unit);
        rest = &rest[4 + len..];
    }
    (params, picture)
}

#[test]
fn decodes_ipad_style_stream_with_rotation() {
    let mut encoder = Encoder::new().unwrap();
    let image = test_image();
    let mut decoder = MirrorDecoder::new().unwrap();

    let mut frames = Vec::new();
    for i in 0..3u64 {
        let avcc = annex_b_to_avcc(&encoder.encode(&image).unwrap().to_vec());
        let (params, picture) = split(&avcc);
        let keyframe = i == 0;
        if keyframe {
            assert!(!params.is_empty(), "first access unit should carry SPS/PPS");
            decoder.handle(&Message::CodecParams { codec: Codec::H264, data: params }).unwrap();
        }
        let msg = Message::VideoFrame { pts_us: i * 16_666, keyframe, rotation: 1, data: picture };
        if let Some(frame) = decoder.handle(&msg).unwrap() {
            frames.push(frame);
        }
    }

    assert!(!frames.is_empty());
    let f = &frames[0];
    // Rotated a quarter turn clockwise: 64x48 becomes 48x64, and the red left
    // half ends up on top.
    assert_eq!((f.width, f.height), (H, W));
    let top = f.pixels[2 * f.width + f.width / 2];
    let bottom = f.pixels[(f.height - 3) * f.width + f.width / 2];
    let red_blue = |p: u32| (p >> 16 & 0xFF, p & 0xFF);
    let (r, b) = red_blue(top);
    assert!(r > 180 && b < 80, "top should be red, got {top:06X}");
    let (r, b) = red_blue(bottom);
    assert!(b > 180 && r < 80, "bottom should be blue, got {bottom:06X}");
}

#[test]
fn delta_frames_before_keyframe_are_dropped() {
    let mut decoder = MirrorDecoder::new().unwrap();
    let msg = Message::VideoFrame { pts_us: 0, keyframe: false, rotation: 0, data: vec![0, 0, 0, 1, 0x41] };
    assert!(decoder.handle(&msg).unwrap().is_none());
}

#[test]
fn hevc_is_rejected() {
    let mut decoder = MirrorDecoder::new().unwrap();
    let msg = Message::StreamConfig { codec: Codec::Hevc, width: 1, height: 1, fps: 60, bitrate_kbps: 1 };
    assert!(decoder.handle(&msg).is_err());
}
