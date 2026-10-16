//! Screen capture. Publishes the latest frame; sessions diff against what
//! they last sent.
//!
//! With more than one display the framebuffer is a virtual desktop: every
//! display keeps its own captured pixels and sits where its desktop position
//! lands at the highest pixel density, so displays with different scale
//! factors never overlap. Gaps (L-shaped arrangements) stay black.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use xcap::Monitor;

/// Which displays to share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    /// Every display, side by side as on the desktop.
    #[default]
    All,
    Primary,
    /// Index as listed by [`displays`].
    Index(usize),
}

impl FromStr for Display {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "alle" => Ok(Display::All),
            "primary" | "main" => Ok(Display::Primary),
            n => n
                .parse()
                .map(Display::Index)
                .map_err(|_| format!("expected a display index, 'all' or 'primary', got {s:?}")),
        }
    }
}

impl std::fmt::Display for Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Display::All => f.write_str("all"),
            Display::Primary => f.write_str("primary"),
            Display::Index(i) => write!(f, "{i}"),
        }
    }
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// RGBA, `width * 4` bytes per row.
    pub rgba: Vec<u8>,
    /// How the frame splits into displays. Shared between frames until a
    /// display is added, removed or changes resolution.
    pub layout: Arc<Layout>,
}

/// Where a display sits on the desktop, in the units the input backend
/// expects (points on macOS, pixels on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// One display's place in the framebuffer and on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenRect {
    pub id: u32,
    pub name: String,
    /// Position and size in framebuffer pixels.
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub desktop: Geometry,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub screens: Vec<ScreenRect>,
}

/// A display as captured: desktop geometry plus the size of its image.
struct Captured {
    id: u32,
    name: String,
    desktop: Geometry,
    pixel_width: usize,
    pixel_height: usize,
}

impl Layout {
    fn build(displays: &[Captured]) -> Layout {
        let density = |d: &Captured| d.pixel_width as f64 / d.desktop.width.max(1) as f64;
        let scale = displays.iter().map(density).fold(1.0, f64::max);
        let min_x = displays.iter().map(|d| d.desktop.x).min().unwrap_or(0);
        let min_y = displays.iter().map(|d| d.desktop.y).min().unwrap_or(0);
        let screens: Vec<ScreenRect> = displays
            .iter()
            .map(|d| ScreenRect {
                id: d.id,
                name: d.name.clone(),
                x: ((d.desktop.x - min_x) as f64 * scale).round() as usize,
                y: ((d.desktop.y - min_y) as f64 * scale).round() as usize,
                width: d.pixel_width,
                height: d.pixel_height,
                desktop: d.desktop,
            })
            .collect();
        Layout {
            width: screens.iter().map(|s| s.x + s.width).max().unwrap_or(0),
            height: screens.iter().map(|s| s.y + s.height).max().unwrap_or(0),
            screens,
        }
    }

    /// Framebuffer pixel → desktop coordinates, through the display the
    /// pixel is on (or the nearest one, for gaps between displays).
    pub fn desktop_point(&self, x: f64, y: f64) -> (i32, i32) {
        let Some(screen) = self.screens.iter().min_by_key(|s| {
            let dx = (s.x as f64 - x).max(x - (s.x + s.width) as f64).max(0.0);
            let dy = (s.y as f64 - y).max(y - (s.y + s.height) as f64).max(0.0);
            (dx * dx + dy * dy) as u64
        }) else {
            return (x as i32, y as i32);
        };
        let g = screen.desktop;
        let rel_x = (x - screen.x as f64).clamp(0.0, (screen.width.max(1) - 1) as f64);
        let rel_y = (y - screen.y as f64).clamp(0.0, (screen.height.max(1) - 1) as f64);
        (
            g.x + (rel_x * g.width as f64 / screen.width.max(1) as f64) as i32,
            g.y + (rel_y * g.height as f64 / screen.height.max(1) as f64) as i32,
        )
    }
}

pub struct Screen {
    latest: Mutex<Arc<Frame>>,
    viewers: AtomicUsize,
    stopped: AtomicBool,
}

/// What the capture thread owns: the monitors and their arrangement.
struct Source {
    monitors: Vec<Monitor>,
    layout: Arc<Layout>,
}

