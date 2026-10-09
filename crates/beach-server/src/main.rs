mod capture;
mod clipboard;
mod input;
mod session;

use std::net::TcpListener;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Parser;

/// vnc-beach server: shares this computer's screen over VNC (RFB).
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    /// TCP port. 5900 is the VNC default.
    #[arg(short, long, default_value_t = 5900)]
    port: u16,
    /// VNC password (only the first 8 characters count). Also read from
    /// VNC_BEACH_PASSWORD. Without one, a random password is generated.
    #[arg(long, env = "VNC_BEACH_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// Accept connections without any password. Only for trusted networks.
    #[arg(long, conflicts_with = "password")]
    no_password: bool,
    /// Display to share (index from --list-displays). Defaults to the primary one.
    #[arg(short, long)]
    display: Option<usize>,
    /// List displays and exit.
    #[arg(long)]
    list_displays: bool,
    /// Maximum frames per second sent to viewers.
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Ignore keyboard and mouse input from viewers.
    #[arg(long)]
    view_only: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    #[cfg(windows)]
    let _ = enigo::set_dpi_awareness();

    if args.list_displays {
        for (i, d) in capture::displays()?.iter().enumerate() {
            println!("{i}: {d}");
        }
        return Ok(());
    }
    if args.fps == 0 {
        bail!("--fps must be at least 1");
    }

    let password = match (&args.password, args.no_password) {
        (_, true) => None,
        (Some(p), _) if p.is_empty() => bail!("password must not be empty (use --no-password)"),
        (Some(p), _) => {
            if p.chars().count() > 8 {
                log::warn!("VNC passwords are limited to 8 characters; the rest is ignored");
            }
            Some(p.clone())
        }
        (None, _) => {
            let p = random_password();
            println!("Password: {p}");
            Some(p)
        }
    };

    let screen = capture::Screen::start(args.display, args.fps).context("starting screen capture")?;
    let clipboard = clipboard::Clipboard::start();
    let input = if args.view_only {
        None
    } else {
        Some(input::Input::start(screen.geometry())?)
    };

    let listener = TcpListener::bind((args.bind.as_str(), args.port))
        .with_context(|| format!("binding {}:{}", args.bind, args.port))?;
    log::info!("listening on {}", listener.local_addr()?);

    let ctx = Arc::new(session::Context {
        screen,
        clipboard,
        input,
        password,
        fps: args.fps,
        name: format!("vnc-beach @ {}", gethostname::gethostname().to_string_lossy()),
    });
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept failed: {e}");
                continue;
            }
        };
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
            log::info!("{peer}: connected");
            match session::run(stream, &ctx) {
                Ok(()) => log::info!("{peer}: disconnected"),
                Err(e) => log::info!("{peer}: disconnected ({e:#})"),
            }
        });
    }
    Ok(())
}

fn random_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let seed = std::collections::hash_map::RandomState::new();
    (0..8u64)
        .map(|i| {
            use std::hash::BuildHasher;
            ALPHABET[(seed.hash_one(i) % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}
