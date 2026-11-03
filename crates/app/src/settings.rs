//! User choices that survive a restart, in `%APPDATA%\PadBridge\settings.json`.

use std::fs;
use std::io;
use std::path::PathBuf;

use padbridge_host::pairing::data_dir;
use serde::{Deserialize, Serialize};

/// Encoder or decoder choice. GPUs are stored by name: their ids (adapter
/// LUIDs) change on every boot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecSetting {
    #[default]
    Auto,
    Cpu,
    Gpu(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// GDI name of the monitor sent to the iPad, e.g. `\\.\DISPLAY3`.
    pub monitor: Option<String>,
    pub allow_input: bool,
    pub encoder: CodecSetting,
    pub decoder: CodecSetting,
    /// `zh` or `en`; follows Windows when unset.
    pub language: Option<String>,
    pub max_width: usize,
    pub bitrate_kbps: u32,
    pub max_fps: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            monitor: None,
            allow_input: true,
            encoder: CodecSetting::Auto,
            decoder: CodecSetting::Auto,
            language: None,
            max_width: 1920,
            bitrate_kbps: 10_000,
            max_fps: 60,
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        data_dir().join("settings.json")
    }

    /// The saved settings, or defaults if there are none (or they can't be read).
    pub fn load() -> Self {
        fs::read(Self::path()).ok().and_then(|json| serde_json::from_slice(&json).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, json)?;
        fs::rename(temp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_fields() {
        let settings = Settings {
            monitor: Some(r"\\.\DISPLAY3".into()),
            encoder: CodecSetting::Gpu("NVIDIA GeForce RTX 3050".into()),
            decoder: CodecSetting::Cpu,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
        // Older files lack newer fields; those take their defaults.
        let old: Settings = serde_json::from_str(r#"{"allow_input": false}"#).unwrap();
        assert!(!old.allow_input);
        assert_eq!(old.max_fps, 60);
    }
}
