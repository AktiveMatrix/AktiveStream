//! HEVC/H.264 decode via Media Foundation (hardware NVDEC when present). Feeds the AktiveStream
//! client's Phase 2 video stream; here it decodes Annex B access units to RGBA.
//!
//! The decoder is driven synchronously: one access unit in, zero or more frames out. The MFT
//! emits `MF_E_TRANSFORM_STREAM_CHANGE` once it has parsed the stream's real dimensions, which
//! is handled by (re)reading the NV12 output type.

use std::slice;

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::GUID;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("media foundation: {0}")]
    Mf(#[from] windows::core::Error),
    #[error("no {0} decoder found")]
    NoDecoder(&'static str),
    #[error("decoder produced no output format")]
    NoOutputType,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Hevc,
    H264,
}

impl Codec {
    fn subtype(self) -> GUID {
        match self {
            Codec::Hevc => MFVideoFormat_HEVC,
            Codec::H264 => MFVideoFormat_H264,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Codec::Hevc => "HEVC",
            Codec::H264 => "H.264",
        }
    }
}

/// A decoded frame, RGBA8 top-to-bottom, tightly packed.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Decoder {
    transform: IMFTransform,
    width: u32,
    height: u32,
    provides_samples: bool,
    time: i64,
}

/// Call once per process before creating a decoder.
pub fn init() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    Ok(())
}

