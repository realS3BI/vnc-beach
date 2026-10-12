//! The app icon, embedded for the window and the tray.

use egui::IconData;

pub fn window() -> IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png")).expect("icon-256.png")
}

/// The tray icon: on macOS a monochrome template image (the menu bar only
/// uses its alpha channel and tints it for light, dark and clicked states),
/// elsewhere the coloured app icon.
pub fn tray() -> IconData {
    #[cfg(target_os = "macos")]
    let bytes = include_bytes!("../../../assets/tray-template.png");
    #[cfg(not(target_os = "macos"))]
    let bytes = include_bytes!("../../../assets/icon-32.png");
    eframe::icon_data::from_png_bytes(bytes).expect("tray icon png")
}
