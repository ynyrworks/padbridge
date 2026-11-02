//! Test session: handshake with an iPad and measure round-trip time.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use padbridge_protocol::{FrameDecoder, Message, Role, PROTOCOL_VERSION};

/// Something the session observed, reported as it happens.
#[derive(Debug)]
pub enum Event {
    Received(Message),
    Rtt(Duration),
}

#[derive(Debug, Default)]
pub struct Summary {
    pub peer_hello: Option<(Role, String)>,
    pub rtts: Vec<Duration>,
}

/// Sends `Hello`, then `count` pings spaced by `interval`, and says `Bye`.
/// Waits up to one second after the last ping for outstanding pongs.
pub fn ping_session(
    stream: TcpStream,
    name: &str,
    count: u32,
    interval: Duration,
    mut on_event: impl FnMut(&Event),
) -> io::Result<Summary> {
    stream.set_nodelay(true)?;
    let start = Instant::now();
    let (tx, rx) = mpsc::channel();
    let mut reader = stream.try_clone()?;
    thread::spawn(move || {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            decoder.push(&buf[..n]);
            loop {
                match decoder.next_message() {
                    Ok(Some(msg)) => {
                        if tx.send(msg).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => return,
                }
            }
        }
    });

    let mut writer = stream;
    let send = |w: &mut TcpStream, msg: Message| -> io::Result<()> {
        w.write_all(&msg.to_frame().map_err(io::Error::other)?)
    };
    send(&mut writer, Message::Hello { version: PROTOCOL_VERSION, role: Role::Host, name: name.into() })?;

    let mut summary = Summary::default();
    let mut handle = |msg: Message, summary: &mut Summary| {
        let event = match msg {
            Message::Pong { t_us, .. } => {
                let rtt = start.elapsed().saturating_sub(Duration::from_micros(t_us));
                summary.rtts.push(rtt);
                Event::Rtt(rtt)
            }
            msg => {
                if let Message::Hello { role, name, .. } = &msg {
                    summary.peer_hello = Some((*role, name.clone()));
                }
                Event::Received(msg)
            }
        };
        on_event(&event);
    };

    for nonce in 0..count as u64 {
        let t_us = start.elapsed().as_micros() as u64;
        send(&mut writer, Message::Ping { nonce, t_us })?;
        let deadline = Instant::now() + interval;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(msg) => handle(msg, &mut summary),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "peer closed the connection"))
                }
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while summary.rtts.len() < count as usize {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
        match rx.recv_timeout(left) {
            Ok(msg) => handle(msg, &mut summary),
            Err(_) => break,
        }
    }
    let _ = send(&mut writer, Message::Bye);
    Ok(summary)
}
