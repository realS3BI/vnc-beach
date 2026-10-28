//! The viewer: the overview window with favorites, account devices and this
//! computer; the favorite editor; the settings window; and one window per
//! session (see [`crate::remote`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use egui::load::SizedTexture;
use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, Id, Key, Layout, Margin, Modifiers, Pos2, Rect, RichText,
    Sense, Stroke, Vec2, ViewportBuilder, ViewportCommand, ViewportId,
};

use crate::access::Action;
use crate::access_ui::{self, Connecting};
use crate::account_ui::{self, AccountPanel};
use crate::cards::{self, Card};
use crate::client::Connection;
use crate::config::{self, Config};
use crate::icon;
use crate::remote::{Close, Remote};
use crate::server_panel::ServerPanel;
use crate::servers::{Directory, Server};
use crate::settings::{Colors, Overrides, Scaling, Settings, Shortcuts};
use crate::theme::{self, Kind, Palette};
use crate::update::{self, Release, Version};

/// Widest the overview's content gets; the rest is margin.
const CONTENT_WIDTH: f32 = 1080.0;
/// The favorite editor's width.
const EDITOR_WIDTH: f32 = 500.0;

pub struct App {
    directory: Directory,
    config: Config,
    server: ServerPanel,
    account: AccountPanel,
    update: Update,
    /// A message at the top of the overview, dismissed with a click.
    notice: Option<Notice>,
    /// Address in the quick-connect field.
    quick: String,
    /// Put the cursor into the quick-connect field (once, at start).
    focus_quick: bool,
    /// Search text over the favorites (only shown for longer lists).
    filter: String,
    editor: Option<Editor>,
    /// Favorite waiting for "Wirklich löschen?".
    confirm_delete: Option<usize>,
    /// The one connection being prepared.
    pending: Option<Pending>,
    sessions: Vec<Session>,
    next_session: u64,
    settings: Option<SettingsWindow>,
    #[cfg(target_os = "macos")]
    menu: crate::menu::MenuBar,
    /// The app icon, for the header.
    logo: egui::TextureHandle,
    /// The app icon, for the session windows.
    icon: Arc<egui::IconData>,
}

struct Notice {
    text: String,
    color: Color32,
}

/// The favorite editor (a modal over the overview).
struct Editor {
    form: Server,
    /// Saved favorite the form came from; `None` for a new one.
    index: Option<usize>,
    show_password: bool,
    /// The per-connection settings are unfolded.
    settings_open: bool,
    /// "Favorit löschen" waits for a second click.
    confirm_delete: bool,
    error: Option<String>,
    /// Why the editor opened on its own, e.g. the server wants a password.
    hint: Option<String>,
    /// Field to put the cursor into once.
    focus: Option<Focus>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Host,
    Password,
}

impl Editor {
    fn new(form: Server, index: Option<usize>) -> Editor {
        let settings_open = !form.settings.is_empty();
        Editor {
            focus: Some(if form.host.is_empty() { Focus::Host } else { Focus::Password }),
            form,
            index,
            show_password: false,
            settings_open,
            confirm_delete: false,
            error: None,
            hint: None,
        }
    }
}

/// Where a connection attempt came from, which decides where its error goes.
enum Origin {
    /// A favorite's or device's card.
    Card,
    /// The quick-connect field, with the address it held.
    Quick(Box<Server>),
    /// The editor, which stays open until the connection is up.
    Editor,
}

/// Everything a connection attempt starts with.
struct Launch {
    server: Server,
    account: Option<crate::account::Target>,
    settings: Settings,
    action: Action,
    label: Option<String>,
    origin: Origin,
}

struct Pending {
    task: Box<Connecting>,
    settings: Settings,
    /// Title of the session window; the server's desktop name otherwise.
    label: Option<String>,
    origin: Origin,
    /// The window the session will get.
    viewport: ViewportId,
    /// Set once that window exists: from then on the connection repaints it
    /// instead of the overview.
    attached: Arc<AtomicBool>,
}

/// A session window. It is a deferred viewport, painted on its own
/// schedule: the overview isn't redrawn for every remote frame, and a
/// session doesn't wait for the overview or for other sessions.
struct Session {
    id: ViewportId,
    builder: ViewportBuilder,
    state: Arc<Mutex<SessionState>>,
    label: String,
}

struct SessionState {
    remote: Remote,
    /// The first frame still has to apply the fullscreen setting.
    fresh: bool,
    /// Set by the window when it should go; the overview then drops it.
    closed: Option<Close>,
}

