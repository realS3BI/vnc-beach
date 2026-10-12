//! The viewer window: favorites, connect form and remote screen.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use egui::load::SizedTexture;
use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Key, Layout, Margin, Modifiers,
    PointerButton, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use rfb::keysym;
use rfb::messages::ClientMessage;

use crate::client::{parse_address, Connection, Notice, Region, RemoteKind, Screen};
use crate::clipboard::Clipboard;
use crate::config::{self, Config};
use crate::icon;
use crate::keys;
use crate::server_panel::ServerPanel;
use crate::servers::{Directory, Server};
use crate::settings::{Colors, Overrides, Scaling, Settings, Shortcuts};
use crate::theme::{self, Kind, Palette};
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
    /// The app icon, for the sidebar header.
    logo: egui::TextureHandle,
    /// Show the form's password in plain text.
    show_password: bool,
    /// The per-connection settings are unfolded.
    settings_open: bool,
    /// Search text over the favorites (only shown for longer lists).
    filter: String,
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
        theme::apply(&cc.egui_ctx);
        let icon = icon::window();
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba),
            egui::TextureOptions::LINEAR,
        );
        let directory = Directory::load();
        update::clean_up();
        App {
            logo,
            show_password: false,
            settings_open: false,
            filter: String::new(),
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
            self.settings_open = !server.settings.is_empty();
            self.error = None;
        }
    }

    fn clear_form(&mut self) {
        self.form = Server::default();
        self.editing = None;
        self.confirm_delete = false;
        self.settings_open = false;
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
        let p = Palette::of(root);
        let mut connect = false;
        egui::Panel::left("favorites")
            .default_size(330.0)
            .min_size(250.0)
            .max_size(500.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(Margin::symmetric(14, 14)),
            )
            .show(root, |ui| {
                egui::Panel::bottom("this-computer")
                    .frame(egui::Frame::NONE)
                    .show_separator_line(false)
                    .show(ui, |ui| {
                        ui.add_space(10.0);
                        if let Some(panel) = &mut self.server {
                            panel.show(ui, &self.config);
                        }
                        ui.add_space(8.0);
                        self.version_row(ui, &ctx);
                    });
                connect |= self.favorites_panel(ui);
            });
        let width = 460.0;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.canvas)
                    .inner_margin(Margin::symmetric(28, 24)),
            )
            .show(root, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.allocate_ui_with_layout(
                            Vec2::new(width, ui.available_height()),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(width);
                                ui.add_space(8.0);
                                connect |= self.form_fields(ui, width);
                                if let Some(err) = self.error.clone() {
                                    ui.add_space(12.0);
                                    theme::banner(ui, theme::CORAL).show(ui, |ui| {
                                        ui.set_width(width - 28.0);
                                        ui.label(RichText::new(err).color(p.text));
                                    });
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

    /// Version and update status at the bottom of the sidebar. The button on
    /// the right is always the next step: check, install, open the download
    /// page, or try again.
    fn version_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        let note = match &self.update {
            Update::Available(r) => Some((format!("VNC Beach {} ist verfügbar.", r.version), p.accent)),
            Update::Installing(_) => Some(("Update wird geladen und installiert …".to_string(), p.weak)),
            Update::Current => Some(("VNC Beach ist aktuell.".to_string(), p.weak)),
            Update::Failed(e) => Some((e.clone(), theme::CORAL)),
            Update::Checking { manual: true, .. } => Some(("Suche nach Updates …".to_string(), p.weak)),
            Update::Idle | Update::Checking { manual: false, .. } => None,
        };
        if let Some((text, color)) = note {
            ui.label(RichText::new(text).size(12.0).color(color));
        }
        let mut next = None;
        ui.horizontal(|ui| {
            ui.small(RichText::new(format!("Version {}", Version::current())).weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &self.update {
                Update::Idle | Update::Current => {
                    if theme::Button::new("Nach Updates suchen", Kind::Ghost)
                        .small()
                        .show(ui)
                        .clicked()
                    {
                        next = Some(Update::check(ctx, true));
                    }
                }
                Update::Failed(_) => {
                    if theme::Button::new("Erneut versuchen", Kind::Ghost)
                        .small()
                        .show(ui)
                        .clicked()
                    {
                        next = Some(Update::check(ctx, true));
                    }
                }
                Update::Available(release) => {
                    if release.asset.is_some() && update::can_install() {
                        if theme::Button::new("Jetzt aktualisieren", Kind::Primary)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            next = Some(Update::install(ctx, release.clone()));
                        }
                    } else if theme::Button::new("Download-Seite öffnen", Kind::Primary)
                        .small()
                        .show(ui)
                        .clicked()
                    {
                        ctx.open_url(egui::OpenUrl::new_tab(&release.page));
                    }
                }
                Update::Checking { .. } | Update::Installing(_) => {
                    ui.spinner();
                }
            });
        });
        if let Some(next) = next {
            self.update = next;
        }
    }

    /// The favorites list in the side panel. Returns true if one should be
    /// connected to (double click or the arrow button).
    fn favorites_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let p = Palette::of(ui);
        let mut connect = false;
        let mut select = None;
        ui.horizontal(|ui| {
            ui.add(
                egui::Image::from_texture(SizedTexture::new(self.logo.id(), Vec2::splat(30.0)))
                    .corner_radius(8.0),
            );
            ui.add_space(2.0);
            ui.label(RichText::new("VNC Beach").font(theme::bold(18.0)).color(p.text));
        });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let heading = theme::section_label(ui, "Favoriten");
            if let Some(path) = self.directory.path() {
                heading.on_hover_text(format!("Gespeichert in {}", path.display()));
            }
            let count = self.directory.servers.len();
            if count > 0 {
                theme::pill(ui, &count.to_string(), p.weak);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let fresh = self.editing.is_none() && self.form == Server::default();
                let new = theme::Button::new("+ Neu", Kind::Ghost)
                    .small()
                    .enabled(!fresh)
                    .show(ui);
                if new
                    .on_hover_text("Formular leeren für einen neuen Favoriten")
                    .clicked()
                {
                    self.clear_form();
                }
            });
        });
        ui.add_space(8.0);
        if self.directory.servers.is_empty() {
            egui::Frame::new()
                .stroke(Stroke::new(1.0, p.card_stroke))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Noch keine Favoriten").font(theme::semibold(14.0)));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(
                            "Rechts Name, Adresse und Passwort eingeben und auf „Als Favorit speichern“ klicken.",
                        )
                        .size(12.5)
                        .color(p.weak),
                    );
                });
            return false;
        }
        if self.directory.servers.len() >= 6 {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Favoriten durchsuchen")
                    .margin(Margin::symmetric(10, 6))
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
        } else {
            self.filter.clear();
        }
        let filter = self.filter.trim().to_lowercase();
        egui::ScrollArea::vertical().id_salt("favorites").show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let mut shown = 0;
            for (i, server) in self.directory.servers.iter().enumerate() {
                if !filter.is_empty()
                    && !server.name.to_lowercase().contains(&filter)
                    && !server.host.to_lowercase().contains(&filter)
                {
                    continue;
                }
                shown += 1;
                let card = favorite_card(ui, server, self.editing == Some(i));
                if card.connect {
                    select = Some(i);
                    connect = true;
                } else if card.select {
                    select = Some(i);
                }
            }
            if shown == 0 {
                ui.add_space(8.0);
                ui.weak("Kein Favorit passt zur Suche.");
            }
            ui.add_space(6.0);
            ui.small(RichText::new("Klick wählt aus, Doppelklick oder der Pfeil verbindet.").weak());
        });
        if let Some(i) = select {
            self.select(i);
        }
        connect
    }

    /// Name, address and password in a card. Returns true if Enter or
    /// "Verbinden" was hit.
    fn form_fields(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        let p = Palette::of(ui);
        let connecting = matches!(self.state, State::Connecting(..));
        let saved = self.editing.and_then(|i| self.directory.servers.get(i)).cloned();
        let modified = saved.as_ref().is_some_and(|s| *s != self.form);
        let (title, subtitle) = if saved.is_some() {
            (
                "Favorit bearbeiten",
                "Änderungen gelten für diesen Favoriten, sobald du speicherst.",
            )
        } else {
            (
                "Neue Verbindung",
                "Adresse des VNC-Servers eingeben, optional mit Name und Passwort.",
            )
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).font(theme::bold(22.0)).color(p.text));
            if modified {
                ui.add_space(4.0);
                theme::pill(ui, "Nicht gespeichert", theme::AMBER);
            }
        });
        ui.label(RichText::new(subtitle).color(p.weak));
        ui.add_space(14.0);

        let inner = width - 34.0;
        let field_margin = Margin::symmetric(10, 7);
        let mut connect = false;
        let mut fields = None;
        theme::card(ui).show(ui, |ui| {
            ui.set_width(inner);
            theme::field_label(ui, "Name");
            let name = ui.add(
                egui::TextEdit::singleline(&mut self.form.name)
                    .hint_text("z. B. Mac Büro (optional)")
                    .margin(field_margin)
                    .desired_width(inner),
            );
            ui.add_space(8.0);
            theme::field_label(ui, "Adresse");
            let host = ui.add(
                egui::TextEdit::singleline(&mut self.form.host)
                    .hint_text("192.168.1.20 oder mac.local, bei Bedarf mit :Port")
                    .margin(field_margin)
                    .desired_width(inner),
            );
            ui.add_space(8.0);
            theme::field_label(ui, "Passwort");
            let pw = ui
                .horizontal(|ui| {
                    let pw = ui.add(
                        egui::TextEdit::singleline(&mut self.form.password)
                            .password(!self.show_password)
                            .hint_text("Passwort des Servers")
                            .margin(field_margin)
                            .desired_width(inner - 96.0),
                    );
                    let eye = if self.show_password {
                        "Verbergen"
                    } else {
                        "Anzeigen"
                    };
                    if theme::Button::new(eye, Kind::Ghost).small().show(ui).clicked() {
                        self.show_password = !self.show_password;
                    }
                    pw
                })
                .inner;
            ui.add_space(12.0);
            self.settings_section(ui, inner);
            ui.add_space(14.0);

            let enter = (name.lost_focus() || host.lost_focus() || pw.lost_focus())
                && ui.input(|i| i.key_pressed(Key::Enter));
            let has_host = !self.form.host.trim().is_empty();
            let gap = 8.0;
            let half = Vec2::new((inner - gap) / 2.0, 36.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                let label = if connecting { "Verbinde …" } else { "Verbinden" };
                let button = theme::Button::new(label, Kind::Primary)
                    .enabled(!connecting && has_host)
                    .min_size(half)
                    .show(ui);
                connect = button.clicked() || enter;
                let label = if saved.is_some() {
                    "Speichern"
                } else {
                    "Als Favorit speichern"
                };
                let save = theme::Button::new(label, Kind::Secondary)
                    .enabled(has_host)
                    .min_size(half)
                    .show(ui);
                if save.clicked() {
                    self.save_form();
                }
            });
            if let Some(index) = self.editing {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    let label = if self.confirm_delete {
                        "Wirklich löschen?"
                    } else {
                        "Favorit löschen"
                    };
                    let delete = theme::Button::new(label, Kind::Danger).min_size(half).show(ui);
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
                    let discard = theme::Button::new("Änderungen verwerfen", Kind::Ghost)
                        .enabled(modified)
                        .min_size(half)
                        .show(ui);
                    if discard.clicked() {
                        self.select(index);
                    }
                });
            }
            fields = Some((name, host, pw));
        });
        if let Some((name, host, pw)) = fields {
            if self.form.host.is_empty() && !name.has_focus() && !host.has_focus() && !pw.has_focus() {
                host.request_focus();
            }
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
        let p = Palette::of(ui);
        let changed = self.form.settings != Overrides::default();
        egui::Frame::new()
            .fill(p.canvas)
            .stroke(Stroke::new(1.0, p.card_stroke))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                let inner = width - 26.0;
                ui.set_width(inner);
                let (rect, header) = ui.allocate_exact_size(Vec2::new(inner, 22.0), Sense::click());
                let header = header.on_hover_cursor(CursorIcon::PointingHand);
                if header.clicked() {
                    self.settings_open = !self.settings_open;
                }
                let chevron = Pos2::new(rect.left() + 7.0, rect.center().y);
                let color = if header.hovered() { p.accent } else { p.weak };
                theme::paint_chevron(ui, chevron, 8.0, self.settings_open, color);
                ui.painter().text(
                    Pos2::new(rect.left() + 22.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    "Einstellungen",
                    theme::semibold(13.5),
                    p.text,
                );
                let (summary, summary_color) = if changed {
                    ("Abweichend", theme::AMBER)
                } else {
                    ("Standard", p.weak)
                };
                let galley =
                    ui.painter()
                        .layout_no_wrap(summary.into(), theme::semibold(11.0), summary_color);
                let size = galley.size() + Vec2::new(14.0, 6.0);
                let tag = Rect::from_min_size(
                    Pos2::new(rect.right() - size.x, rect.center().y - size.y / 2.0),
                    size,
                );
                ui.painter()
                    .rect_filled(tag, CornerRadius::same(99), p.tint(summary_color));
                ui.painter()
                    .galley(tag.center() - galley.size() / 2.0, galley, summary_color);
                if !self.settings_open {
                    return;
                }
                ui.add_space(10.0);
                let defaults = self.config.viewer;
                settings_grid(ui, "overrides", &mut self.form.settings, Some(defaults));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if theme::Button::new("Standardeinstellungen …", Kind::Ghost)
                        .small()
                        .show(ui)
                        .clicked()
                    {
                        self.defaults = Some(all_set(self.config.viewer));
                    }
                    if changed
                        && theme::Button::new("Alles auf Standard", Kind::Ghost)
                            .small()
                            .show(ui)
                            .clicked()
                    {
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
        egui::Window::new(RichText::new("Standardeinstellungen").font(theme::bold(17.0)))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.weak("Gelten für jede Verbindung, bei der nichts anderes eingestellt ist.");
                ui.add_space(10.0);
                settings_grid(ui, "defaults", &mut draft, None);
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    save = theme::button(ui, "Speichern", Kind::Primary).clicked();
                    cancel = theme::button(ui, "Abbrechen", Kind::Ghost).clicked();
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
    // The label column plus spacing; the combos take the rest and truncate
    // long choices instead of widening the card.
    let combo_width = (ui.available_width() - 146.0).clamp(200.0, 320.0);
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            option_combo(
                ui,
                &VIEW_ONLY,
                &mut values.view_only,
                defaults.map(|d| d.view_only),
                combo_width,
            );
            option_combo(
                ui,
                &SCALING,
                &mut values.scaling,
                defaults.map(|d| d.scaling),
                combo_width,
            );
            option_combo(
                ui,
                &COLORS,
                &mut values.colors,
                defaults.map(|d| d.colors),
                combo_width,
            );
            option_combo(
                ui,
                &CLIPBOARD,
                &mut values.clipboard,
                defaults.map(|d| d.clipboard),
                combo_width,
            );
            option_combo(
                ui,
                &SHORTCUTS,
                &mut values.shortcuts,
                defaults.map(|d| d.shortcuts),
                combo_width,
            );
            option_combo(
                ui,
                &FULLSCREEN,
                &mut values.fullscreen,
                defaults.map(|d| d.fullscreen),
                combo_width,
            );
            option_combo(
                ui,
                &FIT_WINDOW,
                &mut values.fit_window,
                defaults.map(|d| d.fit_window),
                combo_width,
            );
        });
}

