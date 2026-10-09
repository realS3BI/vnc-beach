//! The viewer window: server directory, connect form and remote screen.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use egui::{Align, Color32, Event, Key, Layout, Modifiers, PointerButton, Pos2, Rect, RichText, Vec2};
use rfb::keysym;
use rfb::messages::ClientMessage;

use crate::client::{parse_address, Connection, Notice};
use crate::keys;
use crate::servers::{Directory, Server};

pub struct App {
    directory: Directory,
    /// Saved server the form was filled from, if any.
    editing: Option<usize>,
    /// Saved server whose "Löschen" button waits for a second click.
    confirm_delete: Option<usize>,
    form: Server,
    error: Option<String>,
    state: State,
}

enum State {
    Idle,
    Connecting(Receiver<rfb::Result<Connection>>),
    Connected(Box<Remote>),
}

impl App {
    pub fn new(cc: &eframe::CreationContext, host: Option<String>, password: Option<String>) -> Self {
        // Cmd/Ctrl +/- must reach the remote machine instead of zooming our UI.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let directory = Directory::load();
        let mut app = App {
            directory,
            editing: None,
            confirm_delete: None,
            form: Server::default(),
            error: None,
            state: State::Idle,
        };
        if let Some(host) = host {
            // A saved server name on the command line stands for its address.
            match app.directory.find(&host) {
                Some(i) => app.select(i),
                None => app.form.host = host,
            }
            if let Some(pw) = password {
                app.form.password = pw;
            }
            app.connect(&cc.egui_ctx);
        } else if let Some(pw) = password {
            app.form.password = pw;
        }
        app
    }

    /// Fills the form from a saved server.
    fn select(&mut self, index: usize) {
        if let Some(server) = self.directory.servers.get(index) {
            self.form = server.clone();
            self.editing = Some(index);
            self.error = None;
        }
    }

    fn clear_form(&mut self) {
        self.form = Server::default();
        self.editing = None;
        self.error = None;
    }

    fn save_form(&mut self) {
        let mut server = self.form.clone();
        server.name = server.name.trim().to_string();
        server.host = server.host.trim().to_string();
        if server.name.is_empty() {
            server.name = server.host.clone();
        }
        let index = self.directory.upsert(self.editing, server.clone());
        self.form = server;
        self.editing = Some(index);
        self.error = match self.directory.save() {
            Ok(()) => None,
            Err(e) => Some(format!("Speichern fehlgeschlagen: {e}")),
        };
    }