struct SettingsWindow {
    tab: Tab,
    /// Draft of the connection defaults (every field set).
    defaults: Overrides,
    /// The defaults were saved and nothing changed since.
    saved: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Connections,
    ThisComputer,
    Account,
    Updates,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Connections, Tab::ThisComputer, Tab::Account, Tab::Updates];

    fn title(self) -> &'static str {
        match self {
            Tab::Connections => "Verbindungen",
            Tab::ThisComputer => "Dieser Computer",
            Tab::Account => "Konto",
            Tab::Updates => "Updates",
        }
    }
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
        let config = config::load();
        App {
            account: AccountPanel::new(&config),
            update: Update::check(&cc.egui_ctx, false),
            server: ServerPanel::new(cc.egui_ctx.clone()),
            directory,
            config,
            notice: None,
            quick: String::new(),
            focus_quick: true,
            filter: String::new(),
            editor: None,
            confirm_delete: None,
            pending: None,
            sessions: Vec::new(),
            next_session: 0,
            settings: None,
            #[cfg(target_os = "macos")]
            menu: crate::menu::MenuBar::install(&cc.egui_ctx),
            logo,
            icon: Arc::new(icon),
        }
    }

    fn notice(&mut self, text: impl Into<String>, color: Color32) {
        self.notice = Some(Notice {
            text: text.into(),
            color,
        });
    }

    fn save_config(&mut self) {
        if let Err(e) = config::save_keeping_device(&mut self.config) {
            self.notice(format!("Speichern fehlgeschlagen: {e}"), theme::CORAL);
        }
    }

    fn save_directory(&mut self) -> bool {
        match self.directory.save() {
            Ok(()) => true,
            Err(e) => {
                self.notice(format!("Speichern fehlgeschlagen: {e}"), theme::CORAL);
                false
            }
        }
    }

    // ----- connecting -----

    fn start(&mut self, ctx: &egui::Context, launch: Launch) {
        if self.pending.is_some() {
            return;
        }
        let Launch {
            server,
            account,
            settings,
            action,
            label,
            origin,
        } = launch;
        let pf = settings.colors.pixel_format();
        let viewport = ViewportId::from_hash_of(("session", self.next_session));
        self.next_session += 1;
        let attached = Arc::new(AtomicBool::new(false));
        let ctx = ctx.clone();
        let window_ready = attached.clone();
        let repaint = move || {
            if window_ready.load(Ordering::Acquire) {
                ctx.request_repaint_of(viewport);
            } else {
                ctx.request_repaint_of(ViewportId::ROOT);
            }
        };
        let task = crate::access::start(server, account, action, pf, repaint);
        self.pending = Some(Pending {
            task: Box::new(Connecting::new(task)),
            settings,
            label,
            origin,
            viewport,
            attached,
        });
    }

    fn connect_favorite(&mut self, ctx: &egui::Context, index: usize) {
        let Some(server) = self.directory.servers.get(index).cloned() else {
            return;
        };
        let settings = server.settings.apply(self.config.viewer);
        let label = Some(server.name.clone()).filter(|n| !n.trim().is_empty());
        self.start(
            ctx,
            Launch {
                server,
                account: None,
                settings,
                action: Action::Connect,
                label,
                origin: Origin::Card,
            },
        );
    }

    fn connect_quick(&mut self, ctx: &egui::Context) {
        let host = self.quick.trim().to_string();
        if host.is_empty() {
            return;
        }
        // A favorite with that address brings its password and settings.
        if let Some(index) = self
            .directory
            .servers
            .iter()
            .position(|s| s.host.eq_ignore_ascii_case(&host))
        {
            self.connect_favorite(ctx, index);
            return;
        }
        let server = Server {
            host,
            ..Server::default()
        };
        self.start(
            ctx,
            Launch {
                server: server.clone(),
                account: None,
                settings: self.config.viewer,
                action: Action::Connect,
                label: None,
                origin: Origin::Quick(Box::new(server)),
            },
        );
    }

    fn connect_editor(&mut self, ctx: &egui::Context, action: Action) {
        let Some(editor) = &self.editor else {
            return;
        };
        let server = editor.form.clone();
        let settings = server.settings.apply(self.config.viewer);
        let label = Some(server.name.trim().to_string()).filter(|n| !n.is_empty());
        self.start(
            ctx,
            Launch {
                server,
                account: None,
                settings,
                action,
                label,
                origin: Origin::Editor,
            },
        );
    }

    /// Connects to an account device with the default settings.
    fn connect_device(&mut self, ctx: &egui::Context, device: &beach_account::api::Device) {
        let target = crate::account::Target {
            url: self.config.account.url.clone(),
            token: self.config.account.token.clone(),
            device: device.id.clone(),
        };
        let server = account_ui::server_for(device);
        if server.host.is_empty() {
            self.notice(
                format!("{} hat noch keine Adresse gemeldet. Läuft VNC Beach dort?", device.name),
                theme::AMBER,
            );
            return;
        }
        self.start(
            ctx,
            Launch {
                server,
                account: Some(target),
                settings: self.config.viewer,
                action: Action::Connect,
                label: Some(device.name.clone()),
                origin: Origin::Card,
            },
        );
    }

    /// Picks up the result of the connection being prepared.
    fn poll_pending(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        let Some(result) = pending.task.poll() else {
            return;
        };
        let pending = self.pending.take().expect("checked above");
        match result {
            Ok(conn) => {
                let label = pending.label.unwrap_or_else(|| conn.name.clone());
                self.open_session(ctx, conn, label, pending.settings, pending.viewport);
                pending.attached.store(true, Ordering::Release);
                if matches!(pending.origin, Origin::Editor) {
                    self.editor = None;
                }
            }
            Err(error) => match pending.origin {
                Origin::Editor => {
                    if let Some(editor) = &mut self.editor {
                        editor.error = Some(error);
                    } else {
                        self.notice(error, theme::CORAL);
                    }
                }
                Origin::Quick(server) if error.contains("verlangt ein Passwort") => {
                    let mut editor = Editor::new(*server, None);
                    editor.hint = Some("Der Server verlangt ein Passwort. Trage es ein und verbinde erneut.".into());
                    editor.focus = Some(Focus::Password);
                    self.editor = Some(editor);
                    self.quick.clear();
                }
                Origin::Quick(_) | Origin::Card => self.notice(error, theme::CORAL),
            },
        }
    }

    /// Opens a session window for a connection that is up.
    fn open_session(
        &mut self,
        ctx: &egui::Context,
        conn: Connection,
        label: String,
        settings: Settings,
        id: ViewportId,
    ) {
        let remote = Remote::new(conn, label.clone(), settings, ctx, id);
        let size = remote.initial_size(ctx.input(|i| i.viewport().monitor_size));
        let builder = ViewportBuilder::default()
            .with_title(format!("{label} – VNC Beach"))
            .with_inner_size(size)
            .with_min_inner_size([320.0, 200.0])
            .with_icon(self.icon.clone())
            .with_drag_and_drop(true);
        self.sessions.push(Session {
            id,
            builder,
            state: Arc::new(Mutex::new(SessionState {
                remote,
                fresh: true,
                closed: None,
            })),
            label,
        });
    }

    /// Keeps every session window alive; windows that were closed or lost
    /// their connection go away.
    fn show_sessions(&mut self, ctx: &egui::Context) {
        let mut closed = Vec::new();
        for (index, session) in self.sessions.iter().enumerate() {
            if let Some(close) = session.state.lock().unwrap().closed.take() {
                closed.push((index, close));
                continue;
            }
            let state = session.state.clone();
            ctx.show_viewport_deferred(session.id, session.builder.clone(), move |ui, _class| {
                let ctx = ui.ctx().clone();
                let mut state = state.lock().unwrap();
                if state.closed.is_some() {
                    return;
                }
                let result = if ctx.input(|i| i.viewport().close_requested()) {
                    Err(Close::User)
                } else {
                    if std::mem::take(&mut state.fresh) && state.remote.wants_fullscreen() {
                        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                    }
                    state.remote.show(ui)
                };
                if let Err(close) = result {
                    state.closed = Some(close);
                    // The overview owns the window list.
                    ctx.request_repaint_of(ViewportId::ROOT);
                }
            });
        }
        for (index, close) in closed.into_iter().rev() {
            let session = self.sessions.remove(index);
            if let Close::Lost(reason) = close {
                self.notice(
                    format!("Verbindung zu „{}“ getrennt: {reason}", session.label),
                    theme::CORAL,
                );
            }
        }
    }

    /// A session window is in front: keys belong to the remote machine.
    fn session_focused(&self, ctx: &egui::Context) -> bool {
        ctx.input(|i| {
            self.sessions
                .iter()
                .any(|s| i.raw.viewports.get(&s.id).is_some_and(|v| v.focused == Some(true)))
        })
    }

    // ----- editor -----

    fn open_editor(&mut self, index: Option<usize>) {
        let form = index
            .and_then(|i| self.directory.servers.get(i).cloned())
            .unwrap_or_default();
        let index = index.filter(|&i| i < self.directory.servers.len());
        self.editor = Some(Editor::new(form, index));
        self.confirm_delete = None;
    }

    /// Saves the editor's form as a favorite (new or replaced).
    fn save_editor(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let mut server = editor.form.clone();
        server.name = server.name.trim().to_string();
        server.host = server.host.trim().to_string();
        server
            .access
            .alternatives
            .retain(|address| !address.trim().is_empty());
        if let Err(error) = server.access.validate() {
            editor.error = Some(format!("Erreichbarkeit & Start: {error:#}"));
            return;
        }
        if server.name.is_empty() {
            server.name = server.host.clone();
        }
        let index = self.directory.upsert(editor.index, server.clone());
        editor.form = server;
        editor.index = Some(index);
        editor.error = None;
        editor.hint = None;
        if !self.save_directory() {
            if let Some(editor) = &mut self.editor {
                editor.error = Some("Der Favorit konnte nicht gespeichert werden.".into());
            }
        }
    }

    fn delete_favorite(&mut self, index: usize) {
        let Some(server) = self.directory.servers.get(index) else {
            return;
        };
        let name = server.name.clone();
        self.directory.remove(index);
        self.confirm_delete = None;
        if let Some(editor) = &mut self.editor {
            match editor.index {
                Some(i) if i == index => self.editor = None,
                Some(i) if i > index => editor.index = Some(i - 1),
                _ => {}
            }
        }
        if self.save_directory() {
            self.notice(format!("„{name}“ wurde gelöscht."), theme::GREEN);
        }
    }

    // ----- settings window -----

    fn open_settings(&mut self, ctx: &egui::Context, tab: Tab) {
        match &mut self.settings {
            Some(window) => {
                window.tab = tab;
                ctx.send_viewport_cmd_to(settings_viewport(), ViewportCommand::Focus);
            }
            None => {
                self.server.begin_settings(&self.config);
                self.settings = Some(SettingsWindow {
                    tab,
                    defaults: all_set(self.config.viewer),
                    saved: false,
                });
            }
        }
    }

    fn close_settings(&mut self) {
        self.settings = None;
        self.server.end_settings();
    }
}

