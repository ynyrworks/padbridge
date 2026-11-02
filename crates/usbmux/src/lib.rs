//! Minimal usbmuxd client.
//!
//! usbmuxd multiplexes TCP connections to iOS devices over USB. On Windows it
//! is provided by Apple Mobile Device Service (installed with the "Apple
//! Devices" app or iTunes) on `127.0.0.1:27015`.
//!
//! Wire format: a 16-byte little-endian header `[length, version=1,
//! message=8 (plist), tag]` followed by an XML plist. After a successful
//! `Connect`, the same socket becomes a raw byte tunnel to the device port.

use std::fmt;
use std::io::{self, Cursor, Read, Write};
use std::net::TcpStream;

use plist::{Dictionary, Value};

pub const DEFAULT_ADDR: &str = "127.0.0.1:27015";
/// Same override variable libimobiledevice honours.
pub const ADDR_ENV: &str = "USBMUXD_SOCKET_ADDRESS";

const HEADER_LEN: usize = 16;
const PLIST_VERSION: u32 = 1;
const PLIST_MESSAGE: u32 = 8;
const MAX_MESSAGE_LEN: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Plist(plist::Error),
    /// The daemon sent something we don't understand.
    Protocol(String),
    /// The daemon answered a request with a non-zero result code.
    Result(ResultCode),
    UnsupportedAddress(String),
}

/// usbmuxd result codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    BadCommand,
    BadDevice,
    ConnectionRefused,
    BadVersion,
    Other(u64),
}