    fn delete(&mut self, index: usize) {
        self.directory.remove(index);
        if let Err(e) = self.directory.save() {
            self.error = Some(format!("Speichern fehlgeschlagen: {e}"));
        }
        match self.editing {
            Some(i) if i == index => self.editing = None,
            Some(i) if i > index => self.editing = Some(i - 1),
            _ => {}
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let addr = parse_address(&self.form.host);
        let password = (!self.form.password.is_empty()).then(|| self.form.password.clone());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let repaint_ctx = ctx.clone();
            let result = Connection::open(&addr, password.as_deref(), move || repaint_ctx.request_repaint());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.error = None;
        self.state = State::Connecting(rx);
    }

    fn can_connect(&self) -> bool {
        !matches!(self.state, State::Connecting(_)) && !self.form.host.trim().is_empty()
    }

    /// Name shown in the title bar and toolbar: the saved name, else the
    /// desktop name the server reports.
    fn label(&self, conn: &Connection) -> String {
        match self.editing {
            Some(_) if !self.form.name.trim().is_empty() => self.form.name.trim().to_string(),
            _ => conn.name.clone(),
        }
    }

    fn connect_form(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let width = 420.0;
        let mut connect = false;
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.heading("vnc-beach");
                    ui.label("Mit einem VNC-Server verbinden");
                    ui.add_space(16.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(width, ui.available_height()),
                        Layout::top_down(Align::Min),
                        |ui| {
                            if !self.directory.servers.is_empty() {
                                connect |= self.directory_list(ui, width);
                                ui.add_space(16.0);
                            }
                            connect |= self.form_fields(ui, width);
                            if let Some(err) = &self.error {
                                ui.add_space(10.0);
                                ui.colored_label(Color32::from_rgb(230, 90, 80), err);
                            }
                            ui.add_space(24.0);
                        },
                    );
                });
            });
        });
        if connect && self.can_connect() {
            self.connect(&ctx);
        }
    }

    /// The saved servers. Returns true if one should be connected to.
    fn directory_list(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        let mut connect = false;
        let mut delete = None;
        let mut select = None;
        let heading = ui.label(RichText::new("Gespeicherte Server").strong());
        if let Some(path) = self.directory.path() {
            heading.on_hover_text(format!("Gespeichert in {}", path.display()));
        }
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(width - 16.0);
            egui::ScrollArea::vertical()
                .id_salt("directory")
                .max_height(240.0)
                .show(ui, |ui| {
                    for (i, server) in self.directory.servers.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let selected = self.editing == Some(i);
                            let text = format!("{}\n{}", server.name, server.host);
                            let row = ui.add_sized(
                                [width - 220.0, 36.0],
                                egui::Button::selectable(selected, RichText::new(text)),
                            );
                            if row.clicked() {
                                select = Some(i);
                            }
                            if row.double_clicked() {
                                select = Some(i);
                                connect = true;
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let confirm = self.confirm_delete == Some(i);
                                let label = if confirm { "Wirklich löschen?" } else { "Löschen" };
                                if ui.button(label).clicked() {
                                    if confirm {
                                        delete = Some(i);
                                    } else {
                                        self.confirm_delete = Some(i);
                                    }
                                } else if confirm && ui.input(|i| i.pointer.any_click()) {
                                    self.confirm_delete = None;
                                }
                                if ui.button("Verbinden").clicked() {
                                    select = Some(i);
                                    connect = true;
                                }
                            });
                        });
                    }
                });
        });
        if let Some(i) = select {
            self.select(i);
            self.confirm_delete = None;
        }
        if let Some(i) = delete {
            self.confirm_delete = None;
            self.delete(i);
        }
        connect
    }

    /// Name, address and password. Returns true if Enter or "Verbinden" was hit.
    fn form_fields(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        let connecting = matches!(self.state, State::Connecting(_));
        let title = match self.editing {
            Some(_) => "Server bearbeiten",
            None if self.directory.servers.is_empty() => "Verbindung",
            None => "Neuer Server",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            if self.editing.is_some() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Neuer Server").clicked() {
                        self.clear_form();
                    }
                });
            }
        });
        ui.add_space(4.0);
        let name = ui.add(
            egui::TextEdit::singleline(&mut self.form.name)
                .hint_text("Name, z. B. Mac Büro (optional)")
                .desired_width(width),
        );
        ui.add_space(6.0);
        let host = ui.add(
            egui::TextEdit::singleline(&mut self.form.host)
                .hint_text("Adresse, z. B. 192.168.1.20 oder mac.local")
                .desired_width(width),
        );
        ui.add_space(6.0);
        let pw = ui.add(
            egui::TextEdit::singleline(&mut self.form.password)
                .password(true)
                .hint_text("Passwort")
                .desired_width(width),
        );
        ui.add_space(10.0);
        let enter = (name.lost_focus() || host.lost_focus() || pw.lost_focus())
            && ui.input(|i| i.key_pressed(Key::Enter));
        let mut connect = false;
        let has_host = !self.form.host.trim().is_empty();
        ui.horizontal(|ui| {
            let button_width = (width - ui.spacing().item_spacing.x) / 2.0;
            let button = ui.add_enabled(
                !connecting && has_host,
                egui::Button::new(if connecting { "Verbinde …" } else { "Verbinden" })
                    .min_size(Vec2::new(button_width, 28.0)),
            );
            connect = button.clicked() || enter;
            let save = ui.add_enabled(
                has_host,
                egui::Button::new("Speichern").min_size(Vec2::new(button_width, 28.0)),
            );
            if save.clicked() {
                self.save_form();
            }
        });
        if self.form.host.is_empty() && !name.has_focus() && !host.has_focus() && !pw.has_focus() {
            host.request_focus();
        }
        connect
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        if let State::Connecting(rx) = &self.state {
            match rx.try_recv() {
                Ok(Ok(conn)) => {
                    let label = self.label(&conn);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{label} – vnc-beach")));
                    let remote = Remote::new(conn, label);
                    remote.fit_window(ctx);
                    self.state = State::Connected(Box::new(remote));
                }
                Ok(Err(e)) => {
                    self.error = Some(match e {
                        rfb::Error::PasswordRequired => "Der Server verlangt ein Passwort.".into(),
                        rfb::Error::AuthFailed(reason) => format!("Anmeldung fehlgeschlagen: {reason}"),
                        e => format!("Verbindung fehlgeschlagen: {e}"),
                    });
                    self.state = State::Idle;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.state = State::Idle,
            }
        }

        if let State::Connected(remote) = &mut self.state {
            match remote.show(ui) {
                Ok(()) => return,
                Err(reason) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title("vnc-beach".into()));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1100.0, 700.0)));
                    self.error = Some(format!("Getrennt: {reason}"));
                    self.state = State::Idle;
                }
            }
        }
        self.connect_form(ui);
    }
}