impl Screen {
    pub fn start(display: Display, fps: u32) -> anyhow::Result<Arc<Screen>> {
        check_permission();
        // `Monitor` isn't `Send` on Windows, so the capture thread owns it from
        // the start and hands back a first frame.
        let (init_tx, init_rx) = std::sync::mpsc::channel();
        let (screen_tx, screen_rx) = std::sync::mpsc::channel::<Arc<Screen>>();
        std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                #[cfg(windows)]
                crate::desktop::follow_input();
                let source = match open(display) {
                    Ok((source, first)) => {
                        let _ = init_tx.send(Ok(first));
                        source
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };
                if let Ok(screen) = screen_rx.recv() {
                    screen.capture_loop(source, display, fps);
                }
            })?;

        let first = init_rx.recv()??;
        log::info!("framebuffer {}x{}", first.width, first.height);
        let screen = Arc::new(Screen {
            latest: Mutex::new(Arc::new(first)),
            viewers: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
        });
        screen_tx.send(screen.clone())?;
        Ok(screen)
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

    /// Ends the capture thread.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    fn publish(&self, frame: Frame) {
        *self.latest.lock().unwrap() = Arc::new(frame);
    }

    fn capture_loop(&self, mut source: Source, display: Display, fps: u32) {
        let interval = Duration::from_secs_f64(1.0 / fps as f64);
        let mut failures = 0;
        while !self.stopped.load(Ordering::Acquire) {
            if self.viewers.load(Ordering::Acquire) == 0 {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let started = Instant::now();
            // Lock screen and UAC prompts are a different desktop (only
            // reachable when running as the Windows service).
            #[cfg(windows)]
            crate::desktop::follow_input();
            match grab_all(&source) {
                Ok(frame) => {
                    failures = 0;
                    self.publish(frame);
                }
                Err(e) => {
                    failures += 1;
                    if failures == 1 || failures % 100 == 0 {
                        log::warn!("capture failed ({failures}x): {e:#}");
                    }
                    // A display was unplugged or changed resolution: re-read
                    // the arrangement, but not more often than once a second.
                    std::thread::sleep(Duration::from_secs(1));
                    match open(display) {
                        Ok((new_source, first)) => {
                            if *new_source.layout != *source.layout {
                                log::info!("display arrangement changed: framebuffer {}x{}", first.width, first.height);
                            }
                            source = new_source;
                            failures = 0;
                            self.publish(first);
                        }
                        Err(e) => log::debug!("reopening displays failed: {e:#}"),
                    }
                    continue;
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
         app/terminal in System Settings > Privacy & Security > Screen & System Audio Recording, \
         then restart the server"
    );
    unsafe { CGRequestScreenCaptureAccess() };
}

#[cfg(not(target_os = "macos"))]
fn check_permission() {}

/// Captures every display of `source` into one frame.
fn grab_all(source: &Source) -> anyhow::Result<Frame> {
    let layout = &source.layout;
    if source.monitors.len() == 1 {
        let img = source.monitors[0].capture_image()?;
        let (width, height) = (img.width() as usize, img.height() as usize);
        anyhow::ensure!(
            width == layout.width && height == layout.height,
            "display resolution changed ({width}x{height}, expected {}x{})",
            layout.width,
            layout.height
        );
        return Ok(Frame {
            width,
            height,
            rgba: img.into_raw(),
            layout: layout.clone(),
        });
    }
    let mut rgba = vec![0; layout.width * layout.height * 4];
    for (monitor, screen) in source.monitors.iter().zip(&layout.screens) {
        let img = monitor.capture_image()?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        anyhow::ensure!(
            w == screen.width && h == screen.height,
            "display {:?} changed resolution ({w}x{h}, expected {}x{})",
            screen.name,
            screen.width,
            screen.height
        );
        let src = img.as_raw();
        let stride = layout.width * 4;
        for row in 0..h {
            let dst = (screen.y + row) * stride + screen.x * 4;
            rgba[dst..dst + w * 4].copy_from_slice(&src[row * w * 4..(row + 1) * w * 4]);
        }
    }
    Ok(Frame {
        width: layout.width,
        height: layout.height,
        rgba,
        layout: layout.clone(),
    })
}

/// Picks the displays and captures a first frame synchronously, so problems
/// surface right away.
fn open(display: Display) -> anyhow::Result<(Source, Frame)> {
    let monitors = pick(display)?;
    let mut captured = Vec::with_capacity(monitors.len());
    for m in &monitors {
        let desktop = Geometry {
            x: m.x()?,
            y: m.y()?,
            width: m.width()?,
            height: m.height()?,
        };
        let img = m.capture_image().context("capturing the screen failed")?;
        captured.push(Captured {
            id: m.id().unwrap_or(0),
            name: m.name().unwrap_or_default(),
            desktop,
            pixel_width: img.width() as usize,
            pixel_height: img.height() as usize,
        });
    }
    let layout = Arc::new(Layout::build(&captured));
    for s in &layout.screens {
        log::info!(
            "sharing display {:?} {}x{} at ({}, {}) in the framebuffer, desktop {:?}",
            s.name,
            s.width,
            s.height,
            s.x,
            s.y,
            s.desktop
        );
    }
    let source = Source { monitors, layout };
    let first = grab_all(&source)?;
    Ok((source, first))
}

fn pick(display: Display) -> anyhow::Result<Vec<Monitor>> {
    let mut monitors = Monitor::all()?;
    if monitors.is_empty() {
        return Err(anyhow!("no displays found"));
    }
    match display {
        Display::All => Ok(monitors),
        Display::Index(i) if i < monitors.len() => Ok(vec![monitors.swap_remove(i)]),
        Display::Index(i) => Err(anyhow!("display {i} does not exist (see --list-displays)")),
        Display::Primary => {
            let primary = monitors
                .iter()
                .position(|m| m.is_primary().unwrap_or(false))
                .unwrap_or(0);
            Ok(vec![monitors.swap_remove(primary)])
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

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(x: i32, y: i32, w: u32, h: u32, scale: usize) -> Captured {
        Captured {
            id: 0,
            name: String::new(),
            desktop: Geometry {
                x,
                y,
                width: w,
                height: h,
            },
            pixel_width: w as usize * scale,
            pixel_height: h as usize * scale,
        }
    }

    #[test]
    fn single_display_keeps_its_pixels() {
        let layout = Layout::build(&[captured(0, 0, 1728, 1117, 2)]);
        assert_eq!((layout.width, layout.height), (3456, 2234));
        assert_eq!((layout.screens[0].x, layout.screens[0].y), (0, 0));
        assert_eq!(layout.desktop_point(0.0, 0.0), (0, 0));
        assert_eq!(layout.desktop_point(3455.0, 2233.0), (1727, 1116));
    }

    #[test]
    fn mixed_densities_do_not_overlap() {
        // Retina laptop left of a 1x monitor that sits 100 points lower.
        let laptop = captured(-1728, 100, 1728, 1117, 2);
        let external = captured(0, 0, 2560, 1440, 1);
        let layout = Layout::build(&[laptop, external]);
        let [l, e] = &layout.screens[..] else { panic!() };
        assert_eq!((l.x, l.y, l.width, l.height), (0, 200, 3456, 2234));
        assert_eq!((e.x, e.y, e.width, e.height), (3456, 0, 2560, 1440));
        assert_eq!((layout.width, layout.height), (6016, 2434));
        // A pixel on the laptop maps through its own density…
        assert_eq!(layout.desktop_point(10.0, 200.0), (-1723, 100));
        // …one on the external monitor through 1:1…
        assert_eq!(layout.desktop_point(3456.0 + 100.0, 50.0), (100, 50));
        // …and a pixel in the gap below the external monitor snaps to the
        // nearest display.
        assert_eq!(layout.desktop_point(5000.0, 2000.0), (1544, 1439));
        assert_eq!(layout.desktop_point(3500.0, 2000.0), (-1, 1000));
    }

    #[test]
    fn parses_display_choice() {
        assert_eq!("all".parse::<Display>().unwrap(), Display::All);
        assert_eq!("Primary".parse::<Display>().unwrap(), Display::Primary);
        assert_eq!("2".parse::<Display>().unwrap(), Display::Index(2));
        assert!("left".parse::<Display>().is_err());
        assert_eq!(Display::Index(1).to_string(), "1");
    }
}
