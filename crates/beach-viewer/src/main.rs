// No console window next to the viewer on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod client;
mod keys;
mod servers;

use clap::Parser;

/// vnc-beach viewer: connects to any VNC server.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Server to connect to: `host`, `host:display`, `host:port`, `host::port`,
    /// or the name of a saved server.
    host: Option<String>,
    /// VNC password. Also read from VNC_BEACH_PASSWORD. Overrides a saved password.
    #[arg(long, env = "VNC_BEACH_PASSWORD", hide_env_values = true)]
    password: Option<String>,
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = Args::parse();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("vnc-beach")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "vnc-beach",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args.host, args.password)))),
    )
}
