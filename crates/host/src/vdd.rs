//! Integration with the open-source Virtual Display Driver (MIT,
//! github.com/VirtualDrivers/Virtual-Display-Driver), which adds a real
//! extended monitor for the iPad to show.
//!
//! The driver is installed by its own signed control app (fetched with
//! winget); after that it is configured through `vdd_settings.xml` and
//! controlled over a named pipe that any user may open.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

use crate::capture::list_monitors;
use crate::vdisplay::{adapter_outputs, attach, detach, set_mode, virtual_adapters, AdapterOutput, VirtualAdapter};
use crate::BoxError;

pub const WINGET_ID: &str = "VirtualDrivers.Virtual-Display-Driver";
const PIPE: &str = r"\\.\pipe\MTTVirtualDisplayPipe";
const DEFAULT_DIR: &str = r"C:\VirtualDisplayDriver";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether an adapter is this driver (hardware id `Root\MttVDD`).
pub fn is_vdd(adapter: &VirtualAdapter) -> bool {
    adapter.hardware_id.eq_ignore_ascii_case(r"Root\MttVDD") || adapter.name.contains("Virtual Display Driver")
}

/// The installed driver, if any.
pub fn installed() -> Option<VirtualAdapter> {
    virtual_adapters().into_iter().find(is_vdd)
}

/// The driver's settings file; its folder can be moved via the registry.
pub fn settings_path() -> PathBuf {
    let mut buf = [0u16; 260];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\MikeTheTech\VirtualDisplayDriver"),
            w!("VDDPATH"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    let dir = if status == ERROR_SUCCESS {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    } else {
        DEFAULT_DIR.to_string()
    };
    PathBuf::from(dir).join("vdd_settings.xml")
}

/// Adds a `<resolution>` entry to the settings XML unless that size is
/// already listed. Returns the new text, or `None` if nothing changed.
pub fn add_resolution(xml: &str, width: u32, height: u32, refresh: u32) -> Option<String> {
    let listed = xml.split("<resolution>").skip(1).any(|entry| {
        let value = |tag: &str| {
            let start = entry.find(&format!("<{tag}>"))? + tag.len() + 2;
            let end = start + entry[start..].find('<')?;
            entry[start..end].trim().parse::<u32>().ok()
        };
        value("width") == Some(width) && value("height") == Some(height)
    });
    if listed {
        return None;
    }
    let close = xml.find("</resolutions>")?;
    let indent = xml[..close].rsplit('\n').next().filter(|s| s.trim().is_empty()).unwrap_or("");
    let entry = format!(
        "    <resolution>\n{indent}        <width>{width}</width>\n{indent}        <height>{height}</height>\n\
         {indent}        <refresh_rate>{refresh}</refresh_rate>\n{indent}    </resolution>\n{indent}"
    );
    let line_start = close - indent.len();
    Some(format!("{}{indent}{entry}{}", &xml[..line_start], &xml[close..]))
}

/// Sends one command (e.g. `RELOAD_DRIVER`, `SETDISPLAYCOUNT 1`) to the driver.
pub fn send_command(command: &str) -> io::Result<()> {
    let mut pipe = OpenOptions::new().read(true).write(true).open(PIPE)?;
    let bytes: Vec<u8> = command.encode_utf16().flat_map(u16::to_le_bytes).collect();
    pipe.write_all(&bytes)
}

/// Makes `device_name` (a monitor of this driver) run at the iPad's
/// resolution: adds the mode to the driver if needed, then switches to it.
pub fn fit_monitor(device_name: &str, width: u32, height: u32, log: &mut impl FnMut(String)) -> Result<(), BoxError> {
    let path = settings_path();
    let xml = fs::read_to_string(&path).map_err(|e| tr!("讀不到 {}：{e}", "Cannot read {}: {e}", path.display()))?;
    if let Some(updated) = add_resolution(&xml, width, height, 60) {
        fs::write(&path, updated).map_err(|e| tr!("無法寫入 {}：{e}", "Cannot write {}: {e}", path.display()))?;
        log(tr!(
            "已把 {width}×{height} 加入虛擬螢幕的解析度清單，重新載入驅動…",
            "Added {width}×{height} to the virtual display's modes, reloading the driver…"
        ));
        send_command("RELOAD_DRIVER").map_err(|e| tr!("無法通知驅動重新載入：{e}", "Could not ask the driver to reload: {e}"))?;
    }
    // The new mode shows up once the driver has re-announced its monitors.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match set_mode(device_name, width, height) {
            Ok(()) => break,
            Err(e) if Instant::now() >= deadline => {
                return Err(tr!("無法切換到 {width}×{height}：{e}", "Could not switch to {width}×{height}: {e}").into())
            }
            Err(_) => thread::sleep(Duration::from_millis(500)),
        }
    }
    log(tr!("虛擬螢幕已切換為 {width}×{height}", "Virtual display set to {width}×{height}"));
    Ok(())
}

