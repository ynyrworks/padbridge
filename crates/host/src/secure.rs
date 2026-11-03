//! Encrypted, mutually authenticated channel between the host and the iPad.
//!
//! Every connection (USB or Wi‑Fi) starts with this handshake before any
//! protocol message. Each side has a long-term X25519 identity key; the iPad
//! only talks to hosts it has paired with, and the host remembers each
//! iPad's key. Pairing a new device is confirmed by the user on **both**
//! screens after checking that the same 6-digit code is shown.
//!
//! ```text
//! hello (both, plain):  "PBS1" · static public key (32) · ephemeral public key (32)
//! transcript h  = SHA-256("padbridge-secure-v1" · S_host · E_host · S_ipad · E_ipad)
//! secret        = DH(e_host, E_ipad) · DH(s_host, E_ipad) · DH(e_host, S_ipad)
//! keys          = HKDF-SHA256(salt h, secret, "padbridge-secure-v1 keys") → host→iPad (32) · iPad→host (32)
//! code          = SHA-256(h · "padbridge-secure-v1 code")[0..4] as u32 BE mod 10^6
//! record        = length u32 LE · ChaCha20-Poly1305(key, nonce = 0u32 · counter u64 LE, plaintext)
//! ```
//!
//! The first record each way is an intro (`0x01` · name, u16 LE length);
//! the iPad then sends a verdict (`0x02` · status). After that, records carry
//! the ordinary protocol frames as a byte stream.
//!
//! Only proving the static keys (the two mixed-in static DH terms) lets a
//! side derive the keys, so a known key can't be impersonated; a new key is
//! accepted only after both users confirm the code, which defeats a
//! man-in-the-middle unless they guess it (1 in a million).

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub const MAGIC: [u8; 4] = *b"PBS1";
const LABEL: &[u8] = b"padbridge-secure-v1";
const HELLO_LEN: usize = 68;
/// Large enough for the biggest protocol frame plus the AEAD tag.
const MAX_RECORD: usize = 16 * 1024 * 1024 + 64;
const TAG_LEN: usize = 16;
const INTRO: u8 = 0x01;
const VERDICT: u8 = 0x02;

/// A long-term X25519 key pair.
#[derive(Clone)]
pub struct Identity {
    secret: StaticSecret,
}

impl Identity {
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self::from_bytes(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { secret: StaticSecret::from(bytes) }
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    pub fn public_key(&self) -> [u8; 32] {
        PublicKey::from(&self.secret).to_bytes()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Identity({})", hex(&self.public_key()))
    }
}

/// A device asking to pair, shown to the user for confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRequest {
    pub peer_name: String,
    pub peer_key: [u8; 32],
    /// Six digits; must match what the other screen shows.
    pub code: String,
}

/// The iPad's answer to a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted = 0,
    /// The iPad's user declined.
    Rejected = 1,
    /// This port can't pair (the broadcast extension); pair in the app first.
    NotPaired = 2,
    /// The iPad asked its user to approve (the broadcast extension, which
    /// has no UI, ends the broadcast with the code; restarting it approves).
    /// Try again shortly.
    PendingApproval = 3,
}

