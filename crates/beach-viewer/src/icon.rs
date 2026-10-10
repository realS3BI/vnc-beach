//! The app icon, embedded for the window and the tray.

use egui::IconData;

pub fn window() -> IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png")).expect("icon-256.png")
}

pub fn tray() -> IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-32.png")).expect("icon-32.png")
}
