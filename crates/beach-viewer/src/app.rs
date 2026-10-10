//! The viewer window: favorites, connect form and remote screen.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use egui::{Align, Color32, Event, Key, Layout, Modifiers, PointerButton, Pos2, Rect, RichText, Vec2};
use rfb::keysym;
use rfb::messages::ClientMessage;

use crate::client::{parse_address, Connection, Notice, RemoteKind};
use crate::clipboard::Clipboard;
use crate::config::{self, Config};
use crate::keys;
use crate::server_panel::ServerPanel;
use crate::servers::{Directory, Server};
use crate::settings::{Colors, Overrides, Scaling, Settings, Shortcuts};
use crate::update::{self, Release, Version};

pub struct App {
    directory: Directory,
    config: Config,
    /// Saved server the form was filled from, if any.
    editing: Option<usize>,
    /// The "Favorit löschen" button waits for a second click.
    confirm_delete: bool,
    form: Server,
    /// The "Standardeinstellungen" window, with its draft (every field set).
    defaults: Option<Overrides>,
    /// The built-in server's status.
    server: Option<ServerPanel>,
    update: Update,
    error: Option<String>,
    state: State,
}

/// Where the update check stands.
enum Update {
    Idle,
    /// `manual`: started by the button, so report every outcome.
    Checking {
        rx: Receiver<Result<Release, String>>,
        manual: bool,
    },
    Available(Release),
    /// Answer to a manual check.
    Current,
    Installing(Receiver<Result<(), String>>),
    Failed(String),
}

impl Update {
    fn check(ctx: &egui::Context, manual: bool) -> Update {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::latest());
            ctx.request_repaint();
        });
        Update::Checking { rx, manual }
    }

    fn install(ctx: &egui::Context, release: Release) -> Update {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::install(&release));
            ctx.request_repaint();
        });
        Update::Installing(rx)
    }
}