#[derive(Debug)]
pub enum SecureError {
    Io(io::Error),
    /// The peer doesn't speak this handshake (e.g. an older PadBridge).
    BadMagic,
    /// A key that would make the shared secret predictable.
    WeakKey,
    /// Garbled or forged record.
    Decrypt,
    Malformed(&'static str),
    /// Our user declined to pair.
    DeclinedHere,
    /// The iPad refused.
    Refused(Verdict),
    /// We confirmed this new iPad; it is waiting for its own user's approval.
    /// The caller should remember the key and retry.
    AwaitingApproval { peer_key: [u8; 32], peer_name: String },
}

impl fmt::Display for SecureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecureError::Io(e) => write!(f, "{e}"),
            SecureError::BadMagic => f.write_str(&tr!(
                "對方不支援加密連線（請把 iPad 與 Windows 端都更新到同一版）",
                "The other side does not support encrypted connections (update the iPad and Windows apps to the same version)"
            )),
            SecureError::WeakKey => f.write_str(&tr!("對方的金鑰無效", "The other side sent an invalid key")),
            SecureError::Decrypt => f.write_str(&tr!("加密資料驗證失敗", "Encrypted data failed verification")),
            SecureError::Malformed(what) => f.write_str(&tr!("握手資料格式錯誤：{what}", "Malformed handshake: {what}")),
            SecureError::DeclinedHere => f.write_str(&tr!("已取消配對", "Pairing cancelled")),
            SecureError::Refused(Verdict::Rejected) => f.write_str(&tr!("iPad 拒絕了配對", "The iPad declined pairing")),
            SecureError::Refused(Verdict::NotPaired) => {
                f.write_str(&tr!(
                    "這台電腦尚未與 iPad 配對：請先在 iPad 開啟 PadBridge App，從 Windows 按「延伸螢幕」或「測試連線」完成配對",
                    "This computer is not paired with the iPad yet: open the PadBridge app on the iPad, then pair by choosing Extend or Test connection on Windows"
                ))
            }
            SecureError::Refused(Verdict::PendingApproval) | SecureError::AwaitingApproval { .. } => f.write_str(&tr!(
                "請在 iPad 上確認：廣播已暫停並顯示確認碼，數字相同就在 60 秒內重新開始廣播",
                "Confirm on the iPad: the broadcast stopped and shows the code; if it matches, start the broadcast again within 60 seconds"
            )),
            SecureError::Refused(Verdict::Accepted) => write!(f, "unexpected"),
        }
    }
}

impl std::error::Error for SecureError {}

impl From<io::Error> for SecureError {
    fn from(e: io::Error) -> Self {
        SecureError::Io(e)
    }
}

/// An established channel: protocol frames go through `writer` and come out
/// of `reader`.
pub struct SecureLink {
    pub reader: SecureReader,
    pub writer: SecureWriter,
    pub peer_key: [u8; 32],
    pub peer_name: String,
    /// The peer was paired during this handshake.
    pub newly_paired: bool,
    socket: TcpStream,
}

impl SecureLink {
    /// A handle that can end the connection from another thread.
    pub fn shutdown_handle(&self) -> io::Result<TcpStream> {
        self.socket.try_clone()
    }

    pub fn shutdown(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

/// Session keys and the handshake transcript hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKeys {
    pub host_to_ipad: [u8; 32],
    pub ipad_to_host: [u8; 32],
    pub transcript: [u8; 32],
}

impl SessionKeys {
    pub fn code(&self) -> String {
        pairing_code(&self.transcript)
    }
}

fn hello_bytes(identity: &Identity, ephemeral: &StaticSecret) -> [u8; HELLO_LEN] {
    let mut out = [0u8; HELLO_LEN];
    out[..4].copy_from_slice(&MAGIC);
    out[4..36].copy_from_slice(&identity.public_key());
    out[36..].copy_from_slice(PublicKey::from(ephemeral).as_bytes());
    out
}

fn parse_hello(bytes: &[u8; HELLO_LEN]) -> Result<([u8; 32], [u8; 32]), SecureError> {
    if bytes[..4] != MAGIC {
        return Err(SecureError::BadMagic);
    }
    Ok((bytes[4..36].try_into().unwrap(), bytes[36..].try_into().unwrap()))
}

fn dh(secret: &StaticSecret, public: &[u8; 32]) -> Result<[u8; 32], SecureError> {
    let shared = secret.diffie_hellman(&PublicKey::from(*public));
    if !shared.was_contributory() {
        return Err(SecureError::WeakKey);
    }
    Ok(shared.to_bytes())
}

/// Derives the session from both hellos. `host` is the side that connected.
/// Public so both ends (and the Swift implementation's test vectors) share it.
pub fn derive_keys(
    host: bool,
    me: &Identity,
    my_ephemeral: &StaticSecret,
    peer_static: &[u8; 32],
    peer_ephemeral: &[u8; 32],
) -> Result<SessionKeys, SecureError> {
    let (my_static_pub, my_eph_pub) = (me.public_key(), PublicKey::from(my_ephemeral).to_bytes());
    let (hs, he, is, ie) = if host {
        (my_static_pub, my_eph_pub, *peer_static, *peer_ephemeral)
    } else {
        (*peer_static, *peer_ephemeral, my_static_pub, my_eph_pub)
    };
    // ee · (host static, iPad ephemeral) · (host ephemeral, iPad static)
    let terms = if host {
        [dh(my_ephemeral, &ie)?, dh(&me.secret, &ie)?, dh(my_ephemeral, &is)?]
    } else {
        [dh(my_ephemeral, &he)?, dh(my_ephemeral, &hs)?, dh(&me.secret, &he)?]
    };
    let transcript: [u8; 32] = Sha256::new()
        .chain_update(LABEL)
        .chain_update(hs)
        .chain_update(he)
        .chain_update(is)
        .chain_update(ie)
        .finalize()
        .into();
    let mut okm = [0u8; 64];
    Hkdf::<Sha256>::new(Some(&transcript), &terms.concat())
        .expand(b"padbridge-secure-v1 keys", &mut okm)
        .expect("64 bytes is a valid HKDF length");
    Ok(SessionKeys {
        host_to_ipad: okm[..32].try_into().unwrap(),
        ipad_to_host: okm[32..].try_into().unwrap(),
        transcript,
    })
}

/// The 6-digit code both screens show while pairing.
pub fn pairing_code(transcript: &[u8; 32]) -> String {
    let digest = Sha256::new().chain_update(transcript).chain_update(b"padbridge-secure-v1 code").finalize();
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{n:06}")
}

fn nonce(counter: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&counter.to_le_bytes());
    Nonce::from(bytes)
}

