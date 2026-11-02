//! Hardware H.264 encoding on the GPU: a D3D11 video processor scales the
//! captured BGRA texture and converts it to NV12, and a Media Foundation
//! hardware encoder (NVIDIA / AMD / Intel MFT) encodes it. No pixels touch the
//! CPU until the compressed bitstream comes out.

use std::mem::ManuallyDrop;
use std::sync::Once;
use std::time::{Duration, Instant};

use padbridge_protocol::{Codec, Message};
use windows::core::{Interface, GUID};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_VIDEO_ENCODER, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Base, eAVEncH264VProfile_ConstrainedBase, ICodecAPI,
    IMFActivate, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFSample, IMFTransform, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFStartup, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, METransformHaveOutput,
    METransformNeedInput, MFSTARTUP_LITE, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT,
    MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;

use crate::display::{split_access_unit, DisplaySettings};
use crate::mirror::annex_b_to_avcc;
use crate::BoxError;

/// NV12 textures handed to the encoder in rotation, so a frame still being
/// read by the asynchronous encoder is never overwritten.
const RING: usize = 4;

pub struct HardwareEncoder {
    name: String,
    settings: DisplaySettings,
    out_size: (usize, usize),
    in_size: (usize, usize),
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_view: Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    targets: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next_target: usize,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    _manager: IMFDXGIDeviceManager,
    provides_samples: bool,
    output_buffer_size: u32,
    need_input: u32,
    params: Vec<u8>,
    start: Instant,
}

