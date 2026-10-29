//! RFB (VNC) protocol implementation, RFC 6143.
//!
//! Shared by `beach-server` and `beach-viewer`. Speaks plain RFB 3.3/3.7/3.8 so
//! both sides interoperate with TigerVNC, RealVNC, macOS Screen Sharing etc.

pub mod auth;
pub mod camera;
pub mod encoding;
pub mod files;
pub mod handshake;
pub mod io;
pub mod keysym;
pub mod messages;
pub mod pixel;
pub mod zrle;

pub use handshake::{ClientInit, ServerInit};
pub use pixel::PixelFormat;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("server requires a password")]
    PasswordRequired,
    #[error("no supported security type offered (server offered {0:?})")]
    UnsupportedSecurity(Vec<u8>),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Largest framebuffer a client allocates for a server: 64 million pixels
/// (256 MiB of RGBA), room for three 5K displays. The RFB size fields allow
/// 65535 × 65535, which a hostile server could use to exhaust memory.
pub const MAX_FRAMEBUFFER_PIXELS: usize = 64 << 20;

/// Rejects framebuffer sizes a viewer must not allocate.
pub fn check_framebuffer_size(width: u16, height: u16) -> Result<()> {
    if width as usize * height as usize > MAX_FRAMEBUFFER_PIXELS {
        return proto(format!("framebuffer {width}×{height} is too large"));
    }
    Ok(())
}

pub(crate) fn proto<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Protocol(msg.into()))
}
