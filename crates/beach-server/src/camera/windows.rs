//! Windows webcam for the viewer: Media Foundation reads the first camera
//! as NV12 and the synchronous (software) H.264 encoder MFT compresses it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{bail, Context as _};
use rfb::camera::Video;
use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;

use super::h264;
use super::source::Event;

pub struct Source {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Source {
    /// Opens the first camera on a thread of its own and reports through
    /// `events`. Setting `key_frame` makes the next frame a key frame.
    pub fn start(
        max_width: u16,
        max_height: u16,
        fps: u8,
        key_frame: Arc<AtomicBool>,
        mut events: impl FnMut(Event) + Send + 'static,
    ) -> Source {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("webcam".into())
            .spawn({
                let (stop, key_frame) = (stop.clone(), key_frame.clone());
                move || {
                    let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                        .ok()
                        .context("CoInitializeEx")
                        .and_then(|()| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.context("MFStartup"))
                        .and_then(|()| {
                            let r = unsafe { capture(max_width, max_height, fps, &stop, &key_frame, &mut events) };
                            unsafe {
                                let _ = MFShutdown();
                            }
                            r
                        });
                    unsafe { CoUninitialize() };
                    if let Err(e) = result {
                        if !stop.load(Ordering::Acquire) {
                            log::warn!("webcam: {e:#}");
                            events(Event::Failed(format!("{e:#}")));
                        }
                    }
                }
            })
            .ok();
        Source { stop, thread }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // ReadSample returns with the next camera frame, well under a second.
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn size_of_type(t: &IMFMediaType) -> (u32, u32) {
    let v = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
    ((v >> 32) as u32, v as u32)
}

fn rate_of_type(t: &IMFMediaType) -> f64 {
    let v = unsafe { t.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(0);
    let (num, den) = ((v >> 32) as u32, v as u32);
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

unsafe fn allocated_string(a: &IMFActivate, key: &GUID) -> Option<String> {
    let mut s = PWSTR::null();
    let mut len = 0;
    a.GetAllocatedString(key, &mut s, &mut len).ok()?;
    let out = s.to_string().ok();
    CoTaskMemFree(Some(s.0 as _));
    out
}

/// The first video capture device and its name.
unsafe fn first_camera() -> anyhow::Result<(IMFMediaSource, String)> {
    let mut attrs = None;
    MFCreateAttributes(&mut attrs, 1)?;
    let attrs = attrs.context("MFCreateAttributes")?;
    attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
    let mut list = std::ptr::null_mut();
    let mut count = 0;
    MFEnumDeviceSources(&attrs, &mut list, &mut count)?;
    let devices: Vec<Option<IMFActivate>> = (0..count as usize).map(|i| std::ptr::read(list.add(i))).collect();
    CoTaskMemFree(Some(list as _));
    let Some(device) = devices.into_iter().flatten().next() else {
        bail!("Keine Kamera gefunden");
    };
    let name = allocated_string(&device, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
    let source = device.ActivateObject::<IMFMediaSource>().context("Kamera belegt oder nicht verfügbar")?;
    Ok((source, name))
}

/// The camera's own format closest to `max` at a usable frame rate.
unsafe fn pick_native(reader: &IMFSourceReader, max: (u32, u32), fps: f64) -> Option<IMFMediaType> {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let mut best: Option<((bool, u32, bool), IMFMediaType)> = None;
    for i in 0.. {
        let Ok(t) = reader.GetNativeMediaType(stream, i) else { break };
        let (w, h) = size_of_type(&t);
        let rate = rate_of_type(&t);
        if w == 0 || h == 0 {
            continue;
        }
        let fits = w <= max.0 && h <= max.1;
        let score = (fits, if fits { w * h } else { u32::MAX - w * h }, rate >= fps.min(25.0));
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, t));
        }
    }
    best.map(|(_, t)| t)
}

unsafe fn video_type(subtype: &GUID, (w, h): (u32, u32), rate: u64) -> anyhow::Result<IMFMediaType> {
    let t = MFCreateMediaType()?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
    t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
    t.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
    t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    Ok(t)
}

/// The synchronous H.264 encoder (Microsoft's software MFT).
unsafe fn encoder() -> anyhow::Result<IMFTransform> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut list = std::ptr::null_mut();
    let mut count = 0;
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        None,
        Some(&output),
        &mut list,
        &mut count,
    )?;
    let encoders: Vec<Option<IMFActivate>> = (0..count as usize).map(|i| std::ptr::read(list.add(i))).collect();
    CoTaskMemFree(Some(list as _));
    let activate = encoders.into_iter().flatten().next().context("no H.264 encoder")?;
    Ok(activate.ActivateObject::<IMFTransform>()?)
}

unsafe fn sample_bytes(sample: &IMFSample) -> anyhow::Result<Vec<u8>> {
    let buffer = sample.ConvertToContiguousBuffer()?;
    let mut data = std::ptr::null_mut();
    let mut len = 0;
    buffer.Lock(&mut data, None, Some(&mut len))?;
    let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
    buffer.Unlock()?;
    Ok(bytes)
}

unsafe fn capture(
    max_width: u16,
    max_height: u16,
    fps: u8,
    stop: &AtomicBool,
    key_frame: &AtomicBool,
    events: &mut impl FnMut(Event),
) -> anyhow::Result<()> {
    let (source, name) = first_camera()?;
    log::info!("webcam: {name}");
    let mut attrs = None;
    MFCreateAttributes(&mut attrs, 1)?;
    let attrs = attrs.context("MFCreateAttributes")?;
    // Decodes MJPG cameras and converts to NV12.
    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
    let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)?;
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let native = pick_native(&reader, (max_width.into(), max_height.into()), fps.into())
        .context("the camera offers no video format")?;
    reader.SetCurrentMediaType(stream, None, &native)?;
    let (w, h) = size_of_type(&native);
    // Even dimensions for NV12 and H.264 macroblocks.
    let (w, h) = (w & !1, h & !1);
    let rate = native.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(fps.into(), 1));
    reader
        .SetCurrentMediaType(stream, None, &video_type(&MFVideoFormat_NV12, (w, h), rate)?)
        .context("the camera can't deliver NV12")?;
    log::info!("webcam: {w}x{h} at {:.1} fps", rate_of_type(&native));

    let encoder = encoder()?;
    let codec: Option<ICodecAPI> = encoder.cast().ok();
    let bitrate = (u64::from(w) * u64::from(h) * 30 / 10).clamp(500_000, 6_000_000) as u32;
    let out_type = video_type(&MFVideoFormat_H264, (w, h), rate)?;
    out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
    out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
    encoder.SetOutputType(0, &out_type, 0).context("H.264 output type")?;
    encoder
        .SetInputType(0, &video_type(&MFVideoFormat_NV12, (w, h), rate)?, 0)
        .context("H.264 input type")?;
    if let Some(codec) = &codec {
        let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
        let _ = codec.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate));
        let _ = codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(u32::from(fps) * 4));
    }
    // SPS/PPS, for encoders that don't repeat them in front of key frames.
    let header = encoder
        .GetOutputCurrentType(0)
        .ok()
        .and_then(|t| {
            let len = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
            let mut buf = vec![0; len as usize];
            t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut buf, None).ok()?;
            Some(buf)
        })
        .unwrap_or_default();
    encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
    encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    let info = encoder.GetOutputStreamInfo(0)?;
    let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
    events(Event::Started {
        name,
        width: w as u16,
        height: h as u16,
    });

    while !stop.load(Ordering::Acquire) {
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample = None;
        reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
        if flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32 != 0 {
            bail!("Die Kamera hat die Aufnahme beendet");
        }
        let Some(sample) = sample else { continue };
        if key_frame.swap(false, Ordering::AcqRel) {
            if let Some(codec) = &codec {
                let _ = codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
            }
        }
        encoder.ProcessInput(0, &sample, 0)?;
        loop {
            let out_sample = if provides {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1 << 20))?)?;
                Some(s)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(out_sample),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = encoder.ProcessOutput(0, &mut buffers, &mut status);
            let produced = std::mem::ManuallyDrop::take(&mut buffers[0].pSample);
            drop(std::mem::ManuallyDrop::take(&mut buffers[0].pEvents));
            match result {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    let t = encoder.GetOutputAvailableType(0, 0)?;
                    encoder.SetOutputType(0, &t, 0)?;
                    continue;
                }
                Err(e) => return Err(e).context("H.264 encoder"),
            }
            let Some(produced) = produced else { continue };
            let mut data = sample_bytes(&produced)?;
            if data.is_empty() {
                continue;
            }
            let key = h264::is_key_frame(&data);
            if key
                && !header.is_empty()
                && !h264::nal_units(&data).iter().any(|u| h264::nal_type(u) == h264::NAL_SPS)
            {
                data.splice(0..0, header.iter().copied());
            }
            let time = produced.GetSampleTime().unwrap_or(ts);
            events(Event::Video(Video {
                key_frame: key,
                timestamp_us: (time.max(0) / 10) as u64,
                data,
            }));
        }
    }
    let _ = encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
    let _ = source.Shutdown();
    Ok(())
}
