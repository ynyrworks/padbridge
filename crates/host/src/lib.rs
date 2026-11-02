//! Windows-side PadBridge logic shared by the CLI and the desktop app.

pub mod discovery;
pub mod mirror;
pub mod session;
pub mod target;

pub use session::{ping_session, Event, Summary};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Name this computer announces in `Hello`.
pub fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PadBridge host".into())
}
