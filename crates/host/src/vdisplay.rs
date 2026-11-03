//! Finding virtual display adapters (Indirect Display drivers such as
//! Virtual Display Driver, or those bundled with remote-desktop and emulator
//! apps). A monitor on one of them is a real extension of the desktop, so the
//! iPad can show it instead of duplicating a physical screen.
//!
//! DXGI lists an indirect display's outputs under the GPU that renders them,
//! so the adapter name alone can't tell them apart; the display configuration
//! API reports the adapter that actually owns each source.

use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDeviceRegistryPropertyW, DIGCF_PRESENT, GUID_DEVCLASS_DISPLAY, SPDRP_DEVICEDESC, SPDRP_FRIENDLYNAME,
    SPDRP_HARDWAREID, SP_DEVINFO_DATA,
};
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, CDS_NORESET, CDS_TYPE, CDS_UPDATEREGISTRY,
    DEVMODEW, DEVMODEW_0, DEVMODEW_0_1, DISPLAY_DEVICEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISP_CHANGE_SUCCESSFUL,
    DM_PELSHEIGHT, DM_PELSWIDTH, DM_POSITION, ENUM_REGISTRY_SETTINGS,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_XVIRTUALSCREEN};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_ADAPTER_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LUID, POINTL};

/// An installed display adapter with no hardware behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualAdapter {
    pub name: String,
    /// Device instance id, e.g. `ROOT\DISPLAY\0000`.
    pub instance_id: String,
    /// First hardware id, which names the driver, e.g. `Root\MttVDD`.
    pub hardware_id: String,
}

/// Software-enumerated devices; hardware adapters sit under `PCI\` or `USB\`.
pub fn is_virtual_instance(instance_id: &str) -> bool {
    let id = instance_id.to_ascii_uppercase();
    id.starts_with(r"ROOT\") || id.starts_with(r"SWD\")
}

/// `\\?\ROOT#DISPLAY#0000#{guid}` → `ROOT\DISPLAY\0000`.
pub fn instance_id_from_path(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let path = path.find("#{").map_or(path, |end| &path[..end]);
    path.replace('#', "\\")
}

fn utf16(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// Virtual display adapters that are installed, whether or not they currently
/// drive a monitor (most only add one when their own app asks them to).
pub fn virtual_adapters() -> Vec<VirtualAdapter> {
    let mut found = Vec::new();
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), PCWSTR::null(), None, DIGCF_PRESENT) else {
            return found;
        };
        for i in 0.. {
            let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, i, &mut data).is_err() {
                break;
            }
            let mut id = [0u16; 256];
            if SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut id), None).is_err() {
                continue;
            }
            let instance_id = utf16(&id);
            if !is_virtual_instance(&instance_id) {
                continue;
            }
            // A multi-string property's first entry ends at its first NUL, so this reads that.
            let property = |property| {
                let mut buf = [0u8; 512];
                SetupDiGetDeviceRegistryPropertyW(set, &data, property, None, Some(&mut buf), None).ok()?;
                let wide: Vec<u16> = buf.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                Some(utf16(&wide)).filter(|s| !s.is_empty())
            };
            let name = property(SPDRP_FRIENDLYNAME)
                .or_else(|| property(SPDRP_DEVICEDESC))
                .unwrap_or_else(|| instance_id.clone());
            let hardware_id = property(SPDRP_HARDWAREID).unwrap_or_default();
            found.push(VirtualAdapter { name, instance_id, hardware_id });
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    found
}

fn header<T>(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size_of::<T>() as u32, adapterId: adapter, id }
}

/// Device instance id of the adapter with this LUID, e.g. `PCI\VEN_10DE&…` or
/// `ROOT\DISPLAY\0000`; `None` if Windows doesn't know it.
pub fn adapter_instance_id(luid: LUID) -> Option<String> {
    let mut adapter = DISPLAYCONFIG_ADAPTER_NAME {
        header: header::<DISPLAYCONFIG_ADAPTER_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, luid, 0),
        ..Default::default()
    };
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut adapter.header) };
    (status == 0).then(|| instance_id_from_path(&utf16(&adapter.adapterDevicePath)))
}

/// GDI device name (upper case, e.g. `\\.\DISPLAY3`) → instance id of the
/// adapter that owns that source, for every active display.
pub fn source_adapters() -> HashMap<String, String> {
    let mut map = HashMap::new();
    unsafe {
        let (mut num_paths, mut num_modes) = (0, 0);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut num_paths, &mut num_modes) != ERROR_SUCCESS {
            return map;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];
        let status = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            None,
        );
        if status != ERROR_SUCCESS {
            return map;
        }
        for path in &paths[..num_paths as usize] {
            let src = &path.sourceInfo;
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    src.adapterId,
                    src.id,
                ),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }
            let mut adapter = DISPLAYCONFIG_ADAPTER_NAME {
                header: header::<DISPLAYCONFIG_ADAPTER_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, src.adapterId, 0),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut adapter.header) != 0 {
                continue;
            }
            map.insert(
                utf16(&source.viewGdiDeviceName).to_ascii_uppercase(),
                instance_id_from_path(&utf16(&adapter.adapterDevicePath)),
            );
        }
    }
    map
}