fn settings_viewport() -> ViewportId {
    ViewportId::from_hash_of("settings")
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        self.poll_update(ctx);
        self.poll_pending(ctx);
        self.account.poll(ctx, &mut self.config);
        self.show_sessions(ctx);

        #[cfg(target_os = "macos")]
        {
            self.menu.set_shortcuts(!self.session_focused(ctx));
            for action in self.menu.poll() {
                match action {
                    crate::menu::Action::NewConnection => self.open_editor(None),
                    crate::menu::Action::Settings => self.open_settings(ctx, Tab::Connections),
                    crate::menu::Action::CheckUpdates => {
                        self.open_settings(ctx, Tab::Updates);
                        if !matches!(self.update, Update::Checking { .. } | Update::Installing(_)) {
                            self.update = Update::check(ctx, true);
                        }
                    }
                }
            }
        }
        // The same shortcuts through the keyboard; only the overview window's
        // own keys arrive here.
        if !self.session_focused(ctx) {
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
                self.open_editor(None);
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) {
                self.open_settings(ctx, Tab::Connections);
            }
        }

        self.overview(ui);
        self.show_editor(ctx);
        self.show_connecting(ctx);
        self.show_settings(ctx);
    }
}


// ----- the overview -----

impl App {
    fn overview(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let p = Palette::of(root);
        let connecting = self.pending.is_some();
        let mut quick_connect = false;
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(Margin::symmetric(24, 12)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::from_texture(SizedTexture::new(self.logo.id(), Vec2::splat(30.0)))
                            .corner_radius(8.0),
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new("VNC Beach").font(theme::bold(18.0)).color(p.text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::Button::new("Einstellungen", Kind::Ghost)
                            .show(ui)
                            .on_hover_text("Standardeinstellungen, dieser Computer, Konto und Updates")
                            .clicked()
                        {
                            self.open_settings(&ctx, Tab::Connections);
                        }
                        ui.add_space(6.0);
                        let has_address = !self.quick.trim().is_empty();
                        let button = theme::Button::new("Verbinden", Kind::Primary)
                            .enabled(has_address && !connecting)
                            .show(ui);
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut self.quick)
                                .hint_text("Adresse, z. B. mac.local oder 192.168.1.20:5901")
                                .margin(Margin::symmetric(10, 7))
                                .desired_width(ui.available_width().clamp(160.0, 320.0)),
                        );
                        if std::mem::take(&mut self.focus_quick) {
                            field.request_focus();
                        }
                        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        quick_connect = button.clicked() || (enter && has_address && !connecting);
                    });
                });
            });
        if quick_connect {
            self.connect_quick(&ctx);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.canvas))
            .show(root, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        let width = (ui.available_width() - 56.0).clamp(200.0, CONTENT_WIDTH);
                        ui.allocate_ui_with_layout(
                            Vec2::new(width, ui.available_height()),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(width);
                                ui.add_space(20.0);
                                self.banners(ui, &ctx);
                                ui.add_enabled_ui(!connecting, |ui| {
                                    self.favorites(ui, &ctx);
                                    ui.add_space(28.0);
                                    self.account_section(ui, &ctx);
                                    ui.add_space(28.0);
                                    self.this_computer(ui, &ctx);
                                });
                                ui.add_space(24.0);
                                self.footer(ui, &ctx);
                                ui.add_space(24.0);
                            },
                        );
                    });
                });
            });
    }

    /// Update, notice and delete-confirmation banners at the top.
    fn banners(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        if let Update::Available(release) = &self.update {
            let release = release.clone();
            let blocked = release.asset.as_ref().and_then(|_| update::can_install().err());
            theme::banner(ui, p.accent).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("VNC Beach {} ist verfügbar.", release.version))
                            .font(theme::semibold(14.0))
                            .color(p.text),
                    );
                    if let Some(why) = &blocked {
                        ui.label(RichText::new(why).size(12.5).color(p.weak));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if release.asset.is_some() && blocked.is_none() {
                            if theme::Button::new("Jetzt aktualisieren", Kind::Primary)
                                .small()
                                .show(ui)
                                .clicked()
                            {
                                self.update = Update::install(ctx, release.clone());
                            }
                        } else if theme::Button::new("Download-Seite öffnen", Kind::Primary)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            ctx.open_url(egui::OpenUrl::new_tab(&release.page));
                        }
                    });
                });
            });
            ui.add_space(12.0);
        }
        if let Some(notice) = &self.notice {
            let (text, color) = (notice.text.clone(), notice.color);
            let mut dismiss = false;
            theme::banner(ui, color).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(text).color(p.text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        dismiss = theme::Button::new("OK", Kind::Ghost).small().show(ui).clicked();
                    });
                });
            });
            if dismiss {
                self.notice = None;
            }
            ui.add_space(12.0);
        }
        if let Some(index) = self.confirm_delete {
            let Some(name) = self.directory.servers.get(index).map(|s| s.name.clone()) else {
                self.confirm_delete = None;
                return;
            };
            let mut delete = false;
            let mut keep = false;
            theme::banner(ui, theme::CORAL).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("„{name}“ aus den Favoriten löschen?"))
                            .font(theme::semibold(14.0))
                            .color(p.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        delete = theme::Button::new("Löschen", Kind::Danger).small().show(ui).clicked();
                        keep = theme::Button::new("Behalten", Kind::Ghost).small().show(ui).clicked();
                    });
                });
            });
            if delete {
                self.delete_favorite(index);
            } else if keep {
                self.confirm_delete = None;
            }
            ui.add_space(12.0);
        }
    }

    fn favorites(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        let count = self.directory.servers.len();
        ui.horizontal(|ui| {
            let heading = theme::section_header(ui, "Favoriten", Some(count));
            if let Some(path) = self.directory.path() {
                heading.on_hover_text(format!("Gespeichert in {}", path.display()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::Button::new("+ Neu", Kind::Secondary)
                    .small()
                    .show(ui)
                    .on_hover_text("Name, Adresse und Passwort eines Computers speichern")
                    .clicked()
                {
                    self.open_editor(None);
                }
                if count >= 8 {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("Favoriten durchsuchen")
                            .margin(Margin::symmetric(10, 5))
                            .desired_width(200.0),
                    );
                } else {
                    self.filter.clear();
                }
            });
        });
        ui.add_space(10.0);
        if count == 0 {
            theme::card(ui).inner_margin(Margin::same(20)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Noch keine Favoriten").font(theme::semibold(15.0)).color(p.text));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Oben eine Adresse eingeben und verbinden, oder Name, Adresse und Passwort eines \
                         Computers als Favorit speichern. Favoriten verbinden sich mit einem Klick.",
                    )
                    .color(p.weak),
                );
                ui.add_space(10.0);
                if theme::Button::new("+ Ersten Favoriten anlegen", Kind::Primary)
                    .show(ui)
                    .clicked()
                {
                    self.open_editor(None);
                }
            });
            return;
        }
        let filter = self.filter.trim().to_lowercase();
        let shown: Vec<usize> = (0..count)
            .filter(|&i| {
                let s = &self.directory.servers[i];
                filter.is_empty() || s.name.to_lowercase().contains(&filter) || s.host.to_lowercase().contains(&filter)
            })
            .collect();
        if shown.is_empty() {
            ui.label(RichText::new("Kein Favorit passt zur Suche.").color(p.weak));
            return;
        }
        let mut connect = None;
        let mut edit = None;
        let mut delete = None;
        let mut add = false;
        // The "+ Neu" cell closes the grid when nothing is filtered out.
        let cells = shown.len() + usize::from(filter.is_empty());
        cards::grid(ui, cells, |ui, cell, size| {
            let Some(&index) = shown.get(cell) else {
                add |= cards::add_card(ui, size).clicked();
                return;
            };
            let server = &self.directory.servers[index];
            let tags = tags(&server.settings);
            let response = cards::card(
                ui,
                size,
                Card {
                    name: &server.name,
                    line: &server.host,
                    tags: &tags,
                    online: None,
                    editable: true,
                },
            );
            if response.connect {
                connect = Some(index);
            }
            if response.edit {
                edit = Some(index);
            }
            response.response.context_menu(|ui| {
                ui.set_min_width(160.0);
                if ui.button("Verbinden").clicked() {
                    connect = Some(index);
                    ui.close();
                }
                if ui.button("Bearbeiten …").clicked() {
                    edit = Some(index);
                    ui.close();
                }
                ui.separator();
                if ui.button(RichText::new("Löschen …").color(theme::CORAL)).clicked() {
                    delete = Some(index);
                    ui.close();
                }
            });
        });
        if let Some(index) = connect {
            self.connect_favorite(ctx, index);
        } else if let Some(index) = edit {
            self.open_editor(Some(index));
        } else if add {
            self.open_editor(None);
        }
        if let Some(index) = delete {
            self.confirm_delete = Some(index);
        }
    }

    fn account_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        let signed_in = self.config.account.signed_in();
        let devices = self.account.devices().to_vec();
        ui.horizontal(|ui| {
            let heading = theme::section_header(ui, "Konto", Some(devices.len()));
            if signed_in {
                heading.on_hover_text(format!(
                    "Angemeldet als {} ({}) bei {}",
                    self.config.account.name, self.config.account.login, self.config.account.url
                ));
                ui.label(RichText::new(&self.config.account.name).color(p.weak));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if signed_in { "Konto verwalten" } else { "Anmelden …" };
                if theme::Button::new(label, Kind::Ghost).small().show(ui).clicked() {
                    self.open_settings(ctx, Tab::Account);
                }
            });
        });
        ui.add_space(10.0);
        if !signed_in {
            theme::card(ui).inner_margin(Margin::same(20)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Nicht angemeldet").font(theme::semibold(15.0)).color(p.text));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Mit einem Konto erscheinen hier alle deine Geräte und die deines Teams, und du \
                         verbindest dich ohne VNC-Passwort.",
                    )
                    .color(p.weak),
                );
                if let Some(e) = self.account.error() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(e).size(12.5).color(theme::CORAL));
                }
            });
            return;
        }
        if let Some(e) = self.account.error() {
            ui.label(RichText::new(e).size(12.5).color(theme::CORAL));
            ui.add_space(6.0);
        }
        if self.account.loading() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Geräte werden geladen …").color(p.weak));
            });
            return;
        }
        if devices.is_empty() {
            theme::card(ui).inner_margin(Margin::same(20)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Noch keine Geräte im Konto").font(theme::semibold(15.0)).color(p.text));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Füge diesen Computer hinzu oder installiere VNC Beach auf einem anderen und melde \
                         dich dort an.",
                    )
                    .color(p.weak),
                );
                if self.account.loaded() {
                    ui.add_space(10.0);
                    if theme::Button::new("Diesen Computer hinzufügen …", Kind::Secondary)
                        .show(ui)
                        .clicked()
                    {
                        self.open_settings(ctx, Tab::Account);
                    }
                }
            });
            return;
        }
        // Grouped by owner: personal devices first, then each organization.
        let mut groups: Vec<(Option<String>, Vec<&beach_account::api::Device>)> = Vec::new();
        for device in &devices {
            let owner = device.org.as_ref().map(|o| o.name.clone());
            match groups.iter_mut().find(|(o, _)| *o == owner) {
                Some((_, list)) => list.push(device),
                None => groups.push((owner, vec![device])),
            }
        }
        let mut connect = None;
        for (owner, list) in &groups {
            if groups.len() > 1 || owner.is_some() {
                ui.label(
                    RichText::new(owner.as_deref().unwrap_or("Persönlich"))
                        .font(theme::semibold(12.5))
                        .color(p.weak),
                );
                ui.add_space(4.0);
            }
            cards::grid(ui, list.len(), |ui, index, size| {
                let device = list[index];
                let line = account_ui::status(device);
                let response = cards::card(
                    ui,
                    size,
                    Card {
                        name: &device.name,
                        line: &line,
                        tags: &[],
                        online: Some(device.online),
                        editable: false,
                    },
                );
                if response.connect {
                    connect = Some(device.clone());
                }
            });
            ui.add_space(8.0);
        }
        if let Some(device) = connect {
            self.connect_device(ctx, &device);
        }
    }

    fn this_computer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            theme::section_header(ui, "Dieser Computer", None);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::Button::new("Einstellungen …", Kind::Ghost)
                    .small()
                    .show(ui)
                    .on_hover_text("Freigabe, Port, Passwort, Bildschirme")
                    .clicked()
                {
                    self.open_settings(ctx, Tab::ThisComputer);
                }
            });
        });
        ui.add_space(10.0);
        theme::card(ui)
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.server.show(ui, &self.config);
            });
    }

    fn footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        let note = self.update_note(&p);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("VNC Beach {}", Version::current()))
                    .size(12.0)
                    .color(p.weak),
            );
            if let Some((text, color)) = note {
                ui.label(RichText::new("·").size(12.0).color(p.weak));
                ui.label(RichText::new(text).size(12.0).color(color));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::Button::new("Nach Updates suchen", Kind::Ghost)
                    .small()
                    .show(ui)
                    .clicked()
                {
                    self.open_settings(ctx, Tab::Updates);
                    if !matches!(self.update, Update::Checking { .. } | Update::Installing(_)) {
                        self.update = Update::check(ctx, true);
                    }
                }
            });
        });
    }

    /// The update state in one line, if there is something to say.
    fn update_note(&self, p: &Palette) -> Option<(String, Color32)> {
        match &self.update {
            Update::Available(r) => Some((format!("Version {} ist verfügbar.", r.version), p.accent)),
            Update::Installing(_) => Some(("Update wird geladen und installiert …".to_string(), p.weak)),
            Update::Current => Some(("Auf dem neuesten Stand.".to_string(), p.weak)),
            Update::Failed(e) => Some((e.clone(), theme::CORAL)),
            Update::Checking { manual: true, .. } => Some(("Suche nach Updates …".to_string(), p.weak)),
            Update::Idle | Update::Checking { manual: false, .. } => None,
        }
    }
}

