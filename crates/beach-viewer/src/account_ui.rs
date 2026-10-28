//! The account: signing in to a coordination server and adding this
//! computer (the "Konto" tab of the settings window), and the account's
//! devices, which the overview shows as cards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use beach_account::api::{Device, Me};
use egui::{Margin, RichText, Vec2};

use crate::account::{self, AccountConfig, DeviceLink};
use crate::config::{self, Config};
use crate::control::{self, Command};
use crate::servers::Server;
use crate::theme::{self, Kind, Palette};

/// The device list is fetched again after this long.
const REFRESH: Duration = Duration::from_secs(30);

/// The browser sign-in's result and its cancel flag.
type LoginTask = (Receiver<Result<AccountConfig, String>>, Arc<AtomicBool>);
type Refresh = Result<(Me, Vec<Device>), account::Error>;

pub struct AccountPanel {
    /// Address field while signed out.
    url: String,
    login: Option<LoginTask>,
    refresh: Option<Receiver<Refresh>>,
    refreshed: Option<Instant>,
    me: Option<Me>,
    devices: Vec<Device>,
    error: Option<String>,
    add: Option<AddDialog>,
    link_task: Option<Receiver<Result<Option<DeviceLink>, account::Error>>>,
    confirm_remove: bool,
}

struct AddDialog {
    name: String,
    /// Organization ID; `None` is personal.
    org: Option<String>,
}

impl AccountPanel {
    pub fn new(config: &Config) -> AccountPanel {
        AccountPanel {
            url: config.account.url.clone(),
            login: None,
            refresh: None,
            refreshed: None,
            me: None,
            devices: Vec::new(),
            error: None,
            add: None,
            link_task: None,
            confirm_remove: false,
        }
    }

    /// The account's devices, as last fetched.
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// The first fetch after signing in is still running.
    pub fn loading(&self) -> bool {
        self.me.is_none() && self.refresh.is_some()
    }

