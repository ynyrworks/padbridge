//! The machine's GPUs, for choosing which one encodes or decodes video
//! (laptops often have an integrated and a discrete one).

use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
};

pub use crate::GpuId;
use crate::{vdisplay, BoxError};

impl GpuId {
    pub fn from_luid(luid: LUID) -> Self {
        Self((u64::from(luid.HighPart as u32) << 32) | u64::from(luid.LowPart))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub id: GpuId,
    pub name: String,
    pub dedicated_mb: u64,
}

impl Gpu {
    /// Integrated GPUs share system memory and have little or no dedicated VRAM.
    pub fn integrated(&self) -> bool {
        self.dedicated_mb < 512
    }

    pub fn label(&self) -> String {
        let kind = if self.integrated() { "內顯" } else { "獨顯" };
        format!("{}（{kind}）", self.name)
    }
}

fn utf16(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

fn adapters() -> Result<Vec<(IDXGIAdapter1, Gpu)>, BoxError> {
    let mut found = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        for i in 0.. {
            let adapter = match factory.EnumAdapters1(i) {
                Ok(adapter) => adapter,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let desc = adapter.GetDesc1()?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue; // Microsoft Basic Render Driver
            }
            // Indirect display drivers show up as extra adapters named after the GPU
            // that renders for them; they can't encode or decode themselves.
            if vdisplay::adapter_instance_id(desc.AdapterLuid).is_some_and(|id| vdisplay::is_virtual_instance(&id)) {
                continue;
            }
            let gpu = Gpu {
                id: GpuId::from_luid(desc.AdapterLuid),
                name: utf16(&desc.Description),
                dedicated_mb: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
            };
            // The same adapter can be listed once per output on some systems.
            if !found.iter().any(|(_, g): &(IDXGIAdapter1, Gpu)| g.id == gpu.id) {
                found.push((adapter, gpu));
            }
        }
    }
    Ok(found)
}

/// Hardware GPUs, discrete ones first.
pub fn list_gpus() -> Vec<Gpu> {
    let mut gpus: Vec<Gpu> = adapters().unwrap_or_default().into_iter().map(|(_, g)| g).collect();
    gpus.sort_by_key(Gpu::integrated);
    gpus
}

pub(crate) fn create_device(
    adapter: &IDXGIAdapter1,
    flags: D3D11_CREATE_DEVICE_FLAG,
) -> Result<(ID3D11Device, ID3D11DeviceContext), BoxError> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    Ok((device.ok_or("D3D11 device was not created")?, context.ok_or("D3D11 context was not created")?))
}

/// A D3D11 device on `id` (or the first hardware GPU) with video support and
/// multithread protection, as Media Foundation needs.
pub fn open_video_device(id: Option<GpuId>) -> Result<(ID3D11Device, Gpu), BoxError> {
    let list = adapters()?;
    let mut ordered: Vec<_> = list.iter().collect();
    ordered.sort_by_key(|(_, g)| g.integrated());
    let (adapter, gpu) = match id {
        Some(id) => ordered.into_iter().find(|(_, g)| g.id == id).ok_or("找不到選擇的 GPU")?,
        None => ordered.into_iter().next().ok_or("找不到 GPU")?,
    };
    let (device, _) = create_device(adapter, D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT)?;
    if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
        unsafe {
            let _ = multithread.SetMultithreadProtected(true);
        }
    }
    Ok((device, gpu.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luid_packs_both_halves() {
        assert_eq!(GpuId::from_luid(LUID { LowPart: 0x1234, HighPart: 1 }).0, 0x1_0000_1234);
    }

    #[test]
    fn lists_hardware_gpus() {
        // Machine-dependent; checks the calls and that software adapters are skipped.
        for gpu in list_gpus() {
            assert!(!gpu.name.is_empty());
            assert!(!gpu.name.contains("Basic Render"));
        }
    }
}