impl From<u64> for ResultCode {
    fn from(code: u64) -> Self {
        match code {
            1 => ResultCode::BadCommand,
            2 => ResultCode::BadDevice,
            3 => ResultCode::ConnectionRefused,
            6 => ResultCode::BadVersion,
            other => ResultCode::Other(other),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "usbmuxd I/O error: {e}"),
            Error::Plist(e) => write!(f, "usbmuxd plist error: {e}"),
            Error::Protocol(msg) => write!(f, "usbmuxd protocol error: {msg}"),
            Error::Result(ResultCode::ConnectionRefused) => {
                write!(f, "device refused the connection (is the app listening on that port?)")
            }
            Error::Result(code) => write!(f, "usbmuxd returned {code:?}"),
            Error::UnsupportedAddress(addr) => write!(f, "unsupported usbmuxd address {addr:?}; expected host:port"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<plist::Error> for Error {
    fn from(e: plist::Error) -> Self {
        Error::Plist(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionType {
    Usb,
    /// Wi-Fi sync; usbmuxd can tunnel to these too.
    Network,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub device_id: u64,
    /// The device UDID.
    pub serial: String,
    pub connection: ConnectionType,
    pub product_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    Attached(Device),
    Detached { device_id: u64 },
}

/// Handle to a usbmuxd daemon. Every request opens its own socket, as
/// `Connect` permanently turns its socket into a device tunnel.
#[derive(Debug, Clone)]
pub struct Usbmux {
    addr: String,
}

impl Default for Usbmux {
    fn default() -> Self {
        Self::new()
    }
}

impl Usbmux {
    /// Uses `$USBMUXD_SOCKET_ADDRESS` if set, otherwise [`DEFAULT_ADDR`].
    pub fn new() -> Self {
        Self::with_addr(std::env::var(ADDR_ENV).unwrap_or_else(|_| DEFAULT_ADDR.into()))
    }

    pub fn with_addr(addr: impl Into<String>) -> Self {
        Self { addr: addr.into() }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn list_devices(&self) -> Result<Vec<Device>> {
        let mut conn = self.open()?;
        let reply = conn.request("ListDevices", Dictionary::new())?;
        let list = reply
            .get("DeviceList")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Protocol("ListDevices reply has no DeviceList".into()))?;
        list.iter()
            .map(|v| v.as_dictionary().ok_or_else(|| Error::Protocol("DeviceList entry is not a dict".into())))
            .map(|d| d.and_then(parse_device))
            .collect()
    }

    /// Opens a TCP tunnel to `port` on the device. The device-side app must be
    /// listening on that port, otherwise this fails with `ConnectionRefused`.
    pub fn connect(&self, device_id: u64, port: u16) -> Result<TcpStream> {
        let mut conn = self.open()?;
        let mut args = Dictionary::new();
        args.insert("DeviceID".into(), Value::Integer(device_id.into()));
        // usbmuxd expects the port in network byte order, i.e. htons(port).
        args.insert("PortNumber".into(), Value::Integer(u64::from(port.to_be()).into()));
        let reply = conn.request("Connect", args)?;
        check_result(&reply)?;
        Ok(conn.stream)
    }

    /// Subscribes to attach/detach notifications. Devices already connected
    /// are reported as `Attached` first.
    pub fn listen(&self) -> Result<impl Iterator<Item = Result<DeviceEvent>>> {
        let mut conn = self.open()?;
        let reply = conn.request("Listen", Dictionary::new())?;
        check_result(&reply)?;
        Ok(std::iter::from_fn(move || {
            loop {
                let msg = match conn.read_message() {
                    Ok(msg) => msg,
                    Err(Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return None,
                    Err(e) => return Some(Err(e)),
                };
                let event = match msg.get("MessageType").and_then(Value::as_string) {
                    Some("Attached") => parse_device(&msg).map(DeviceEvent::Attached),
                    Some("Detached") => device_id(&msg).map(|device_id| DeviceEvent::Detached { device_id }),
                    _ => continue, // e.g. "Paired"
                };
                return Some(event);
            }
        }))
    }

    fn open(&self) -> Result<Conn> {
        if self.addr.starts_with("UNIX:") || !self.addr.contains(':') {
            return Err(Error::UnsupportedAddress(self.addr.clone()));
        }
        let stream = TcpStream::connect(&self.addr)?;
        stream.set_nodelay(true)?;
        Ok(Conn { stream, tag: 0 })
    }
}

struct Conn {
    stream: TcpStream,
    tag: u32,
}

impl Conn {
    fn request(&mut self, message_type: &str, mut args: Dictionary) -> Result<Dictionary> {
        args.insert("MessageType".into(), message_type.into());
        args.insert("ClientVersionString".into(), "padbridge".into());
        args.insert("ProgName".into(), "padbridge".into());
        args.insert("kLibUSBMuxVersion".into(), Value::Integer(3.into()));
        self.tag += 1;
        write_message(&mut self.stream, self.tag, &args)?;
        self.read_message()
    }

    fn read_message(&mut self) -> Result<Dictionary> {
        read_message(&mut self.stream).map(|(_, dict)| dict)
    }
}

fn write_message(w: &mut impl Write, tag: u32, dict: &Dictionary) -> Result<()> {
    let mut body = Vec::new();
    Value::Dictionary(dict.clone()).to_writer_xml(&mut body)?;
    let mut frame = Vec::with_capacity(HEADER_LEN + body.len());
    for field in [(HEADER_LEN + body.len()) as u32, PLIST_VERSION, PLIST_MESSAGE, tag] {
        frame.extend_from_slice(&field.to_le_bytes());
    }
    frame.extend_from_slice(&body);
    w.write_all(&frame)?;
    Ok(())
}

fn read_message(r: &mut impl Read) -> Result<(u32, Dictionary)> {
    let mut header = [0u8; HEADER_LEN];
    r.read_exact(&mut header)?;
    let field = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    let (len, version, message, tag) = (field(0) as usize, field(1), field(2), field(3));
    if !(HEADER_LEN..=MAX_MESSAGE_LEN).contains(&len) {
        return Err(Error::Protocol(format!("bad message length {len}")));
    }
    if version != PLIST_VERSION || message != PLIST_MESSAGE {
        return Err(Error::Protocol(format!("unsupported message version {version} type {message}")));
    }
    let mut body = vec![0u8; len - HEADER_LEN];
    r.read_exact(&mut body)?;
    let dict = Value::from_reader(Cursor::new(body))?
        .into_dictionary()
        .ok_or_else(|| Error::Protocol("message is not a dict".into()))?;
    Ok((tag, dict))
}

fn check_result(reply: &Dictionary) -> Result<()> {
    if reply.get("MessageType").and_then(Value::as_string) != Some("Result") {
        return Err(Error::Protocol(format!("expected Result, got {reply:?}")));
    }
    match reply.get("Number").and_then(Value::as_unsigned_integer) {
        Some(0) => Ok(()),
        Some(code) => Err(Error::Result(code.into())),
        None => Err(Error::Protocol("Result has no Number".into())),
    }
}

fn device_id(dict: &Dictionary) -> Result<u64> {
    dict.get("DeviceID")
        .and_then(Value::as_unsigned_integer)
        .ok_or_else(|| Error::Protocol("missing DeviceID".into()))
}

fn parse_device(dict: &Dictionary) -> Result<Device> {
    let props = dict
        .get("Properties")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| Error::Protocol("device entry has no Properties".into()))?;
    let connection = match props.get("ConnectionType").and_then(Value::as_string) {
        Some("USB") => ConnectionType::Usb,
        Some("Network") => ConnectionType::Network,
        other => ConnectionType::Other(other.unwrap_or_default().into()),
    };
    Ok(Device {
        device_id: device_id(dict)?,
        serial: props.get("SerialNumber").and_then(Value::as_string).unwrap_or_default().into(),
        connection,
        product_id: props.get("ProductID").and_then(Value::as_unsigned_integer),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn attached(id: u64, serial: &str, conn: &str) -> Value {
        let mut props = Dictionary::new();
        props.insert("ConnectionType".into(), conn.into());
        props.insert("DeviceID".into(), Value::Integer(id.into()));
        props.insert("ProductID".into(), Value::Integer(4776u64.into()));
        props.insert("SerialNumber".into(), serial.into());
        let mut d = Dictionary::new();
        d.insert("MessageType".into(), "Attached".into());
        d.insert("DeviceID".into(), Value::Integer(id.into()));
        d.insert("Properties".into(), Value::Dictionary(props));
        Value::Dictionary(d)
    }

    fn result(code: u64) -> Dictionary {
        let mut d = Dictionary::new();
        d.insert("MessageType".into(), "Result".into());
        d.insert("Number".into(), Value::Integer(code.into()));
        d
    }

    /// Serves one request per accepted socket, like the real daemon.
    fn fake_usbmuxd(handle: impl Fn(Dictionary, &mut TcpStream) + Send + 'static, sockets: usize) -> Usbmux {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            for _ in 0..sockets {
                let (mut s, _) = listener.accept().unwrap();
                let (_, req) = read_message(&mut s).unwrap();
                assert_eq!(req.get("kLibUSBMuxVersion").and_then(Value::as_unsigned_integer), Some(3));
                handle(req, &mut s);
            }
        });
        Usbmux::with_addr(addr)
    }

    #[test]
    fn message_roundtrip() {
        let mut buf = Vec::new();
        write_message(&mut buf, 7, &result(0)).unwrap();
        assert_eq!(&buf[4..16], &[1, 0, 0, 0, 8, 0, 0, 0, 7, 0, 0, 0]);
        assert_eq!(u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize, buf.len());
        let (tag, dict) = read_message(&mut Cursor::new(buf)).unwrap();
        assert_eq!((tag, dict), (7, result(0)));
    }

    #[test]
    fn list_devices_parses_entries() {
        let mux = fake_usbmuxd(
            |req, s| {
                assert_eq!(req.get("MessageType").and_then(Value::as_string), Some("ListDevices"));
                let mut reply = Dictionary::new();
                reply.insert(
                    "DeviceList".into(),
                    Value::Array(vec![attached(3, "00008103-AAAA", "USB"), attached(9, "00008103-BBBB", "Network")]),
                );
                write_message(s, 1, &reply).unwrap();
            },
            1,
        );
        let devices = mux.list_devices().unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(
            devices[0],
            Device { device_id: 3, serial: "00008103-AAAA".into(), connection: ConnectionType::Usb, product_id: Some(4776) }
        );
        assert_eq!(devices[1].connection, ConnectionType::Network);
    }

    #[test]
    fn connect_swaps_port_and_becomes_tunnel() {
        let mux = fake_usbmuxd(
            |req, s| {
                assert_eq!(req.get("MessageType").and_then(Value::as_string), Some("Connect"));
                assert_eq!(req.get("DeviceID").and_then(Value::as_unsigned_integer), Some(3));
                // 47800 = 0xBAB8 -> htons -> 0xB8BA
                assert_eq!(req.get("PortNumber").and_then(Value::as_unsigned_integer), Some(0xB8BA));
                write_message(s, 1, &result(0)).unwrap();
                let mut ping = [0u8; 4];
                s.read_exact(&mut ping).unwrap();
                s.write_all(&ping).unwrap();
            },
            1,
        );
        let mut tunnel = mux.connect(3, 47800).unwrap();
        tunnel.write_all(b"ping").unwrap();
        let mut echo = [0u8; 4];
        tunnel.read_exact(&mut echo).unwrap();
        assert_eq!(&echo, b"ping");
    }

    #[test]
    fn connect_refused_is_reported() {
        let mux = fake_usbmuxd(|_, s| write_message(s, 1, &result(3)).unwrap(), 1);
        assert!(matches!(mux.connect(3, 47800), Err(Error::Result(ResultCode::ConnectionRefused))));
    }

    #[test]
    fn listen_yields_attach_and_detach() {
        let mux = fake_usbmuxd(
            |_, s| {
                write_message(s, 1, &result(0)).unwrap();
                write_message(s, 0, attached(5, "UDID", "USB").as_dictionary().unwrap()).unwrap();
                let mut paired = Dictionary::new();
                paired.insert("MessageType".into(), "Paired".into());
                paired.insert("DeviceID".into(), Value::Integer(5u64.into()));
                write_message(s, 0, &paired).unwrap();
                let mut detached = Dictionary::new();
                detached.insert("MessageType".into(), "Detached".into());
                detached.insert("DeviceID".into(), Value::Integer(5u64.into()));
                write_message(s, 0, &detached).unwrap();
            },
            1,
        );
        let events: Vec<_> = mux.listen().unwrap().collect::<Result<_>>().unwrap();
        assert!(matches!(&events[0], DeviceEvent::Attached(d) if d.device_id == 5));
        assert_eq!(events[1], DeviceEvent::Detached { device_id: 5 });
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn unix_address_is_rejected() {
        assert!(matches!(
            Usbmux::with_addr("UNIX:/var/run/usbmuxd").list_devices(),
            Err(Error::UnsupportedAddress(_))
        ));
    }
}
