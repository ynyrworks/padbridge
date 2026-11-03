//! This computer's identity key and the iPads it has paired with, kept in
//! `%APPDATA%\PadBridge`. The private key is encrypted with DPAPI, so only
//! the same Windows user can read it.

use std::fs;
use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::secure::{connect_as_host, hex, Identity, PairingRequest, SecureError, SecureLink};

/// Where PadBridge keeps its files.
pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA").map_or_else(|| PathBuf::from(".padbridge"), |dir| PathBuf::from(dir).join("PadBridge"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    /// The iPad's public key, hex.
    pub key: String,
    pub name: String,
    /// Seconds since the Unix epoch.
    pub paired_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PairedFile {
    devices: Vec<PairedDevice>,
}

/// The host's side of pairing: who we are and which iPads we trust.
pub struct HostSecurity {
    identity: Identity,
    devices: Mutex<Vec<PairedDevice>>,
    dir: PathBuf,
}

impl HostSecurity {
    /// Loads (or creates) the identity and paired list in [`data_dir`].
    pub fn load() -> io::Result<Self> {
        Self::load_from(&data_dir())
    }

    pub fn load_from(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let key_path = dir.join("identity.key");
        let identity = match fs::read(&key_path) {
            Ok(sealed) => {
                let bytes = unprotect(&sealed)?;
                Identity::from_bytes(bytes.try_into().map_err(|_| io::Error::other("identity.key is damaged"))?)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let identity = Identity::generate()?;
                fs::write(&key_path, protect(&identity.to_bytes())?)?;
                identity
            }
            Err(e) => return Err(e),
        };
        let devices = match fs::read(dir.join("paired.json")) {
            Ok(json) => serde_json::from_slice::<PairedFile>(&json).map_err(io::Error::other)?.devices,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { identity, devices: Mutex::new(devices), dir: dir.to_path_buf() })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public_key()
    }

    pub fn devices(&self) -> Vec<PairedDevice> {
        self.devices.lock().unwrap().clone()
    }

    pub fn is_paired(&self, key: &[u8; 32]) -> bool {
        let key = hex(key);
        self.devices.lock().unwrap().iter().any(|d| d.key == key)
    }

    /// Forgets an iPad; it will have to pair again.
    pub fn forget(&self, key: &str) -> io::Result<()> {
        let mut devices = self.devices.lock().unwrap();
        devices.retain(|d| d.key != key);
        self.save(&devices)
    }

    fn remember(&self, key: &[u8; 32], name: &str) -> io::Result<()> {
        let mut devices = self.devices.lock().unwrap();
        let key = hex(key);
        devices.retain(|d| d.key != key);
        let paired_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        devices.push(PairedDevice { key, name: name.to_string(), paired_at });
        self.save(&devices)
    }

    fn save(&self, devices: &[PairedDevice]) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(&PairedFile { devices: devices.to_vec() }).map_err(io::Error::other)?;
        let path = self.dir.join("paired.json");
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, json)?;
        fs::rename(temp, path)
    }

    /// Runs the secure handshake on a fresh connection to an iPad. `confirm`
    /// asks the user about an iPad we haven't paired with.
    pub fn connect(
        &self,
        stream: TcpStream,
        my_name: &str,
        confirm: impl FnOnce(&PairingRequest) -> bool,
    ) -> Result<SecureLink, SecureError> {
        match connect_as_host(stream, &self.identity, my_name, |key| self.is_paired(key), confirm) {
            Ok(link) => {
                if link.newly_paired {
                    self.remember(&link.peer_key, &link.peer_name)?;
                }
                Ok(link)
            }
            // Our user approved; remember it so the retry after the iPad's
            // approval doesn't ask again.
            Err(SecureError::AwaitingApproval { peer_key, peer_name }) => {
                self.remember(&peer_key, &peer_name)?;
                Err(SecureError::AwaitingApproval { peer_key, peer_name })
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(windows)]
fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(data, true)
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(data, false)
}

/// DPAPI with the current user's key: other users (and other machines) can't decrypt.
#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        } else {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        }
        .map_err(|e| io::Error::other(format!("DPAPI: {e}")))?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(bytes)
    }
}

#[cfg(not(windows))]
fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(not(windows))]
fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure::{accept_as_device, Verdict};
    use std::net::TcpListener;
    use std::thread;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("padbridge-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn identity_persists_and_pairing_is_remembered() {
        let dir = temp_dir("pairing");
        let security = HostSecurity::load_from(&dir).unwrap();
        let key = security.public_key();
        assert_eq!(HostSecurity::load_from(&dir).unwrap().public_key(), key, "identity must survive a restart");
        // The key file is not the raw key.
        assert_ne!(fs::read(dir.join("identity.key")).unwrap(), security.identity.to_bytes());

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let server = listener.accept().unwrap().0;
        let device = thread::spawn(move || {
            let ipad = Identity::generate().unwrap();
            accept_as_device(server, &ipad, "Test iPad", |_| false, |_| Verdict::Accepted).map(|l| l.peer_key)
        });
        let link = security.connect(client, "PC", |_| true).unwrap();
        assert_eq!(device.join().unwrap().unwrap(), key);

        let reloaded = HostSecurity::load_from(&dir).unwrap();
        assert!(reloaded.is_paired(&link.peer_key));
        assert_eq!(reloaded.devices()[0].name, "Test iPad");
        reloaded.forget(&hex(&link.peer_key)).unwrap();
        assert!(!HostSecurity::load_from(&dir).unwrap().is_paired(&link.peer_key));
        let _ = fs::remove_dir_all(&dir);
    }
}