// ----- the editor -----

/// What the editor's buttons asked for, applied after the modal closes
/// its borrow.
enum EditorAction {
    Connect(Action),
    Save,
    Delete(usize),
    Discard,
    Close,
    OpenDefaults,
}

impl App {
    fn show_editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let connecting = self.pending.is_some();
        let saved = editor.index.and_then(|i| self.directory.servers.get(i)).cloned();
        let modified = match &saved {
            Some(s) => *s != editor.form,
            None => editor.form != Server::default(),
        };
        let defaults = self.config.viewer;
        let mut action = None;
        let width = EDITOR_WIDTH.min(ctx.content_rect().width() - 48.0);
        let modal = egui::Modal::new(Id::new("favorite-editor"))
            .frame(theme::card_in(&Palette::for_context(ctx)).inner_margin(Margin::same(22)))
            .show(ctx, |ui| {
                ui.set_width(width);
                let p = Palette::of(ui);
                let (title, subtitle) = if saved.is_some() {
                    ("Favorit bearbeiten", "Änderungen gelten, sobald du speicherst.")
                } else {
                    ("Neue Verbindung", "Adresse des VNC-Servers, optional mit Name und Passwort.")
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).font(theme::bold(20.0)).color(p.text));
                    if modified {
                        ui.add_space(4.0);
                        theme::pill(ui, "Nicht gespeichert", theme::AMBER);
                    }
                });
                ui.label(RichText::new(subtitle).color(p.weak));
                ui.add_space(12.0);
                let max_height = (ctx.content_rect().height() - 230.0).max(160.0);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .show(ui, |ui| {
                        ui.add_enabled_ui(!connecting, |ui| {
                            if let Some(a) = editor_fields(ui, editor, defaults, width) {
                                action = Some(a);
                            }
                        });
                    });
                ui.add_space(14.0);
                if let Some(a) = editor_buttons(ui, editor, saved.is_some(), modified, connecting, width) {
                    action = Some(a);
                }
            });
        if action.is_none() && modal.should_close() && !modified {
            action = Some(EditorAction::Close);
        }
        match action {
            Some(EditorAction::Connect(a)) => self.connect_editor(ctx, a),
            Some(EditorAction::Save) => self.save_editor(),
            Some(EditorAction::Delete(index)) => self.delete_favorite(index),
            Some(EditorAction::Discard) => {
                if let Some(index) = saved.and(editor.index) {
                    self.open_editor(Some(index));
                }
            }
            Some(EditorAction::Close) => self.editor = None,
            Some(EditorAction::OpenDefaults) => self.open_settings(ctx, Tab::Connections),
            None => {}
        }
    }

    /// The progress or prompt while a connection is prepared.
    fn show_connecting(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        let modal = egui::Modal::new(Id::new("connection-preparation")).show(ctx, |ui| {
            ui.set_width(420.0_f32.min(ctx.content_rect().width() - 48.0));
            pending.task.show(ui)
        });
        if modal.inner || modal.should_close() {
            self.pending = None;
        }
    }
}