enum State {
    Idle,
    Connecting(Receiver<rfb::Result<Connection>>, Settings),
    Connected(Box<Remote>),
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        // Cmd/Ctrl +/- must reach the remote machine instead of zooming our UI.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let directory = Directory::load();
        update::clean_up();
        App {
            update: Update::check(&cc.egui_ctx, false),
            directory,
            config: config::load(),
            editing: None,
            confirm_delete: false,
            form: Server::default(),
            defaults: None,
            server: Some(ServerPanel::new(cc.egui_ctx.clone())),
            error: None,
            state: State::Idle,
        }
    }

    /// Fills the form from a saved server.
    fn select(&mut self, index: usize) {
        if let Some(server) = self.directory.servers.get(index) {
            self.form = server.clone();
            self.editing = Some(index);
            self.confirm_delete = false;
            self.error = None;
        }
    }

    fn clear_form(&mut self) {
        self.form = Server::default();
        self.editing = None;
        self.confirm_delete = false;
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

    /// The form's settings with the defaults filled in.
    fn settings(&self) -> Settings {
        self.form.settings.apply(self.config.viewer)
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let addr = parse_address(&self.form.host);
        let password = (!self.form.password.is_empty()).then(|| self.form.password.clone());
        let settings = self.settings();
        let pf = settings.colors.pixel_format();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let repaint_ctx = ctx.clone();
            let result = Connection::open(&addr, password.as_deref(), pf, move || {
                repaint_ctx.request_repaint()
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.error = None;
        self.state = State::Connecting(rx, settings);
    }

    fn can_connect(&self) -> bool {
        !matches!(self.state, State::Connecting(..)) && !self.form.host.trim().is_empty()
    }

    fn save_config(&mut self) {
        if let Err(e) = config::save(&self.config) {
            self.error = Some(format!("Speichern fehlgeschlagen: {e}"));
        }
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
        let mut connect = false;
        egui::Panel::left("favorites")
            .default_size(320.0)
            .min_size(220.0)
            .max_size(480.0)
            .resizable(true)
            .show(root, |ui| {
                egui::Panel::bottom("this-computer").show(ui, |ui| {
                    if let Some(panel) = &mut self.server {
                        panel.show(ui, &self.config);
                        ui.separator();
                    }
                    self.version_row(ui, &ctx);
                    ui.add_space(4.0);
                });
                connect |= self.favorites_panel(ui);
            });
        let width = 420.0;
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    self.update_banner(ui, &ctx, width);
                    ui.add_space(24.0);
                    ui.heading("VNC Beach");
                    ui.label("Mit einem VNC-Server verbinden");
                    ui.add_space(16.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(width, ui.available_height()),
                        Layout::top_down(Align::Min),
                        |ui| {
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

    /// Picks up results of the update check or install.
    fn poll_update(&mut self, ctx: &egui::Context) {
        match &self.update {
            Update::Checking { rx, manual } => {
                let manual = *manual;
                match rx.try_recv() {
                    Ok(Ok(release)) if update::is_newer(&release) => {
                        self.update = Update::Available(release);
                    }
                    Ok(Ok(_)) => {
                        self.update = if manual { Update::Current } else { Update::Idle };
                    }
                    Ok(Err(e)) => {
                        log::info!("update check: {e}");
                        self.update = if manual { Update::Failed(e) } else { Update::Idle };
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => self.update = Update::Idle,
                }
            }
            Update::Installing(rx) => match rx.try_recv() {
                // The new version is starting; make room for it.
                Ok(Ok(())) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Ok(Err(e)) => self.update = Update::Failed(format!("Update fehlgeschlagen: {e}")),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.update = Update::Idle,
            },
            _ => {}
        }
    }

    /// "Version x.y.z" and the manual update check, at the bottom of the
    /// sidebar.
    fn version_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal_wrapped(|ui| {
            ui.weak(format!("Version {}", Version::current()));
            let busy = matches!(self.update, Update::Checking { .. } | Update::Installing(_));
            let button = ui.add_enabled(!busy, egui::Button::new("Nach Updates suchen").small());
            if button.clicked() {
                self.update = Update::check(ctx, true);
            }
            if matches!(self.update, Update::Checking { manual: true, .. }) {
                ui.spinner();
            }
        });
    }

    /// News about updates above the connect form.
    fn update_banner(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, width: f32) {
        let accent = Color32::from_rgb(63, 180, 230);
        let text = match &self.update {
            Update::Available(r) => format!(
                "VNC Beach {} ist verfügbar. Installiert ist {}.",
                r.version,
                Version::current()
            ),
            Update::Installing(_) => "Update wird geladen und installiert …".to_string(),
            Update::Current => format!("VNC Beach ist aktuell ({}).", Version::current()),
            Update::Failed(e) => e.clone(),
            Update::Idle | Update::Checking { .. } => return,
        };
        let failed = matches!(self.update, Update::Failed(_));
        ui.add_space(12.0);
        egui::Frame::group(ui.style())
            .stroke(egui::Stroke::new(
                1.0,
                if failed {
                    Color32::from_rgb(230, 90, 80)
                } else {
                    accent
                },
            ))
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(width - 22.0);
                ui.horizontal_wrapped(|ui| {
                    if matches!(self.update, Update::Installing(_)) {
                        ui.spinner();
                    }
                    ui.label(text);
                });
                let mut next = None;
                ui.horizontal(|ui| match &self.update {
                    Update::Available(release) => {
                        if release.asset.is_some() && update::can_install() {
                            if ui.button("Jetzt aktualisieren").clicked() {
                                next = Some(Update::install(ctx, release.clone()));
                            }
                        } else if ui.button("Download-Seite öffnen").clicked() {
                            ctx.open_url(egui::OpenUrl::new_tab(&release.page));
                        }
                        if ui.button("Später").clicked() {
                            next = Some(Update::Idle);
                        }
                    }
                    Update::Current | Update::Failed(_) if ui.button("OK").clicked() => {
                        next = Some(Update::Idle);
                    }
                    _ => {}
                });
                if let Some(next) = next {
                    self.update = next;
                }
            });
    }

    /// The favorites list in the side panel. Returns true if one should be
    /// connected to (double click).
    fn favorites_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let mut connect = false;
        let mut select = None;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let heading = ui.label(RichText::new("Favoriten").strong().size(16.0));
            if let Some(path) = self.directory.path() {
                heading.on_hover_text(format!("Gespeichert in {}", path.display()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let new = ui.add_enabled(self.editing.is_some(), egui::Button::new("Neu"));
                if new
                    .on_hover_text("Formular leeren für einen neuen Favoriten")
                    .clicked()
                {
                    self.clear_form();
                }
            });
        });
        ui.add_space(6.0);
        if self.directory.servers.is_empty() {
            ui.add_space(8.0);
            ui.weak("Noch keine Favoriten.");
            ui.add_space(4.0);
            ui.weak("Rechts Name, Adresse und Passwort eingeben und auf „Als Favorit speichern“ klicken.");
            return false;
        }
        egui::ScrollArea::vertical().id_salt("favorites").show(ui, |ui| {
            for (i, server) in self.directory.servers.iter().enumerate() {
                let selected = self.editing == Some(i);
                let row = favorite_row(ui, server, selected);
                if row.double_clicked() {
                    select = Some(i);
                    connect = true;
                } else if row.clicked() {
                    select = Some(i);
                }
            }
            ui.add_space(8.0);
            ui.weak("Klick lädt den Favoriten ins Formular, Doppelklick verbindet.");
        });
        if let Some(i) = select {
            self.select(i);
        }
        connect
    }

    /// Name, address and password. Returns true if Enter or "Verbinden" was hit.
    fn form_fields(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        let connecting = matches!(self.state, State::Connecting(..));
        let saved = self.editing.and_then(|i| self.directory.servers.get(i)).cloned();
        let modified = saved.as_ref().is_some_and(|s| *s != self.form);
        let title = if saved.is_some() {
            "Favorit bearbeiten"
        } else {
            "Neue Verbindung"
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            if modified {
                ui.weak("– nicht gespeicherte Änderungen");
            }
        });
        ui.add_space(4.0);
        let name = ui.add(
            egui::TextEdit::singleline(&mut self.form.name)
                .hint_text("Name in der Favoritenliste, z. B. Mac Büro (optional)")
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
        ui.add_space(6.0);
        self.settings_section(ui, width);
        ui.add_space(10.0);
        let enter = (name.lost_focus() || host.lost_focus() || pw.lost_focus())
            && ui.input(|i| i.key_pressed(Key::Enter));
        let has_host = !self.form.host.trim().is_empty();
        let gap = ui.spacing().item_spacing.x;
        let mut connect = false;
        ui.horizontal(|ui| {
            let button_width = (width - gap) / 2.0;
            let button = ui.add_enabled(
                !connecting && has_host,
                egui::Button::new(if connecting { "Verbinde …" } else { "Verbinden" })
                    .min_size(Vec2::new(button_width, 28.0)),
            );
            connect = button.clicked() || enter;
            let label = if saved.is_some() {
                "Speichern"
            } else {
                "Als Favorit speichern"
            };
            let save = ui.add_enabled(
                has_host,
                egui::Button::new(label).min_size(Vec2::new(button_width, 28.0)),
            );
            if save.clicked() {
                self.save_form();
            }
        });
        if let Some(index) = self.editing {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let button_width = (width - gap) / 2.0;
                let label = if self.confirm_delete {
                    "Wirklich löschen?"
                } else {
                    "Favorit löschen"
                };
                let delete = ui.add(egui::Button::new(label).min_size(Vec2::new(button_width, 28.0)));
                if delete.clicked() {
                    if self.confirm_delete {
                        self.confirm_delete = false;
                        self.delete(index);
                    } else {
                        self.confirm_delete = true;
                    }
                } else if self.confirm_delete && ui.input(|i| i.pointer.any_click()) {
                    self.confirm_delete = false;
                }
                let discard = ui.add_enabled(
                    modified,
                    egui::Button::new("Änderungen verwerfen").min_size(Vec2::new(button_width, 28.0)),
                );
                if discard.clicked() {
                    self.select(index);
                }
            });
        }
        if self.form.host.is_empty() && !name.has_focus() && !host.has_focus() && !pw.has_focus() {
            host.request_focus();
        }
        connect
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        self.poll_update(ctx);
        if let State::Connecting(rx, settings) = &self.state {
            let settings = *settings;
            match rx.try_recv() {
                Ok(Ok(conn)) => {
                    let label = self.label(&conn);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{label} – VNC Beach")));
                    let remote = Remote::new(conn, label, settings, ctx);
                    if settings.fullscreen {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                    } else if settings.fit_window {
                        remote.fit_window(ctx);
                    }
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
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title("VNC Beach".into()));
                    if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1100.0, 700.0)));
                    self.error = Some(format!("Getrennt: {reason}"));
                    self.state = State::Idle;
                }
            }
        }
        self.connect_form(ui);
        self.defaults_window(ctx);
        if let Some(panel) = &mut self.server {
            panel.window(ctx, &mut self.config);
        }
    }
}

