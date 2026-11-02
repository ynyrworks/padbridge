use padbridge_protocol::*;

fn samples() -> Vec<Message> {
    vec![
        Message::Hello { version: PROTOCOL_VERSION, role: Role::DisplaySink, name: "Ynyr 的 iPad".into() },
        Message::DisplayInfo {
            width_px: 2732,
            height_px: 2048,
            scale_milli: 2000,
            max_fps: 120,
            codecs: Codec::H264.mask_bit() | Codec::Hevc.mask_bit(),
        },
        Message::StreamConfig { codec: Codec::Hevc, width: 2732, height: 2048, fps: 60, bitrate_kbps: 20_000 },
        Message::CodecParams { codec: Codec::H264, data: vec![0, 0, 0, 2, 0x67, 0x42] },
        Message::VideoFrame { pts_us: 1_234_567, keyframe: true, data: vec![0, 0, 0, 1, 0x65] },
        Message::VideoFrame { pts_us: 1_250_000, keyframe: false, data: vec![] },
        Message::RequestKeyframe,
        Message::Pointer {
            id: 7,
            kind: PointerKind::Pen,
            phase: Phase::Move,
            x: 0.25,
            y: 0.75,
            pressure: 0.5,
            altitude: 1.2,
            azimuth: -0.3,
        },
        Message::Ping { nonce: 42, t_us: 99 },
        Message::Pong { nonce: 42, t_us: 99 },
        Message::Log { level: 2, text: "hello".into() },
        Message::Bye,
    ]
}

#[test]
fn every_message_roundtrips() {
    for msg in samples() {
        let frame = msg.to_frame().unwrap();
        let mut dec = FrameDecoder::new();
        dec.push(&frame);
        assert_eq!(dec.next_message().unwrap(), Some(msg));
        assert_eq!(dec.next_message().unwrap(), None);
    }
}

#[test]
fn stream_split_into_single_bytes() {
    let mut stream = Vec::new();
    for msg in samples() {
        msg.encode(&mut stream).unwrap();
    }
    let mut dec = FrameDecoder::new();
    let mut got = Vec::new();
    for b in stream {
        dec.push(&[b]);
        while let Some(msg) = dec.next_message().unwrap() {
            got.push(msg);
        }
    }
    assert_eq!(got, samples());
}

/// Golden bytes the Swift implementation must reproduce.
#[test]
fn golden_ping_and_hello() {
    let ping = Message::Ping { nonce: 1, t_us: 2 }.to_frame().unwrap();
    assert_eq!(
        ping,
        [17, 0, 0, 0, 0x30, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]
    );

    let hello = Message::Hello { version: 1, role: Role::Host, name: "PC".into() }.to_frame().unwrap();
    assert_eq!(hello, [12, 0, 0, 0, 0x01, b'P', b'D', b'B', b'R', 1, 0, 0, 2, 0, b'P', b'C']);
}

#[test]
fn unknown_kind_is_skipped() {
    let mut stream = vec![3, 0, 0, 0, 0x55, 0xAA, 0xBB];
    Message::Bye.encode(&mut stream).unwrap();
    let mut dec = FrameDecoder::new();
    dec.push(&stream);
    assert_eq!(dec.next_message().unwrap(), Some(Message::Bye));
}

#[test]
fn malformed_input_is_rejected() {
    let mut dec = FrameDecoder::new();
    dec.push(&[0xFF, 0xFF, 0xFF, 0x7F, 0x01]);
    assert!(matches!(dec.next_message(), Err(Error::FrameTooLarge(_))));

    assert_eq!(Message::decode(0x01, b"XXXX\x01\x00\x00\x00\x00"), Err(Error::BadMagic));
    assert_eq!(Message::decode(0x30, &[0; 15]), Err(Error::Truncated));
    assert_eq!(Message::decode(0x12, &[0]), Err(Error::TrailingBytes));
    assert_eq!(
        Message::decode(0x03, &[9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(Error::BadEnum { field: "Codec", value: 9 })
    );
}
