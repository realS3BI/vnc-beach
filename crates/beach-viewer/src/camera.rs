//! Shares this computer's webcam with the server (VNC Beach camera v1, see
//! `rfb::camera`). The camera only runs while the user switched sharing on
//! and an application on the server reads the virtual camera.

use std::io::{BufWriter, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use beach_server::camera::source::{self, Event};
use rfb::camera::{ClientMessage, ServerMessage, Video};
use rfb::messages;

type Writer = Arc<Mutex<BufWriter<TcpStream>>>;
type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Off,
    /// Shared, but no application on the server reads the camera.
    Ready,
    #[cfg_attr(not(windows), allow(dead_code))]
    Starting,
    Live(String),
    Failed(String),
}

struct State {
    /// The server's Hello: largest picture and frame rate.
    offer: Option<(u16, u16, u8)>,
    enabled: bool,
    /// The server asked for video (Start without Stop).
    wanted: bool,
    #[cfg(windows)]
    source: Option<source::Source>,
    status: Status,
}

pub struct Camera {
    state: Mutex<State>,
    writer: Writer,
    video: SyncSender<Video>,
    /// Set when a frame had to be dropped: the next one must be a key frame.
    key_frame: Arc<AtomicBool>,
    repaint: Repaint,
}

impl Camera {
    /// `None` if this platform has no webcam support.
    pub fn new(writer: Writer, repaint: Repaint) -> Option<Arc<Camera>> {
        if !source::supported() {
            return None;
        }
        // Video goes through its own thread so a slow network never blocks
        // the camera; at most a few frames wait.
        let (video, frames) = sync_channel::<Video>(3);
        std::thread::Builder::new()
            .name("camera-send".into())
            .spawn({
                let writer = writer.clone();
                move || {
                    for video in frames {
                        let mut w = writer.lock().unwrap();
                        let sent = messages::ClientMessage::Camera(ClientMessage::Video(video))
                            .write(&mut *w)
                            .and_then(|()| Ok(w.flush()?));
                        if let Err(e) = sent {
                            log::debug!("camera send failed: {e}");
                            break;
                        }
                    }
                }
            })
            .ok()?;
        Some(Arc::new(Camera {
            state: Mutex::new(State {
                offer: None,
                enabled: false,
                wanted: false,
                #[cfg(windows)]
                source: None,
                status: Status::Off,
            }),
            writer,
            video,
            key_frame: Arc::new(AtomicBool::new(false)),
            repaint,
        }))
    }

    /// The server can show our camera.
    pub fn offered(&self) -> bool {
        self.state.lock().unwrap().offer.is_some()
    }

    pub fn enabled(&self) -> bool {
        self.state.lock().unwrap().enabled
    }

    pub fn status(&self) -> Status {
        self.state.lock().unwrap().status.clone()
    }

    fn send(&self, msg: ClientMessage) {
        let mut w = self.writer.lock().unwrap();
        let sent = messages::ClientMessage::Camera(msg)
            .write(&mut *w)
            .and_then(|()| Ok(w.flush()?));
        if let Err(e) = sent {
            log::debug!("camera send failed: {e}");
        }
    }

    pub fn set_enabled(self: &Arc<Self>, on: bool) {
        let offered = {
            let mut s = self.state.lock().unwrap();
            s.enabled = on;
            s.offer.is_some()
        };
        if offered {
            self.send(ClientMessage::Sharing(on));
        }
        self.update();
    }

    /// A camera message from the server (reader thread).
    pub fn handle(self: &Arc<Self>, msg: ServerMessage) {
        match msg {
            ServerMessage::Hello { width, height, fps } => {
                log::info!("server shows our camera up to {width}x{height} at {fps} fps");
                let enabled = {
                    let mut s = self.state.lock().unwrap();
                    s.offer = Some((width, height, fps));
                    s.enabled
                };
                self.send(ClientMessage::Sharing(enabled));
            }
            ServerMessage::Start => self.state.lock().unwrap().wanted = true,
            ServerMessage::Stop => self.state.lock().unwrap().wanted = false,
            ServerMessage::KeyFrame => self.key_frame.store(true, Ordering::Release),
        }
        self.update();
        (self.repaint)();
    }

    /// Starts or stops the webcam to match the state.
    fn update(self: &Arc<Self>) {
        let mut s = self.state.lock().unwrap();
        let run = s.enabled && s.wanted;
        #[cfg(windows)]
        {
            let old = if run { None } else { s.source.take() };
            if run && s.source.is_none() {
                let (w, h, fps) = s.offer.unwrap_or((1280, 720, 30));
                s.status = Status::Starting;
                self.key_frame.store(true, Ordering::Release);
                let me = Arc::downgrade(self);
                s.source = Some(source::Source::start(w, h, fps, self.key_frame.clone(), move |event| {
                    if let Some(me) = me.upgrade() {
                        me.event(event);
                    }
                }));
            }
            if !run {
                s.status = if s.enabled { Status::Ready } else { Status::Off };
            }
            // Stopping waits for the capture thread, which may need the
            // state lock for its last event.
            drop(s);
            drop(old);
        }
        #[cfg(not(windows))]
        {
            s.status = match (s.enabled, run) {
                (false, _) => Status::Off,
                (true, false) => Status::Ready,
                (true, true) => Status::Failed("Kamera wird auf diesem System nicht unterstützt".into()),
            };
        }
        (self.repaint)();
    }

    /// From the capture thread.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn event(&self, event: Event) {
        match event {
            Event::Started { name, width, height } => {
                log::info!("sharing camera {name} at {width}x{height}");
                self.set_status(Status::Live(name));
            }
            Event::Video(video) => {
                if let Err(TrySendError::Full(_)) = self.video.try_send(video) {
                    log::debug!("network behind, dropping a camera frame");
                    self.key_frame.store(true, Ordering::Release);
                }
            }
            Event::Failed(message) => self.set_status(Status::Failed(message)),
        }
    }

    fn set_status(&self, status: Status) {
        let mut s = self.state.lock().unwrap();
        // A late event of a stopped camera must not overwrite Off/Ready.
        if s.enabled && s.wanted {
            s.status = status;
            drop(s);
            (self.repaint)();
        }
    }
}
