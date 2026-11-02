//! Stand-in for the iPad broadcast extension, for testing the viewer without
//! an iPad: serves an animated test pattern on 127.0.0.1:47801.
//!
//!     cargo run -p padbridge-cli --example fake_mirror
//!     cargo run -p padbridge-cli -- mirror 127.0.0.1

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use openh264::encoder::Encoder;
use openh264::formats::{RgbSliceU8, YUVBuffer};
use padbridge_cli::mirror::annex_b_to_avcc;
use padbridge_protocol::{Codec, Message, Role, MIRROR_PORT, PROTOCOL_VERSION};

const W: usize = 640;
const H: usize = 480;
const FPS: u64 = 30;

fn main() {
    let listener = TcpListener::bind(("127.0.0.1", MIRROR_PORT)).expect("bind");
    println!("fake mirror source on 127.0.0.1:{MIRROR_PORT}");
    for stream in listener.incoming().flatten() {
        thread::spawn(move || {
            if let Err(e) = serve(stream) {
                println!("client gone: {e}");
            }
        });
    }
}

fn serve(mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = stream.try_clone()?;
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
    });
    let send = |s: &mut TcpStream, m: Message| s.write_all(&m.to_frame().unwrap());
    send(&mut stream, Message::Hello { version: PROTOCOL_VERSION, role: Role::MirrorSource, name: "Fake iPad".into() })?;

    let mut encoder = Encoder::new().expect("encoder");
    let start = Instant::now();
    for i in 0u64.. {
        let avcc = annex_b_to_avcc(&encoder.encode(&pattern(i)).expect("encode").to_vec());
        let (params, picture, keyframe) = split(&avcc);
        if keyframe {
            send(&mut stream, Message::StreamConfig { codec: Codec::H264, width: W as u32, height: H as u32, fps: FPS as u16, bitrate_kbps: 0 })?;
            send(&mut stream, Message::CodecParams { codec: Codec::H264, data: params })?;
        }
        let pts_us = start.elapsed().as_micros() as u64;
        send(&mut stream, Message::VideoFrame { pts_us, keyframe, rotation: 0, data: picture })?;
        thread::sleep(Duration::from_millis(1000 / FPS));
    }
    Ok(())
}

/// A vertical bar sweeping across a gradient.
fn pattern(frame: u64) -> YUVBuffer {
    let bar = (frame as usize * 8) % W;
    let mut rgb = vec![0u8; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 3;
            let on_bar = x.abs_diff(bar) < 16;
            rgb[i] = if on_bar { 255 } else { (x * 255 / W) as u8 };
            rgb[i + 1] = if on_bar { 255 } else { (y * 255 / H) as u8 };
            rgb[i + 2] = if on_bar { 255 } else { 128 };
        }
    }
    YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (W, H)))
}

/// Splits an AVCC access unit into (SPS/PPS, picture NALs, is IDR).
fn split(avcc: &[u8]) -> (Vec<u8>, Vec<u8>, bool) {
    let (mut params, mut picture, mut idr) = (Vec::new(), Vec::new(), false);
    let mut rest = avcc;
    while rest.len() >= 4 {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let nal_type = rest[4] & 0x1F;
        idr |= nal_type == 5;
        if nal_type == 7 || nal_type == 8 { &mut params } else { &mut picture }.extend_from_slice(&rest[..4 + len]);
        rest = &rest[4 + len..];
    }
    (params, picture, idr)
}
