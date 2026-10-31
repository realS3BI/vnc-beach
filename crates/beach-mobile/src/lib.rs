//! VNC Beach client for mobile apps.
//!
//! [`client`] runs an RFB session (handshake, decoding into an RGBA
//! framebuffer, sending input) without any UI; [`ffi`] exposes it as a C API
//! declared in `include/beach_mobile.h`, which the Swift app links as a static
//! library.
//! [`account`] fetches account tickets for devices of a VNC Beach account.

pub mod account;
pub mod client;
pub mod ffi;
