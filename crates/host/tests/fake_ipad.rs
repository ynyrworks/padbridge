//! Runs the ping session against a minimal in-process stand-in for the iPad server.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use padbridge_host::ping_session;
use padbridge_protocol::{FrameDecoder, Message, Role, PROTOCOL_VERSION};

fn fake_ipad(mut stream: TcpStream) {
    let hello = Message::Hello { version: PROTOCOL_VERSION, role: Role::DisplaySink, name: "Fake iPad".into() };
    stream.write_all(&hello.to_frame().unwrap()).unwrap();
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        if n == 0 {
            return;
        }
        decoder.push(&buf[..n]);
        while let Some(msg) = decoder.next_message().unwrap() {
            match msg {
                Message::Ping { nonce, t_us } => {
                    stream.write_all(&Message::Pong { nonce, t_us }.to_frame().unwrap()).unwrap();
                }
                Message::Bye => return,
                _ => {}
            }
        }
    }
}

#[test]
fn handshake_and_pings() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || fake_ipad(listener.accept().unwrap().0));

    let stream = TcpStream::connect(addr).unwrap();
    let summary = ping_session(stream, "Test PC", 5, Duration::from_millis(20), |_| {}).unwrap();

    assert_eq!(summary.peer_hello, Some((Role::DisplaySink, "Fake iPad".into())));
    assert_eq!(summary.rtts.len(), 5);
    server.join().unwrap();
}