/// Name, address, password, settings and access of the editor. Returns an
/// action when Enter was pressed in a field or a link was clicked.
fn editor_fields(ui: &mut egui::Ui, editor: &mut Editor, defaults: Settings, width: f32) -> Option<EditorAction> {
    let p = Palette::of(ui);
    let inner = width - 8.0;
    ui.set_width(inner);
    let field_margin = Margin::symmetric(10, 7);
    let mut action = None;
    if let Some(hint) = &editor.hint {
        theme::banner(ui, theme::AMBER).show(ui, |ui| {
            ui.set_width(inner - 28.0);
            ui.label(RichText::new(hint).color(p.text));
        });
        ui.add_space(10.0);
    }
    theme::field_label(ui, "Name (optional)");
    let name = ui.add(
        egui::TextEdit::singleline(&mut editor.form.name)
            .hint_text("z. B. Mac Büro")
            .margin(field_margin)
            .desired_width(inner),
    );
    ui.add_space(8.0);
    theme::field_label(ui, "Adresse");
    let host = ui.add(
        egui::TextEdit::singleline(&mut editor.form.host)
            .hint_text("192.168.1.20 oder mac.local, bei Bedarf mit :Port")
            .margin(field_margin)
            .desired_width(inner),
    );
    ui.add_space(8.0);
    theme::field_label(ui, "Passwort");
    let pw = ui
        .horizontal(|ui| {
            let pw = ui.add(
                egui::TextEdit::singleline(&mut editor.form.password)
                    .password(!editor.show_password)
                    .hint_text("Passwort des Servers")
                    .margin(field_margin)
                    .desired_width(inner - 96.0),
            );
            let eye = if editor.show_password { "Verbergen" } else { "Anzeigen" };
            if theme::Button::new(eye, Kind::Ghost).small().show(ui).clicked() {
                editor.show_password = !editor.show_password;
            }
            pw
        })
        .inner;
    match editor.focus.take() {
        Some(Focus::Host) => host.request_focus(),
        Some(Focus::Password) => pw.request_focus(),
        None => {}
    }
    ui.add_space(12.0);
    if settings_section(ui, &mut editor.form.settings, &mut editor.settings_open, defaults, inner) {
        action = Some(EditorAction::OpenDefaults);
    }
    access_ui::configuration(ui, &mut editor.form.access);
    let enter = (name.lost_focus() || host.lost_focus() || pw.lost_focus())
        && ui.input(|i| i.key_pressed(Key::Enter));
    if enter && !editor.form.host.trim().is_empty() {
        action = Some(EditorAction::Connect(Action::Connect));
    }
    if let Some(err) = &editor.error {
        ui.add_space(12.0);
        theme::banner(ui, theme::CORAL).show(ui, |ui| {
            ui.set_width(inner - 28.0);
            ui.label(RichText::new(err).color(p.text));
        });
    }
    action
}

