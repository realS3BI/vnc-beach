//! macOS output: decodes the viewer's H.264 with VideoToolbox, scales it into
//! the camera format and hands it to the sink stream of the VNC Beach camera
//! extension (`apps/macos-camera`) through CoreMediaIO.

#![allow(non_upper_case_globals, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::{c_char, c_void};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _};
use rfb::camera::Video;

use super::h264::{self, NAL_PPS, NAL_SPS};
use super::{FPS, HEIGHT, WIDTH};

/// `deviceID` of the extension's device (apps/macos-camera/Extension/main.swift).
const DEVICE_UID: &str = "6F3B1D52-6C0E-4D8C-9C1A-5B7E2A4F9D10";

type OSStatus = i32;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CMIOObjectID = u32;
type CMSimpleQueueRef = *mut c_void;
type CMFormatDescriptionRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;
type CVPixelBufferRef = *mut c_void;
type CVPixelBufferPoolRef = *mut c_void;
type VTSessionRef = *mut c_void;

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

const INVALID_TIME: CMTime = CMTime {
    value: 0,
    timescale: 0,
    flags: 0,
    epoch: 0,
};

#[repr(C)]
struct SampleTiming {
    duration: CMTime,
    presentation: CMTime,
    decode: CMTime,
}

#[repr(C)]
struct DecompressionCallback {
    callback: extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CVPixelBufferRef, CMTime, CMTime),
    refcon: *mut c_void,
}

const fn cc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const SYSTEM_OBJECT: CMIOObjectID = 1;
const SCOPE_GLOBAL: u32 = cc(b"glob");
const HARDWARE_DEVICES: u32 = cc(b"dev#");
const DEVICE_UID_PROP: u32 = cc(b"uid ");
const DEVICE_STREAMS: u32 = cc(b"stm#");
const STREAM_DIRECTION: u32 = cc(b"sdir");
/// Custom property of the sink stream: "1" while an app reads the camera.
const STREAM_DEMAND: u32 = cc(b"bcdm");
const PIXEL_BGRA: u32 = cc(b"BGRA");
const UTF8: u32 = 0x0800_0100;
const NUMBER_SINT32: isize = 3;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: CFAllocatorRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFDataGetLength(d: CFTypeRef) -> isize;
    fn CFDataGetBytePtr(d: CFTypeRef) -> *const u8;
    fn CFNumberCreate(alloc: CFAllocatorRef, kind: isize, value: *const c_void) -> CFTypeRef;
    fn CFDictionaryCreate(
        alloc: CFAllocatorRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
}