/// Encrypts one record (length prefix included). Public for test vectors.
pub fn seal_record(key: &[u8; 32], counter: u64, plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let sealed = cipher.encrypt(&nonce(counter), plaintext).expect("ChaCha20-Poly1305 encryption cannot fail");
    let mut out = Vec::with_capacity(4 + sealed.len());
    out.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
    out.extend_from_slice(&sealed);
    out
}

/// Writing half: each `write` call becomes one encrypted record, so writing a
/// whole protocol frame with `write_all` keeps frames and records aligned.
pub struct SecureWriter {
    socket: TcpStream,
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl SecureWriter {
    fn new(socket: TcpStream, key: &[u8; 32]) -> Self {
        Self { socket, cipher: ChaCha20Poly1305::new(Key::from_slice(key)), counter: 0 }
    }

    fn write_record(&mut self, plaintext: &[u8]) -> io::Result<()> {
        let sealed = self
            .cipher
            .encrypt(&nonce(self.counter), plaintext)
            .map_err(|_| io::Error::other("encryption failed"))?;
        self.counter += 1;
        let mut out = Vec::with_capacity(4 + sealed.len());
        out.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
        out.extend_from_slice(&sealed);
        self.socket.write_all(&out)
    }
}

impl Write for SecureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(MAX_RECORD - TAG_LEN);
        self.write_record(&buf[..n])?;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

/// Reading half: decrypts records and hands out their bytes as a stream.
pub struct SecureReader {
    socket: TcpStream,
    cipher: ChaCha20Poly1305,
    counter: u64,
    plain: Vec<u8>,
    position: usize,
}

impl SecureReader {
    fn new(socket: TcpStream, key: &[u8; 32]) -> Self {
        Self { socket, cipher: ChaCha20Poly1305::new(Key::from_slice(key)), counter: 0, plain: Vec::new(), position: 0 }
    }

    /// The next record, or `None` at a clean end of stream.
    fn read_record(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut length = [0u8; 4];
        match self.socket.read_exact(&mut length) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let length = u32::from_le_bytes(length) as usize;
        if !(TAG_LEN..=MAX_RECORD).contains(&length) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad record length"));
        }
        let mut sealed = vec![0u8; length];
        self.socket.read_exact(&mut sealed)?;
        let plain = self
            .cipher
            .decrypt(&nonce(self.counter), sealed.as_slice())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record failed authentication"))?;
        self.counter += 1;
        Ok(Some(plain))
    }
}

impl Read for SecureReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.position == self.plain.len() {
            match self.read_record()? {
                Some(plain) => {
                    self.plain = plain;
                    self.position = 0;
                }
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.plain.len() - self.position);
        buf[..n].copy_from_slice(&self.plain[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}

fn intro_record(name: &str) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(1024)];
    let mut out = vec![INTRO];
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(name);
    out
}