/// The driver's outputs, attached to the desktop or not.
fn outputs() -> Vec<AdapterOutput> {
    adapter_outputs(|hardware_id, name| {
        hardware_id.eq_ignore_ascii_case(r"Root\MttVDD") || name.contains("Virtual Display Driver")
    })
}

/// Whether the driver's monitor is part of the desktop.
pub fn monitor_on() -> Result<bool, BoxError> {
    Ok(list_monitors()?.iter().any(|m| m.virtual_adapter.as_deref().is_some_and(|a| a.contains("Virtual Display"))))
}

/// Waits for the monitor, putting it back on the desktop if Windows kept it off.
fn wait_for_monitor(limit: Duration) -> Result<bool, BoxError> {
    let deadline = Instant::now() + limit;
    loop {
        if monitor_on()? {
            return Ok(true);
        }
        // Outputs without a monitor refuse this; only a real one comes back.
        for output in outputs().iter().filter(|o| !o.attached) {
            let _ = attach(&output.device_name);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Turns the driver's monitor on when it is installed but showing none: puts
/// back one that `disable_monitor` took off, or asks the driver for one.
pub fn enable_monitor() -> Result<(), BoxError> {
    if wait_for_monitor(Duration::from_secs(2))? {
        return Ok(());
    }
    send_command("SETDISPLAYCOUNT 1").map_err(|e| tr!("無法連到虛擬螢幕驅動：{e}", "Cannot reach the virtual display driver: {e}"))?;
    if wait_for_monitor(Duration::from_secs(10))? {
        Ok(())
    } else {
        Err(tr!("驅動沒有新增螢幕", "The driver did not add a display").into())
    }
}

/// Takes the driver's monitor off the desktop, so windows move back to the
/// real screens. The driver keeps it, so `enable_monitor` brings it back
/// without a reload. (The driver treats a count of 0 as 1, so asking it for
/// no monitors would not work.) Returns how many were taken off.
pub fn disable_monitor() -> Result<usize, BoxError> {
    let mut count = 0;
    for output in outputs().iter().filter(|o| o.attached) {
        detach(&output.device_name)
            .map_err(|e| tr!("無法關閉虛擬螢幕 {}：{e}", "Could not turn off the virtual display {}: {e}", output.device_name))?;
        count += 1;
    }
    Ok(count)
}

/// Installs the driver's control app with winget and opens it, where the
/// user presses "Install" (which asks for administrator rights).
pub fn install(log: &mut impl FnMut(String)) -> Result<(), BoxError> {
    log(tr!("以 winget 下載 Virtual Display Driver（{WINGET_ID}）…", "Downloading Virtual Display Driver ({WINGET_ID}) with winget…"));
    let output = Command::new("winget")
        .args(["install", "--id", WINGET_ID, "-e", "--accept-source-agreements", "--accept-package-agreements"])
        .args(["--disable-interactivity"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| tr!("無法執行 winget（需要 Windows 的「應用程式安裝程式」）：{e}", "Cannot run winget (needs Windows' App Installer): {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    // winget exits non-zero when the package is already installed; finding the app is what matters.
    let app = control_app().ok_or_else(|| {
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        tr!("winget 沒有裝好控制程式：{last}", "winget did not install the control app: {last}")
    })?;
    log(tr!(
        "開啟 {}，請在裡面按「Install Driver」並允許系統管理員權限",
        "Opening {}: press Install Driver there and allow administrator rights",
        app.display()
    ));
    // Through Explorer so Windows shows the elevation prompt if the app asks for it.
    Command::new("explorer").arg(&app).spawn()?;
    Ok(())
}

/// The control app unpacked by winget, if present.
pub fn control_app() -> Option<PathBuf> {
    let packages = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(r"Microsoft\WinGet\Packages");
    let dir = fs::read_dir(packages)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with(&format!("{WINGET_ID}_")))?
        .path();
    let mut exes = Vec::new();
    collect_exes(&dir, 3, &mut exes);
    exes.sort_by_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        !(name.contains("vdd") || name.contains("control"))
    });
    exes.into_iter().next()
}

fn collect_exes(dir: &std::path::Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            collect_exes(&path, depth - 1, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<vdd_settings>\n    <resolutions>\n        <resolution>\n            <width>1920</width>\n            <height>1080</height>\n            <refresh_rate>60</refresh_rate>\n        </resolution>\n    </resolutions>\n</vdd_settings>\n";

    #[test]
    fn keeps_listed_resolution() {
        assert_eq!(add_resolution(SAMPLE, 1920, 1080, 60), None);
    }

    #[test]
    fn adds_missing_resolution() {
        let updated = add_resolution(SAMPLE, 2360, 1640, 60).unwrap();
        assert_eq!(add_resolution(&updated, 2360, 1640, 60), None);
        assert!(updated.contains("<width>1920</width>"));
        assert!(updated.contains(
            "        <resolution>\n            <width>2360</width>\n            <height>1640</height>\n            <refresh_rate>60</refresh_rate>\n        </resolution>\n    </resolutions>"
        ));
    }

    #[test]
    fn needs_resolutions_section() {
        assert_eq!(add_resolution("<vdd_settings/>", 2360, 1640, 60), None);
    }
}