fn option_combo<T: Copy + PartialEq + 'static>(
    ui: &mut egui::Ui,
    field: &Field<T>,
    value: &mut Option<T>,
    default: Option<T>,
    width: f32,
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
        .width(width)
        .truncate()
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

/// What a click on a favorite's card asked for.
struct CardResponse {
    select: bool,
    connect: bool,
}

/// One favorite: avatar with monogram, name and address, tags for settings
/// that deviate from the defaults, and an arrow button that connects.
fn favorite_card(ui: &mut egui::Ui, server: &Server, selected: bool) -> CardResponse {
    let p = Palette::of(ui);
    let height = 60.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let mut out = CardResponse {
        select: response.clicked(),
        connect: response.double_clicked(),
    };
    // The button is registered after the row, so it gets the clicks there.
    let play_rect =
        Rect::from_center_size(Pos2::new(rect.right() - 28.0, rect.center().y), Vec2::splat(32.0));
    let play = ui
        .interact(play_rect, response.id.with("connect"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Verbinden");
    if play.clicked() {
        out.connect = true;
    }
    if !ui.is_rect_visible(rect) {
        return out;
    }
    let hovered = response.contains_pointer();
    let (fill, stroke) = if selected {
        (p.tint(p.accent), Stroke::new(1.5, p.accent))
    } else if hovered {
        (p.button, Stroke::new(1.0, p.card_stroke))
    } else {
        (p.card, Stroke::new(1.0, p.card_stroke))
    };
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(12), fill, stroke, StrokeKind::Inside);
    let avatar = Rect::from_min_size(
        rect.left_top() + Vec2::new(12.0, (height - 36.0) / 2.0),
        Vec2::splat(36.0),
    );
    theme::paint_avatar(ui, avatar, &server.name);
    if selected || hovered {
        let fill = if play.hovered() {
            p.accent.lerp_to_gamma(Color32::WHITE, 0.15)
        } else {
            p.accent
        };
        painter.circle_filled(play_rect.center(), 15.0, fill);
        theme::paint_play(ui, play_rect.center() + Vec2::new(1.0, 0.0), 11.0, p.on_accent);
    } else {
        theme::paint_chevron(ui, play_rect.center(), 9.0, false, p.weak.gamma_multiply(0.6));
    }
    let text_left = avatar.right() + 12.0;
    let name = painter.layout_no_wrap(server.name.clone(), theme::semibold(14.0), p.text);
    let host = painter.layout_no_wrap(server.host.clone(), FontId::proportional(12.0), p.weak);
    // Tags only take the room the name and address leave over.
    let text_right = text_left + name.size().x.max(host.size().x) + 10.0;
    let mut right = play_rect.left() - 6.0;
    for tag in tags(&server.settings).iter().take(2) {
        let galley = painter.layout_no_wrap((*tag).to_owned(), theme::semibold(10.5), p.weak);
        let size = galley.size() + Vec2::new(12.0, 5.0);
        if right - size.x < text_right {
            break;
        }
        let tag_rect = Rect::from_min_size(Pos2::new(right - size.x, rect.center().y - size.y / 2.0), size);
        painter.rect_filled(tag_rect, CornerRadius::same(99), p.tint(p.weak));
        painter.galley(tag_rect.center() - galley.size() / 2.0, galley, p.weak);
        right = tag_rect.left() - 6.0;
    }
    let clip = Rect::from_min_max(
        Pos2::new(text_left, rect.top()),
        Pos2::new(right - 4.0, rect.bottom()),
    );
    let painter = painter.with_clip_rect(clip);
    painter.galley(
        Pos2::new(text_left, rect.center().y - 1.0 - name.size().y),
        name,
        p.text,
    );
    painter.galley(Pos2::new(text_left, rect.center().y + 2.0), host, p.weak);
    out
}

/// Short labels for the settings a favorite overrides.
fn tags(o: &Overrides) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if o.view_only == Some(true) {
        tags.push("Nur ansehen");
    }
    match o.colors {
        Some(Colors::Low) => tags.push("256 Farben"),
        Some(Colors::Medium) => tags.push("16 Bit"),
        _ => {}
    }
    if o.fullscreen == Some(true) {
        tags.push("Vollbild");
    }
    if tags.is_empty() && !o.is_empty() {
        tags.push("Angepasst");
    }
    tags
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
    fn new(mut conn: Connection, label: String, settings: Settings, ctx: &egui::Context) -> Self {
        let swap_cmd_ctrl = match settings.shortcuts {
            Shortcuts::Auto => cfg!(target_os = "macos") != conn.remote.is_mac(),
            Shortcuts::Swap => true,
            Shortcuts::Plain => false,
        };
        let keys = KeyMap::for_remote(conn.remote);
        let clipboard = settings.clipboard.then(|| Clipboard::start(ctx.clone()));
        let files = crate::files::Panel::new(
            conn.files.take().expect("one file worker per connection"),
            ctx.clone(),
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

    /// Sizes the window to the remote screen, within the monitor.
    fn fit_window(&self, ctx: &egui::Context) {
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
            return Err("vom Benutzer".into());
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

const TOOLBAR_HEIGHT: f32 = 30.0;

/// Toolbar text for the display choice.
fn screen_label(screens: &[Screen], view: Option<usize>) -> String {
    match view.and_then(|i| screens.get(i).map(|s| (i, s))) {
        Some((i, s)) => format!("Bildschirm {} ({}×{})", i + 1, s.width, s.height),
        None => "Alle Bildschirme".to_string(),
    }
}
