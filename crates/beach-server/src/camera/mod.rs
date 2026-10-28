//! Shows a viewer's webcam as a camera on this computer (VNC Beach camera v1,
//! see `rfb::camera`). Only macOS has an output so far: the CoreMediaIO
//! extension from `apps/macos-camera`, fed by `mac.rs`. The viewer's webcam
//! is in `source`.
//!
//! Several viewers may share a camera at once; the one that started sharing
//! last is shown. A viewer is asked for video only while an application on
//! this computer reads the camera.

pub mod h264;
#[cfg(target_os = "macos")]
mod mac;
pub mod source;
#[cfg(windows)]
mod windows;

use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use rfb::camera::{ServerMessage, Video};

/// Whether this computer has the virtual camera (macOS: the extension is
/// installed and allowed).
pub fn installed() -> bool {
    #[cfg(target_os = "macos")]
    return mac::installed();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Picture size and rate of the virtual camera.
pub const WIDTH: u16 = 1280;
pub const HEIGHT: u16 = 720;
pub const FPS: u8 = 30;

#[derive(Default)]
struct State {
    next_id: u64,
    /// Sessions that share a camera, oldest first; the last one is shown.
    sharing: Vec<u64>,
}

pub struct Hub {
    state: Mutex<State>,
    #[cfg(target_os = "macos")]
    output: Mutex<(Option<Arc<mac::Output>>, Option<Instant>)>,
}

impl Hub {
    /// `None` where this platform can't show a camera.
    pub fn new() -> Option<Arc<Hub>> {
        cfg!(target_os = "macos").then(|| {
            Arc::new(Hub {
                state: Mutex::default(),
                #[cfg(target_os = "macos")]
                output: Mutex::new((None, None)),
            })
        })
    }

    /// The virtual camera, opened on first use. While the extension isn't
    /// installed this retries at most every few seconds.
    #[cfg(target_os = "macos")]
    fn output(&self) -> Option<Arc<mac::Output>> {
        let mut out = self.output.lock().unwrap();
        if out.0.is_none() && out.1.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            out.1 = Some(Instant::now());
            match mac::Output::open() {
                Ok(o) => out.0 = Some(Arc::new(o)),
                Err(e) => log::debug!("virtual camera unavailable: {e:#}"),
            }
        }
        out.0.clone()
    }

    /// A session's link to the camera, if the virtual camera is installed.
    pub fn link(self: &Arc<Self>) -> Option<Link> {
        #[cfg(target_os = "macos")]
        self.output()?;
        let mut state = self.state.lock().unwrap();
        state.next_id += 1;
        Some(Link {
            hub: self.clone(),
            id: state.next_id,
            started: false,
        })
    }

    fn demand(&self) -> bool {
        #[cfg(target_os = "macos")]
        if let Some(out) = &self.output.lock().unwrap().0 {
            return out.demand();
        }
        false
    }

    fn owner(&self) -> Option<u64> {
        self.state.lock().unwrap().sharing.last().copied()
    }
}

/// One session's view of the camera.
pub struct Link {
    hub: Arc<Hub>,
    id: u64,
    /// The viewer was told to send video.
    started: bool,
}

impl Link {
    pub fn hello(&self) -> ServerMessage {
        ServerMessage::Hello {
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
        }
    }

    pub fn sharing(&mut self, on: bool) {
        let mut state = self.hub.state.lock().unwrap();
        state.sharing.retain(|&id| id != self.id);
        if on {
            state.sharing.push(self.id);
        }
        log::info!("viewer camera {}", if on { "shared" } else { "no longer shared" });
    }

    pub fn video(&self, video: Video) {
        if !self.started || self.hub.owner() != Some(self.id) {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(out) = &self.hub.output.lock().unwrap().0 {
            out.push(video);
        }
        #[cfg(not(target_os = "macos"))]
        drop(video);
    }

    /// What the viewer should hear now: Start or Stop when its turn changes,
    /// KeyFrame when the decoder lost track.
    pub fn poll(&mut self) -> Option<ServerMessage> {
        let wanted = self.hub.owner() == Some(self.id) && self.hub.demand();
        if wanted != self.started {
            self.started = wanted;
            log::info!("asking the viewer to {} its camera", if wanted { "start" } else { "stop" });
            #[cfg(target_os = "macos")]
            if wanted {
                if let Some(out) = &self.hub.output.lock().unwrap().0 {
                    out.reset();
                }
            }
            return Some(if wanted { ServerMessage::Start } else { ServerMessage::Stop });
        }
        #[cfg(target_os = "macos")]
        if self.started {
            if let Some(out) = &self.hub.output.lock().unwrap().0 {
                if out.take_key_frame_request() {
                    return Some(ServerMessage::KeyFrame);
                }
            }
        }
        None
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.hub.state.lock().unwrap().sharing.retain(|&id| id != self.id);
    }
}
