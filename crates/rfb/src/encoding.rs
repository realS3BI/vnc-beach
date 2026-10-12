//! Encoding and pseudo-encoding numbers.

pub const RAW: i32 = 0;
pub const COPY_RECT: i32 = 1;
pub const ZRLE: i32 = 16;
pub const DESKTOP_SIZE: i32 = -223;
/// Screen layout (community RFB extension, used by TigerVNC and others):
/// tells the viewer how the framebuffer splits into physical displays.
pub const EXTENDED_DESKTOP_SIZE: i32 = -308;