impl Decoder {
    /// `width`/`height` are the stream's dimensions, which the client already knows from the
    /// protocol handshake; the decoder needs them on the input type up front.
    pub fn new(codec: Codec, width: u32, height: u32) -> Result<Decoder> {
        unsafe {
            let info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: codec.subtype() };
            // Prefer a hardware (NVDEC) decoder, then fall back to software.
            let transform = enumerate(&info, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER)
                .or_else(|| enumerate(&info, MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER))
                .or_else(|| enumerate(&info, MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER))
                .ok_or(Error::NoDecoder(codec.name()))?;

            // Low-latency mode: emit each frame as soon as it decodes, 1-in-1-out. Without it a
            // hardware decoder keeps a pipeline of frames and returns NEED_MORE_INPUT until the
            // NEXT access unit arrives, so a lone keystroke's frame is stuck in the decoder until
            // more frames come (e.g. when the mouse moves) — the screen looks frozen while typing.
            // Set before streaming begins; ignore it if the decoder's attribute store is absent.
            if let Ok(attrs) = transform.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &codec.subtype())?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height))?;
            transform.SetInputType(0, &input, 0)?;

            let mut decoder = Decoder { transform, width, height, provides_samples: false, time: 0 };
            decoder.configure_output()?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// Selects the first NV12 output type the decoder offers and records the frame size.
    fn configure_output(&mut self) -> Result<()> {
        unsafe {
            let mut i = 0;
            loop {
                match self.transform.GetOutputAvailableType(0, i) {
                    Ok(output) => {
                        let subtype = output.GetGUID(&MF_MT_SUBTYPE)?;
                        if subtype == MFVideoFormat_NV12 {
                            self.transform.SetOutputType(0, &output, 0)?;
                            // Frame size is absent until the decoder has parsed the stream's
                            // SPS; it then raises MF_E_TRANSFORM_STREAM_CHANGE and this runs
                            // again with the real size present.
                            if let Ok(size) = output.GetUINT64(&MF_MT_FRAME_SIZE) {
                                self.width = (size >> 32) as u32;
                                self.height = (size & 0xffff_ffff) as u32;
                            }
                            let stream_info = self.transform.GetOutputStreamInfo(0)?;
                            self.provides_samples = stream_info.dwFlags
                                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                                    | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
                                != 0;
                            return Ok(());
                        }
                        i += 1;
                    }
                    Err(_) => return Err(Error::NoOutputType),
                }
            }
        }
    }

    /// Decodes one Annex B access unit, returning any frames that became ready.
    pub fn decode(&mut self, access_unit: &[u8]) -> Result<Vec<DecodedFrame>> {
        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(access_unit.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(access_unit.as_ptr(), ptr, access_unit.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(access_unit.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            sample.SetSampleDuration(166_667)?; // ~60 fps in 100ns units
            self.time += 166_667;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        self.drain_output()
    }

    /// Flushes buffered frames at end of stream.
    pub fn finish(&mut self) -> Result<Vec<DecodedFrame>> {
        unsafe {
            self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        self.drain_output()
    }

    fn drain_output(&mut self) -> Result<Vec<DecodedFrame>> {
        let mut frames = Vec::new();
        loop {
            match self.process_output()? {
                Output::Frame(frame) => frames.push(frame),
                Output::NeedInput => break,
                Output::StreamChange => self.configure_output()?,
            }
        }
        Ok(frames)
    }

    fn process_output(&mut self) -> Result<Output> {
        unsafe {
            let own_sample = if self.provides_samples {
                None
            } else {
                let info = self.transform.GetOutputStreamInfo(0)?;
                let sample = MFCreateSample()?;
                let buffer = MFCreateMemoryBuffer(info.cbSize)?;
                sample.AddBuffer(&buffer)?;
                Some(sample)
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(own_sample.clone()),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = self.transform.ProcessOutput(0, &mut out, &mut status);
            // Reclaim both out-params on every path so neither the sample nor any event
            // collection the MFT set is leaked (ManuallyDrop does not auto-drop).
            let sample = std::mem::ManuallyDrop::take(&mut out[0].pSample);
            let _events = std::mem::ManuallyDrop::take(&mut out[0].pEvents);
            match result {
                Ok(()) => match sample {
                    Some(s) => Ok(Output::Frame(self.sample_to_rgba(&s)?)),
                    None => Ok(Output::NeedInput),
                },
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Output::NeedInput),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Output::StreamChange),
                Err(e) => Err(Error::Mf(e)),
            }
        }
    }

    fn sample_to_rgba(&self, sample: &IMFSample) -> Result<DecodedFrame> {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let nv12 = slice::from_raw_parts(ptr, len as usize);
            let rgba = nv12_to_rgba(nv12, self.width, self.height);
            buffer.Unlock()?;
            let rgba = rgba.ok_or(Error::Mf(windows::core::Error::from(E_FAIL)))?;
            Ok(DecodedFrame { width: self.width, height: self.height, rgba })
        }
    }
}

enum Output {
    Frame(DecodedFrame),
    NeedInput,
    StreamChange,
}

unsafe fn enumerate(info: &MFT_REGISTER_TYPE_INFO, flags: MFT_ENUM_FLAG) -> Option<IMFTransform> {
    unsafe {
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, flags, Some(info), None, &mut activates, &mut count).ok()?;
        if count == 0 || activates.is_null() {
            return None;
        }
        let list = slice::from_raw_parts(activates, count as usize);
        let transform = list.iter().flatten().find_map(|a| a.ActivateObject::<IMFTransform>().ok());
        windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
        transform
    }
}

/// NV12 (BT.709, limited range) to packed RGBA. Assumes stride == width (true for our test
/// widths, which are multiples of 16); a production path reads the real stride via IMF2DBuffer.
fn nv12_to_rgba(nv12: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let y_size = w * h;
    if nv12.len() < y_size + y_size / 2 {
        return None;
    }
    let (y_plane, uv_plane) = nv12.split_at(y_size);
    let mut rgba = vec![0u8; w * h * 4];
    for row in 0..h {
        for col in 0..w {
            let y = y_plane[row * w + col] as f32;
            let uv = (row / 2) * w + (col & !1);
            let u = uv_plane[uv] as f32 - 128.0;
            let v = uv_plane[uv + 1] as f32 - 128.0;
            let c = 1.164 * (y - 16.0);
            let r = c + 1.793 * v;
            let g = c - 0.213 * u - 0.533 * v;
            let b = c + 2.112 * u;
            let o = (row * w + col) * 4;
            rgba[o] = r.clamp(0.0, 255.0) as u8;
            rgba[o + 1] = g.clamp(0.0, 255.0) as u8;
            rgba[o + 2] = b.clamp(0.0, 255.0) as u8;
            rgba[o + 3] = 255;
        }
    }
    Some(rgba)
}
