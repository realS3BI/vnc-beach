//! One remote session: the toolbar, the remote screen and the input it
//! forwards. Shown in its own window (viewport), one per connection.

use std::collections::HashMap;
use std::sync::mpsc::TryRecvError;

use egui::{Color32, Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2};
use rfb::keysym;
use rfb::messages::ClientMessage;

use crate::client::{Connection, Notice, Region, RemoteKind, Screen};
use crate::clipboard::Clipboard;
use crate::keys;
use crate::settings::{Scaling, Settings, Shortcuts};

/// Why a session ended.
pub enum Close {
    /// "Trennen" or the window's close button.
    User,
    /// The connection went away, with the reason.
    Lost(String),
}

/// Which keysyms stand for Ctrl, Cmd and Alt/Option on the remote machine.
/// Apple's Screen Sharing reads them differently from everyone else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyMap {
    ctrl: u32,
    cmd: u32,
    alt: u32,
}

impl KeyMap {
    fn for_remote(remote: RemoteKind) -> KeyMap {
        match remote {
            RemoteKind::AppleScreenSharing => KeyMap {
                ctrl: keysym::CONTROL_L,
                cmd: keysym::ALT_L,
                alt: keysym::META_L,
            },
            RemoteKind::BeachMac | RemoteKind::Other => KeyMap {
                ctrl: keysym::CONTROL_L,
                cmd: keysym::SUPER_L,
                alt: keysym::ALT_L,
            },
        }
    }
}

/// Wheel distance (in points) that counts as one wheel notch.
const POINTS_PER_NOTCH: f32 = 40.0;

pub struct Remote {
    conn: Connection,
    files: crate::files::Panel,
    /// Shown in the toolbar.
    label: String,
    settings: Settings,
    /// Local clipboard sync; `None` if switched off.
    clipboard: Option<Clipboard>,
    /// Keysyms the remote understands for the logical modifiers.
    keys: KeyMap,
    /// Send Ctrl as Cmd and Cmd as Ctrl.
    swap_cmd_ctrl: bool,
    texture: Option<egui::TextureHandle>,
    generation: u64,
    fb_size: [usize; 2],
    /// The server's displays, if it told us (ExtendedDesktopSize).
    screens: Vec<Screen>,
    /// Index into `screens` of the display shown alone; `None` shows all.
    view: Option<usize>,
    buttons: u8,
    /// Modifier keysyms currently held on the server.
    modifiers: Vec<u32>,
    /// Character keys sent as key presses (shortcuts), to release them later.
    keys_down: HashMap<Key, u32>,
    scroll: Vec2,
}

impl Remote {
    /// `viewport` is the session's own window.
    pub fn new(
        mut conn: Connection,
        label: String,
        settings: Settings,
        ctx: &egui::Context,
        viewport: egui::ViewportId,
    ) -> Self {
        let swap_cmd_ctrl = match settings.shortcuts {
            Shortcuts::Auto => cfg!(target_os = "macos") != conn.remote.is_mac(),
            Shortcuts::Swap => true,
            Shortcuts::Plain => false,
        };
        let keys = KeyMap::for_remote(conn.remote);
        let clipboard = settings.clipboard.then(|| Clipboard::start(ctx.clone(), viewport));
        let files = crate::files::Panel::new(
            conn.files.take().expect("one file worker per connection"),
            ctx.clone(),
            viewport,
        );
        Remote {
            conn,
            files,
            label,
            settings,
            clipboard,
            keys,
            swap_cmd_ctrl,
            texture: None,
            generation: u64::MAX,
            fb_size: [0, 0],
            screens: Vec::new(),
            view: None,
            buttons: 0,
            modifiers: Vec::new(),
            keys_down: HashMap::new(),
            scroll: Vec2::ZERO,
        }
    }

