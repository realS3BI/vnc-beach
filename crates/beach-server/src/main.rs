use anyhow::bail;
use clap::Parser;

use beach_server::{Display, Options, Server};

/// VNC Beach server: shares this computer's screen over VNC (RFB).
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    /// TCP port. 5901 (display 1), since macOS Screen Sharing usually holds
    /// the VNC default 5900.
    #[arg(short, long, default_value_t = 5901)]
    port: u16,
    /// VNC password (only the first 8 characters count). Also read from
    /// VNC_BEACH_PASSWORD. Without one, a random password is generated.
    #[arg(long, env = "VNC_BEACH_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// Accept connections without any password. Only for trusted networks.
    #[arg(long, conflicts_with = "password")]
    no_password: bool,
    /// Displays to share: "all" (one virtual desktop, the default), "primary"
    /// or an index from --list-displays.
    #[arg(short, long, default_value_t = Display::All)]
    display: Display,
    /// List displays and exit.
    #[arg(long)]
    list_displays: bool,
    /// Maximum frames per second sent to viewers.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Ignore keyboard and mouse input and disable file transfers.
    #[arg(long)]
    view_only: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    if args.list_displays {
        for (i, d) in beach_server::displays()?.iter().enumerate() {
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
            let p = beach_server::random_password();
            println!("Password: {p}");
            Some(p)
        }
    };

    let _server = Server::start(Options {
        bind: args.bind,
        port: args.port,
        password,
        display: args.display,
        fps: args.fps,
        view_only: args.view_only,
        name: None,
        home: None,
    })?;
    serve_forever()
}

/// Keeps the process alive while the server threads work.
#[cfg(target_os = "macos")]
fn serve_forever() -> ! {
    // The main queue has to run: the input thread reads the keyboard layout
    // there (see `keymap`).
    dispatch2::dispatch_main()
}

#[cfg(not(target_os = "macos"))]
fn serve_forever() -> ! {
    loop {
        std::thread::park();
    }
}