impl App {
    /// Collapsible per-connection settings; each one can follow the defaults.
    fn settings_section(&mut self, ui: &mut egui::Ui, width: f32) {
        let changed = self.form.settings != Overrides::default();
        let title = if changed {
            "Einstellungen (abweichend)"
        } else {
            "Einstellungen"
        };
        egui::CollapsingHeader::new(title)
            .id_salt("settings")
            .default_open(changed)
            .show(ui, |ui| {
                ui.set_width(width);
                let defaults = self.config.viewer;
                settings_grid(ui, "overrides", &mut self.form.settings, Some(defaults));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.small_button("Standardeinstellungen …").clicked() {
                        self.defaults = Some(all_set(self.config.viewer));
                    }
                    if changed && ui.small_button("Alles auf Standard").clicked() {
                        self.form.settings = Overrides::default();
                    }
                });
            });
    }

    /// The window for editing the defaults that every favorite starts from.
    fn defaults_window(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.defaults else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Standardeinstellungen")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Gelten für jede Verbindung, bei der nichts anderes eingestellt ist.");
                ui.add_space(8.0);
                settings_grid(ui, "defaults", &mut draft, None);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    save = ui.button("Speichern").clicked();
                    cancel = ui.button("Abbrechen").clicked();
                });
            });
        if save {
            self.config.viewer = draft.apply(self.config.viewer);
            self.save_config();
            self.defaults = None;
        } else if open && !cancel {
            self.defaults = Some(draft);
        } else {
            self.defaults = None;
        }
    }
}

