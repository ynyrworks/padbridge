//! Hardware H.264 decoding for Mirror: Media Foundation's H.264 decoder runs
//! on the chosen GPU through DXVA, and a D3D11 video processor converts each
//! picture to BGRA and rotates it upright before it is copied back for display.

use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEX2D_VPIV,
    D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CAPS, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_ROTATION, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    ICodecAPI, IMFActivate, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFTransform,
    MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFTEnumEx, MFVideoArea, MFVideoFormat_H264, MFVideoFormat_NV12, CODECAPI_AVLowLatencyMode,
    MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;

use crate::gpu::{open_video_device, Gpu, GpuId};
use crate::hwenc::{friendly_name, start_media_foundation};
use crate::mirror::{rotate, Frame};
use crate::BoxError;

pub struct HardwareDecoder {
    name: String,
    gpu: Gpu,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    transform: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    /// Visible part of the decoded picture (codec sizes are padded to 16).
    crop: Option<RECT>,
    /// Size of the decoder's output textures.
    coded: (u32, u32),
    converter: Option<Converter>,
    input_views: Vec<(ID3D11Texture2D, u32, ID3D11VideoProcessorInputView)>,
    pictures: i64,
}

/// Video processor set up for one picture size and rotation.
struct Converter {
    key: ((u32, u32), (i32, i32, i32, i32), u8),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    target_view: ID3D11VideoProcessorOutputView,
    target: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    size: (usize, usize),
    /// Quarter turns still to apply on the CPU when the GPU can't rotate.
    cpu_turns: u8,
}

impl HardwareDecoder {
    /// Opens the H.264 decoder on `gpu` (the discrete GPU when `None`).
    pub fn new(gpu: Option<GpuId>) -> Result<Self, BoxError> {
        start_media_foundation();
        let (device, gpu) = open_video_device(gpu)?;
        unsafe {
            let context = device.GetImmediateContext()?;
            let video_device: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext = context.cast()?;
            let (transform, mft_name) = activate_decoder()?;
            let attributes = transform.GetAttributes()?;
            if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(format!("{mft_name} 不支援 D3D11 硬體解碼").into());
            }
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                // Emit each picture as soon as it is decoded instead of buffering for reordering.
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
            }

            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or("no DXGI device manager")?;
            manager.ResetDevice(&device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0)?;

