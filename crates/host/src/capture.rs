//! Monitor capture with DXGI Desktop Duplication.
//!
//! Works for physical monitors and for virtual ones added by an Indirect
//! Display Driver, which is how the iPad becomes an extended display.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

use crate::gpu::{create_device, GpuId};
use crate::{vdisplay, BoxError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Position in [`list_monitors`]; pass to [`Capturer::new`].
    pub index: usize,
    /// GDI device name, e.g. `\\.\DISPLAY2`.
    pub device_name: String,
    /// Graphics adapter, e.g. the GPU or "Virtual Display Driver".
    pub adapter: String,
    /// The GPU that renders this monitor (and so captures it).
    pub gpu: GpuId,
    /// Top-left corner on the virtual desktop, in physical pixels.
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    /// Set when a virtual display adapter owns this monitor, so it extends
    /// the desktop rather than being a physical screen.
    pub virtual_adapter: Option<String>,
}

impl Monitor {
    pub fn label(&self) -> String {
        let name = self.device_name.trim_start_matches(r"\\.\");
        let primary = if self.primary { tr!(" · 主螢幕", " · primary") } else { String::new() };
        match &self.virtual_adapter {
            Some(adapter) => tr!("{name} · {}×{}{primary} · 虛擬 · {adapter}", "{name} · {}×{}{primary} · virtual · {adapter}", self.width, self.height),
            None => format!("{name} · {}×{}{primary} · {}", self.width, self.height, self.adapter),
        }
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
    let owners = vdisplay::source_adapters();
    let virtual_names: Vec<_> = vdisplay::virtual_adapters();
    let virtual_adapter = |device_name: &str| {
        let owner = owners.get(&device_name.to_ascii_uppercase())?;
        vdisplay::is_virtual_instance(owner).then(|| {
            virtual_names
                .iter()
                .find(|a| a.instance_id.eq_ignore_ascii_case(owner))
                .map_or_else(|| owner.clone(), |a| a.name.clone())
        })
    };
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        for a in 0.. {
            let adapter = match factory.EnumAdapters1(a) {
                Ok(adapter) => adapter,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let adapter_desc = adapter.GetDesc1()?;
            let adapter_name = utf16(&adapter_desc.Description);
            let gpu = GpuId::from_luid(adapter_desc.AdapterLuid);
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
                let device_name = utf16(&desc.DeviceName);
                let monitor = Monitor {
                    index: found.len(),
                    virtual_adapter: virtual_adapter(&device_name),
                    device_name,
                    adapter: adapter_name.clone(),
                    gpu,
                    left: r.left,
                    top: r.top,
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

/// What a [`Capturer::poll`] observed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub image: bool,
    pub cursor: bool,
}

impl Changes {
    pub fn any(self) -> bool {
        self.image || self.cursor
    }
}

pub struct Capturer {
    monitor: Monitor,
    output: IDXGIOutput1,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// Copy every image to system memory (software encoding) or keep it on the GPU.
    cpu_readback: bool,
    staging: Option<ID3D11Texture2D>,
    bgra: Vec<u8>,
    /// GPU copy of the latest image; Desktop Duplication's own texture is only
    /// valid until the frame is released.
    gpu_copy: Option<ID3D11Texture2D>,
    width: usize,
    height: usize,
    cursor: Option<(i32, i32)>,
}

impl Capturer {
    /// `cpu_readback`: provide images as BGRA bytes ([`Self::last_frame`]) as well
    /// as on the GPU ([`Self::texture`]).
    pub fn new(monitor_index: usize, cpu_readback: bool) -> Result<Self, BoxError> {
        let (adapter, output, monitor) = outputs()?
            .into_iter()
            .nth(monitor_index)
            .ok_or_else(|| tr!("找不到第 {monitor_index} 個螢幕", "Monitor {monitor_index} not found"))?;
        // Video support is needed for GPU scaling / hardware encoding; fall back without it.
        let (device, context) = create_device(&adapter, D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT)
            .or_else(|_| create_device(&adapter, D3D11_CREATE_DEVICE_BGRA_SUPPORT))?;
        // Media Foundation uses the device from its own threads.
        if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
            unsafe {
                let _ = multithread.SetMultithreadProtected(true);
            }
        }
        let duplication = unsafe { output.DuplicateOutput(&device)? };
        Ok(Self {
            monitor,
            output,
            device,
            context,
            duplication,
            cpu_readback,
            staging: None,
            bgra: Vec::new(),
            gpu_copy: None,
            width: 0,
            height: 0,
            cursor: None,
        })
    }

    pub fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Size of the latest image (0×0 before the first one).
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Cursor hot spot relative to the monitor, if visible.
    pub fn cursor(&self) -> Option<(i32, i32)> {
        self.cursor
    }

    /// The latest image on the GPU (BGRA), even if nothing changed since.
    pub fn texture(&self) -> Option<&ID3D11Texture2D> {
        self.gpu_copy.as_ref()
    }

    /// The latest image in system memory; only with `cpu_readback`.
    pub fn last_frame(&self) -> Option<CapturedFrame<'_>> {
        (self.cpu_readback && self.width > 0).then(|| CapturedFrame {
            width: self.width,
            height: self.height,
            bgra: &self.bgra,
            cursor: self.cursor,
        })
    }

    /// Waits up to `timeout_ms` for the desktop image or cursor to change.
    pub fn poll(&mut self, timeout_ms: u32) -> Result<Changes, BoxError> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe { self.duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Changes::default()),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                // Mode change, secure desktop (UAC / lock screen) etc.: start over.
                self.duplication = unsafe { self.output.DuplicateOutput(&self.device)? };
                self.staging = None;
                self.gpu_copy = None;
                return Ok(Changes::default());
            }
            Err(e) => return Err(e.into()),
        }
        let result = self.read_frame(&info, resource);
        unsafe { self.duplication.ReleaseFrame()? };
        result
    }

