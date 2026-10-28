//! The viewer's native macOS menu bar: the app menu with "Einstellungen …",
//! "Ablage" with "Neue Verbindung …" and a "Fenster" menu that AppKit
//! fills with the open session windows.
//!
//! The shortcuts (⌘N, ⌘,) only exist while the overview window is in
//! front: a session window passes every key to the remote machine, so the
//! menu must not take them there.

use std::sync::mpsc::{channel, Receiver};

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// What a menu item asks the viewer to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewConnection,
    Settings,
    CheckUpdates,
}

pub struct MenuBar {
    /// Keeps the menu and its items alive.
    _menu: Menu,
    new_connection: MenuItem,
    settings: MenuItem,
    events: Receiver<MenuId>,
    shortcuts: bool,
}

impl MenuBar {
    /// Replaces winit's default menu. Call on the main thread once the
    /// application has launched (eframe's creation context is fine).
    pub fn install(ctx: &egui::Context) -> MenuBar {
        let menu = Menu::new();
        let about = PredefinedMenuItem::about(
            Some("Über VNC Beach"),
            Some(AboutMetadata {
                name: Some("VNC Beach".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                copyright: Some("MIT-Lizenz".into()),
                website: Some("https://github.com/realS3BI/vnc-beach".into()),
                website_label: Some("VNC Beach auf GitHub".into()),
                ..Default::default()
            }),
        );
        let settings = MenuItem::with_id("settings", "Einstellungen …", true, None);
        let updates = MenuItem::with_id("updates", "Nach Updates suchen …", true, None);
        let app = Submenu::with_items(
            "VNC Beach",
            true,
            &[
                &about,
                &PredefinedMenuItem::separator(),
                &settings,
                &updates,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(Some("Dienste")),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(Some("VNC Beach ausblenden")),
                &PredefinedMenuItem::hide_others(Some("Andere ausblenden")),
                &PredefinedMenuItem::show_all(Some("Alle einblenden")),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(Some("VNC Beach beenden")),
            ],
        )
        .expect("app menu");
        let new_connection = MenuItem::with_id("new-connection", "Neue Verbindung …", true, None);
        let file = Submenu::with_items("Ablage", true, &[&new_connection]).expect("file menu");
        // No "Im Dock ablegen" (⌘M): that key belongs to the remote machine.
        let window = Submenu::with_items(
            "Fenster",
            true,
            &[&PredefinedMenuItem::bring_all_to_front(Some("Alle nach vorne bringen"))],
        )
        .expect("window menu");
        for submenu in [&app, &file, &window] {
            if let Err(e) = menu.append(submenu) {
                log::warn!("menu: {e}");
            }
        }
        window.set_as_windows_menu_for_nsapp();
        menu.init_for_nsapp();

        let (tx, events) = channel();
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = tx.send(event.id);
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }));
        let mut bar = MenuBar {
            _menu: menu,
            new_connection,
            settings,
            events,
            shortcuts: false,
        };
        bar.set_shortcuts(true);
        bar
    }

    /// Gives the items their shortcuts (overview in front) or takes them
    /// away (a session window in front).
    pub fn set_shortcuts(&mut self, enabled: bool) {
        if self.shortcuts == enabled {
            return;
        }
        self.shortcuts = enabled;
        let (new, settings) = if enabled {
            (
                Some(Accelerator::new(Modifiers::META, Code::KeyN)),
                Some(Accelerator::new(Modifiers::META, Code::Comma)),
            )
        } else {
            (None, None)
        };
        for (item, accelerator) in [(&self.new_connection, new), (&self.settings, settings)] {
            if let Err(e) = item.set_accelerator(accelerator) {
                log::warn!("menu shortcut: {e}");
            }
        }
    }

    /// The actions chosen since the last call.
    pub fn poll(&self) -> Vec<Action> {
        self.events
            .try_iter()
            .filter_map(|id| match id.as_ref() {
                "new-connection" => Some(Action::NewConnection),
                "settings" => Some(Action::Settings),
                "updates" => Some(Action::CheckUpdates),
                _ => None,
            })
            .collect()
    }
}