fn parse_intro(record: &[u8]) -> Result<String, SecureError> {
    let [INTRO, a, b, name @ ..] = record else { return Err(SecureError::Malformed("intro")) };
    if name.len() != u16::from_le_bytes([*a, *b]) as usize {
        return Err(SecureError::Malformed("intro length"));
    }
    String::from_utf8(name.to_vec()).map_err(|_| SecureError::Malformed("intro name"))
}

fn expect_record(reader: &mut SecureReader) -> Result<Vec<u8>, SecureError> {
    match reader.read_record() {
        Ok(Some(record)) => Ok(record),
        Ok(None) => Err(SecureError::Io(io::ErrorKind::UnexpectedEof.into())),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Err(SecureError::Decrypt),
        Err(e) => Err(e.into()),
    }
}

struct Handshake {
    keys: SessionKeys,
    peer_key: [u8; 32],
    reader: SecureReader,
    writer: SecureWriter,
    socket: TcpStream,
}

fn handshake(stream: TcpStream, me: &Identity, host: bool) -> Result<Handshake, SecureError> {
    let ephemeral = Identity::generate()?.secret;
    let mut socket = stream;
    socket.set_nodelay(true)?;
    socket.write_all(&hello_bytes(me, &ephemeral))?;
    let mut peer = [0u8; HELLO_LEN];
    socket.read_exact(&mut peer)?;
    let (peer_key, peer_ephemeral) = parse_hello(&peer)?;
    let keys = derive_keys(host, me, &ephemeral, &peer_key, &peer_ephemeral)?;
    let (send, receive) = if host { (keys.host_to_ipad, keys.ipad_to_host) } else { (keys.ipad_to_host, keys.host_to_ipad) };
    let writer = SecureWriter::new(socket.try_clone()?, &send);
    let reader = SecureReader::new(socket.try_clone()?, &receive);
    Ok(Handshake { keys, peer_key, reader, writer, socket })
}

/// Host side. `is_known` says whether an iPad key is already paired;
/// `confirm` asks our user about a new one (it may block while they decide).
pub fn connect_as_host(
    stream: TcpStream,
    me: &Identity,
    my_name: &str,
    is_known: impl FnOnce(&[u8; 32]) -> bool,
    confirm: impl FnOnce(&PairingRequest) -> bool,
) -> Result<SecureLink, SecureError> {
    let Handshake { keys, peer_key, mut reader, mut writer, socket } = handshake(stream, me, true)?;
    writer.write_record(&intro_record(my_name))?;
    let peer_name = parse_intro(&expect_record(&mut reader)?)?;
    let known = is_known(&peer_key);
    if !known {
        let request = PairingRequest { peer_name: peer_name.clone(), peer_key, code: keys.code() };
        if !confirm(&request) {
            let _ = socket.shutdown(Shutdown::Both);
            return Err(SecureError::DeclinedHere);
        }
    }
    match expect_record(&mut reader)?.as_slice() {
        [VERDICT, 0] => {}
        [VERDICT, 1] => return Err(SecureError::Refused(Verdict::Rejected)),
        [VERDICT, 2] => return Err(SecureError::Refused(Verdict::NotPaired)),
        [VERDICT, 3] if !known => return Err(SecureError::AwaitingApproval { peer_key, peer_name }),
        [VERDICT, 3] => return Err(SecureError::Refused(Verdict::PendingApproval)),
        _ => return Err(SecureError::Malformed("verdict")),
    }
    Ok(SecureLink { reader, writer, peer_key, peer_name, newly_paired: !known, socket })
}

/// iPad side (used by tests and the fake iPads). `approve` decides about an
/// unpaired host: `Accepted` / `Rejected` from the user, `NotPaired` if this
/// endpoint can't pair, `PendingApproval` to have the host retry later.
pub fn accept_as_device(
    stream: TcpStream,
    me: &Identity,
    my_name: &str,
    is_paired: impl FnOnce(&[u8; 32]) -> bool,
    approve: impl FnOnce(&PairingRequest) -> Verdict,
) -> Result<SecureLink, SecureError> {
    let Handshake { keys, peer_key, mut reader, mut writer, socket } = handshake(stream, me, false)?;
    writer.write_record(&intro_record(my_name))?;
    let peer_name = parse_intro(&expect_record(&mut reader)?)?;
    let known = is_paired(&peer_key);
    let verdict = if known {
        Verdict::Accepted
    } else {
        let request = PairingRequest { peer_name: peer_name.clone(), peer_key, code: keys.code() };
        approve(&request)
    };
    writer.write_record(&[VERDICT, verdict as u8])?;
    if verdict != Verdict::Accepted {
        let _ = socket.shutdown(Shutdown::Write);
        return Err(SecureError::Refused(verdict));
    }
    Ok(SecureLink { reader, writer, peer_key, peer_name, newly_paired: !known, socket })
}