#[link(name = "CoreMediaIO", kind = "framework")]
extern "C" {
    fn CMIOObjectGetPropertyDataSize(
        object: CMIOObjectID,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> OSStatus;
    fn CMIOObjectGetPropertyData(
        object: CMIOObjectID,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        used: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
    fn CMIOStreamCopyBufferQueue(
        stream: CMIOObjectID,
        altered: extern "C" fn(CMIOObjectID, *mut c_void, *mut c_void),
        refcon: *mut c_void,
        queue: *mut CMSimpleQueueRef,
    ) -> OSStatus;
    fn CMIODeviceStartStream(device: CMIOObjectID, stream: CMIOObjectID) -> OSStatus;
    fn CMIODeviceStopStream(device: CMIOObjectID, stream: CMIOObjectID) -> OSStatus;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSimpleQueueEnqueue(queue: CMSimpleQueueRef, element: *const c_void) -> OSStatus;
    fn CMSimpleQueueGetCount(queue: CMSimpleQueueRef) -> i32;
    fn CMSimpleQueueGetCapacity(queue: CMSimpleQueueRef) -> i32;
    fn CMClockGetHostTimeClock() -> *const c_void;
    fn CMClockGetTime(clock: *const c_void) -> CMTime;
    fn CMVideoFormatDescriptionCreateForImageBuffer(
        alloc: CFAllocatorRef,
        image: CVPixelBufferRef,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        alloc: CFAllocatorRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    fn CMSampleBufferCreateForImageBuffer(
        alloc: CFAllocatorRef,
        image: CVPixelBufferRef,
        data_ready: u8,
        callback: *const c_void,
        refcon: *const c_void,
        format: CMFormatDescriptionRef,
        timing: *const SampleTiming,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        alloc: CFAllocatorRef,
        memory: *mut c_void,
        length: usize,
        block_allocator: CFAllocatorRef,
        source: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(
        source: *const c_void,
        block: CMBlockBufferRef,
        offset: usize,
        length: usize,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        alloc: CFAllocatorRef,
        data: CMBlockBufferRef,
        format: CMFormatDescriptionRef,
        samples: isize,
        timings: isize,
        timing: *const SampleTiming,
        sizes: isize,
        size: *const usize,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;
    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    fn CVPixelBufferPoolCreate(
        alloc: CFAllocatorRef,
        pool_attrs: CFDictionaryRef,
        buffer_attrs: CFDictionaryRef,
        out: *mut CVPixelBufferPoolRef,
    ) -> i32;
    fn CVPixelBufferPoolCreatePixelBuffer(
        alloc: CFAllocatorRef,
        pool: CVPixelBufferPoolRef,
        out: *mut CVPixelBufferRef,
    ) -> i32;
    fn CVPixelBufferRetain(buffer: CVPixelBufferRef) -> CVPixelBufferRef;
    fn CVPixelBufferLockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buffer: CVPixelBufferRef) -> *mut u8;
    fn CVPixelBufferGetBytesPerRow(buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(buffer: CVPixelBufferRef) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    static kVTPixelTransferPropertyKey_ScalingMode: CFStringRef;
    static kVTScalingMode_Letterbox: CFStringRef;
    fn VTDecompressionSessionCreate(
        alloc: CFAllocatorRef,
        format: CMFormatDescriptionRef,
        decoder_spec: CFDictionaryRef,
        image_attrs: CFDictionaryRef,
        callback: *const DecompressionCallback,
        out: *mut VTSessionRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: VTSessionRef,
        sample: CMSampleBufferRef,
        flags: u32,
        refcon: *mut c_void,
        info: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: VTSessionRef) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: VTSessionRef);
    fn VTPixelTransferSessionCreate(alloc: CFAllocatorRef, out: *mut VTSessionRef) -> OSStatus;
    fn VTPixelTransferSessionTransferImage(
        session: VTSessionRef,
        source: CVPixelBufferRef,
        destination: CVPixelBufferRef,
    ) -> OSStatus;
    fn VTSessionSetProperty(session: VTSessionRef, key: CFStringRef, value: CFTypeRef) -> OSStatus;
}

fn address(selector: u32) -> PropertyAddress {
    PropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: 0,
    }
}

fn check(status: OSStatus, what: &str) -> anyhow::Result<()> {
    if status != 0 {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

/// An array property such as the device or stream list.
unsafe fn ids(object: CMIOObjectID, selector: u32) -> anyhow::Result<Vec<CMIOObjectID>> {
    let addr = address(selector);
    let mut size = 0;
    check(
        CMIOObjectGetPropertyDataSize(object, &addr, 0, null(), &mut size),
        "CMIOObjectGetPropertyDataSize",
    )?;
    let mut ids = vec![0u32; size as usize / 4];
    let mut used = 0;
    check(
        CMIOObjectGetPropertyData(object, &addr, 0, null(), size, &mut used, ids.as_mut_ptr().cast()),
        "CMIOObjectGetPropertyData",
    )?;
    ids.truncate(used as usize / 4);
    Ok(ids)
}

unsafe fn u32_property(object: CMIOObjectID, selector: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut used = 0;
    let status = CMIOObjectGetPropertyData(object, &address(selector), 0, null(), 4, &mut used, (&raw mut value).cast());
    (status == 0).then_some(value)
}

unsafe fn cf_string(s: CFStringRef) -> Option<String> {
    let mut buf = [0 as c_char; 256];
    if CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, UTF8) == 0 {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
}

unsafe fn device_uid(device: CMIOObjectID) -> Option<String> {
    let mut uid: CFStringRef = null();
    let mut used = 0;
    let status = CMIOObjectGetPropertyData(
        device,
        &address(DEVICE_UID_PROP),
        0,
        null(),
        size_of::<CFStringRef>() as u32,
        &mut used,
        (&raw mut uid).cast(),
    );
    if status != 0 || uid.is_null() {
        return None;
    }
    let s = cf_string(uid);
    CFRelease(uid);
    s
}

/// The extension's demand flag. A CFString/CFData reference or raw bytes,
/// depending on how CoreMediaIO hands out custom properties.
unsafe fn demand(stream: CMIOObjectID) -> Option<bool> {
    let addr = address(STREAM_DEMAND);
    let mut size = 0;
    if CMIOObjectGetPropertyDataSize(stream, &addr, 0, null(), &mut size) != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let mut used = 0;
    if CMIOObjectGetPropertyData(stream, &addr, 0, null(), size, &mut used, buf.as_mut_ptr().cast()) != 0 {
        return None;
    }
    buf.truncate(used as usize);
    if buf.len() == size_of::<CFTypeRef>() {
        let obj = usize::from_ne_bytes(buf[..].try_into().ok()?) as CFTypeRef;
        if !obj.is_null() {
            let kind = CFGetTypeID(obj);
            let value = if kind == CFStringGetTypeID() {
                cf_string(obj).map(|s| s == "1")
            } else if kind == CFDataGetTypeID() {
                let len = CFDataGetLength(obj) as usize;
                let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(obj), len);
                Some(bytes.first().is_some_and(|&b| b == b'1' || b == 1))
            } else {
                None
            };
            CFRelease(obj);
            return value;
        }
    }
    Some(buf.first().is_some_and(|&b| b == b'1' || b == 1))
}

unsafe fn find_device() -> anyhow::Result<CMIOObjectID> {
    ids(SYSTEM_OBJECT, HARDWARE_DEVICES)?
        .into_iter()
        .find(|&d| device_uid(d).is_some_and(|uid| uid.eq_ignore_ascii_case(DEVICE_UID)))
        .context("the VNC Beach camera extension is not installed")
}

/// Whether the extension's camera device exists.
pub fn installed() -> bool {
    unsafe { find_device() }.is_ok()
}

extern "C" fn queue_altered(_stream: CMIOObjectID, _token: *mut c_void, _refcon: *mut c_void) {}

/// CoreFoundation object released on drop.
struct Cf(*const c_void);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

unsafe fn dictionary(pairs: &[(CFStringRef, CFTypeRef)]) -> Cf {
    let keys: Vec<_> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<_> = pairs.iter().map(|p| p.1).collect();
    Cf(CFDictionaryCreate(
        kCFAllocatorDefault,
        keys.as_ptr(),
        values.as_ptr(),
        pairs.len() as isize,
        &raw const kCFTypeDictionaryKeyCallBacks,
        &raw const kCFTypeDictionaryValueCallBacks,
    ))
}

unsafe fn number(v: i32) -> Cf {
    Cf(CFNumberCreate(kCFAllocatorDefault, NUMBER_SINT32, (&raw const v).cast()))
}

enum Job {
    Video(Video),
    Reset,
}

pub struct Output {
    jobs: SyncSender<Job>,
    demand: Arc<AtomicBool>,
    key_frame: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

struct Sink {
    device: CMIOObjectID,
    stream: CMIOObjectID,
    queue: CMSimpleQueueRef,
}

// The CoreMediaIO objects are plain IDs and the queue is thread safe.
unsafe impl Send for Sink {}

impl Output {
    pub fn open() -> anyhow::Result<Output> {
        let sink = unsafe { Self::find_sink()? };
        log::info!("virtual camera found (device {}, sink stream {})", sink.device, sink.stream);
        let demand = Arc::new(AtomicBool::new(false));
        let key_frame = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (jobs, rx) = sync_channel(4);
        let stream = sink.stream;
        std::thread::Builder::new().name("camera-demand".into()).spawn({
            let (demand, stop) = (demand.clone(), stop.clone());
            move || {
                let mut warned = false;
                while !stop.load(Ordering::Acquire) {
                    // Without the custom property (older extension), assume
                    // an app is always watching.
                    let now = unsafe { self::demand(stream) }.unwrap_or_else(|| {
                        if !warned {
                            log::warn!("virtual camera doesn't report readers; streaming whenever shared");
                            warned = true;
                        }
                        true
                    });
                    if demand.swap(now, Ordering::AcqRel) != now {
                        log::info!("virtual camera {}", if now { "in use" } else { "idle" });
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        })?;
        std::thread::Builder::new().name("camera-decode".into()).spawn({
            let key_frame = key_frame.clone();
            move || decode_loop(sink, rx, key_frame)
        })?;
        Ok(Output {
            jobs,
            demand,
            key_frame,
            stop,
        })
    }

    unsafe fn find_sink() -> anyhow::Result<Sink> {
        let device = find_device()?;
        let streams = ids(device, DEVICE_STREAMS)?;
        // The sink is the stream data flows into (direction 0, "output");
        // the extension adds it second.
        let stream = streams
            .iter()
            .copied()
            .find(|&s| u32_property(s, STREAM_DIRECTION) == Some(0))
            .or(streams.get(1).copied())
            .context("the virtual camera has no sink stream")?;
        let mut queue = null_mut();
        check(
            CMIOStreamCopyBufferQueue(stream, queue_altered, null_mut(), &mut queue),
            "CMIOStreamCopyBufferQueue",
        )?;
        check(CMIODeviceStartStream(device, stream), "CMIODeviceStartStream")?;
        Ok(Sink { device, stream, queue })
    }

    /// An application reads the camera.
    pub fn demand(&self) -> bool {
        self.demand.load(Ordering::Acquire)
    }

    pub fn push(&self, video: Video) {
        if let Err(TrySendError::Full(_)) = self.jobs.try_send(Job::Video(video)) {
            // The decoder fell behind; dropping a frame breaks the reference
            // chain, so start over at the next key frame.
            log::debug!("camera decoder busy, dropping a frame");
            self.key_frame.store(true, Ordering::Release);
            let _ = self.jobs.try_send(Job::Reset);
        }
    }

    /// A new viewer stream begins; forget the old decoder state.
    pub fn reset(&self) {
        let _ = self.jobs.try_send(Job::Reset);
    }

    pub fn take_key_frame_request(&self) -> bool {
        self.key_frame.swap(false, Ordering::AcqRel)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// VideoToolbox state for one H.264 stream.
struct Decoder {
    session: VTSessionRef,
    format: CMFormatDescriptionRef,
    sps: Vec<u8>,
    pps: Vec<u8>,
    /// Filled by the output callback, which VideoToolbox calls before
    /// `DecodeFrame` returns for synchronous decoding.
    decoded: Box<CVPixelBufferRef>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            VTDecompressionSessionInvalidate(self.session);
            CFRelease(self.session);
            CFRelease(self.format);
        }
    }
}

extern "C" fn decoded(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: OSStatus,
    _flags: u32,
    image: CVPixelBufferRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    let slot = refcon as *mut CVPixelBufferRef;
    unsafe {
        if status == 0 && !image.is_null() {
            if !(*slot).is_null() {
                CFRelease(*slot);
            }
            *slot = CVPixelBufferRetain(image);
        } else {
            log::debug!("camera frame failed to decode ({status})");
        }
    }
}

impl Decoder {
    unsafe fn new(sps: &[u8], pps: &[u8]) -> anyhow::Result<Decoder> {
        let mut format = null_mut();
        let sets = [sps.as_ptr(), pps.as_ptr()];
        let sizes = [sps.len(), pps.len()];
        check(
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                kCFAllocatorDefault,
                2,
                sets.as_ptr(),
                sizes.as_ptr(),
                4,
                &mut format,
            ),
            "CMVideoFormatDescriptionCreateFromH264ParameterSets",
        )?;
        let mut decoded_slot: Box<CVPixelBufferRef> = Box::new(null_mut());
        let callback = DecompressionCallback {
            callback: decoded,
            refcon: (&mut *decoded_slot as *mut CVPixelBufferRef).cast(),
        };
        let mut session = null_mut();
        let status = VTDecompressionSessionCreate(kCFAllocatorDefault, format, null(), null(), &callback, &mut session);
        if status != 0 {
            CFRelease(format);
            bail!("VTDecompressionSessionCreate failed ({status})");
        }
        Ok(Decoder {
            session,
            format,
            sps: sps.to_vec(),
            pps: pps.to_vec(),
            decoded: decoded_slot,
        })
    }

    /// Decodes one access unit (length-prefixed slices) into a pixel buffer.
    unsafe fn decode(&mut self, avcc: &[u8]) -> anyhow::Result<Option<CVPixelBufferRef>> {
        let mut block = null_mut();
        check(
            CMBlockBufferCreateWithMemoryBlock(
                kCFAllocatorDefault,
                null_mut(),
                avcc.len(),
                kCFAllocatorDefault,
                null(),
                0,
                avcc.len(),
                0,
                &mut block,
            ),
            "CMBlockBufferCreateWithMemoryBlock",
        )?;
        let block = Cf(block);
        check(
            CMBlockBufferReplaceDataBytes(avcc.as_ptr().cast(), block.0 as _, 0, avcc.len()),
            "CMBlockBufferReplaceDataBytes",
        )?;
        let mut sample = null_mut();
        let size = avcc.len();
        check(
            CMSampleBufferCreateReady(
                kCFAllocatorDefault,
                block.0 as _,
                self.format,
                1,
                0,
                null(),
                1,
                &size,
                &mut sample,
            ),
            "CMSampleBufferCreateReady",
        )?;
        let sample = Cf(sample);
        let mut info = 0;
        check(
            VTDecompressionSessionDecodeFrame(self.session, sample.0 as _, 0, null_mut(), &mut info),
            "VTDecompressionSessionDecodeFrame",
        )?;
        VTDecompressionSessionWaitForAsynchronousFrames(self.session);
        let image = std::mem::replace(&mut *self.decoded, null_mut());
        Ok((!image.is_null()).then_some(image))
    }
}

/// Scales decoded pictures into camera-sized BGRA buffers and queues them.
struct Writer {
    sink: Sink,
    transfer: VTSessionRef,
    pool: CVPixelBufferPoolRef,
}

impl Writer {
    unsafe fn new(sink: Sink) -> anyhow::Result<Writer> {
        let mut transfer = null_mut();
        check(VTPixelTransferSessionCreate(kCFAllocatorDefault, &mut transfer), "VTPixelTransferSessionCreate")?;
        VTSessionSetProperty(transfer, kVTPixelTransferPropertyKey_ScalingMode, kVTScalingMode_Letterbox);
        let surface = dictionary(&[]);
        let (format, width, height) = (number(PIXEL_BGRA as i32), number(WIDTH as i32), number(HEIGHT as i32));
        let attrs = dictionary(&[
            (kCVPixelBufferPixelFormatTypeKey, format.0),
            (kCVPixelBufferWidthKey, width.0),
            (kCVPixelBufferHeightKey, height.0),
            (kCVPixelBufferIOSurfacePropertiesKey, surface.0),
        ]);
        let mut pool = null_mut();
        let status = CVPixelBufferPoolCreate(kCFAllocatorDefault, null(), attrs.0, &mut pool);
        if status != 0 {
            CFRelease(transfer);
            bail!("CVPixelBufferPoolCreate failed ({status})");
        }
        Ok(Writer { sink, transfer, pool })
    }

    unsafe fn write(&mut self, image: CVPixelBufferRef) -> anyhow::Result<()> {
        let queue = self.sink.queue;
        if CMSimpleQueueGetCount(queue) >= CMSimpleQueueGetCapacity(queue) {
            return Ok(()); // the extension is behind; skip this picture
        }
        let mut out = null_mut();
        check(
            CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, self.pool, &mut out),
            "CVPixelBufferPoolCreatePixelBuffer",
        )?;
        let out = Cf(out);
        // Black bars where the picture's aspect ratio differs from 16:9.
        CVPixelBufferLockBaseAddress(out.0 as _, 0);
        let base = CVPixelBufferGetBaseAddress(out.0 as _);
        if !base.is_null() {
            let len = CVPixelBufferGetBytesPerRow(out.0 as _) * CVPixelBufferGetHeight(out.0 as _);
            std::slice::from_raw_parts_mut(base, len)
                .chunks_exact_mut(4)
                .for_each(|px| px.copy_from_slice(&[0, 0, 0, 255]));
        }
        CVPixelBufferUnlockBaseAddress(out.0 as _, 0);
        check(
            VTPixelTransferSessionTransferImage(self.transfer, image, out.0 as _),
            "VTPixelTransferSessionTransferImage",
        )?;
        let mut format = null_mut();
        check(
            CMVideoFormatDescriptionCreateForImageBuffer(kCFAllocatorDefault, out.0 as _, &mut format),
            "CMVideoFormatDescriptionCreateForImageBuffer",
        )?;
        let format = Cf(format);
        let timing = SampleTiming {
            duration: CMTime {
                value: 1,
                timescale: FPS as i32,
                flags: 1,
                epoch: 0,
            },
            presentation: CMClockGetTime(CMClockGetHostTimeClock()),
            decode: INVALID_TIME,
        };
        let mut sample = null_mut();
        check(
            CMSampleBufferCreateForImageBuffer(
                kCFAllocatorDefault,
                out.0 as _,
                1,
                null(),
                null(),
                format.0 as _,
                &timing,
                &mut sample,
            ),
            "CMSampleBufferCreateForImageBuffer",
        )?;
        // The queue takes over our reference; the extension releases it.
        if CMSimpleQueueEnqueue(queue, sample) != 0 {
            CFRelease(sample);
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe {
            CMIODeviceStopStream(self.sink.device, self.sink.stream);
            CFRelease(self.transfer);
            CFRelease(self.pool);
        }
    }
}

fn decode_loop(sink: Sink, jobs: Receiver<Job>, key_frame: Arc<AtomicBool>) {
    let mut writer = match unsafe { Writer::new(sink) } {
        Ok(w) => w,
        Err(e) => {
            log::error!("virtual camera: {e:#}");
            return;
        }
    };
    let mut decoder: Option<Decoder> = None;
    // After a reset or error, wait for a key frame before decoding again.
    let mut synced = false;
    let mut avcc = Vec::new();
    while let Ok(job) = jobs.recv() {
        let video = match job {
            Job::Reset => {
                synced = false;
                continue;
            }
            Job::Video(v) => v,
        };
        if !synced && !video.key_frame {
            key_frame.store(true, Ordering::Release);
            continue;
        }
        let units = h264::nal_units(&video.data);
        let sps = units.iter().find(|u| h264::nal_type(u) == NAL_SPS);
        let pps = units.iter().find(|u| h264::nal_type(u) == NAL_PPS);
        if let (Some(sps), Some(pps)) = (sps, pps) {
            if decoder.as_ref().is_none_or(|d| d.sps != *sps || d.pps != *pps) {
                decoder = None;
                match unsafe { Decoder::new(sps, pps) } {
                    Ok(d) => {
                        log::info!("camera stream started");
                        decoder = Some(d);
                    }
                    Err(e) => log::warn!("camera decoder: {e:#}"),
                }
            }
        }
        let Some(dec) = decoder.as_mut() else {
            key_frame.store(true, Ordering::Release);
            continue;
        };
        avcc.clear();
        for unit in units.iter().filter(|u| !matches!(h264::nal_type(u), NAL_SPS | NAL_PPS)) {
            avcc.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            avcc.extend_from_slice(unit);
        }
        if avcc.is_empty() {
            continue;
        }
        match unsafe { dec.decode(&avcc) } {
            Ok(Some(image)) => {
                synced = true;
                let result = unsafe { writer.write(image) };
                unsafe { CFRelease(image) };
                if let Err(e) = result {
                    log::warn!("virtual camera: {e:#}");
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::debug!("camera: {e:#}");
                synced = false;
                key_frame.store(true, Ordering::Release);
            }
        }
    }
}