    /// The part of the framebuffer on show: one display or all of it.
    fn crop_of(&self, fb_width: usize, fb_height: usize) -> Region {
        match self.view.and_then(|i| self.screens.get(i)) {
            Some(s) => (s.x as usize, s.y as usize, s.width as usize, s.height as usize),
            None => (0, 0, fb_width, fb_height),
        }
    }

    fn crop(&self) -> Region {
        self.crop_of(self.fb_size[0], self.fb_size[1])
    }

    /// Shows one display (or all, `None`) and refits the window.
    fn select_screen(&mut self, view: Option<usize>, ctx: &egui::Context) {
        self.view = view;
        let (fb_width, fb_height) = {
            let fb = self.conn.fb.lock().unwrap();
            (fb.width, fb.height)
        };
        let region = view.map(|_| self.crop_of(fb_width, fb_height));
        self.conn.set_region(region);
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if self.settings.fit_window && !fullscreen {
            self.fit_window(ctx);
        }
    }

    /// Size for a new session window: the remote screen within the
    /// monitor, or a default when the window keeps its own size.
    pub fn initial_size(&self, monitor: Option<Vec2>) -> Vec2 {
        let fallback = Vec2::new(1100.0, 700.0);
        if !self.settings.fit_window {
            return fallback;
        }
        let fb = self.conn.fb.lock().unwrap();
        let (_, _, w, h) = self.crop_of(fb.width, fb.height);
        drop(fb);
        let (w, h) = (w.max(1) as f32, h.max(1) as f32);
        let Some(monitor) = monitor else {
            return fallback;
        };
        let max = monitor * 0.9 - Vec2::new(0.0, TOOLBAR_HEIGHT);
        let scale = (max.x / w).min(max.y / h).min(1.0);
        if scale > 0.0 {
            Vec2::new(w * scale, h * scale + TOOLBAR_HEIGHT)
        } else {
            fallback
        }
    }

    /// Whether the session was set up to start in fullscreen.
    pub fn wants_fullscreen(&self) -> bool {
        self.settings.fullscreen
    }

    /// Sizes the window to the remote screen, within the monitor.
    pub fn fit_window(&self, ctx: &egui::Context) {
        let fb = self.conn.fb.lock().unwrap();
        let (_, _, w, h) = self.crop_of(fb.width, fb.height);
        let (w, h) = (w.max(1) as f32, h.max(1) as f32);
        drop(fb);
        let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) else {
            return;
        };
        let max = monitor * 0.9 - Vec2::new(0.0, TOOLBAR_HEIGHT);
        let scale = (max.x / w).min(max.y / h).min(1.0);
        if scale > 0.0 {
            let size = Vec2::new(w * scale, h * scale + TOOLBAR_HEIGHT);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
    }