impl HardwareEncoder {
    /// Sets up GPU scaling from `in_size` and a hardware H.264 encoder on
    /// `device` (which must have video support and multithread protection).
    pub fn new(device: &ID3D11Device, in_size: (usize, usize), settings: DisplaySettings) -> Result<Self, BoxError> {
        static MF: Once = Once::new();
        MF.call_once(|| unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_LITE);
        });
        let out_size = output_size(in_size, settings.max_width);
        unsafe {
            let video_device: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext = device.GetImmediateContext()?.cast()?;
            let fps = DXGI_RATIONAL { Numerator: settings.max_fps, Denominator: 1 };
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: fps,
                InputWidth: in_size.0 as u32,
                InputHeight: in_size.1 as u32,
                OutputFrameRate: fps,
                OutputWidth: out_size.0 as u32,
                OutputHeight: out_size.1 as u32,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = video_device.CreateVideoProcessorEnumerator(&content)?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;

            let mut targets = Vec::with_capacity(RING);
            for _ in 0..RING {
                let texture = create_nv12(device, out_size)?;
                let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut view = None;
                video_device.CreateVideoProcessorOutputView(&texture, &enumerator, &desc, Some(&mut view))?;
                targets.push((texture, view.ok_or("no output view")?));
            }

            let (transform, name) = activate_encoder()?;
            let attributes = transform.GetAttributes()?;
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);

            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or("no DXGI device manager")?;
            manager.ResetDevice(device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            set_output_type(&transform, out_size, &settings)?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(out_size.0 as u32, out_size.1 as u32))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(settings.max_fps, 1))?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            transform.SetInputType(0, &input, 0)?;

            let codec_api: Option<ICodecAPI> = transform.cast().ok();
            if let Some(api) = &codec_api {
                // Best effort: encoders differ in what they accept.
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                let _ = api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32));
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(settings.bitrate_kbps * 1000));
                let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
                let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(settings.max_fps * 10));
            }

            let info = transform.GetOutputStreamInfo(0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let events: IMFMediaEventGenerator = transform.cast()?;

            Ok(Self {
                name,
                settings,
                out_size,
                in_size,
                video_device,
                video_context,
                enumerator,
                processor,
                input_view: None,
                targets,
                next_target: 0,
                transform,
                events,
                codec_api,
                _manager: manager,
                provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                output_buffer_size: info.cbSize.max(1 << 20),
                need_input: 0,
                params: Vec::new(),
                start: Instant::now(),
            })
        }
    }

    /// Encoder name as Windows reports it, e.g. "NVIDIA H.264 Encoder MFT".
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn input_size(&self) -> (usize, usize) {
        self.in_size
    }

    pub fn output_size(&self) -> (usize, usize) {
        self.out_size
    }

    pub fn force_keyframe(&mut self) {
        if let Some(api) = &self.codec_api {
            unsafe {
                let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
            }
        }
    }

    /// Encodes the BGRA `texture` (of `input_size`). Returns the messages for
    /// every frame the encoder emitted; normally exactly this one.
    pub fn encode(&mut self, texture: &ID3D11Texture2D) -> Result<Vec<Message>, BoxError> {
        unsafe {
            let (target, target_view) = self.targets[self.next_target].clone();
            self.next_target = (self.next_target + 1) % RING;
            let input_view = self.input_view_for(texture)?;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            };
            let mut streams = [stream];
            let result = self.video_context.VideoProcessorBlt(&self.processor, &target_view, 0, &streams);
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            result?;

            let sample = MFCreateSample()?;
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &target, 0, false)?;
            sample.AddBuffer(&buffer)?;
            let pts = (self.start.elapsed().as_nanos() / 100) as i64;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(10_000_000 / i64::from(self.settings.max_fps.max(1)))?;

            let mut messages = Vec::new();
            // Wait for the encoder to accept input, collecting finished frames meanwhile.
            let deadline = Instant::now() + Duration::from_secs(1);
            while self.need_input == 0 {
                if !self.pump_event(&mut messages)? {
                    if Instant::now() > deadline {
                        return Err("硬體編碼器沒有回應".into());
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            self.need_input -= 1;
            self.transform.ProcessInput(0, &sample, 0)?;

            // Low-latency mode emits each frame right away; give it a moment so
            // it ships now rather than with the next frame.
            let deadline = Instant::now() + Duration::from_millis(40);
            let had = messages.len();
            while messages.len() == had && Instant::now() < deadline {
                if !self.pump_event(&mut messages)? {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            Ok(messages)
        }
    }

    /// Handles one pending encoder event. Returns false when none was pending.
    unsafe fn pump_event(&mut self, messages: &mut Vec<Message>) -> Result<bool, BoxError> {
        let event = match self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
            Ok(event) => event,
            Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let kind = event.GetType()?;
        if kind == METransformNeedInput.0 as u32 {
            self.need_input += 1;
        } else if kind == METransformHaveOutput.0 as u32 {
            if let Some(bytes) = self.take_output()? {
                messages.extend(self.package(&bytes));
            }
        }
        Ok(true)
    }

    unsafe fn take_output(&mut self) -> Result<Option<Vec<u8>>, BoxError> {
        let sample = if self.provides_samples {
            None
        } else {
            let sample = MFCreateSample()?;
            sample.AddBuffer(&MFCreateMemoryBuffer(self.output_buffer_size)?)?;
            Some(sample)
        };
        let mut output = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0;
        let result = self.transform.ProcessOutput(0, &mut output, &mut status);
        let sample: Option<IMFSample> = ManuallyDrop::take(&mut output[0].pSample);
        drop(ManuallyDrop::take(&mut output[0].pEvents));
        match result {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder settled its output format; accept it and carry on.
                let available = self.transform.GetOutputAvailableType(0, 0)?;
                self.transform.SetOutputType(0, &available, 0)?;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
        let Some(sample) = sample else { return Ok(None) };
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = std::ptr::null_mut();
        let mut length = 0;
        buffer.Lock(&mut data, None, Some(&mut length))?;
        let bytes = std::slice::from_raw_parts(data, length as usize).to_vec();
        buffer.Unlock()?;
        Ok(Some(bytes))
    }

    fn package(&mut self, annex_b: &[u8]) -> Vec<Message> {
        let (params, picture, keyframe) = split_access_unit(&annex_b_to_avcc(annex_b));
        if !params.is_empty() {
            self.params = params;
        }
        if picture.is_empty() {
            return Vec::new();
        }
        let mut messages = Vec::with_capacity(3);
        if keyframe {
            messages.push(Message::StreamConfig {
                codec: Codec::H264,
                width: self.out_size.0 as u32,
                height: self.out_size.1 as u32,
                fps: self.settings.max_fps as u16,
                bitrate_kbps: self.settings.bitrate_kbps,
            });
            messages.push(Message::CodecParams { codec: Codec::H264, data: self.params.clone() });
        }
        let pts_us = self.start.elapsed().as_micros() as u64;
        messages.push(Message::VideoFrame { pts_us, keyframe, rotation: 0, data: picture });
        messages
    }

    unsafe fn input_view_for(&mut self, texture: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView, BoxError> {
        if let Some((cached, view)) = &self.input_view {
            if cached == texture {
                return Ok(view.clone());
            }
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut view = None;
        self.video_device.CreateVideoProcessorInputView(texture, &self.enumerator, &desc, Some(&mut view))?;
        let view = view.ok_or("no input view")?;
        self.input_view = Some((texture.clone(), view.clone()));
        Ok(view)
    }
}

impl Drop for HardwareEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        }
    }
}

/// Same scaling rule as the software pipeline: fit `max_width`, even sizes.
fn output_size((w, h): (usize, usize), max_width: usize) -> (usize, usize) {
    let scale = (max_width as f64 / w as f64).min(1.0);
    let even = |v: f64| ((v.round() as usize) & !1).max(2);
    (even(w as f64 * scale), even(h as f64 * scale))
}

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn create_nv12(device: &ID3D11Device, (w, h): (usize, usize)) -> Result<ID3D11Texture2D, BoxError> {
    let mut desc = D3D11_TEXTURE2D_DESC {
        Width: w as u32,
        Height: h as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    if device.CreateTexture2D(&desc, None, Some(&mut texture)).is_err() {
        // Some drivers reject the encoder bind flag; render target alone works there.
        desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
    }
    Ok(texture.ok_or("NV12 texture was not created")?)
}

/// Finds and activates the first hardware H.264 encoder (NV12 in).
unsafe fn activate_encoder() -> Result<(IMFTransform, String), BoxError> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0;
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        Some(&input),
        Some(&output),
        &mut activates,
        &mut count,
    )?;
    let list: Vec<Option<IMFActivate>> =
        (0..count as usize).map(|i| std::ptr::read(activates.add(i))).collect();
    CoTaskMemFree(Some(activates as *const _));
    let mut last_error: BoxError = "找不到硬體 H.264 編碼器".into();
    for activate in list.into_iter().flatten() {
        let name = friendly_name(&activate).unwrap_or_else(|| "Hardware H.264 encoder".into());
        match activate.ActivateObject::<IMFTransform>() {
            Ok(transform) => return Ok((transform, name)),
            Err(e) => last_error = format!("{name}：{e}").into(),
        }
    }
    Err(last_error)
}

unsafe fn friendly_name(activate: &IMFActivate) -> Option<String> {
    let mut text = windows::core::PWSTR::null();
    let mut length = 0;
    activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut length).ok()?;
    let name = text.to_string().ok();
    CoTaskMemFree(Some(text.0 as *const _));
    name
}