/// Verbinden, Speichern and the secondary actions under the fields.
fn editor_buttons(
    ui: &mut egui::Ui,
    editor: &mut Editor,
    saved: bool,
    modified: bool,
    connecting: bool,
    width: f32,
) -> Option<EditorAction> {
    let mut action = None;
    let has_host = !editor.form.host.trim().is_empty();
    let gap = 8.0;
    let half = Vec2::new((width - gap) / 2.0, 36.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let label = if connecting { "Verbinde …" } else { "Verbinden" };
        if theme::Button::new(label, Kind::Primary)
            .enabled(!connecting && has_host)
            .min_size(half)
            .show(ui)
            .clicked()
        {
            action = Some(EditorAction::Connect(Action::Connect));
        }
        let label = if saved { "Speichern" } else { "Als Favorit speichern" };
        if theme::Button::new(label, Kind::Secondary)
            .enabled(has_host && (modified || !saved))
            .min_size(half)
            .show(ui)
            .clicked()
        {
            action = Some(EditorAction::Save);
        }
    });
    if editor.form.access.ssh.is_some() || editor.form.access.wake.is_some() {
        ui.horizontal_wrapped(|ui| {
            if editor.form.access.ssh.is_some()
                && theme::Button::new("Per SSH anmelden", Kind::Secondary)
                    .enabled(has_host && !connecting)
                    .show(ui)
                    .on_hover_text("SSH-Anmeldung starten und anschließend bis zu 90 Sekunden auf VNC warten. Kann FileVault auf unterstützten Macs entsperren.")
                    .clicked()
            {
                action = Some(EditorAction::Connect(Action::SignIn));
            }
            if editor.form.access.wake.is_some()
                && theme::Button::new("Aufwecken", Kind::Secondary)
                    .enabled(has_host && !connecting)
                    .show(ui)
                    .on_hover_text("Ein Wake-on-LAN-Signal senden und anschließend bis zu 90 Sekunden auf VNC warten.")
                    .clicked()
            {
                action = Some(EditorAction::Connect(Action::Wake));
            }
        });
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        if let Some(index) = editor.index {
            let label = if editor.confirm_delete { "Wirklich löschen?" } else { "Favorit löschen" };
            let delete = theme::Button::new(label, Kind::Danger).small().show(ui);
            if delete.clicked() {
                if editor.confirm_delete {
                    editor.confirm_delete = false;
                    action = Some(EditorAction::Delete(index));
                } else {
                    editor.confirm_delete = true;
                }
            } else if editor.confirm_delete && ui.input(|i| i.pointer.any_click()) {
                editor.confirm_delete = false;
            }
            if theme::Button::new("Änderungen verwerfen", Kind::Ghost)
                .small()
                .enabled(modified)
                .show(ui)
                .clicked()
            {
                action = Some(EditorAction::Discard);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if modified { "Abbrechen" } else { "Schließen" };
            if theme::Button::new(label, Kind::Ghost).small().show(ui).clicked() {
                action = Some(EditorAction::Close);
            }
        });
    });
    action
}

