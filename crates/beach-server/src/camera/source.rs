//! The viewer's side: a local webcam encoded as H.264 for `rfb::camera`.
//! Only Windows has one so far (`windows.rs`).

#[cfg(windows)]
pub use super::windows::Source;

pub enum Event {
    /// The camera runs; `width` × `height` is what the video will have.
    Started { name: String, width: u16, height: u16 },
    Video(rfb::camera::Video),
    /// The camera stopped; a German message for the user.
    Failed(String),
}

/// Whether this platform can share a webcam.
pub fn supported() -> bool {
    cfg!(windows)
}