/// A setting with its selectable values and their labels.
struct Field<T: 'static> {
    label: &'static str,
    options: &'static [(T, &'static str)],
}

const VIEW_ONLY: Field<bool> = Field {
    label: "Eingabe",
    options: &[(false, "Tastatur und Maus senden"), (true, "Nur ansehen")],
};
const SCALING: Field<Scaling> = Field {
    label: "Darstellung",
    options: &[
        (Scaling::Fit, "An das Fenster anpassen"),
        (Scaling::Original, "Originalgröße"),
    ],
};
const COLORS: Field<Colors> = Field {
    label: "Farben",
    options: &[
        (Colors::Full, "Volle Farbtiefe"),
        (Colors::Medium, "16 Bit"),
        (Colors::Low, "256 Farben (langsame Leitung)"),
    ],
};
const CLIPBOARD: Field<bool> = Field {
    label: "Zwischenablage",
    options: &[(true, "Synchronisieren"), (false, "Nicht synchronisieren")],
};
const SHORTCUTS: Field<Shortcuts> = Field {
    label: "Tastenkürzel",
    options: &[
        (Shortcuts::Auto, "Automatisch (Strg und Cmd je nach System)"),
        (Shortcuts::Swap, "Strg und Cmd tauschen"),
        (Shortcuts::Plain, "Unverändert senden"),
    ],
};
const FULLSCREEN: Field<bool> = Field {
    label: "Vollbild",
    options: &[(false, "Nein"), (true, "Beim Verbinden")],
};
const FIT_WINDOW: Field<bool> = Field {
    label: "Fenstergröße",
    options: &[(true, "An den Bildschirm anpassen"), (false, "Beibehalten")],
};

/// Overrides with every field set, for editing the defaults themselves.
fn all_set(s: Settings) -> Overrides {
    Overrides {
        view_only: Some(s.view_only),
        scaling: Some(s.scaling),
        colors: Some(s.colors),
        clipboard: Some(s.clipboard),
        shortcuts: Some(s.shortcuts),
        fullscreen: Some(s.fullscreen),
        fit_window: Some(s.fit_window),
    }
}

/// One row per setting. With `defaults`, each row also offers "Standard".
fn settings_grid(ui: &mut egui::Ui, id: &str, values: &mut Overrides, defaults: Option<Settings>) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            option_combo(
                ui,
                &VIEW_ONLY,
                &mut values.view_only,
                defaults.map(|d| d.view_only),
            );
            option_combo(ui, &SCALING, &mut values.scaling, defaults.map(|d| d.scaling));
            option_combo(ui, &COLORS, &mut values.colors, defaults.map(|d| d.colors));
            option_combo(
                ui,
                &CLIPBOARD,
                &mut values.clipboard,
                defaults.map(|d| d.clipboard),
            );
            option_combo(
                ui,
                &SHORTCUTS,
                &mut values.shortcuts,
                defaults.map(|d| d.shortcuts),
            );
            option_combo(
                ui,
                &FULLSCREEN,
                &mut values.fullscreen,
                defaults.map(|d| d.fullscreen),
            );
            option_combo(
                ui,
                &FIT_WINDOW,
                &mut values.fit_window,
                defaults.map(|d| d.fit_window),
            );
        });
}

