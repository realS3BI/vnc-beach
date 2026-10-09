//! Screen capture. Publishes the latest frame; sessions diff against what
//! they last sent.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use xcap::Monitor;

pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// RGBA, `width * 4` bytes per row.
    pub rgba: Vec<u8>,
}

/// Where the shared display sits on the desktop, in the units the input
/// backend expects (points on macOS, pixels on Windows).
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub struct Screen {
    latest: Mutex<Arc<Frame>>,
    geometry: Geometry,
    viewers: AtomicUsize,
}

impl Screen {
    pub fn start(index: Option<usize>, fps: u32) -> anyhow::Result<Arc<Screen>> {
        check_permission();
        // `Monitor` isn't `Send` on Windows, so the capture thread owns it from
        // the start and hands back the geometry and a first frame.
        let (init_tx, init_rx) = std::sync::mpsc::channel();
        let (screen_tx, screen_rx) = std::sync::mpsc::channel::<Arc<Screen>>();
        std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let monitor = match open(index) {
                    Ok((monitor, geometry, first)) => {
                        let _ = init_tx.send(Ok((geometry, first)));
                        monitor
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };
                if let Ok(screen) = screen_rx.recv() {
                    screen.capture_loop(monitor, fps);
                }
            })?;

        let (geometry, first) = init_rx.recv()??;
        log::info!("framebuffer {}x{}", first.width, first.height);
        let screen = Arc::new(Screen {
            latest: Mutex::new(Arc::new(first)),
            geometry,
            viewers: AtomicUsize::new(0),
        });
        screen_tx.send(screen.clone())?;
        Ok(screen)
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The newest frame. A new capture is always a new `Arc`.
    pub fn latest(&self) -> Arc<Frame> {
        self.latest.lock().unwrap().clone()
    }

    /// Keeps capturing running while the returned guard lives. Without
    /// viewers the capture loop idles.
    pub fn watch(self: &Arc<Self>) -> Watch {
        self.viewers.fetch_add(1, Ordering::AcqRel);
        Watch(self.clone())
    }

    fn publish(&self, frame: Frame) {
        *self.latest.lock().unwrap() = Arc::new(frame);
    }

    fn capture_loop(&self, monitor: Monitor, fps: u32) {
        let interval = Duration::from_secs_f64(1.0 / fps as f64);
        let mut failures = 0;
        loop {
            if self.viewers.load(Ordering::Acquire) == 0 {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let started = Instant::now();
            match grab(&monitor) {
                Ok(frame) => {
                    failures = 0;
                    self.publish(frame);
                }
                Err(e) => {
                    failures += 1;
                    if failures == 1 || failures % 100 == 0 {
                        log::warn!("capture failed ({failures}x): {e:#}");
                    }
                }
            }
            if let Some(rest) = interval.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    }
}

pub struct Watch(Arc<Screen>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Without the Screen Recording permission macOS still "captures" fine but
/// returns only the wallpaper and menu bar, so check explicitly.
#[cfg(target_os = "macos")]
fn check_permission() {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    // SAFETY: plain CoreGraphics calls without arguments.
    if unsafe { CGPreflightScreenCaptureAccess() } {
        return;
    }
    log::warn!(
        "no Screen Recording permission: viewers will only see the wallpaper. Allow it for this \
         app/terminal in System Settings → Privacy & Security → Screen & System Audio Recording, \
         then restart the server"
    );
    unsafe { CGRequestScreenCaptureAccess() };
}

#[cfg(not(target_os = "macos"))]
fn check_permission() {}

fn grab(monitor: &Monitor) -> anyhow::Result<Frame> {
    let img = monitor.capture_image()?;
    let (width, height) = (img.width() as usize, img.height() as usize);
    Ok(Frame {
        width,
        height,
        rgba: img.into_raw(),
    })
}

fn open(index: Option<usize>) -> anyhow::Result<(Monitor, Geometry, Frame)> {
    let monitor = pick(index)?;
    let geometry = Geometry {
        x: monitor.x()?,
        y: monitor.y()?,
        width: monitor.width()?,
        height: monitor.height()?,
    };
    log::info!(
        "sharing display {:?} {geometry:?}",
        monitor.name().unwrap_or_default()
    );
    // A first frame synchronously, so problems surface right away.
    let first = grab(&monitor).context("capturing the screen failed")?;
    Ok((monitor, geometry, first))
}

fn pick(index: Option<usize>) -> anyhow::Result<Monitor> {
    let mut monitors = Monitor::all()?;
    match index {
        Some(i) if i < monitors.len() => Ok(monitors.swap_remove(i)),
        Some(i) => Err(anyhow!("display {i} does not exist (see --list-displays)")),
        None => {
            let primary = monitors
                .iter()
                .position(|m| m.is_primary().unwrap_or(false))
                .unwrap_or(0);
            if monitors.is_empty() {
                return Err(anyhow!("no displays found"));
            }
            Ok(monitors.swap_remove(primary))
        }
    }
}

pub fn displays() -> anyhow::Result<Vec<String>> {
    Ok(Monitor::all()?
        .iter()
        .map(|m| {
            format!(
                "{} {}x{} at ({}, {}){}",
                m.name().unwrap_or_default(),
                m.width().unwrap_or(0),
                m.height().unwrap_or(0),
                m.x().unwrap_or(0),
                m.y().unwrap_or(0),
                if m.is_primary().unwrap_or(false) {
                    " [primary]"
                } else {
                    ""
                }
            )
        })
        .collect())
}