            let mut decoder = Self {
                name: format!("{mft_name} · {}", gpu.label()),
                gpu,
                device,
                context,
                video_device,
                video_context,
                transform,
                _manager: manager,
                crop: None,
                coded: (0, 0),
                converter: None,
                input_views: Vec::new(),
                pictures: 0,
            };
            // Usually only settles after the first picture (stream change); fine either way.
            let _ = decoder.select_output_type();
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let info = decoder.transform.GetOutputStreamInfo(0)?;
            if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                return Err("解碼器沒有輸出 GPU 畫面".into());
            }
            Ok(decoder)
        }
    }

    /// e.g. "Microsoft H264 Video Decoder MFT · NVIDIA GeForce RTX 3050（獨顯）".
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Decodes one Annex-B access unit (with SPS/PPS before keyframes).
    /// With `want_image`, returns the upright picture after `rotation`
    /// clockwise quarter turns; otherwise only advances the decoder.
    pub fn decode(&mut self, annex_b: &[u8], rotation: u8, want_image: bool) -> Result<Option<Frame>, BoxError> {
        unsafe {
            let sample = self.sample(annex_b)?;
            let mut picture = None;
            loop {
                match self.transform.ProcessInput(0, &sample, 0) {
                    Ok(()) => break,
                    // Full: collect what's ready, then offer the input again.
                    Err(e) if e.code() == MF_E_NOTACCEPTING => {
                        if !self.drain(rotation, want_image, &mut picture)? {
                            return Err("硬體解碼器不接受輸入".into());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            self.drain(rotation, want_image, &mut picture)?;
            Ok(picture)
        }
    }

    unsafe fn sample(&mut self, bytes: &[u8]) -> Result<IMFSample, BoxError> {
        let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(bytes.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        // Timestamps only need to increase; the iPad's own are carried separately.
        sample.SetSampleTime(self.pictures * 166_667)?;
        self.pictures += 1;
        Ok(sample)
    }

    /// Pulls every finished picture; keeps the last one as `picture`.
    /// Returns whether anything came out.
    unsafe fn drain(&mut self, rotation: u8, want_image: bool, picture: &mut Option<Frame>) -> Result<bool, BoxError> {
        let mut produced = false;
        loop {
            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = self.transform.ProcessOutput(0, &mut output, &mut status);
            let sample: Option<IMFSample> = ManuallyDrop::take(&mut output[0].pSample);
            drop(ManuallyDrop::take(&mut output[0].pEvents));
            match result {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(produced),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.select_output_type()?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            let Some(sample) = sample else { continue };
            produced = true;
            if want_image {
                *picture = Some(self.convert(&sample, rotation)?);
            }
        }
    }

    /// Picks NV12 output and records the picture's coded and visible size.
    unsafe fn select_output_type(&mut self) -> Result<(), BoxError> {
        let mut chosen: Option<IMFMediaType> = None;
        for i in 0.. {
            let Ok(available) = self.transform.GetOutputAvailableType(0, i) else { break };
            if available.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                chosen = Some(available);
                break;
            }
        }
        let output = chosen.ok_or("解碼器沒有 NV12 輸出")?;
        self.transform.SetOutputType(0, &output, 0)?;
        let size = output.GetUINT64(&MF_MT_FRAME_SIZE)?;
        self.coded = ((size >> 32) as u32, size as u32);
        let mut area = MFVideoArea::default();
        let mut length = 0;
        let bytes = std::slice::from_raw_parts_mut((&mut area as *mut MFVideoArea).cast::<u8>(), size_of::<MFVideoArea>());
        self.crop = output.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, Some(&mut length)).ok().and_then(|()| {
            let (x, y) = (i32::from(area.OffsetX.value), i32::from(area.OffsetY.value));
            (area.Area.cx > 0 && area.Area.cy > 0).then_some(RECT {
                left: x,
                top: y,
                right: x + area.Area.cx,
                bottom: y + area.Area.cy,
            })
        });
        self.input_views.clear();
        Ok(())
    }

    unsafe fn convert(&mut self, sample: &IMFSample, rotation: u8) -> Result<Frame, BoxError> {
        let buffer: IMFDXGIBuffer = sample.GetBufferByIndex(0)?.cast()?;
        let mut raw = std::ptr::null_mut();
        buffer.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
        let texture = ID3D11Texture2D::from_raw(raw);
        let slice = buffer.GetSubresourceIndex()?;

        let crop = self.crop.unwrap_or(RECT { left: 0, top: 0, right: self.coded.0 as i32, bottom: self.coded.1 as i32 });
        let key = (self.coded, (crop.left, crop.top, crop.right, crop.bottom), rotation & 3);
        if self.converter.as_ref().is_none_or(|c| c.key != key) {
            self.converter = Some(self.build_converter(key, crop)?);
            self.input_views.clear();
        }
        let view = self.input_view(&texture, slice)?;
        let converter = self.converter.as_ref().unwrap();

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(view)),
            ..Default::default()
        };
        let mut streams = [stream];
        let result = self.video_context.VideoProcessorBlt(&converter.processor, &converter.target_view, 0, &streams);
        ManuallyDrop::drop(&mut streams[0].pInputSurface);
        result?;

        self.context.CopyResource(&converter.staging, &converter.target);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        self.context.Map(&converter.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let (w, h) = converter.size;
        let pitch = mapped.RowPitch as usize;
        let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h);
        // BGRA bytes read as little-endian u32 are 0xAARRGGBB, i.e. 0RGB plus alpha.
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = &src[y * pitch..y * pitch + w * 4];
            pixels.extend(row.as_chunks::<4>().0.iter().map(|p| u32::from_le_bytes(*p)));
        }
        self.context.Unmap(&converter.staging, 0);

        let (width, height, pixels) = rotate(w, h, pixels, converter.cpu_turns);
        Ok(Frame { width, height, pixels, pts_us: 0 })
    }

    unsafe fn build_converter(
        &self,
        key: ((u32, u32), (i32, i32, i32, i32), u8),
        crop: RECT,
    ) -> Result<Converter, BoxError> {
        let (cw, ch) = ((crop.right - crop.left) as u32, (crop.bottom - crop.top) as u32);
        let turns = key.2;
        let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: key.0 .0,
            InputHeight: key.0 .1,
            OutputFrameRate: rate,
            OutputWidth: cw.max(ch),
            OutputHeight: cw.max(ch),
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = self.video_device.CreateVideoProcessorEnumerator(&content)?;
        let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
        enumerator.GetVideoProcessorCaps(&mut caps)?;
        let gpu_rotates = caps.FeatureCaps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION.0 as u32 != 0;
        let gpu_turns = if gpu_rotates { turns } else { 0 };
        let (ow, oh) = if gpu_turns % 2 == 1 { (ch, cw) } else { (cw, ch) };

        let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
        self.video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&crop));
        let full = RECT { left: 0, top: 0, right: ow as i32, bottom: oh as i32 };
        self.video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full));
        self.video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));
        if gpu_turns != 0 {
            // D3D11 rotations are clockwise, like ours.
            self.video_context.VideoProcessorSetStreamRotation(
                &processor,
                0,
                true,
                D3D11_VIDEO_PROCESSOR_ROTATION(i32::from(gpu_turns)),
            );
        }

        let desc = D3D11_TEXTURE2D_DESC {
            Width: ow,
            Height: oh,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut target = None;
        self.device.CreateTexture2D(&desc, None, Some(&mut target))?;
        let target = target.ok_or("no target texture")?;
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..desc
        };
        let mut staging = None;
        self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
        let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut target_view = None;
        self.video_device.CreateVideoProcessorOutputView(&target, &enumerator, &view_desc, Some(&mut target_view))?;
        Ok(Converter {
            key,
            enumerator,
            processor,
            target_view: target_view.ok_or("no output view")?,
            target,
            staging: staging.ok_or("no staging texture")?,
            size: (ow as usize, oh as usize),
            cpu_turns: turns - gpu_turns,
        })
    }

    /// Decoders hand out slices of a few texture arrays; views are reused per slice.
    unsafe fn input_view(&mut self, texture: &ID3D11Texture2D, slice: u32) -> Result<ID3D11VideoProcessorInputView, BoxError> {
        if let Some((_, _, view)) = self.input_views.iter().find(|(t, s, _)| t == texture && *s == slice) {
            return Ok(view.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice },
            },
        };
        let enumerator = &self.converter.as_ref().ok_or("no converter")?.enumerator;
        let mut view = None;
        self.video_device.CreateVideoProcessorInputView(texture, enumerator, &desc, Some(&mut view))?;
        let view = view.ok_or("no input view")?;
        self.input_views.push((texture.clone(), slice, view.clone()));
        Ok(view)
    }
}

