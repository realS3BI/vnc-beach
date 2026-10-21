//! C API for the Swift app; declared in `include/beach_mobile.h`.
//!
//! Every function is safe to call from any thread. Event callbacks run on the
//! reader thread and stop for good once `beach_session_free` returns, so the
//! caller may release the context afterwards.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

use rfb::messages::ClientMessage;
use rfb::PixelFormat;

use crate::client::{parse_address, Connection, Event, RemoteKind};

mod preparation;
pub use preparation::*;

pub const EVENT_FRAME: i32 = 0;
pub const EVENT_CLIPBOARD: i32 = 1;
pub const EVENT_CLOSED: i32 = 2;
pub const EVENT_RESIZED: i32 = 3;

pub const REMOTE_OTHER: i32 = 0;
pub const REMOTE_BEACH_MAC: i32 = 1;
pub const REMOTE_APPLE_SCREEN_SHARING: i32 = 2;

pub type EventCallback = unsafe extern "C" fn(context: *mut c_void, kind: i32, text: *const c_char);
pub type FrameCallback = unsafe extern "C" fn(
    context: *mut c_void,
    rgba: *const u8,
    width: u32,
    height: u32,
    dirty_x: u32,
    dirty_y: u32,
    dirty_width: u32,
    dirty_height: u32,
);

/// Raw pointer handed to us by Swift; we only pass it back.
#[derive(Clone, Copy)]
struct Context(*mut c_void);
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

/// Cleared by `beach_session_free`, under the lock, so no callback outlives it.
type Sink = Arc<Mutex<Option<(EventCallback, Context)>>>;

pub struct BeachSession {
    conn: Connection,
    sink: Sink,
}

fn emit(sink: &Sink, event: Event) {
    let guard = sink.lock().unwrap();
    let Some((cb, ctx)) = *guard else {
        return;
    };
    let (kind, text) = match event {
        Event::Frame => (EVENT_FRAME, None),
        Event::Resized => (EVENT_RESIZED, None),
        Event::Clipboard(t) => (EVENT_CLIPBOARD, Some(t)),
        Event::Closed(t) => (EVENT_CLOSED, Some(t)),
    };
    let text = text.map(|t| CString::new(t.replace('\0', "")).unwrap());
    let ptr = text.as_ref().map_or(std::ptr::null(), |t| t.as_ptr());
    unsafe { cb(ctx.0, kind, ptr) };
}

unsafe fn c_str<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// Copies `s` into `buf` (NUL-terminated, truncated to fit).
unsafe fn fill(buf: *mut c_char, len: usize, s: &str) {
    if buf.is_null() || len == 0 {
        return;
    }
    let mut end = s.len().min(len - 1);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, end);
        *buf.add(end) = 0;
    }
}

/// Connects and authenticates (blocking). `password` may be NULL or empty.
/// `depth` is 8, 16 or 24 bits per pixel. Returns NULL on failure and writes
/// the reason into `error`.
///
/// # Safety
/// Pointers must be valid C strings / buffers; `context` is passed to
/// `callback` unchanged.
#[no_mangle]
pub unsafe extern "C" fn beach_session_connect(
    address: *const c_char,
    password: *const c_char,
    depth: u8,
    callback: Option<EventCallback>,
    context: *mut c_void,
    error: *mut c_char,
    error_len: usize,
) -> *mut BeachSession {
    let Some(address) = (unsafe { c_str(address) }) else {
        unsafe { fill(error, error_len, "address is not valid UTF-8") };
        return std::ptr::null_mut();
    };
    let password = unsafe { c_str(password) }.filter(|p| !p.is_empty());
    let pf = match depth {
        8 => PixelFormat::BGR233,
        16 => PixelFormat::RGB565,
        _ => PixelFormat::BGRX,
    };
    let sink: Sink = Arc::new(Mutex::new(callback.map(|cb| (cb, Context(context)))));
    let events = sink.clone();
    match Connection::open(&parse_address(address), password, pf, move |e| emit(&events, e)) {
        Ok(conn) => Box::into_raw(Box::new(BeachSession { conn, sink })),
        Err(e) => {
            unsafe { fill(error, error_len, &e.to_string()) };
            std::ptr::null_mut()
        }
    }
}

