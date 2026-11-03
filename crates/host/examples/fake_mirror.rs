//! Stand-in for the iPad broadcast extension, for testing the viewers without
//! an iPad: serves an animated test pattern on port 47801 and announces it
//! over Bonjour like the real extension.
//!
//!     cargo run -p padbridge-host --example fake_mirror
//!     cargo run -p padbridge-cli -- mirror 127.0.0.1     (or open PadBridge.exe)

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceInfo};
use openh264::encoder::Encoder;
use openh264::formats::{RgbSliceU8, YUVBuffer};
use padbridge_host::mirror::annex_b_to_avcc;
use padbridge_host::secure::{accept_as_device, Identity, SecureLink, SecureWriter, Verdict};
use padbridge_protocol::{Codec, Message, Role, MIRROR_PORT, PROTOCOL_VERSION};

const W: usize = 640;
const H: usize = 480;
const FPS: u64 = 30;

fn main() {
    let listener = TcpListener::bind(("0.0.0.0", MIRROR_PORT)).expect("bind");
    println!("fake mirror source on port {MIRROR_PORT}");

    let mdns = ServiceDaemon::new().expect("mdns");
    let service = ServiceInfo::new(
        "_padbridge._tcp.local.",
        "Fake iPad Mirror",
        "fake-ipad.local.",
        "",
        MIRROR_PORT,
        &[("proto", "2"), ("role", "2")][..],
    )
    .expect("service info")
    .enable_addr_auto();
    mdns.register(service).expect("register");
    // A fresh identity each run, so every host pairs anew (the code is printed instead of asked).
    let identity = Identity::generate().expect("identity");
    for stream in listener.incoming().flatten() {
        let identity = identity.clone();
        thread::spawn(move || {
            if let Err(e) = serve(stream, &identity) {
                println!("client gone: {e}");
            }
        });
    }
}

fn serve(stream: TcpStream, identity: &Identity) -> Result<(), Box<dyn std::error::Error>> {
    let link = accept_as_device(stream, identity, "Fake iPad Mirror", |_| false, |request| {
        println!("pairing with {} — code {}", request.peer_name, request.code);
        Verdict::Accepted
    })?;
    let SecureLink { mut reader, writer: mut stream, .. } = link;
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
    });
    let send = |s: &mut SecureWriter, m: Message| s.write_all(&m.to_frame().unwrap());
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
