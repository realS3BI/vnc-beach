//! Layout-dependent virtual keycodes for characters (macOS only).
//!
//! Shortcuts like Cmd+C have to arrive as a press of the key that types `c`
//! in the current keyboard layout. Only the Text Input Sources API knows that
//! key, and since macOS 26 it asserts that it runs on the main queue: enigo's
//! own lookup for `Key::Unicode` kills the input thread. So the layout is
//! read on the main queue and matched here.

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

/// How long the input thread waits for the main queue. A main thread that
/// never services its queue must not hang input forever.
const MAIN_QUEUE_TIMEOUT: Duration = Duration::from_secs(1);

/// The virtual keycode that types `c` (with or without Shift) in the current
/// keyboard layout. `None` if no key does, or the main queue didn't answer.
pub fn keycode_for(c: char) -> Option<u16> {
    let table = on_main_queue(layout)??;
    find(&table, c)
}

/// What one key types without and with Shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyChars {
    code: u16,
    plain: Option<char>,
    shifted: Option<char>,
}

/// Prefers the key that types `c` unshifted, then shifted, then ignoring
/// case. Lower keycodes win, so the main block beats the keypad.
fn find(table: &[KeyChars], c: char) -> Option<u16> {
    let same_letter = |o: char| o.to_lowercase().eq(c.to_lowercase());
    table
        .iter()
        .find(|k| k.plain == Some(c))
        .or_else(|| table.iter().find(|k| k.shifted == Some(c)))
        .or_else(|| {
            table
                .iter()
                .find(|k| [k.plain, k.shifted].into_iter().flatten().any(same_letter))
        })
        .map(|k| k.code)
}

/// Runs `work` on the main queue and waits for its result, at most
/// [`MAIN_QUEUE_TIMEOUT`].
fn on_main_queue<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    extern "C" {
        fn pthread_main_np() -> i32;
    }
    // SAFETY: plain libc call without arguments.
    if unsafe { pthread_main_np() } != 0 {
        return Some(work());
    }
    let (tx, rx) = mpsc::sync_channel(1);
    dispatch2::DispatchQueue::main().exec_async(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(MAIN_QUEUE_TIMEOUT) {
        Ok(result) => Some(result),
        Err(_) => {
            log::warn!("main queue didn't answer the keyboard layout lookup");
            None
        }
    }
}

#[repr(C)]
struct TISInputSource {
    _private: [u8; 0],
}
type TISInputSourceRef = *const TISInputSource;
type CFTypeRef = *const c_void;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    // "Copy": returned with +1 retain, released with CFRelease.
    fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
    fn TISCopyCurrentKeyboardLayoutInputSource() -> TISInputSourceRef;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> TISInputSourceRef;
    // "Get": a CFDataRef owned by the input source.
    fn TISGetInputSourceProperty(source: TISInputSourceRef, key: CFTypeRef) -> CFTypeRef;
    static kTISPropertyUnicodeKeyLayoutData: CFTypeRef;
    fn UCKeyTranslate(
        layout: *const u8,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> i32;
    fn LMGetKbdType() -> u8;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFRelease(cf: CFTypeRef);
}

const K_UC_KEY_ACTION_DISPLAY: u16 = 3;
const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK: u32 = 1;
/// `shiftKey >> 8`, the form UCKeyTranslate takes modifiers in.
const SHIFT: u32 = 0x02;

/// What keycodes 0..128 type in the current layout. Main queue only.
fn layout() -> Option<Vec<KeyChars>> {
    // Some input methods (e.g. Japanese) have no layout data themselves;
    // fall back like enigo does.
    let sources: [unsafe extern "C" fn() -> TISInputSourceRef; 3] = [
        TISCopyCurrentKeyboardInputSource,
        TISCopyCurrentKeyboardLayoutInputSource,
        TISCopyCurrentASCIICapableKeyboardLayoutInputSource,
    ];
    for copy in sources {
        // SAFETY: Text Input Sources calls on the main queue; the layout data
        // belongs to `source` and is only read before `source` is released.
        unsafe {
            let source = copy();
            if source.is_null() {
                continue;
            }
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            let table = (!data.is_null()).then(|| {
                let layout = CFDataGetBytePtr(data);
                let kbd_type = LMGetKbdType() as u32;
                (0..128)
                    .map(|code| KeyChars {
                        code,
                        plain: translate(layout, code, 0, kbd_type),
                        shifted: translate(layout, code, SHIFT, kbd_type),
                    })
                    .collect()
            });
            CFRelease(source.cast());
            if table.is_some() {
                return table;
            }
        }
    }
    log::warn!("no keyboard layout data");
    None
}

/// The single character a key types, if it types exactly one.
///
/// # Safety
/// `layout` must point to valid `uchr` layout data.
unsafe fn translate(layout: *const u8, code: u16, modifiers: u32, kbd_type: u32) -> Option<char> {
    let mut dead_keys = 0;
    let mut buf = [0u16; 4];
    let mut len = 0;
    let status = UCKeyTranslate(
        layout,
        code,
        K_UC_KEY_ACTION_DISPLAY,
        modifiers,
        kbd_type,
        K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK,
        &mut dead_keys,
        buf.len(),
        &mut len,
        buf.as_mut_ptr(),
    );
    if status != 0 {
        return None;
    }
    let mut chars = char::decode_utf16(buf[..len.min(buf.len())].iter().copied());
    match (chars.next(), chars.next()) {
        (Some(Ok(c)), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u16, plain: char, shifted: char) -> KeyChars {
        KeyChars {
            code,
            plain: Some(plain),
            shifted: Some(shifted),
        }
    }

    #[test]
    fn finds_keys_in_a_german_layout() {
        let table = [
            key(6, 'y', 'Y'),
            key(16, 'z', 'Z'),
            key(26, '7', '/'),
            key(39, 'ä', 'Ä'),
            key(89, '7', '7'),
        ];
        assert_eq!(find(&table, 'z'), Some(16));
        assert_eq!(find(&table, 'y'), Some(6));
        assert_eq!(find(&table, 'Y'), Some(6));
        assert_eq!(find(&table, '/'), Some(26));
        assert_eq!(find(&table, 'Ä'), Some(39));
        // Main block before keypad.
        assert_eq!(find(&table, '7'), Some(26));
        assert_eq!(find(&table, 'q'), None);
    }

    #[test]
    fn falls_back_to_ignoring_case() {
        // French AZERTY: the 2 key types é, Shift gives 2; no key types É.
        let table = [key(19, 'é', '2')];
        assert_eq!(find(&table, 'É'), Some(19));
        assert_eq!(find(&table, '2'), Some(19));
    }

    #[test]
    fn unshifted_match_beats_shifted() {
        let table = [key(10, 'x', '1'), key(18, '1', '!')];
        assert_eq!(find(&table, '1'), Some(18));
    }

    #[test]
    fn lookup_off_the_main_thread_returns() {
        // Test threads aren't the main thread and nothing services the main
        // queue here, so this has to give up after the timeout.
        let start = std::time::Instant::now();
        let _ = keycode_for('a');
        assert!(start.elapsed() < MAIN_QUEUE_TIMEOUT * 3);
    }
}