/// Short public fingerprint the iPad advertises over Bonjour (`id` in the
/// TXT record), so its Wi‑Fi service can be matched to a USB connection.
pub fn device_id(public_key: &[u8; 32]) -> String {
    hex(&public_key[..8])
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (client, listener.accept().unwrap().0)
    }

    /// Fixed keys shared with the Swift tests (`SecureTests.swift`): both
    /// implementations must produce exactly these bytes.
    #[test]
    fn test_vectors() {
        let host = Identity::from_bytes([1; 32]);
        let host_eph = StaticSecret::from([2; 32]);
        let ipad = Identity::from_bytes([3; 32]);
        let ipad_eph = StaticSecret::from([4; 32]);
        let ipad_eph_pub = PublicKey::from(&ipad_eph).to_bytes();
        let host_eph_pub = PublicKey::from(&host_eph).to_bytes();

        let host_side = derive_keys(true, &host, &host_eph, &ipad.public_key(), &ipad_eph_pub).unwrap();
        let ipad_side = derive_keys(false, &ipad, &ipad_eph, &host.public_key(), &host_eph_pub).unwrap();
        assert_eq!(host_side, ipad_side);
        assert_eq!(hex(&host.public_key()), "a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209");
        assert_eq!(hex(&host_eph_pub), "ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59");
        assert_eq!(hex(&ipad.public_key()), "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22");
        assert_eq!(hex(&ipad_eph_pub), "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b");
        assert_eq!(hex(&host_side.transcript), "3da1af07a054eb739bfaa9980de5c55f2789df5a5c163ebd8a35887ff35a06a5");
        assert_eq!(hex(&host_side.host_to_ipad), "e426ab3631da972e309f56211fbc829f7b22d8e80f61249c401c81163794f7da");
        assert_eq!(hex(&host_side.ipad_to_host), "561d707dae8bab0d15e86f575311cc119e916324c8d4dd8f814c4427dc8c3f94");
        assert_eq!(host_side.code(), "785065");
        assert_eq!(
            hex(&seal_record(&host_side.host_to_ipad, 0, &intro_record("PC"))),
            "150000001a1784824de9225753fbe3822c86c3cb06cab6c838"
        );
        assert_eq!(
            hex(&seal_record(&host_side.ipad_to_host, 5, b"PadBridge")),
            "190000003aa9d7fd74e52fe5ae8c57c0cb53e5375e4f0d42a04414e28b"
        );
    }

    #[test]
    fn pairs_then_reconnects_without_asking() {
        let host = Identity::generate().unwrap();
        let ipad = Identity::generate().unwrap();
        let host_key = host.public_key();

        let (a, b) = pair();
        let device = {
            let ipad = ipad.clone();
            thread::spawn(move || {
                let mut link =
                    accept_as_device(b, &ipad, "iPad", |_| false, |request| if request.peer_name == "PC" { Verdict::Accepted } else { Verdict::Rejected }).unwrap();
                let mut buf = [0u8; 5];
                link.reader.read_exact(&mut buf).unwrap();
                link.writer.write_all(&buf).unwrap();
                (link.peer_key, link.newly_paired)
            })
        };
        let mut shown_code = String::new();
        let mut link = connect_as_host(a, &host, "PC", |_| false, |request| {
            shown_code = request.code.clone();
            true
        })
        .unwrap();
        assert!(link.newly_paired);
        assert_eq!(link.peer_name, "iPad");
        assert_eq!(shown_code.len(), 6);
        link.writer.write_all(b"hello").unwrap();
        let mut echo = [0u8; 5];
        link.reader.read_exact(&mut echo).unwrap();
        assert_eq!(&echo, b"hello");
        assert_eq!(device.join().unwrap(), (host_key, true));

        // Second time both sides know each other: no prompts.
        let (a, b) = pair();
        let device = thread::spawn(move || {
            accept_as_device(b, &ipad, "iPad", |key| *key == host_key, |_| panic!("asked again")).map(|l| l.newly_paired)
        });
        let ipad_key = connect_as_host(a, &host, "PC", |_| true, |_| panic!("asked again")).unwrap().peer_key;
        assert!(!device.join().unwrap().unwrap());
        assert_ne!(ipad_key, host_key);
    }

    #[test]
    fn unpaired_host_is_refused_where_pairing_is_impossible() {
        let (a, b) = pair();
        let device = thread::spawn(move || {
            let ipad = Identity::generate().unwrap();
            accept_as_device(b, &ipad, "iPad Mirror", |_| false, |_| Verdict::NotPaired).err()
        });
        let host = Identity::generate().unwrap();
        let error = connect_as_host(a, &host, "PC", |_| false, |_| true).err().unwrap();
        assert!(matches!(error, SecureError::Refused(Verdict::NotPaired)), "{error}");
        assert!(matches!(device.join().unwrap(), Some(SecureError::Refused(Verdict::NotPaired))));
    }

    #[test]
    fn pending_approval_then_retry_gets_in() {
        let ipad = Identity::generate().unwrap();
        let host = Identity::generate().unwrap();
        let host_key = host.public_key();

        // First try: the iPad (broadcast extension) asks its user and defers.
        let (a, b) = pair();
        let first = {
            let ipad = ipad.clone();
            thread::spawn(move || accept_as_device(b, &ipad, "iPad Mirror", |_| false, |_| Verdict::PendingApproval).err())
        };
        let error = connect_as_host(a, &host, "PC", |_| false, |_| true).err().unwrap();
        assert!(matches!(error, SecureError::AwaitingApproval { .. }), "{error}");
        assert!(matches!(first.join().unwrap(), Some(SecureError::Refused(Verdict::PendingApproval))));

        // After the user restarted the broadcast, the extension knows the host.
        let (a, b) = pair();
        let second = thread::spawn(move || {
            accept_as_device(b, &ipad, "iPad Mirror", |key| *key == host_key, |_| panic!("asked again")).map(|_| ())
        });
        // The host remembered the iPad after its own confirmation, so no prompt either.
        connect_as_host(a, &host, "PC", |_| true, |_| panic!("asked again")).unwrap();
        second.join().unwrap().unwrap();
    }

    #[test]
    fn tampered_record_is_rejected() {
        let (a, b) = pair();
        let ipad = Identity::generate().unwrap();
        let device = thread::spawn(move || {
            let mut link = accept_as_device(b, &ipad, "iPad", |_| false, |_| Verdict::Accepted).unwrap();
            let mut buf = [0u8; 4];
            link.reader.read_exact(&mut buf).map(|()| buf)
        });
        let host = Identity::generate().unwrap();
        let link = connect_as_host(a, &host, "PC", |_| false, |_| true).unwrap();
        // A record injected by someone without the session key must not decrypt.
        let mut forged = seal_record(&[9; 32], 0, b"evil");
        forged[6] ^= 1;
        link.shutdown_handle().unwrap().write_all(&forged).unwrap();
        let error = device.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn declining_locally_closes_the_connection() {
        let (a, b) = pair();
        let device = thread::spawn(move || {
            let ipad = Identity::generate().unwrap();
            accept_as_device(b, &ipad, "iPad", |_| false, |_| Verdict::Accepted).map(|_| ())
        });
        let host = Identity::generate().unwrap();
        assert!(matches!(connect_as_host(a, &host, "PC", |_| false, |_| false), Err(SecureError::DeclinedHere)));
        // The iPad's verdict write may or may not land before the reset; either way no session.
        let _ = device.join().unwrap();
    }

    #[test]
    fn device_id_matches_swift() {
        // Same as `ServerTests.testDeviceIDMatchesRust`.
        let key: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
        assert_eq!(device_id(&key), "0102030405060708");
    }

    #[test]
    fn hex_round_trip() {
        let key = [0xAB; 32];
        assert_eq!(parse_hex32(&hex(&key)), Some(key));
        assert_eq!(parse_hex32("zz"), None);
    }
}