impl Drop for HardwareDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        }
    }
}

/// Media Foundation's H.264 decoder (synchronous, DXVA-capable).
unsafe fn activate_decoder() -> Result<(IMFTransform, String), BoxError> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0;
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_DECODER,
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        Some(&input),
        Some(&output),
        &mut activates,
        &mut count,
    )?;
    let list: Vec<Option<IMFActivate>> = (0..count as usize).map(|i| std::ptr::read(activates.add(i))).collect();
    CoTaskMemFree(Some(activates as *const _));
    let mut last_error: BoxError = "找不到 H.264 解碼器".into();
    for activate in list.into_iter().flatten() {
        let name = friendly_name(&activate).unwrap_or_else(|| "H.264 decoder".into());
        match activate.ActivateObject::<IMFTransform>() {
            Ok(transform) => return Ok((transform, name)),
            Err(e) => last_error = format!("{name}：{e}").into(),
        }
    }
    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{DisplaySettings, ScreenImage, VideoPipeline};
    use crate::mirror::avcc_to_annex_b;
    use padbridge_protocol::Message;

    fn gradient(w: usize, h: usize, shift: usize) -> Vec<u8> {
        let mut bgra = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                bgra[i..i + 4].copy_from_slice(&[((x + shift) % 256) as u8, (y % 256) as u8, 200, 255]);
            }
        }
        bgra
    }

    /// Encodes `count` frames of `w`×`h` and returns their Annex-B access units.
    fn stream(w: usize, h: usize, count: usize) -> Vec<Vec<u8>> {
        let settings = DisplaySettings { max_width: w, ..DisplaySettings::default() };
        let mut pipeline = VideoPipeline::new(settings).unwrap();
        let mut units = Vec::new();
        let mut params = Vec::new();
        for i in 0..count {
            for m in pipeline.encode(&ScreenImage { width: w, height: h, bgra: &gradient(w, h, i * 2) }).unwrap() {
                match m {
                    Message::CodecParams { data, .. } => params = avcc_to_annex_b(&data).unwrap(),
                    Message::VideoFrame { keyframe, data, .. } => {
                        let mut unit = if keyframe { params.clone() } else { Vec::new() };
                        unit.extend(avcc_to_annex_b(&data).unwrap());
                        units.push(unit);
                    }
                    _ => {}
                }
            }
        }
        units
    }

    /// Needs a GPU with H.264 decoding (skipped otherwise):
    /// `cargo test --release -p padbridge-host hardware_decode -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn hardware_decode_matches_size_and_colour() {
        // Odd-height content checks cropping of the codec's 16-pixel padding.
        let (w, h) = (320, 210);
        let units = stream(w, h, 5);
        for gpu in crate::gpu::list_gpus() {
            let mut decoder = match HardwareDecoder::new(Some(gpu.id)) {
                Ok(d) => d,
                Err(e) => {
                    println!("{}: skipped ({e})", gpu.label());
                    continue;
                }
            };
            for turns in [0u8, 1] {
                let mut last = None;
                for unit in &units {
                    last = decoder.decode(unit, turns, true).unwrap().or(last);
                }
                let frame = last.expect("a decoded picture");
                let expected = if turns == 1 { (h, w) } else { (w, h) };
                assert_eq!((frame.width, frame.height), expected, "{}", decoder.name());
                // The source's red channel is a constant 200.
                let red = (frame.pixels[frame.pixels.len() / 2] >> 16) & 255;
                assert!((185..=215).contains(&red), "red {red} via {}", decoder.name());
            }
            println!("{}: ok", decoder.name());
        }
    }

    /// Low latency: every access unit must come out as a picture right away,
    /// or the newest picture would only show after the next one arrives.
    #[test]
    #[ignore]
    fn hardware_decode_has_no_delay() {
        let units = stream(320, 200, 6);
        let Some(gpu) = crate::gpu::list_gpus().into_iter().next() else { return };
        let mut decoder = HardwareDecoder::new(Some(gpu.id)).unwrap();
        let out: Vec<bool> = units.iter().map(|u| decoder.decode(u, 0, true).unwrap().is_some()).collect();
        assert!(out.iter().all(|&o| o), "pictures per input: {out:?}");
    }

    /// `cargo test --release -p padbridge-host bench_hardware_decode -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_hardware_decode() {
        let units = stream(1920, 1334, 60);
        for gpu in crate::gpu::list_gpus() {
            let Ok(mut decoder) = HardwareDecoder::new(Some(gpu.id)) else { continue };
            let started = std::time::Instant::now();
            for unit in &units {
                decoder.decode(unit, 1, true).unwrap();
            }
            let ms = started.elapsed().as_secs_f64() * 1000.0 / units.len() as f64;
            println!("{}: {ms:.1} ms/frame (decode + rotate + readback)", decoder.name());
        }
    }
}