    /// The account answered at least once.
    pub fn loaded(&self) -> bool {
        self.me.is_some()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Takes the results of the sign-in, the device refresh and the link
    /// task, and starts the next refresh. Call every frame.
    pub fn poll(&mut self, ctx: &egui::Context, config: &mut Config) {
        if let Some((rx, _)) = &self.login {
            if let Ok(result) = rx.try_recv() {
                self.login = None;
                match result {
                    Ok(session) => {
                        log::info!("signed in to {} as {}", session.url, session.login);
                        config.account = session;
                        self.save(config);
                        self.refreshed = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        if let Some(rx) = &self.refresh {
            if let Ok(result) = rx.try_recv() {
                self.refresh = None;
                self.refreshed = Some(Instant::now());
                match result {
                    Ok((me, devices)) => {
                        self.me = Some(me);
                        self.devices = devices;
                        self.error = None;
                    }
                    Err(account::Error::Unauthorized) => {
                        self.sign_out(config, false);
                        self.error = Some(account::Error::Unauthorized.to_string());
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
                // The background process drops the link when the device was
                // removed on the web.
                config.server.device = config::load().server.device;
            }
        }
        if let Some(rx) = &self.link_task {
            if let Ok(result) = rx.try_recv() {
                self.link_task = None;
                match result {
                    Ok(link) => {
                        config.server.device = link;
                        // This is the one save that must write the link itself.
                        if let Err(e) = config::save(config) {
                            self.error = Some(format!("Speichern fehlgeschlagen: {e}"));
                        }
                        control::call(Command::Reload);
                        self.add = None;
                        self.refreshed = None;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
        }
        let stale = self.refreshed.is_none_or(|t| t.elapsed() > REFRESH);
        if config.account.signed_in() && self.refresh.is_none() && stale {
            let (tx, rx) = channel();
            let account = config.account.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = account::me(&account).and_then(|me| Ok((me, account::devices(&account)?)));
                let _ = tx.send(result);
                ctx.request_repaint();
            });
            self.refresh = Some(rx);
        }
        if config.account.signed_in() {
            ctx.request_repaint_after(REFRESH);
        }
    }

    fn save(&mut self, config: &mut Config) {
        if let Err(e) = config::save_keeping_device(config) {
            self.error = Some(format!("Speichern fehlgeschlagen: {e}"));
        }
    }

    fn start_login(&mut self, ctx: &egui::Context) {
        let url = match account::normalize_url(&self.url) {
            Ok(url) => url,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let login = match account::Login::start(&url) {
            Ok(login) => login,
            Err(e) => {
                self.error = Some(format!("Anmeldung nicht möglich: {e}"));
                return;
            }
        };
        self.url = url;
        self.error = None;
        ctx.open_url(egui::OpenUrl::new_tab(login.browser_url()));
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let ctx = ctx.clone();
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let _ = tx.send(login.finish(&stop));
            ctx.request_repaint();
        });
        self.login = Some((rx, cancel));
    }

    fn sign_out(&mut self, config: &mut Config, tell_server: bool) {
        if tell_server {
            let account = config.account.clone();
            std::thread::spawn(move || account::logout(&account));
        }
        config.account.token.clear();
        self.save(config);
        self.me = None;
        self.devices.clear();
        self.refreshed = None;
    }

    /// The "Konto" tab of the settings window.
    pub fn settings(&mut self, ui: &mut egui::Ui, config: &mut Config) {
        let ctx = ui.ctx().clone();
        let p = Palette::of(ui);
        if config.account.signed_in() {
            ui.label(RichText::new(&config.account.name).font(theme::semibold(15.0)).color(p.text));
            ui.label(
                RichText::new(format!("{} · {}", config.account.login, config.account.url))
                    .size(12.5)
                    .color(p.weak),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::Button::new("Im Browser verwalten", Kind::Secondary)
                    .small()
                    .show(ui)
                    .on_hover_text("Organisationen, Mitglieder und Einladungen")
                    .clicked()
                {
                    ctx.open_url(egui::OpenUrl::new_tab(&config.account.url));
                }
                if theme::Button::new("Abmelden", Kind::Ghost)
                    .small()
                    .show(ui)
                    .clicked()
                {
                    self.sign_out(config, true);
                }
            });
        } else {
            self.signed_out(ui, &ctx);
        }
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).size(12.5).color(theme::CORAL));
        }
        ui.add_space(18.0);
        ui.label(RichText::new("Dieser Computer im Konto").font(theme::semibold(14.0)).color(p.text));
        ui.add_space(4.0);
        self.this_computer(ui, config);
        self.add_window(&ctx, config);
    }

    fn signed_out(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        if self.login.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Anmeldung im Browser …");
            });
            ui.label(
                RichText::new("Melde dich im geöffneten Browserfenster an. Danach geht es hier automatisch weiter.")
                    .size(12.5)
                    .color(p.weak),
            );
            if theme::Button::new("Abbrechen", Kind::Ghost)
                .small()
                .show(ui)
                .clicked()
            {
                if let Some((_, cancel)) = self.login.take() {
                    cancel.store(true, Ordering::Release);
                }
            }
            return;
        }
        ui.label(
            RichText::new("Mit einem Konto siehst du in der Übersicht alle deine Geräte und die deines Teams und verbindest dich ohne VNC-Passwort.")
                .color(p.text),
        );
        ui.add_space(8.0);
        theme::field_label(ui, "Koordinationsserver");
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.url)
                .hint_text("z. B. beach.example.com")
                .margin(Margin::symmetric(10, 7))
                .desired_width(f32::INFINITY),
        );
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(8.0);
        let login = theme::Button::new("Im Browser anmelden", Kind::Primary)
            .enabled(!self.url.trim().is_empty())
            .show(ui)
            .on_hover_text("Öffnet den Browser zur Anmeldung mit GitHub oder Google");
        if login.clicked() || (enter && !self.url.trim().is_empty()) {
            self.start_login(ctx);
        }
    }

    /// Whether this computer is in the account, with add and remove. The
    /// link works on its own, so removing is possible while signed out.
    fn this_computer(&mut self, ui: &mut egui::Ui, config: &mut Config) {
        let p = Palette::of(ui);
        let busy = self.link_task.is_some();
        match config.server.device.clone() {
            Some(link) => {
                ui.label(
                    RichText::new(format!("Im Konto als „{}“ ({}).", link.name, link.owner)).color(p.text),
                );
                ui.label(
                    RichText::new("Wer Zugriff hat, verbindet sich über das Konto ohne VNC-Passwort.")
                        .size(12.5)
                        .color(p.weak),
                );
                ui.add_space(6.0);
                let label = if self.confirm_remove {
                    "Wirklich entfernen?"
                } else {
                    "Aus dem Konto entfernen"
                };
                let kind = if self.confirm_remove { Kind::Danger } else { Kind::Secondary };
                let remove = theme::Button::new(label, kind)
                    .small()
                    .enabled(!busy)
                    .show(ui)
                    .on_hover_text("Danach ist dieser Computer nur noch über sein VNC-Passwort erreichbar.");
                if remove.clicked() {
                    if self.confirm_remove {
                        self.confirm_remove = false;
                        let (tx, rx) = channel();
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(account::unregister(&link).map(|()| None));
                            ctx.request_repaint();
                        });
                        self.link_task = Some(rx);
                    } else {
                        self.confirm_remove = true;
                    }
                } else if self.confirm_remove && ui.input(|i| i.pointer.any_click()) {
                    self.confirm_remove = false;
                }
            }
            None => {
                ui.label(RichText::new("Dieser Computer ist noch nicht im Konto.").color(p.text));
                if !config.account.signed_in() {
                    ui.label(
                        RichText::new("Melde dich oben an, um ihn hinzuzufügen.")
                            .size(12.5)
                            .color(p.weak),
                    );
                    return;
                }
                ui.add_space(6.0);
                if theme::Button::new("Hinzufügen …", Kind::Secondary)
                    .small()
                    .enabled(!busy && self.me.is_some())
                    .show(ui)
                    .clicked()
                {
                    self.add = Some(AddDialog {
                        name: computer_name(),
                        org: None,
                    });
                }
            }
        }
    }

    fn add_window(&mut self, ctx: &egui::Context, config: &Config) {
        let Some(dialog) = &mut self.add else {
            return;
        };
        let busy = self.link_task.is_some();
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        let orgs = self.me.as_ref().map(|m| m.orgs.clone()).unwrap_or_default();
        egui::Window::new("Diesen Computer zum Konto hinzufügen")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(360.0);
                theme::field_label(ui, "Name");
                ui.add(egui::TextEdit::singleline(&mut dialog.name).desired_width(f32::INFINITY));
                ui.add_space(8.0);
                theme::field_label(ui, "Gehört zu");
                ui.radio_value(&mut dialog.org, None, "Persönlich (nur du)");
                for org in &orgs {
                    ui.radio_value(&mut dialog.org, Some(org.id.clone()), format!("{} (alle Mitglieder)", org.name));
                }
                ui.add_space(6.0);
                ui.weak(
                    "Wer Zugriff hat, verbindet sich über das Konto ohne VNC-Passwort. Das Passwort gilt \
                     weiterhin für andere VNC-Viewer. Damit das Gerät erreichbar ist, muss der Server laufen.",
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let label = if busy { "Wird hinzugefügt …" } else { "Hinzufügen" };
                    submit = theme::Button::new(label, Kind::Primary)
                        .enabled(!busy && !dialog.name.trim().is_empty())
                        .show(ui)
                        .clicked();
                    cancel = theme::Button::new("Abbrechen", Kind::Ghost).show(ui).clicked();
                });
                if let Some(e) = &self.error {
                    ui.label(RichText::new(e).size(12.0).color(theme::CORAL));
                }
            });
        if submit {
            let account = config.account.clone();
            let name = dialog.name.clone();
            let org = dialog
                .org
                .as_ref()
                .and_then(|id| orgs.iter().find(|o| &o.id == id))
                .map(|o| (o.id.clone(), o.name.clone()));
            let (tx, rx) = channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let org = org.as_ref().map(|(id, name)| (id.as_str(), name.as_str()));
                let _ = tx.send(account::register(&account, &name, org).map(Some));
                ctx.request_repaint();
            });
            self.error = None;
            self.link_task = Some(rx);
        }
        if !open || cancel {
            self.add = None;
        }
    }
}

/// The card's status line: online with the first address, or when the
/// device was last seen.
pub fn status(device: &Device) -> String {
    if device.online {
        return match device.addresses.first() {
            Some(address) => format!("online · {address}"),
            None => "online".into(),
        };
    }
    let Some(seen) = device.last_seen else {
        return "noch nie online".into();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    match now - seen {
        ..=3599 => format!("offline · vor {} Min.", ((now - seen) / 60).max(1)),
        s @ ..=86_399 => format!("offline · vor {} Std.", s / 3600),
        s => format!("offline · vor {} Tagen", s / 86_400),
    }
}

/// This computer's name without `.local`.
fn computer_name() -> String {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    name.strip_suffix(".local").unwrap_or(&name).to_string()
}

/// Connection data for an account device: the first reported address,
/// the others as alternatives.
pub fn server_for(device: &Device) -> Server {
    let mut addresses = device.addresses.iter().map(|a| account::endpoint(a, device.port));
    let host = addresses.next().unwrap_or_default();
    let mut server = Server {
        name: device.name.clone(),
        host,
        ..Server::default()
    };
    server.access.alternatives = addresses.collect();
    server
}
