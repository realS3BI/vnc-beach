//! The VNC Beach server as a library: `beach-server` is a thin CLI around it,
//! and the viewer embeds it as its background server on Windows.

pub mod camera;
pub mod capture;
pub mod clipboard;
#[cfg(windows)]
pub mod desktop;
pub mod files;
pub mod headless;
pub mod input;
#[cfg(target_os = "macos")]
pub mod keymap;
pub mod permissions;
pub mod session;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Context as _;

pub use capture::{Display, Layout, ScreenRect};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub bind: String,
    pub port: u16,
    /// `None` accepts viewers without a password.
    pub password: Option<String>,
    /// Which displays to share; all of them by default.
    pub display: Display,
    pub fps: u32,
    pub view_only: bool,
    /// Desktop name shown to viewers; defaults to `VNC Beach @ hostname
    /// (macOS)` / `(Windows)`. The suffix lets the viewer pick the right
    /// Ctrl/Cmd mapping.
    pub name: Option<String>,
    /// Root for file transfers. The Windows service runs as SYSTEM and sets
    /// the signed-in user's profile here; `None` is the current user's home.
    pub home: Option<std::path::PathBuf>,
    /// Also accept tickets of the coordination server this device is
    /// registered with (see `beach_account`).
    pub tickets: Option<beach_account::ticket::Verifier>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            bind: "0.0.0.0".into(),
            port: 5901,
            password: None,
            display: Display::All,
            fps: 60,
            view_only: false,
            name: None,
            home: None,
            tickets: None,
        }
    }
}

/// A running server. Dropping it stops everything.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    viewers: Arc<Mutex<Vec<String>>>,
    ctx: Arc<session::Context>,
    accept: Option<JoinHandle<()>>,
    input_missing: bool,
}

impl Server {
    /// Binds the port, starts capturing and accepts viewers in the background.
    pub fn start(opts: Options) -> anyhow::Result<Server> {
        anyhow::ensure!(opts.fps > 0, "fps must be at least 1");
        #[cfg(windows)]
        let _ = enigo::set_dpi_awareness();

        // Bind first: a busy port (the macOS login window agent while the
        // tray process serves) must not start capture and input every retry.
        let listener = TcpListener::bind((opts.bind.as_str(), opts.port))
            .with_context(|| format!("binding {}:{}", opts.bind, opts.port))?;
        let screen = capture::Screen::start(opts.display, opts.fps).context("starting screen capture")?;
        let clipboard = clipboard::Clipboard::start();
        // Without the permission to inject input, still share the screen.
        let input = if opts.view_only {
            None
        } else {
            match input::Input::start() {
                Ok(input) => Some(input),
                Err(e) => {
                    log::warn!("sharing without keyboard and mouse: {e:#}");
                    None
                }
            }
        };
        let input_missing = !opts.view_only && input.is_none();
        let addr = listener.local_addr()?;
        log::info!("listening on {addr}");

        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(session::Context {
            screen,
            clipboard,
            input,
            file_transfer: !opts.view_only,
            camera: if opts.view_only { None } else { camera::Hub::new() },
            home: opts.home,
            password: opts.password,
            tickets: opts.tickets,
            fps: opts.fps,
            name: opts.name.unwrap_or_else(|| {
                format!(
                    "VNC Beach @ {} ({})",
                    gethostname::gethostname().to_string_lossy(),
                    platform_name()
                )
            }),
            stop: stop.clone(),
        });
        let viewers = Arc::new(Mutex::new(Vec::new()));
        let accept = std::thread::Builder::new().name("accept".into()).spawn({
            let (ctx, stop, viewers) = (ctx.clone(), stop.clone(), viewers.clone());
            move || accept_loop(listener, ctx, stop, viewers)
        })?;
        Ok(Server {
            addr,
            stop,
            viewers,
            ctx,
            accept: Some(accept),
            input_missing,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Addresses of the connected viewers.
    pub fn viewers(&self) -> Vec<String> {
        self.viewers.lock().unwrap().clone()
    }

    /// Input was wanted but couldn't be set up (macOS: no Accessibility
    /// permission); viewers can watch but not control.
    pub fn input_missing(&self) -> bool {
        self.input_missing
    }

    pub fn password(&self) -> Option<&str> {
        self.ctx.password.as_deref()
    }

    /// Stops accepting, disconnects viewers and ends the capture.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if self.stop.swap(true, Ordering::AcqRel) {
            return;
        }
        self.ctx.screen.stop();
        self.ctx.clipboard.stop();
        // Unblock `accept` with a throwaway connection.
        let mut target = self.addr;
        if target.ip().is_unspecified() {
            target.set_ip(if target.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        let _ = TcpStream::connect_timeout(&target, Duration::from_secs(1));
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
        log::info!("server stopped");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(
    listener: TcpListener,
    ctx: Arc<session::Context>,
    stop: Arc<AtomicBool>,
    viewers: Arc<Mutex<Vec<String>>>,
) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept failed: {e}");
                continue;
            }
        };
        let (ctx, viewers) = (ctx.clone(), viewers.clone());
        std::thread::spawn(move || {
            let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
            log::info!("{peer}: connected");
            viewers.lock().unwrap().push(peer.clone());
            match session::run(stream, &ctx) {
                Ok(()) => log::info!("{peer}: disconnected"),
                Err(e) => log::info!("{peer}: disconnected ({e:#})"),
            }
            viewers.lock().unwrap().retain(|p| *p != peer);
        });
    }
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    }
}

/// Human-readable list of displays, index = position.
pub fn displays() -> anyhow::Result<Vec<String>> {
    capture::displays()
}

/// Eight characters without look-alikes, for a generated VNC password.
pub fn random_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let seed = std::collections::hash_map::RandomState::new();
    (0..8u64)
        .map(|i| {
            use std::hash::BuildHasher;
            ALPHABET[(seed.hash_one(i) % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}