fn option_combo<T: Copy + PartialEq + 'static>(
    ui: &mut egui::Ui,
    field: &Field<T>,
    value: &mut Option<T>,
    default: Option<T>,
) {
    let name = |v: T| {
        field
            .options
            .iter()
            .find(|(o, _)| *o == v)
            .map(|(_, n)| *n)
            .unwrap_or("?")
    };
    let selected = match (*value, default) {
        (Some(v), _) => name(v).to_string(),
        (None, Some(d)) => format!("Standard: {}", name(d)),
        (None, None) => String::from("?"),
    };
    ui.label(field.label);
    egui::ComboBox::from_id_salt(field.label)
        .selected_text(selected)
        .width(260.0)
        .show_ui(ui, |ui| {
            if let Some(d) = default {
                ui.selectable_value(value, None, format!("Standard: {}", name(d)));
            }
            for &(v, n) in field.options {
                ui.selectable_value(value, Some(v), n);
            }
        });
    ui.end_row();
}

/// One entry of the favorites list: name and address, left-aligned, with the
/// usual hover and selection colours.
fn favorite_row(ui: &mut egui::Ui, server: &Server, selected: bool) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 44.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        let fill = if selected || response.hovered() {
            visuals.weak_bg_fill
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let inner = rect.shrink2(Vec2::new(10.0, 6.0));
        ui.painter().text(
            inner.left_top(),
            egui::Align2::LEFT_TOP,
            &server.name,
            egui::FontId::proportional(14.0),
            visuals.text_color(),
        );
        ui.painter().text(
            inner.left_bottom(),
            egui::Align2::LEFT_BOTTOM,
            &server.host,
            egui::FontId::proportional(12.0),
            ui.visuals().weak_text_color(),
        );
    }
    response
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

struct Remote {
    conn: Connection,
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
    buttons: u8,
    /// Modifier keysyms currently held on the server.
    modifiers: Vec<u32>,
    /// Character keys sent as key presses (shortcuts), to release them later.
    keys_down: HashMap<Key, u32>,
    scroll: Vec2,
}

impl Remote {
    fn new(conn: Connection, label: String, settings: Settings, ctx: &egui::Context) -> Self {
        let swap_cmd_ctrl = match settings.shortcuts {
            Shortcuts::Auto => cfg!(target_os = "macos") != conn.remote.is_mac(),
            Shortcuts::Swap => true,
            Shortcuts::Plain => false,
        };
        let keys = KeyMap::for_remote(conn.remote);
        let clipboard = settings.clipboard.then(|| Clipboard::start(ctx.clone()));
        Remote {
            conn,
            label,
            settings,
            clipboard,
            keys,
            swap_cmd_ctrl,
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
                Ok(Notice::Clipboard(text)) => {
                    log::debug!("server clipboard: {} chars", text.chars().count());
                    if let Some(clipboard) = &self.clipboard {
                        clipboard.set(text);
                    }
                }
                Ok(Notice::Closed(reason)) => return Err(reason),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err("Verbindung verloren".into()),
            }
        }
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
            return Err("vom Benutzer".into());
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(root, |ui| match self.settings.scaling {
                Scaling::Fit => {
                    let avail = ui.available_rect_before_wrap();
                    let [w, h] = self.fb_size.map(|v| v.max(1) as f32);
                    let scale = (avail.width() / w).min(avail.height() / h);
                    let image_rect = Rect::from_center_size(avail.center(), Vec2::new(w * scale, h * scale));
                    self.screen(ctx, ui, avail, image_rect);
                }
                Scaling::Original => {
                    // One remote pixel per physical pixel.
                    let [w, h] = self.fb_size.map(|v| v.max(1) as f32 / ctx.pixels_per_point());
                    egui::ScrollArea::both().show(ui, |ui| {
                        let avail = ui.available_rect_before_wrap();
                        let size = Vec2::new(w.max(avail.width()), h.max(avail.height()));
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        let image_rect = Rect::from_center_size(rect.center(), Vec2::new(w, h));
                        self.screen(ctx, ui, rect, image_rect);
                    });
                }
            });
        Ok(())
    }

    /// Draws the remote screen into `image_rect` and forwards input from `area`.
    fn screen(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, area: Rect, image_rect: Rect) {
        let response = ui.interact(area, ui.id().with("remote"), egui::Sense::click_and_drag());
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
        if !self.settings.view_only {
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

const TOOLBAR_HEIGHT: f32 = 30.0;