unsafe fn set_output_type(transform: &IMFTransform, (w, h): (usize, usize), settings: &DisplaySettings) -> Result<(), BoxError> {
    // Constrained Baseline keeps the stream decodable by OpenH264 (Windows-side
    // tools and tests) as well as by the iPad.
    let profiles: [(GUID, u32); 2] = [
        (MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_ConstrainedBase.0 as u32),
        (MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32),
    ];
    let mut last = None;
    for (key, profile) in profiles {
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate_kbps * 1000)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32))?;
        output.SetUINT64(&MF_MT_FRAME_RATE, pack(settings.max_fps, 1))?;
        output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        output.SetUINT32(&key, profile)?;
        match transform.SetOutputType(0, &output, 0) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map(Into::into).unwrap_or_else(|| "no output type".into()))
}

#[allow(dead_code)]
const _: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS = MF_EVENT_FLAG_NO_WAIT;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{list_monitors, Capturer};

    /// Encodes each monitor's current image repeatedly to measure the GPU
    /// path's throughput independent of screen activity. Needs a desktop and a
    /// hardware encoder:
    /// `cargo test --release -p padbridge-host bench_hardware -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_hardware() {
        for monitor in list_monitors().unwrap() {
            let mut capturer = Capturer::new(monitor.index, false).unwrap();
            while !capturer.poll(100).unwrap().image {}
            let mut encoder = HardwareEncoder::new(capturer.device(), capturer.size(), DisplaySettings::default()).unwrap();
            let texture = capturer.texture().unwrap().clone();
            let (mut frames, mut keyframes) = (0, 0);
            let started = Instant::now();
            for i in 0..120 {
                if i % 30 == 0 {
                    encoder.force_keyframe();
                }
                for message in encoder.encode(&texture).unwrap() {
                    if let Message::VideoFrame { keyframe, .. } = message {
                        frames += 1;
                        keyframes += usize::from(keyframe);
                    }
                }
            }
            let per_frame = started.elapsed().as_secs_f64() * 1000.0 / 120.0;
            println!(
                "{} → {:?}: {per_frame:.2} ms/frame, {frames} frames out ({keyframes} keyframes), {}",
                monitor.label(),
                encoder.output_size(),
                encoder.name()
            );
            assert!(frames >= 110, "encoder dropped frames: {frames}/120");
        }
    }
}