/// Switches a monitor (GDI name, e.g. `\\.\DISPLAY3`) to another resolution
/// it supports. Per-user, so no administrator rights are needed.
pub fn set_mode(device_name: &str, width: u32, height: u32) -> Result<(), String> {
    let name: Vec<u16> = device_name.encode_utf16().chain([0]).collect();
    let mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        dmPelsWidth: width,
        dmPelsHeight: height,
        dmFields: DM_PELSWIDTH | DM_PELSHEIGHT,
        ..Default::default()
    };
    let result =
        unsafe { ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&mode), None, CDS_UPDATEREGISTRY, None) };
    if result == DISP_CHANGE_SUCCESSFUL {
        Ok(())
    } else {
        Err(tr!("ChangeDisplaySettingsEx 回傳 {}", "ChangeDisplaySettingsEx returned {}", result.0))
    }
}

/// A display output (source) of an adapter, whether or not it is part of the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterOutput {
    /// GDI name, e.g. `\\.\DISPLAY3`.
    pub device_name: String,
    pub attached: bool,
}

/// The outputs of the adapters for which `matches(hardware id, name)` holds.
pub fn adapter_outputs(matches: impl Fn(&str, &str) -> bool) -> Vec<AdapterOutput> {
    let mut found = Vec::new();
    for i in 0.. {
        let mut device = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut device, 0) }.as_bool() {
            break;
        }
        if matches(&utf16(&device.DeviceID), &utf16(&device.DeviceString)) {
            found.push(AdapterOutput {
                device_name: utf16(&device.DeviceName),
                attached: device.StateFlags.contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP),
            });
        }
    }
    found
}

/// Stages a change for one output and applies it, saving it for next time.
fn change_output(device_name: &str, mode: &DEVMODEW) -> Result<(), String> {
    let name: Vec<u16> = device_name.encode_utf16().chain([0]).collect();
    let staged = unsafe {
        ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(mode), None, CDS_UPDATEREGISTRY | CDS_NORESET, None)
    };
    if staged != DISP_CHANGE_SUCCESSFUL {
        return Err(tr!("ChangeDisplaySettingsEx 回傳 {}", "ChangeDisplaySettingsEx returned {}", staged.0));
    }
    let applied = unsafe { ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None) };
    if applied == DISP_CHANGE_SUCCESSFUL {
        Ok(())
    } else {
        Err(tr!("ChangeDisplaySettingsEx 回傳 {}", "ChangeDisplaySettingsEx returned {}", applied.0))
    }
}

/// Takes an output off the desktop (like "Disconnect this display" in
/// Settings); the monitor itself stays, ready for `attach`.
pub fn detach(device_name: &str) -> Result<(), String> {
    let mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        dmFields: DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT,
        ..Default::default()
    };
    change_output(device_name, &mode)
}

/// Puts a detached output back on the desktop, right of the other monitors,
/// at the size it last had.
pub fn attach(device_name: &str) -> Result<(), String> {
    let name: Vec<u16> = device_name.encode_utf16().chain([0]).collect();
    let mut saved = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    let known = unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_REGISTRY_SETTINGS, &mut saved) }.as_bool();
    let (width, height) = if known && saved.dmPelsWidth > 0 { (saved.dmPelsWidth, saved.dmPelsHeight) } else { (1920, 1080) };
    let right = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        dmFields: DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT,
        dmPelsWidth: width,
        dmPelsHeight: height,
        ..Default::default()
    };
    mode.Anonymous1 = DEVMODEW_0 { Anonymous2: DEVMODEW_0_1 { dmPosition: POINTL { x: right, y: 0 }, ..Default::default() } };
    change_output(device_name, &mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_ids() {
        assert_eq!(
            instance_id_from_path(r"\\?\ROOT#DISPLAY#0000#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}"),
            r"ROOT\DISPLAY\0000"
        );
        assert_eq!(
            instance_id_from_path(r"\\?\PCI#VEN_10DE&DEV_2C02&SUBSYS_41961458&REV_A1#4&1#{5b45201d-f2f2}"),
            r"PCI\VEN_10DE&DEV_2C02&SUBSYS_41961458&REV_A1\4&1"
        );
        assert!(is_virtual_instance(r"ROOT\DISPLAY\0001"));
        assert!(is_virtual_instance(r"SWD\REMOTEDISPLAYENUM\1"));
        assert!(!is_virtual_instance(r"PCI\VEN_10DE&DEV_2C02\9EE4"));
        assert!(!is_virtual_instance(r"USB\VID_17E9&PID_6000\1"));
    }

    #[test]
    fn lists_without_crashing() {
        // Contents depend on the machine; this checks the API calls themselves.
        let adapters = virtual_adapters();
        let sources = source_adapters();
        assert!(adapters.iter().all(|a| is_virtual_instance(&a.instance_id)));
        assert!(sources.keys().all(|k| k.starts_with(r"\\.\DISPLAY")));
        let outputs = adapter_outputs(|_, _| true);
        assert!(outputs.iter().all(|o| o.device_name.starts_with(r"\\.\DISPLAY")));
        // Every active display is one of the enumerated outputs, and attached.
        for name in sources.keys() {
            assert!(outputs.iter().any(|o| o.device_name.eq_ignore_ascii_case(name) && o.attached), "{name}");
        }
    }
}