/// Collapsible per-connection settings; each one can follow the defaults.
/// Returns true for "Standardeinstellungen …".
fn settings_section(
    ui: &mut egui::Ui,
    overrides: &mut Overrides,
    open: &mut bool,
    defaults: Settings,
    width: f32,
) -> bool {
    let p = Palette::of(ui);
    let changed = !overrides.is_empty();
    let mut open_defaults = false;
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
                *open = !*open;
            }
            let chevron = Pos2::new(rect.left() + 7.0, rect.center().y);
            let color = if header.hovered() { p.accent } else { p.weak };
            theme::paint_chevron(ui, chevron, 8.0, *open, color);
            ui.painter().text(
                Pos2::new(rect.left() + 22.0, rect.center().y),
                Align2::LEFT_CENTER,
                "Einstellungen für diese Verbindung",
                theme::semibold(13.5),
                p.text,
            );
            let (summary, summary_color) = if changed {
                ("Abweichend", theme::AMBER)
            } else {
                ("Standard", p.weak)
            };
            let galley = ui
                .painter()
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
            if !*open {
                return;
            }
            ui.add_space(10.0);
            settings_grid(ui, "overrides", overrides, Some(defaults));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::Button::new("Standardeinstellungen …", Kind::Ghost)
                    .small()
                    .show(ui)
                    .clicked()
                {
                    open_defaults = true;
                }
                if changed
                    && theme::Button::new("Alles auf Standard", Kind::Ghost)
                        .small()
                        .show(ui)
                        .clicked()
                {
                    *overrides = Overrides::default();
                }
            });
        });
    open_defaults
}

// ----- the settings window -----

