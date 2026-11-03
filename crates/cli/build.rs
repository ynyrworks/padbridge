//! Embeds the Windows version resource (product name, version, copyright),
//! so the executable identifies itself in Explorer and to code signing.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "PadBridge");
        res.set("FileDescription", "PadBridge command-line tool");
        res.set("CompanyName", "PadBridge contributors");
        res.set("LegalCopyright", "Copyright (c) 2026 PadBridge contributors. MIT License.");
        res.compile().expect("compile the version resource");
    }
}