    /// One frame of the session window. `Err` means the window should go.
    pub fn show(&mut self, root: &mut egui::Ui) -> Result<(), Close> {
        let ctx = &root.ctx().clone();
        loop {
            match self.conn.notices.try_recv() {
                Ok(Notice::Clipboard(text)) => {
                    log::debug!("server clipboard: {} chars", text.chars().count());
                    if let Some(clipboard) = &self.clipboard {
                        clipboard.set(text);
                    }
                }
                Ok(Notice::Closed(reason)) => return Err(Close::Lost(reason)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(Close::Lost("Verbindung verloren".into())),
            }
        }
        let files_were_open = self.files.open;
        self.files.tick(ctx, self.settings.view_only);
        self.upload(ctx);
        if let Some(clipboard) = &self.clipboard {
            let changes: Vec<_> = clipboard
                .changes()
                .into_iter()
                .map(ClientMessage::ClientCutText)
                .collect();
            if !changes.is_empty() && !self.settings.view_only {
                log::debug!("sending {} clipboard change(s)", changes.len());
                self.conn.send(&changes);
            }
        }

        let mut disconnect = false;
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        egui::Panel::top("toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .show(root, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(&self.label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(self.files.button_label()).clicked() {
                            self.files.open = !self.files.open;
                        }
                        if let Some(camera) = self.conn.camera.as_ref().filter(|c| c.offered()) {
                            camera_toggle(ui, camera);
                        }
                        if ui.button("Trennen").clicked() {
                            disconnect = true;
                        }
                        if ui.button("Strg+Alt+Entf").clicked() {
                            self.ctrl_alt_del();
                        }
                        let label = if fullscreen {
                            "Vollbild beenden"
                        } else {
                            "Vollbild"
                        };
                        if ui.button(label).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                        }
                        let fit = self.settings.scaling == Scaling::Fit;
                        if ui
                            .selectable_label(!fit, "1:1")
                            .on_hover_text("Originalgröße")
                            .clicked()
                        {
                            self.settings.scaling = if fit { Scaling::Original } else { Scaling::Fit };
                        }
                        if self.screens.len() > 1 {
                            let mut view = self.view;
                            egui::ComboBox::from_id_salt("screen")
                                .selected_text(screen_label(&self.screens, view))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut view, None, screen_label(&self.screens, None));
                                    for i in 0..self.screens.len() {
                                        ui.selectable_value(
                                            &mut view,
                                            Some(i),
                                            screen_label(&self.screens, Some(i)),
                                        );
                                    }
                                });
                            if view != self.view {
                                self.select_screen(view, ctx);
                            }
                        }
                        let mut view_only = self.settings.view_only;
                        if ui.checkbox(&mut view_only, "Nur ansehen").changed() {
                            self.settings.view_only = view_only;
                            if view_only {
                                let out = self.release_all();
                                self.conn.send(&out);
                            }
                        }
                    });
                });
            });
        if disconnect {
            return Err(Close::User);
        }
        if self.files.open && !files_were_open {
            let out = self.release_all();
            self.conn.send(&out);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(root, |ui| match self.settings.scaling {
                Scaling::Fit => {
                    let avail = ui.available_rect_before_wrap();
                    let (_, _, w, h) = self.crop();
                    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
                    let scale = (avail.width() / w).min(avail.height() / h);
                    let image_rect = Rect::from_center_size(avail.center(), Vec2::new(w * scale, h * scale));
                    self.screen(ctx, ui, avail, image_rect);
                }
                Scaling::Original => {
                    // One remote pixel per physical pixel.
                    let (_, _, w, h) = self.crop();
                    let [w, h] = [w, h].map(|v| v.max(1) as f32 / ctx.pixels_per_point());
                    egui::ScrollArea::both().show(ui, |ui| {
                        let avail = ui.available_rect_before_wrap();
                        let size = Vec2::new(w.max(avail.width()), h.max(avail.height()));
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        let image_rect = Rect::from_center_size(rect.center(), Vec2::new(w, h));
                        self.screen(ctx, ui, rect, image_rect);
                    });
                }
            });
        self.files.show(ctx, self.settings.view_only);
        Ok(())
    }

    /// Draws the remote screen into `image_rect` and forwards input from `area`.
    fn screen(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, area: Rect, image_rect: Rect) {
        let response = ui.interact(area, ui.id().with("remote"), egui::Sense::click_and_drag());
        // Keep keyboard focus on the remote screen and stop egui from using
        // Tab/arrows/Esc for its own focus navigation.
        if !self.files.open {
            response.request_focus();
        }
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        if let Some(tex) = &self.texture {
            let [fw, fh] = self.fb_size.map(|v| v.max(1) as f32);
            let (x, y, w, h) = self.crop();
            let uv = Rect::from_min_max(
                Pos2::new(x as f32 / fw, y as f32 / fh),
                Pos2::new((x + w) as f32 / fw, (y + h) as f32 / fh),
            );
            ui.painter().image(tex.id(), image_rect, uv, Color32::WHITE);
        }
        if !self.settings.view_only
            && !self.files.open
            && ctx.input(|i| i.raw.hovered_files.is_empty() && i.raw.dropped_files.is_empty())
        {
            self.handle_input(ctx, image_rect);
        }
    }

    fn upload(&mut self, ctx: &egui::Context) {
        let fb = self.conn.fb.lock().unwrap();
        if fb.generation == self.generation {
            return;
        }
        self.generation = fb.generation;
        self.fb_size = [fb.width, fb.height];
        let image = egui::ColorImage::from_rgba_unmultiplied(self.fb_size, &fb.rgba);
        let screens_changed = fb.screens != self.screens;
        if screens_changed {
            self.screens = fb.screens.clone();
        }
        drop(fb);
        match &mut self.texture {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("remote", image, egui::TextureOptions::LINEAR)),
        }
        if screens_changed {
            // The arrangement changed under us: the client dropped its
            // region, so ask again for the display we show (if it still exists).
            let view = self.view.filter(|&i| i < self.screens.len());
            if view.is_some() {
                self.select_screen(view, ctx);
            } else if self.view.is_some() {
                self.view = None;
            }
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context, image_rect: Rect) {
        let (events, modifiers) = ctx.input(|i| (i.events.clone(), i.modifiers));
        self.sync_modifiers(modifiers);
        let has_text = events.iter().any(|e| matches!(e, Event::Text(_)));
        let mut out = Vec::new();

        for event in events {
            match event {
                Event::PointerMoved(pos) => {
                    if image_rect.contains(pos) || self.buttons != 0 {
                        out.push(self.pointer(pos, image_rect));
                    }
                }
                Event::PointerButton {
                    pos, button, pressed, ..
                } => {
                    let bit = match button {
                        PointerButton::Primary => 1,
                        PointerButton::Middle => 2,
                        PointerButton::Secondary => 4,
                        _ => continue,
                    };
                    if pressed && !image_rect.contains(pos) {
                        continue;
                    }
                    if pressed {
                        self.buttons |= bit;
                    } else {
                        self.buttons &= !bit;
                    }
                    out.push(self.pointer(pos, image_rect));
                }
                Event::MouseWheel { unit, delta, .. } => {
                    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
                        continue;
                    };
                    if !image_rect.contains(pos) {
                        continue;
                    }
                    let notches = match unit {
                        egui::MouseWheelUnit::Point => delta / POINTS_PER_NOTCH,
                        egui::MouseWheelUnit::Line => delta,
                        egui::MouseWheelUnit::Page => delta * 10.0,
                    };
                    self.scroll += notches;
                    out.extend(self.wheel(pos, image_rect));
                }
                Event::Key { key, pressed, .. } => {
                    if let Some(sym) = keys::special(key) {
                        out.push(ClientMessage::KeyEvent {
                            down: pressed,
                            key: sym,
                        });
                    } else if !pressed {
                        if let Some(sym) = self.keys_down.remove(&key) {
                            out.push(ClientMessage::KeyEvent {
                                down: false,
                                key: sym,
                            });
                        }
                    } else if !has_text {
                        // No text for this key press: a shortcut like Ctrl+S.
                        if let Some(sym) = keys::shortcut(key) {
                            self.keys_down.insert(key, sym);
                            out.push(ClientMessage::KeyEvent { down: true, key: sym });
                        }
                    }
                }
                Event::Text(text) => {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        out.extend(self.type_char(c));
                    }
                }
                // egui turns Cmd/Ctrl+C/X/V into these instead of key events.
                Event::Copy => out.extend(self.tap_shortcut('c', keysym::INSERT)),
                Event::Cut => out.extend(self.tap_shortcut('x', keysym::DELETE)),
                Event::Paste(text) => {
                    if self.conn.remote == RemoteKind::AppleScreenSharing {
                        // Screen Sharing ignores cut text from other viewers, so
                        // paste by typing instead.
                        if self.settings.clipboard {
                            out.extend(text.chars().flat_map(|c| self.type_text_char(c)));
                        }
                    } else {
                        if self.settings.clipboard {
                            out.push(ClientMessage::ClientCutText(text));
                        }
                        out.extend(self.tap_shortcut('v', keysym::INSERT));
                    }
                }
                Event::WindowFocused(false) => out.extend(self.release_all()),
                _ => {}
            }
        }
        if !out.is_empty() {
            self.conn.send(&out);
        }
    }

    fn pointer(&self, pos: Pos2, image_rect: Rect) -> ClientMessage {
        let (cx, cy, w, h) = self.crop();
        let rel = (pos - image_rect.min) / image_rect.size();
        let x = cx as f32 + (rel.x * w as f32).clamp(0.0, w.saturating_sub(1) as f32);
        let y = cy as f32 + (rel.y * h as f32).clamp(0.0, h.saturating_sub(1) as f32);
        let (x, y) = (x as u16, y as u16);
        ClientMessage::PointerEvent {
            buttons: self.buttons,
            x,
            y,
        }
    }

    /// Turns accumulated wheel movement into wheel button clicks (buttons 4–7).
    fn wheel(&mut self, pos: Pos2, image_rect: Rect) -> Vec<ClientMessage> {
        let mut out = Vec::new();
        let ClientMessage::PointerEvent { x, y, .. } = self.pointer(pos, image_rect) else {
            unreachable!()
        };
        let click = |bit: u8, out: &mut Vec<ClientMessage>| {
            out.push(ClientMessage::PointerEvent {
                buttons: self.buttons | bit,
                x,
                y,
            });
            out.push(ClientMessage::PointerEvent {
                buttons: self.buttons,
                x,
                y,
            });
        };
        // Positive delta moves content down/right, i.e. scrolls up/left.
        while self.scroll.y >= 1.0 {
            click(8, &mut out);
            self.scroll.y -= 1.0;
        }
        while self.scroll.y <= -1.0 {
            click(16, &mut out);
            self.scroll.y += 1.0;
        }
        while self.scroll.x >= 1.0 {
            click(32, &mut out);
            self.scroll.x -= 1.0;
        }
        while self.scroll.x <= -1.0 {
            click(64, &mut out);
            self.scroll.x += 1.0;
        }
        out
    }

    fn sync_modifiers(&mut self, m: Modifiers) {
        let (ctrl, cmd) = if self.swap_cmd_ctrl {
            (self.keys.cmd, self.keys.ctrl)
        } else {
            (self.keys.ctrl, self.keys.cmd)
        };
        let wanted: Vec<u32> = [
            (m.shift, keysym::SHIFT_L),
            (m.ctrl, ctrl),
            (m.alt, self.keys.alt),
            (m.mac_cmd, cmd),
        ]
        .into_iter()
        .filter_map(|(on, sym)| on.then_some(sym))
        .collect();
        let mut out = Vec::new();
        for &sym in self.modifiers.iter().filter(|s| !wanted.contains(s)) {
            out.push(ClientMessage::KeyEvent {
                down: false,
                key: sym,
            });
        }
        for &sym in wanted.iter().filter(|s| !self.modifiers.contains(s)) {
            out.push(ClientMessage::KeyEvent { down: true, key: sym });
        }
        if !out.is_empty() {
            self.modifiers = wanted;
            self.conn.send(&out);
        }
    }

    /// Sends one typed character. Held modifiers are lifted around it, since
    /// they were already used locally to produce the character (Shift for
    /// capitals, Option/AltGr for `@`, `€`, …).
    fn type_char(&self, c: char) -> Vec<ClientMessage> {
        let sym = keysym::from_char(c);
        let lift = |down| {
            self.modifiers
                .iter()
                .map(move |&key| ClientMessage::KeyEvent { down, key })
        };
        lift(false)
            .chain([
                ClientMessage::KeyEvent { down: true, key: sym },
                ClientMessage::KeyEvent {
                    down: false,
                    key: sym,
                },
            ])
            .chain(lift(true))
            .collect()
    }

    /// One character of pasted text as key presses, with modifiers lifted
    /// (the paste shortcut is still held) and newlines as Return.
    fn type_text_char(&self, c: char) -> Vec<ClientMessage> {
        if c == '\n' || c == '\r' {
            let lift = |down| {
                self.modifiers
                    .iter()
                    .map(move |&key| ClientMessage::KeyEvent { down, key })
            };
            return lift(false)
                .chain([
                    ClientMessage::KeyEvent {
                        down: true,
                        key: keysym::RETURN,
                    },
                    ClientMessage::KeyEvent {
                        down: false,
                        key: keysym::RETURN,
                    },
                ])
                .chain(lift(true))
                .collect();
        }
        if c.is_control() {
            return Vec::new();
        }
        self.type_char(c)
    }

    /// Copy/Cut/Paste: Ctrl/Cmd+letter, or Shift+Insert/Delete on Windows.
    fn tap_shortcut(&self, letter: char, shift_key: u32) -> Vec<ClientMessage> {
        let has_command = self
            .modifiers
            .iter()
            .any(|&m| m == self.keys.ctrl || m == self.keys.cmd);
        let key = if has_command {
            keysym::from_char(letter)
        } else {
            shift_key
        };
        vec![
            ClientMessage::KeyEvent { down: true, key },
            ClientMessage::KeyEvent { down: false, key },
        ]
    }

    fn ctrl_alt_del(&self) {
        use keysym::*;
        let seq = [CONTROL_L, ALT_L, DELETE];
        let mut out: Vec<_> = seq
            .iter()
            .map(|&key| ClientMessage::KeyEvent { down: true, key })
            .collect();
        out.extend(
            seq.iter()
                .rev()
                .map(|&key| ClientMessage::KeyEvent { down: false, key }),
        );
        self.conn.send(&out);
    }

    fn release_all(&mut self) -> Vec<ClientMessage> {
        let mut out: Vec<_> = self
            .keys_down
            .drain()
            .map(|(_, key)| ClientMessage::KeyEvent { down: false, key })
            .chain(
                self.modifiers
                    .drain(..)
                    .map(|key| ClientMessage::KeyEvent { down: false, key }),
            )
            .collect();
        if self.buttons != 0 {
            self.buttons = 0;
            out.push(ClientMessage::PointerEvent {
                buttons: 0,
                x: 0,
                y: 0,
            });
        }
        out
    }
}