    fn read_frame(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO, resource: Option<IDXGIResource>) -> Result<Changes, BoxError> {
        let mut changes = Changes::default();
        if info.LastMouseUpdateTime != 0 {
            let p = info.PointerPosition;
            let cursor = p.Visible.as_bool().then_some((p.Position.x, p.Position.y));
            changes.cursor = cursor != self.cursor;
            self.cursor = cursor;
        }
        if info.LastPresentTime == 0 {
            return Ok(changes);
        }
        changes.image = true;
        let texture: ID3D11Texture2D = resource.ok_or("no desktop image")?.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let (width, height) = (desc.Width as usize, desc.Height as usize);
        let resized = (self.width, self.height) != (width, height);

        if self.gpu_copy.is_none() || resized {
            let copy_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                ..desc
            };
            let mut copy = None;
            unsafe { self.device.CreateTexture2D(&copy_desc, None, Some(&mut copy))? };
            self.gpu_copy = Some(copy.ok_or("GPU copy texture was not created")?);
        }
        unsafe { self.context.CopyResource(self.gpu_copy.as_ref().unwrap(), &texture) };

        if self.cpu_readback {
            if self.staging.is_none() || resized {
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                    ..desc
                };
                let mut staging = None;
                unsafe { self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))? };
                self.staging = Some(staging.ok_or("staging texture was not created")?);
            }
            let staging = self.staging.as_ref().unwrap();
            unsafe {
                self.context.CopyResource(staging, &texture);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let pitch = mapped.RowPitch as usize;
                let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * height);
                self.bgra.resize(width * height * 4, 0);
                for (y, row) in self.bgra.chunks_exact_mut(width * 4).enumerate() {
                    row.copy_from_slice(&src[y * pitch..y * pitch + width * 4]);
                }
                self.context.Unmap(staging, 0);
            }
        }
        self.width = width;
        self.height = height;
        Ok(changes)
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
        let Ok(mut capturer) = Capturer::new(first.index, true) else { return };
        // The first acquire returns the current desktop image.
        let mut got = false;
        for _ in 0..10 {
            match capturer.poll(200) {
                Ok(c) if c.image && capturer.last_frame().is_some() => {
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
