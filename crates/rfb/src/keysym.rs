//! X11 keysyms used by RFB key events.

pub const BACKSPACE: u32 = 0xff08;
pub const TAB: u32 = 0xff09;
pub const RETURN: u32 = 0xff0d;
pub const ESCAPE: u32 = 0xff1b;
pub const INSERT: u32 = 0xff63;
pub const DELETE: u32 = 0xffff;
pub const HOME: u32 = 0xff50;
pub const END: u32 = 0xff57;
pub const PAGE_UP: u32 = 0xff55;
pub const PAGE_DOWN: u32 = 0xff56;
pub const LEFT: u32 = 0xff51;
pub const UP: u32 = 0xff52;
pub const RIGHT: u32 = 0xff53;
pub const DOWN: u32 = 0xff54;
pub const F1: u32 = 0xffbe;
pub const SHIFT_L: u32 = 0xffe1;
pub const SHIFT_R: u32 = 0xffe2;
pub const CONTROL_L: u32 = 0xffe3;
pub const CONTROL_R: u32 = 0xffe4;
pub const CAPS_LOCK: u32 = 0xffe5;
pub const META_L: u32 = 0xffe7;
pub const META_R: u32 = 0xffe8;
pub const ALT_L: u32 = 0xffe9;
pub const ALT_R: u32 = 0xffea;
pub const SUPER_L: u32 = 0xffeb;
pub const SUPER_R: u32 = 0xffec;
pub const ISO_LEVEL3_SHIFT: u32 = 0xfe03;

/// Keysym for a printable character: Latin-1 maps directly, everything else
/// uses the Unicode range `0x0100_0000 + code point`.
pub fn from_char(c: char) -> u32 {
    match c as u32 {
        cp @ (0x20..=0x7e | 0xa0..=0xff) => cp,
        cp => 0x0100_0000 | cp,
    }
}

/// Inverse of [`from_char`] for printable keysyms.
pub fn to_char(keysym: u32) -> Option<char> {
    match keysym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(keysym),
        0x0100_0000..=0x0110_ffff => char::from_u32(keysym - 0x0100_0000),
        _ => None,
    }
}
