//! Monitor capture with DXGI Desktop Duplication.
//!
//! Works for physical monitors and for virtual ones added by an Indirect
//! Display Driver, which is how the iPad becomes an extended display.

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

use crate::BoxError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Position in [`list_monitors`]; pass to [`Capturer::new`].
    pub index: usize,
    /// GDI device name, e.g. `\\.\DISPLAY2`.
    pub device_name: String,
    /// Graphics adapter, e.g. the GPU or "Virtual Display Driver".
    pub adapter: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl Monitor {
    pub fn label(&self) -> String {
        let name = self.device_name.trim_start_matches(r"\\.\");
        let primary = if self.primary { " · 主螢幕" } else { "" };
        format!("{name} · {}×{}{primary} · {}", self.width, self.height, self.adapter)
    }
}

fn utf16(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// Without per-monitor DPI awareness Windows reports scaled ("logical")
/// monitor sizes and cursor positions, while captured images are physical.
fn ensure_dpi_aware() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        // Fails harmlessly if the process (e.g. the GUI toolkit) already set it.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

fn outputs() -> Result<Vec<(IDXGIAdapter1, IDXGIOutput1, Monitor)>, BoxError> {
    ensure_dpi_aware();
    let mut found = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        for a in 0.. {
            let adapter = match factory.EnumAdapters1(a) {
                Ok(adapter) => adapter,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let adapter_name = utf16(&adapter.GetDesc1()?.Description);
            for o in 0.. {
                let output = match adapter.EnumOutputs(o) {
                    Ok(output) => output,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e.into()),
                };
                let desc = output.GetDesc()?;
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let r = desc.DesktopCoordinates;
                let monitor = Monitor {
                    index: found.len(),
                    device_name: utf16(&desc.DeviceName),
                    adapter: adapter_name.clone(),
                    width: (r.right - r.left) as u32,
                    height: (r.bottom - r.top) as u32,
                    primary: r.left == 0 && r.top == 0,
                };
                found.push((adapter.clone(), output.cast()?, monitor));
            }
        }
    }
    Ok(found)
}

pub fn list_monitors() -> Result<Vec<Monitor>, BoxError> {
    Ok(outputs()?.into_iter().map(|(_, _, m)| m).collect())
}

/// A captured desktop image as tightly packed BGRA.
pub struct CapturedFrame<'a> {
    pub width: usize,
    pub height: usize,
    pub bgra: &'a [u8],
    /// Cursor hot spot relative to the monitor, if visible.
    pub cursor: Option<(i32, i32)>,
}

pub struct Capturer {
    monitor: Monitor,
    output: IDXGIOutput1,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    bgra: Vec<u8>,
    width: usize,
    height: usize,
    cursor: Option<(i32, i32)>,
}

impl Capturer {
    pub fn new(monitor_index: usize) -> Result<Self, BoxError> {
        let (adapter, output, monitor) = outputs()?
            .into_iter()
            .nth(monitor_index)
            .ok_or_else(|| format!("找不到第 {monitor_index} 個螢幕"))?;
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.ok_or("D3D11 device was not created")?;
        let context = context.ok_or("D3D11 context was not created")?;
        let duplication = unsafe { output.DuplicateOutput(&device)? };
        Ok(Self {
            monitor,
            output,
            device,
            context,
            duplication,
            staging: None,
            bgra: Vec::new(),
            width: 0,
            height: 0,
            cursor: None,
        })
    }

    pub fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    /// The most recent image, even if nothing changed since.
    pub fn last_frame(&self) -> Option<CapturedFrame<'_>> {
        (self.width > 0).then(|| CapturedFrame {
            width: self.width,
            height: self.height,
            bgra: &self.bgra,
            cursor: self.cursor,
        })
    }

    /// Waits up to `timeout_ms` for the desktop image or cursor to change.
    /// Returns `Ok(false)` when nothing changed.
    pub fn poll(&mut self, timeout_ms: u32) -> Result<bool, BoxError> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe { self.duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                // Mode change, secure desktop (UAC / lock screen) etc.: start over.
                self.duplication = unsafe { self.output.DuplicateOutput(&self.device)? };
                self.staging = None;
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
        let result = self.read_frame(&info, resource);
        unsafe { self.duplication.ReleaseFrame()? };
        result
    }

    fn read_frame(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO, resource: Option<IDXGIResource>) -> Result<bool, BoxError> {
        let mut changed = false;
        if info.LastMouseUpdateTime != 0 {
            let p = info.PointerPosition;
            self.cursor = p.Visible.as_bool().then_some((p.Position.x, p.Position.y));
            changed = true;
        }
        if info.LastPresentTime == 0 {
            return Ok(changed);
        }
        let texture: ID3D11Texture2D = resource.ok_or("no desktop image")?.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let staging = match &self.staging {
            Some(s) if (self.width, self.height) == (desc.Width as usize, desc.Height as usize) => s.clone(),
            _ => {
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                    ..desc
                };
                let mut staging = None;
                unsafe { self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))? };
                let staging = staging.ok_or("staging texture was not created")?;
                self.staging = Some(staging.clone());
                staging
            }
        };
        let (width, height) = (desc.Width as usize, desc.Height as usize);
        unsafe {
            self.context.CopyResource(&staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let pitch = mapped.RowPitch as usize;
            let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * height);
            self.bgra.resize(width * height * 4, 0);
            for (y, row) in self.bgra.chunks_exact_mut(width * 4).enumerate() {
                row.copy_from_slice(&src[y * pitch..y * pitch + width * 4]);
            }
            self.context.Unmap(&staging, 0);
        }
        self.width = width;
        self.height = height;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an interactive desktop; skipped where none exists (e.g. some CI).
    #[test]
    fn captures_first_monitor() {
        let Ok(monitors) = list_monitors() else { return };
        let Some(first) = monitors.first() else { return };
        let Ok(mut capturer) = Capturer::new(first.index) else { return };
        // The first acquire returns the current desktop image.
        let mut got = false;
        for _ in 0..10 {
            match capturer.poll(200) {
                Ok(true) if capturer.last_frame().is_some() => {
                    got = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
        assert!(got, "no frame from {}", first.label());
        let frame = capturer.last_frame().unwrap();
        assert_eq!((frame.width as u32, frame.height as u32), (first.width, first.height));
        assert_eq!(frame.bgra.len(), frame.width * frame.height * 4);
    }
}