/// Closes the connection. No callback runs after this returns.
///
/// # Safety
/// `session` must come from `beach_session_connect` and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn beach_session_free(session: *mut BeachSession) {
    if session.is_null() {
        return;
    }
    let session = unsafe { Box::from_raw(session) };
    *session.sink.lock().unwrap() = None;
    drop(session);
}

unsafe fn with<'a>(session: *mut BeachSession) -> Option<&'a BeachSession> {
    unsafe { session.as_ref() }
}

/// Desktop name from the server's ServerInit.
///
/// # Safety
/// `buf` must have room for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn beach_session_name(session: *mut BeachSession, buf: *mut c_char, len: usize) {
    if let Some(s) = unsafe { with(session) } {
        unsafe { fill(buf, len, &s.conn.name) };
    }
}

/// What the server is (`BEACH_REMOTE_*`), relevant for modifier mapping.
///
/// # Safety
/// `session` must be a live session.
#[no_mangle]
pub unsafe extern "C" fn beach_session_remote_kind(session: *mut BeachSession) -> i32 {
    match unsafe { with(session) }.map(|s| s.conn.remote) {
        Some(RemoteKind::BeachMac) => REMOTE_BEACH_MAC,
        Some(RemoteKind::AppleScreenSharing) => REMOTE_APPLE_SCREEN_SHARING,
        _ => REMOTE_OTHER,
    }
}

/// Current framebuffer size in pixels.
///
/// # Safety
/// Out pointers may be NULL.
#[no_mangle]
pub unsafe extern "C" fn beach_session_size(session: *mut BeachSession, width: *mut u32, height: *mut u32) {
    let (w, h) = unsafe { with(session) }.map_or((0, 0), |s| s.conn.size());
    unsafe {
        if !width.is_null() {
            *width = w as u32;
        }
        if !height.is_null() {
            *height = h as u32;
        }
    }
}

/// If pixels changed since the last call, invokes `callback` once with the
/// whole RGBA buffer (row stride `width * 4`) and the changed rectangle, while
/// holding the framebuffer lock, and returns true.
///
/// # Safety
/// `callback` must not call back into this session.
#[no_mangle]
pub unsafe extern "C" fn beach_session_take_frame(
    session: *mut BeachSession,
    callback: Option<FrameCallback>,
    context: *mut c_void,
) -> bool {
    let (Some(s), Some(cb)) = (unsafe { with(session) }, callback) else {
        return false;
    };
    let mut fb = s.conn.fb.lock().unwrap();
    let Some([x, y, w, h]) = fb.take_dirty() else {
        return false;
    };
    unsafe {
        cb(
            context,
            fb.rgba.as_ptr(),
            fb.width as u32,
            fb.height as u32,
            x as u32,
            y as u32,
            w as u32,
            h as u32,
        )
    };
    true
}

/// One of the server's displays inside the framebuffer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BeachScreen {
    pub id: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Copies up to `max` displays into `out` and returns how many the server
/// has (0 if it didn't say). Query again after `BEACH_EVENT_RESIZED`.
///
/// # Safety
/// `out` must have room for `max` entries (or be NULL with `max` 0).
#[no_mangle]
pub unsafe extern "C" fn beach_session_screens(
    session: *mut BeachSession,
    out: *mut BeachScreen,
    max: usize,
) -> usize {
    let Some(s) = (unsafe { with(session) }) else {
        return 0;
    };
    let screens = s.conn.screens();
    if !out.is_null() {
        for (i, screen) in screens.iter().take(max).enumerate() {
            unsafe {
                *out.add(i) = BeachScreen {
                    id: screen.id,
                    x: screen.x as u32,
                    y: screen.y as u32,
                    width: screen.width as u32,
                    height: screen.height as u32,
                };
            }
        }
    }
    screens.len()
}

/// Asks the server only for `x, y, width, height` from now on (one display)
/// and fetches it in full. `width` or `height` 0 goes back to everything.
///
/// # Safety
/// `session` must be a live session.
#[no_mangle]
pub unsafe extern "C" fn beach_session_set_region(
    session: *mut BeachSession,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) {
    if let Some(s) = unsafe { with(session) } {
        let region =
            (width > 0 && height > 0).then_some((x as usize, y as usize, width as usize, height as usize));
        s.conn.set_region(region);
    }
}

