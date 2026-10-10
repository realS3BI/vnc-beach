// No console window next to the viewer on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autostart;
mod background;
mod client;
mod clipboard;
mod config;
mod control;
mod icon;
mod keys;
mod server_panel;
mod servers;
mod settings;
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
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = Args::parse();
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
            .with_icon(std::sync::Arc::new(icon::window()))
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([480.0, 320.0]),
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
