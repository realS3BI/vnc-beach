// No console window next to the viewer on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod access;
mod account;
mod access_ui;
mod account_ui;
mod app;
mod autostart;
mod background;
mod camera;
#[cfg(target_os = "macos")]
mod camera_extension;
mod cards;
mod client;
mod clipboard;
mod config;
mod control;
#[cfg(windows)]
mod display_driver;
mod files;
mod icon;
#[cfg(windows)]
mod install;
mod keys;
#[cfg(target_os = "macos")]
mod mac_server;
#[cfg(target_os = "macos")]
mod menu;
mod remote;
mod server_panel;
mod servers;
mod service;
mod settings;
mod theme;
mod update;

use clap::Parser;

/// VNC Beach viewer: connects to any VNC server. Connections are picked from
/// the favorites list in the window.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Run without a window: tray/menu bar icon plus the built-in server.
    /// Started at login when autostart is on.
    #[arg(long)]
    background: bool,
    /// Windows service entry point (started by the service control manager).
    #[arg(long, hide = true)]
    service: bool,
    /// The server as the Windows service runs it in the console session.
    #[arg(long, hide = true)]
    service_agent: bool,
    /// Set up the Windows service (needs administrator rights).
    #[arg(long, hide = true)]
    install_service: bool,
    /// Install the virtual display driver used while no monitor is attached
    /// (Windows, needs administrator rights).
    #[arg(long)]
    install_display_driver: bool,
    /// Install the macOS camera extension that shows a viewer's webcam as
    /// the camera "VNC Beach" (asks for approval in System Settings).
    #[arg(long)]
    install_camera: bool,
    /// Remove the Windows service (needs administrator rights).
    #[arg(long, hide = true)]
    uninstall_service: bool,
    /// Explicit opt-in to unattended macOS startup; password is JSON on stdin.
    #[arg(long, hide = true)]
    install_server_mode: bool,
    /// Start user services too, then lock the desktop after login.
    #[arg(long, hide = true, requires = "install_server_mode")]
    automatic_login: bool,
    /// Config directory to use instead of the current user's.
    #[arg(long, hide = true)]
    config_dir: Option<std::path::PathBuf>,
    /// Root for file transfers (service agent).
    #[arg(long, hide = true)]
    home: Option<std::path::PathBuf>,
    /// The download the installed copy was started from; it is deleted.
    #[arg(long, hide = true)]
    installed_from: Option<std::path::PathBuf>,
}

/// Dispatches to the service/setup modes, the tray process or the viewer
/// window, depending on the command line.
fn main() {
    let args = Args::parse();
    init_logging(&args);
    if args.install_camera {
        #[cfg(target_os = "macos")]
        let result = camera_extension::install_and_wait();
        #[cfg(not(target_os = "macos"))]
        let result: anyhow::Result<()> = Err(anyhow::anyhow!("--install-camera is macOS only"));
        if let Err(e) = result {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        println!("camera extension installed");
        return;
    }
    if let Some(dir) = &args.config_dir {
        servers::set_config_dir(dir.clone());
    }
    if args.service
        || args.service_agent
        || args.install_service
        || args.uninstall_service
        || args.install_display_driver
        || args.install_server_mode
    {
        std::process::exit(match run_service_mode(&args) {
            Ok(()) => 0,
            Err(e) => {
                log::error!("{e:#}");
                1
            }
        });
    }
    // A downloaded exe installs itself and hands over to the installed copy.
    #[cfg(windows)]
    {
        if install::relocate() {
            return;
        }
        if let Some(download) = &args.installed_from {
            install::remove_download(download);
        }
    }
    if args.background {
        if let Err(e) = background::run() {
            log::error!("background process failed: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VNC Beach")
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(icon::window()))
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([520.0, 400.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "VNC Beach",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    );
    if let Err(e) = result {
        log::error!("viewer failed: {e}");
        std::process::exit(1);
    }
}

/// The service modes have no console: they log to a file in
/// `%ProgramData%\VNC Beach`.
fn init_logging(args: &Args) {
    let file = if args.service {
        "service.log"
    } else if args.service_agent {
        "agent.log"
    } else if args.install_service || args.uninstall_service || args.install_display_driver || args.install_server_mode {
        "setup.log"
    } else {
        ""
    };
    let default = if file.is_empty() { "warn" } else { "info" };
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default));
    if let Some(log) = Some(file).filter(|f| !f.is_empty()).and_then(service::log_path) {
        if let Ok(log) = std::fs::File::create(log) {
            builder.target(env_logger::Target::Pipe(Box::new(log)));
        }
    }
    builder.init();
}

/// Runs one of the setup or service modes (`--service`, `--install-service`,
/// `--install-display-driver`, ...); `main` exits with its result.
#[cfg(any(windows, target_os = "macos"))]
fn run_service_mode(args: &Args) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let paths = || -> anyhow::Result<service::Paths> {
        Ok(service::Paths {
            config_dir: args.config_dir.clone().context("--config-dir missing")?,
            home: args.home.clone().context("--home missing")?,
        })
    };
    if args.install_server_mode {
        #[cfg(target_os = "macos")]
        return mac_server::install(&paths()?, args.automatic_login);
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("--install-server-mode is macOS only");
    } else if args.service {
        #[cfg(windows)]
        return service::run_service(paths()?);
        #[cfg(not(windows))]
        anyhow::bail!("--service is Windows only");
    } else if args.service_agent {
        background::run_agent(args.home.clone())
    } else if args.install_display_driver {
        #[cfg(windows)]
        return display_driver::install();
        #[cfg(not(windows))]
        anyhow::bail!("--install-display-driver is Windows only");
    } else if args.install_service {
        service::install(&paths()?)
    } else {
        service::uninstall()
    }
}

/// The service modes don't exist on other platforms.
#[cfg(not(any(windows, target_os = "macos")))]
fn run_service_mode(_args: &Args) -> anyhow::Result<()> {
    anyhow::bail!("the service is only available on Windows and macOS")
}
