//! Writes a short Display-mode stream (protocol frames, as sent to the iPad)
//! for the Swift tests, which check that VideoToolbox decodes what the
//! Windows encoder produces.
//!
//!     cargo run -p padbridge-host --example display_fixture -- ios/PadBridgeKit/Tests/PadBridgeMediaTests/Fixtures/windows_h264.bin

use padbridge_host::display::{DisplaySettings, ScreenImage, VideoPipeline};

fn main() {
    let path = std::env::args().nth(1).expect("output path");
    let (w, h) = (320, 240);
    let mut pipeline = VideoPipeline::new(DisplaySettings::default()).expect("encoder");
    let mut out = Vec::new();
    for frame in 0..3usize {
        let mut bgra = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                bgra[i..i + 4].copy_from_slice(&[((x + frame * 10) % 256) as u8, (y % 256) as u8, 128, 255]);
            }
        }
        let cursor = Some((40 + frame as i32 * 5, 60));
        for message in pipeline.encode(&ScreenImage { width: w, height: h, bgra: &bgra, cursor }).expect("encode") {
            message.encode(&mut out).expect("frame");
        }
    }
    std::fs::write(&path, &out).expect("write");
    println!("wrote {} bytes to {path}", out.len());
}
