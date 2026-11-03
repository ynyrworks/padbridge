//! Stand-in for the iPad in Display mode, for testing without an iPad:
//! listens on port 47800 like the iPad app, decodes the stream the host
//! sends, prints fps, and saves the latest frame as a PNG.
//!
//!     cargo run -p padbridge-host --example fake_display -- [frame.png]
//!     cargo run -p padbridge-cli -- display 127.0.0.1 [monitor]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Instant;

use padbridge_host::mirror::{Frame, MirrorDecoder};
use padbridge_host::secure::{accept_as_device, Identity, SecureLink, SecureWriter, Verdict};
use padbridge_protocol::{Codec, FrameDecoder, Message, Phase, PointerKind, Role, DISPLAY_PORT, PROTOCOL_VERSION};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--input-test`: after the first frame, send harmless input (Pencil hover,
    // then a trackpad-style pointer move to 25%/25% of the screen). No clicks.
    let input_test = args.iter().any(|a| a == "--input-test");
    let png_path = args.into_iter().find(|a| !a.starts_with("--"));
    let mut input_sent = false;
    let listener = TcpListener::bind(("127.0.0.1", DISPLAY_PORT)).expect("bind");
    println!("fake iPad display on 127.0.0.1:{DISPLAY_PORT}");
    // A fresh identity each run, so every host pairs anew (the code is printed instead of asked).
    let identity = Identity::generate().expect("identity");
    for stream in listener.incoming() {
        let stream = stream.expect("accept");
        let link = accept_as_device(stream, &identity, "Fake iPad", |_| false, |request| {
            println!("pairing with {} — code {}", request.peer_name, request.code);
            Verdict::Accepted
        });
        let SecureLink { reader: mut stream, mut writer, .. } = match link {
            Ok(link) => link,
            Err(e) => {
                println!("handshake failed: {e}");
                continue;
            }
        };
        let send = |s: &mut SecureWriter, m: Message| s.write_all(&m.to_frame().unwrap());
        let _ = send(&mut writer, Message::Hello { version: PROTOCOL_VERSION, role: Role::DisplaySink, name: "Fake iPad".into() });
        let _ = send(&mut writer, Message::DisplayInfo {
            width_px: 2732,
            height_px: 2048,
            scale_milli: 2000,
            max_fps: 120,
            codecs: Codec::H264.mask_bit(),
        });

        let mut decoder = MirrorDecoder::new().expect("decoder");
        let mut frames = FrameDecoder::new();
        let mut buf = vec![0u8; 256 * 1024];
        let (mut count, mut bytes, mut window) = (0u32, 0usize, Instant::now());
        let mut last: Option<Frame> = None;
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            bytes += n;
            frames.push(&buf[..n]);
            while let Ok(Some(msg)) = frames.next_message() {
                match &msg {
                    Message::Hello { name, .. } => println!("host: {name}"),
                    Message::StreamConfig { width, height, fps, bitrate_kbps, .. } => {
                        println!("stream {width}x{height} @{fps} fps, {bitrate_kbps} kbps")
                    }
                    _ => {}
                }
                match decoder.handle(&msg) {
                    Ok(Some(frame)) => {
                        count += 1;
                        if last.is_none() {
                            println!("first frame decoded: {}x{}", frame.width, frame.height);
                            if let Some(path) = &png_path {
                                save_png(path, &frame);
                            }
                        }
                        last = Some(frame);
                        if input_test && !input_sent {
                            input_sent = true;
                            let pointer = |kind, phase, x, y| Message::Pointer {
                                id: 0,
                                kind,
                                phase,
                                x,
                                y,
                                pressure: 0.0,
                                altitude: 1.2,
                                azimuth: 0.5,
                            };
                            for x in [0.5, 0.55, 0.6] {
                                let _ = send(&mut writer, pointer(PointerKind::Pen, Phase::Hover, x, 0.5));
                            }
                            let _ = send(&mut writer, pointer(PointerKind::Pen, Phase::Cancel, 0.6, 0.5));
                            let _ = send(&mut writer, pointer(PointerKind::Mouse, Phase::Hover, 0.25, 0.25));
                            println!("sent input test");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        println!("decode error: {e}");
                        let _ = send(&mut writer, Message::RequestKeyframe);
                    }
                }
            }
            if window.elapsed().as_secs_f64() >= 1.0 {
                let secs = window.elapsed().as_secs_f64();
                println!("{:.1} fps, {:.2} Mbps", f64::from(count) / secs, bytes as f64 * 8.0 / 1e6 / secs);
                (count, bytes, window) = (0, 0, Instant::now());
                if let (Some(path), Some(frame)) = (&png_path, &last) {
                    save_png(path, frame);
                }
            }
        }
        println!("host disconnected");
    }
}

fn save_png(path: &str, frame: &Frame) {
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), frame.width as u32, frame.height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let rgb: Vec<u8> = frame.pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    encoder.write_header().expect("png header").write_image_data(&rgb).expect("png data");
}
