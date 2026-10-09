//! RFB (VNC) protocol implementation, RFC 6143.
//!
//! Shared by `beach-server` and `beach-viewer`. Speaks plain RFB 3.3/3.7/3.8 so
//! both sides interoperate with TigerVNC, RealVNC, macOS Screen Sharing etc.

pub mod auth;
pub mod encoding;
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

pub(crate) fn proto<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Protocol(msg.into()))
}
