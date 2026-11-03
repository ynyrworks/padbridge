//! Windows-side PadBridge logic shared by the CLI and the desktop app.

// First, so its tr! macro is visible in the other modules.
#[macro_use]
pub mod i18n;

#[cfg(windows)]
pub mod capture;
pub mod discovery;
pub mod display;
#[cfg(windows)]
pub mod gpu;
#[cfg(windows)]
pub mod hwdec;
#[cfg(windows)]
pub mod hwenc;
pub mod input;
pub mod mirror;
pub mod pairing;
pub mod secure;
pub mod session;
pub mod target;
#[cfg(windows)]
pub mod vdd;
#[cfg(windows)]
pub mod vdisplay;

pub use session::{ping_session, Event, Summary};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Identifies a GPU for this boot (its adapter LUID); see `gpu::list_gpus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuId(pub u64);

/// Name this computer announces in `Hello`.
pub fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PadBridge host".into())
}