pub const TOOLBAR_HEIGHT: f32 = 30.0;

/// Toolbar text for the display choice.
/// Toolbar switch for sharing the webcam; the hover text tells what it does.
fn camera_toggle(ui: &mut egui::Ui, camera: &std::sync::Arc<crate::camera::Camera>) {
    use crate::camera::Status;
    let status = camera.status();
    let label = match status {
        Status::Live(_) => "Kamera ●",
        _ => "Kamera",
    };
    let hint = match &status {
        Status::Off => "Eigene Kamera auf dem entfernten Rechner als Kamera „VNC Beach“ bereitstellen".to_string(),
        Status::Ready => "Bereit: Die Kamera startet, sobald dort eine App die Kamera „VNC Beach“ nutzt".to_string(),
        Status::Starting => "Kamera wird gestartet …".to_string(),
        Status::Live(name) => format!("{name} wird übertragen"),
        Status::Failed(message) => format!("Kamera nicht verfügbar: {message}"),
    };
    let response = ui.selectable_label(camera.enabled(), label).on_hover_text(hint);
    if response.clicked() {
        camera.set_enabled(!camera.enabled());
    }
}

fn screen_label(screens: &[Screen], view: Option<usize>) -> String {
    match view.and_then(|i| screens.get(i).map(|s| (i, s))) {
        Some((i, s)) => format!("Bildschirm {} ({}×{})", i + 1, s.width, s.height),
        None => "Alle Bildschirme".to_string(),
    }
}
