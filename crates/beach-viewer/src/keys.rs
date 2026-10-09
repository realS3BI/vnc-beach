//! egui keys → X11 keysyms.

use egui::Key;
use rfb::keysym::*;

/// Keys that never produce text.
pub fn special(key: Key) -> Option<u32> {
    Some(match key {
        Key::ArrowDown => DOWN,
        Key::ArrowLeft => LEFT,
        Key::ArrowRight => RIGHT,
        Key::ArrowUp => UP,
        Key::Escape => ESCAPE,
        Key::Tab => TAB,
        Key::Backspace => BACKSPACE,
        Key::Enter => RETURN,
        Key::Insert => INSERT,
        Key::Delete => DELETE,
        Key::Home => HOME,
        Key::End => END,
        Key::PageUp => PAGE_UP,
        Key::PageDown => PAGE_DOWN,
        Key::F1 => F1,
        Key::F2 => F1 + 1,
        Key::F3 => F1 + 2,
        Key::F4 => F1 + 3,
        Key::F5 => F1 + 4,
        Key::F6 => F1 + 5,
        Key::F7 => F1 + 6,
        Key::F8 => F1 + 7,
        Key::F9 => F1 + 8,
        Key::F10 => F1 + 9,
        Key::F11 => F1 + 10,
        Key::F12 => F1 + 11,
        _ => return None,
    })
}

/// Keysym for a character key pressed together with Ctrl/Cmd, where the
/// platform delivers no text. Letters are sent lowercase, like a real keyboard.
pub fn shortcut(key: Key) -> Option<u32> {
    if key == Key::Space {
        return Some(0x20);
    }
    let mut chars = key.symbol_or_name().chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(from_char(c.to_ascii_lowercase())),
        _ => None,
    }
}