impl App {
    fn show_settings(&mut self, ctx: &egui::Context) {
        if self.settings.is_none() {
            return;
        }
        let builder = ViewportBuilder::default()
            .with_title("Einstellungen – VNC Beach")
            .with_inner_size([620.0, 600.0])
            .with_min_inner_size([480.0, 360.0])
            .with_icon(self.icon.clone());
        let close = ctx.show_viewport_immediate(settings_viewport(), builder, |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                return true;
            }
            self.settings_window(ui);
            false
        });
        if close {
            self.close_settings();
        }
    }

    fn settings_window(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let p = Palette::of(root);
        let Some(window) = &mut self.settings else {
            return;
        };
        let mut tab = window.tab;
        egui::Panel::top("settings-tabs")
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(Margin::symmetric(16, 10)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for candidate in Tab::ALL {
                        if theme::tab(ui, candidate.title(), candidate == tab).clicked() {
                            tab = candidate;
                        }
                    }
                });
            });
        window.tab = tab;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.canvas)
                    .inner_margin(Margin::symmetric(24, 20)),
            )
            .show(root, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    match tab {
                        Tab::Connections => self.defaults_tab(ui),
                        Tab::ThisComputer => {
                            ui.label(
                                RichText::new("Freigabe dieses Computers")
                                    .font(theme::bold(17.0))
                                    .color(p.text),
                            );
                            ui.add_space(10.0);
                            self.server.settings(ui, &mut self.config);
                        }
                        Tab::Account => {
                            ui.label(RichText::new("Konto").font(theme::bold(17.0)).color(p.text));
                            ui.add_space(10.0);
                            self.account.settings(ui, &mut self.config);
                        }
                        Tab::Updates => self.updates_tab(ui, &ctx),
                    }
                    ui.add_space(12.0);
                });
            });
    }

    /// The defaults every favorite starts from.
    fn defaults_tab(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        let Some(window) = &mut self.settings else {
            return;
        };
        ui.label(RichText::new("Standardeinstellungen").font(theme::bold(17.0)).color(p.text));
        ui.label(
            RichText::new("Gelten für jede Verbindung, bei der im Favoriten nichts anderes eingestellt ist.")
                .color(p.weak),
        );
        ui.add_space(12.0);
        settings_grid(ui, "defaults", &mut window.defaults, None);
        ui.add_space(14.0);
        let current = all_set(self.config.viewer);
        let modified = window.defaults != current;
        let mut save = false;
        let mut discard = false;
        ui.horizontal(|ui| {
            save = theme::Button::new("Speichern", Kind::Primary)
                .enabled(modified)
                .show(ui)
                .clicked();
            discard = theme::Button::new("Verwerfen", Kind::Ghost)
                .enabled(modified)
                .show(ui)
                .clicked();
            if window.saved && !modified {
                ui.label(RichText::new("Gespeichert").color(theme::GREEN));
            }
        });
        if save {
            let draft = window.defaults;
            self.config.viewer = draft.apply(self.config.viewer);
            self.save_config();
            if let Some(window) = &mut self.settings {
                window.saved = true;
            }
        } else if discard {
            window.defaults = current;
            window.saved = false;
        }
    }

    fn updates_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        ui.label(RichText::new("Updates").font(theme::bold(17.0)).color(p.text));
        ui.label(
            RichText::new(format!("Installiert ist VNC Beach {}.", Version::current())).color(p.weak),
        );
        ui.add_space(12.0);
        if let Some((text, color)) = self.update_note(&p) {
            ui.label(RichText::new(text).color(color));
        }
        // Why "Jetzt aktualisieren" isn't offered, e.g. a translocated app.
        let blocked = match &self.update {
            Update::Available(r) if r.asset.is_some() => update::can_install().err(),
            _ => None,
        };
        if let Some(why) = &blocked {
            ui.label(RichText::new(why).size(12.5).color(p.weak));
        }
        ui.add_space(8.0);
        let mut next = None;
        ui.horizontal(|ui| match &self.update {
            Update::Idle | Update::Current => {
                if theme::Button::new("Nach Updates suchen", Kind::Secondary).show(ui).clicked() {
                    next = Some(Update::check(ctx, true));
                }
            }
            Update::Failed(_) => {
                if theme::Button::new("Erneut versuchen", Kind::Secondary).show(ui).clicked() {
                    next = Some(Update::check(ctx, true));
                }
            }
            Update::Available(release) => {
                if release.asset.is_some() && blocked.is_none() {
                    if theme::Button::new("Jetzt aktualisieren", Kind::Primary).show(ui).clicked() {
                        next = Some(Update::install(ctx, release.clone()));
                    }
                } else if theme::Button::new("Download-Seite öffnen", Kind::Primary).show(ui).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(&release.page));
                }
            }
            Update::Checking { .. } | Update::Installing(_) => {
                ui.spinner();
            }
        });
        if let Some(next) = next {
            self.update = next;
        }
        ui.add_space(16.0);
        ui.label(
            RichText::new("VNC Beach prüft bei jedem Start, ob eine neue Version vorliegt, und zeigt sie oben in der Übersicht an.")
                .size(12.5)
                .color(p.weak),
        );
        ui.hyperlink_to("Alle Versionen auf GitHub", "https://github.com/realS3BI/vnc-beach/releases");
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
                Ok(Ok(())) => ctx.send_viewport_cmd(ViewportCommand::Close),
                Ok(Err(e)) => self.update = Update::Failed(format!("Update fehlgeschlagen: {e}")),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.update = Update::Idle,
            },
            _ => {}
        }
    }
}

// ----- the settings grid -----

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
            option_combo(ui, &VIEW_ONLY, &mut values.view_only, defaults.map(|d| d.view_only), combo_width);
            option_combo(ui, &SCALING, &mut values.scaling, defaults.map(|d| d.scaling), combo_width);
            option_combo(ui, &COLORS, &mut values.colors, defaults.map(|d| d.colors), combo_width);
            option_combo(ui, &CLIPBOARD, &mut values.clipboard, defaults.map(|d| d.clipboard), combo_width);
            option_combo(ui, &SHORTCUTS, &mut values.shortcuts, defaults.map(|d| d.shortcuts), combo_width);
            option_combo(ui, &FULLSCREEN, &mut values.fullscreen, defaults.map(|d| d.fullscreen), combo_width);
            option_combo(ui, &FIT_WINDOW, &mut values.fit_window, defaults.map(|d| d.fit_window), combo_width);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_summarize_overrides() {
        assert!(tags(&Overrides::default()).is_empty());
        let o = Overrides {
            view_only: Some(true),
            colors: Some(Colors::Low),
            ..Default::default()
        };
        assert_eq!(tags(&o), vec!["Nur ansehen", "256 Farben"]);
        let o = Overrides {
            clipboard: Some(false),
            ..Default::default()
        };
        assert_eq!(tags(&o), vec!["Angepasst"]);
    }

    #[test]
    fn all_set_round_trips() {
        let s = Settings {
            colors: Colors::Medium,
            fullscreen: true,
            ..Settings::default()
        };
        assert_eq!(all_set(s).apply(Settings::default()), s);
    }
}