/// Wheel distance (in points) that counts as one wheel notch.
const POINTS_PER_NOTCH: f32 = 40.0;

struct Remote {
    conn: Connection,
    /// Shown in the toolbar.
    label: String,
    texture: Option<egui::TextureHandle>,
    generation: u64,
    fb_size: [usize; 2],
    buttons: u8,
    /// Modifier keysyms currently held on the server.
    modifiers: Vec<u32>,
    /// Character keys sent as key presses (shortcuts), to release them later.
    keys_down: HashMap<Key, u32>,
    scroll: Vec2,
}

impl Remote {
    fn new(conn: Connection, label: String) -> Self {
        Remote {
            conn,
            label,
            texture: None,
            generation: u64::MAX,
            fb_size: [0, 0],
            buttons: 0,
            modifiers: Vec::new(),
            keys_down: HashMap::new(),
            scroll: Vec2::ZERO,
        }
    }

    /// Sizes the window to the remote screen, within the monitor.
    fn fit_window(&self, ctx: &egui::Context) {
        let fb = self.conn.fb.lock().unwrap();
        let (w, h) = (fb.width as f32, fb.height as f32);
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

    fn show(&mut self, root: &mut egui::Ui) -> Result<(), String> {
        let ctx = &root.ctx().clone();
        loop {
            match self.conn.notices.try_recv() {
                Ok(Notice::Clipboard(text)) => ctx.copy_text(text),
                Ok(Notice::Closed(reason)) => return Err(reason),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err("Verbindung verloren".into()),
            }
        }
        self.upload(ctx);

        let mut disconnect = false;
        egui::Panel::top("toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .show(root, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(&self.label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Trennen").clicked() {
                            disconnect = true;
                        }
                        if ui.button("Strg+Alt+Entf").clicked() {
                            self.ctrl_alt_del();
                        }
                    });
                });
            });
        if disconnect {
            return Err("vom Benutzer".into());
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(root, |ui| {
                let avail = ui.available_rect_before_wrap();
                let [w, h] = self.fb_size.map(|v| v.max(1) as f32);
                let scale = (avail.width() / w).min(avail.height() / h);
                let image_rect = Rect::from_center_size(avail.center(), Vec2::new(w * scale, h * scale));
                let response = ui.interact(avail, ui.id().with("remote"), egui::Sense::click_and_drag());
                // Keep keyboard focus on the remote screen and stop egui from using
                // Tab/arrows/Esc for its own focus navigation.
                response.request_focus();
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
                    ui.painter().image(
                        tex.id(),
                        image_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                self.handle_input(ctx, image_rect);
            });
        Ok(())
    }

    fn upload(&mut self, ctx: &egui::Context) {
        let fb = self.conn.fb.lock().unwrap();
        if fb.generation == self.generation {
            return;
        }
        self.generation = fb.generation;
        self.fb_size = [fb.width, fb.height];
        let image = egui::ColorImage::from_rgba_unmultiplied(self.fb_size, &fb.rgba);
        drop(fb);
        match &mut self.texture {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("remote", image, egui::TextureOptions::LINEAR)),
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
                    out.push(ClientMessage::ClientCutText(text));
                    out.extend(self.tap_shortcut('v', keysym::INSERT));
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
        let [w, h] = self.fb_size;
        let rel = (pos - image_rect.min) / image_rect.size();
        let x = (rel.x * w as f32).clamp(0.0, w.saturating_sub(1) as f32) as u16;
        let y = (rel.y * h as f32).clamp(0.0, h.saturating_sub(1) as f32) as u16;
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
        let wanted: Vec<u32> = [
            (m.shift, keysym::SHIFT_L),
            (m.ctrl, keysym::CONTROL_L),
            (m.alt, keysym::ALT_L),
            (m.mac_cmd, keysym::SUPER_L),
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

    /// Copy/Cut/Paste: Ctrl/Cmd+letter, or Shift+Insert/Delete on Windows.
    fn tap_shortcut(&self, letter: char, shift_key: u32) -> Vec<ClientMessage> {
        let has_command = self
            .modifiers
            .iter()
            .any(|&m| m == keysym::CONTROL_L || m == keysym::SUPER_L);
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

const TOOLBAR_HEIGHT: f32 = 30.0;