/// Sends a pointer event. `buttons` is the RFB button mask (bit 0 left,
/// bit 1 middle, bit 2 right, bits 3–6 wheel up/down/left/right).
///
/// # Safety
/// `session` must be a live session.
#[no_mangle]
pub unsafe extern "C" fn beach_session_pointer(session: *mut BeachSession, buttons: u8, x: u16, y: u16) {
    if let Some(s) = unsafe { with(session) } {
        s.conn.send(&[ClientMessage::PointerEvent { buttons, x, y }]);
    }
}

/// Clicks a wheel "button" (press and release) at `x, y`, keeping `buttons` held.
///
/// # Safety
/// `session` must be a live session.
#[no_mangle]
pub unsafe extern "C" fn beach_session_wheel(
    session: *mut BeachSession,
    buttons: u8,
    wheel_bit: u8,
    x: u16,
    y: u16,
) {
    if let Some(s) = unsafe { with(session) } {
        s.conn.send(&[
            ClientMessage::PointerEvent {
                buttons: buttons | wheel_bit,
                x,
                y,
            },
            ClientMessage::PointerEvent { buttons, x, y },
        ]);
    }
}

/// Sends an X11 keysym press or release.
///
/// # Safety
/// `session` must be a live session.
#[no_mangle]
pub unsafe extern "C" fn beach_session_key(session: *mut BeachSession, keysym: u32, down: bool) {
    if let Some(s) = unsafe { with(session) } {
        s.conn.send(&[ClientMessage::KeyEvent { down, key: keysym }]);
    }
}

/// Sends clipboard text to the server.
///
/// # Safety
/// `text` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn beach_session_cut_text(session: *mut BeachSession, text: *const c_char) {
    if let (Some(s), Some(text)) = (unsafe { with(session) }, unsafe { c_str(text) }) {
        s.conn.send(&[ClientMessage::ClientCutText(text.to_string())]);
    }
}

/// Requests the whole screen again (after the app was suspended).
///
/// # Safety
/// `session` must be a live session.
#[no_mangle]
pub unsafe extern "C" fn beach_session_refresh(session: *mut BeachSession) {
    if let Some(s) = unsafe { with(session) } {
        s.conn.refresh();
    }
}

/// Keysym for a Unicode scalar, as the server expects it.
#[no_mangle]
pub extern "C" fn beach_keysym_for_char(scalar: u32) -> u32 {
    char::from_u32(scalar).map_or(0, rfb::keysym::from_char)
}

/// Normalises user input like `host:1` or `host::5901` to `host:port`.
///
/// # Safety
/// `buf` must have room for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn beach_normalize_address(input: *const c_char, buf: *mut c_char, len: usize) {
    let input = unsafe { c_str(input) }.unwrap_or("");
    unsafe { fill(buf, len, &parse_address(input)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_truncates_at_char_boundary() {
        let mut buf = [1 as c_char; 6];
        unsafe { fill(buf.as_mut_ptr(), buf.len(), "Zurück") };
        let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
        assert_eq!(s, "Zurü");
        unsafe { fill(buf.as_mut_ptr(), 5, "Zurück") };
        assert_eq!(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(), "Zur");
        unsafe { fill(buf.as_mut_ptr(), buf.len(), "ab") };
        assert_eq!(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(), "ab");
    }

    #[test]
    fn keysyms() {
        assert_eq!(beach_keysym_for_char('a' as u32), 0x61);
        assert_eq!(beach_keysym_for_char('ü' as u32), 0xfc);
        assert_eq!(beach_keysym_for_char('€' as u32), 0x0100_20ac);
        assert_eq!(beach_keysym_for_char(0xd800), 0);
    }

    #[test]
    fn connect_failure_reports_error() {
        let addr = CString::new("127.0.0.1::1").unwrap();
        let mut err = [0 as c_char; 128];
        let s = unsafe {
            beach_session_connect(
                addr.as_ptr(),
                std::ptr::null(),
                24,
                None,
                std::ptr::null_mut(),
                err.as_mut_ptr(),
                err.len(),
            )
        };
        assert!(s.is_null());
        let msg = unsafe { CStr::from_ptr(err.as_ptr()) }.to_str().unwrap();
        assert!(!msg.is_empty(), "error message expected");
        unsafe { beach_session_free(s) };
    }
}
