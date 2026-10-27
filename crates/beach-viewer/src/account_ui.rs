//! The "Konto" section of the sidebar: signing in to a coordination
//! server, the account's devices grouped by owner, and adding this computer
//! to the account.

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
    /// Device shown in the main area.
    selected: Option<String>,
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
            selected: None,
            error: None,
            add: None,
            link_task: None,
            confirm_remove: false,
        }
    }

    /// The selected account device, for the main area.
    pub fn selected(&self) -> Option<&Device> {
        let id = self.selected.as_ref()?;
        self.devices.iter().find(|d| &d.id == id)
    }

    pub fn deselect(&mut self) {
        self.selected = None;
    }

    fn poll(&mut self, ctx: &egui::Context, config: &mut Config) {
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
                        self.save(config);
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

    fn save(&mut self, config: &Config) {
        if let Err(e) = config::save(config) {
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
        self.selected = None;
        self.refreshed = None;
    }

    /// The sidebar section. Returns a device to connect to.
    pub fn show(&mut self, ui: &mut egui::Ui, config: &mut Config) -> Option<Device> {
        let ctx = ui.ctx().clone();
        self.poll(&ctx, config);
        let p = Palette::of(ui);
        let mut connect = None;
        if !config.account.signed_in() {
            self.signed_out(ui, &ctx);
            return None;
        }
        ui.horizontal(|ui| {
            theme::section_label(ui, "Konto").on_hover_text(format!(
                "Angemeldet als {} ({}) bei {}",
                config.account.name, config.account.login, config.account.url
            ));
            if !self.devices.is_empty() {
                theme::pill(ui, &self.devices.len().to_string(), p.weak);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::Button::new("Abmelden", Kind::Ghost)
                    .small()
                    .show(ui)
                    .clicked()
                {
                    self.sign_out(config, true);
                }
                if theme::Button::new("Verwalten", Kind::Ghost)
                    .small()
                    .show(ui)
                    .on_hover_text("Organisationen, Mitglieder und Einladungen im Browser verwalten")
                    .clicked()
                {
                    ctx.open_url(egui::OpenUrl::new_tab(&config.account.url));
                }
            });
        });
        if !config.account.signed_in() {
            return None;
        }
        ui.label(RichText::new(&config.account.name).size(12.0).color(p.weak));
        ui.add_space(6.0);
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).size(12.0).color(theme::CORAL));
            ui.add_space(4.0);
        }
        if self.me.is_none() && self.refresh.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Geräte werden geladen …");
            });
        } else if self.devices.is_empty() && self.me.is_some() {
            ui.label(
                RichText::new("Noch keine Geräte im Konto. Füge unten diesen Computer hinzu oder installiere VNC Beach auf einem anderen.")
                    .size(12.5)
                    .color(p.weak),
            );
        }
        egui::ScrollArea::vertical()
            .id_salt("account-devices")
            .max_height(260.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let mut group: Option<Option<&str>> = None;
                for device in &self.devices {
                    let owner = device.org.as_ref().map(|o| o.name.as_str());
                    if group != Some(owner) {
                        group = Some(owner);
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(owner.unwrap_or("Persönlich"))
                                .font(theme::semibold(11.5))
                                .color(p.weak),
                        );
                    }
                    let card = crate::app::favorite_card(
                        ui,
                        &card_server(device),
                        self.selected.as_ref() == Some(&device.id),
                    );
                    if card.connect {
                        connect = Some(device.clone());
                    }
                    if card.select || card.connect {
                        self.selected = Some(device.id.clone());
                    }
                }
            });
        ui.add_space(6.0);
        self.this_computer(ui, config);
        self.add_window(&ctx, config);
        connect
    }

    fn signed_out(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::of(ui);
        theme::section_label(ui, "Konto");
        ui.add_space(6.0);
        theme::card(ui).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.login.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Anmeldung im Browser …");
                });
                ui.label(
                    RichText::new(
                        "Melde dich im geöffneten Browserfenster an. Danach geht es hier automatisch weiter.",
                    )
                    .size(12.0)
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
                RichText::new("Mit einem Konto siehst du hier alle deine Geräte und die deines Teams.")
                    .size(12.5)
                    .color(p.text),
            );
            ui.add_space(6.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text("Koordinationsserver, z. B. beach.example.com")
                    .margin(Margin::symmetric(8, 5))
                    .desired_width(f32::INFINITY),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(6.0);
            let login = theme::Button::new("Anmelden", Kind::Primary)
                .small()
                .enabled(!self.url.trim().is_empty())
                .show(ui)
                .on_hover_text("Öffnet den Browser zur Anmeldung mit GitHub oder Google");
            if login.clicked() || (enter && !self.url.trim().is_empty()) {
                self.start_login(ctx);
            }
            if let Some(e) = &self.error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).size(12.0).color(theme::CORAL));
            }
        });
    }

    /// Whether this computer is in the account, with add and remove.
    fn this_computer(&mut self, ui: &mut egui::Ui, config: &mut Config) {
        let p = Palette::of(ui);
        let busy = self.link_task.is_some();
        match config.server.device.clone() {
            Some(link) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Dieser Computer ist als „{}“ im Konto ({}).",
                            link.name, link.owner
                        ))
                        .size(12.0)
                        .color(p.weak),
                    );
                    let label = if self.confirm_remove {
                        "Wirklich entfernen?"
                    } else {
                        "Entfernen"
                    };
                    let remove = theme::Button::new(label, Kind::Ghost)
                        .small()
                        .enabled(!busy)
                        .show(ui)
                        .on_hover_text(
                            "Danach ist dieser Computer nur noch über sein VNC-Passwort erreichbar.",
                        );
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
                });
            }
            None => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("Dieser Computer ist noch nicht im Konto.")
                            .size(12.0)
                            .color(p.weak),
                    );
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
                });
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

    /// The main area for the selected device. Returns true for "Verbinden".
    pub fn details(&self, ui: &mut egui::Ui, width: f32, connecting: bool) -> bool {
        let Some(device) = self.selected() else {
            return false;
        };
        let p = Palette::of(ui);
        let owner = device
            .org
            .as_ref()
            .map(|o| o.name.as_str())
            .unwrap_or("Persönlich");
        ui.label(RichText::new(&device.name).font(theme::bold(22.0)).color(p.text));
        ui.label(RichText::new(format!("Gerät im Konto · {owner}")).color(p.weak));
        ui.add_space(14.0);
        let mut connect = false;
        theme::card(ui).show(ui, |ui| {
            ui.set_width(width - 34.0);
            egui::Grid::new("device-details")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Status").color(p.weak));
                    ui.horizontal(|ui| {
                        theme::dot(ui, if device.online { theme::GREEN } else { p.weak });
                        ui.label(status(device));
                    });
                    ui.end_row();
                    ui.label(RichText::new("System").color(p.weak));
                    ui.label(match device.platform.as_str() {
                        "macos" => "macOS",
                        "windows" => "Windows",
                        other => other,
                    });
                    ui.end_row();
                    ui.label(RichText::new("Adressen").color(p.weak));
                    ui.vertical(|ui| {
                        if device.addresses.is_empty() {
                            ui.label("–");
                        }
                        for address in &device.addresses {
                            ui.monospace(account::endpoint(address, device.port));
                        }
                    });
                    ui.end_row();
                });
            ui.add_space(12.0);
            if device.addresses.is_empty() {
                ui.weak("Das Gerät hat noch keine Adresse gemeldet. Läuft VNC Beach dort?");
                ui.add_space(6.0);
            }
            let label = if connecting { "Verbinde …" } else { "Verbinden" };
            connect = theme::Button::new(label, Kind::Primary)
                .enabled(!connecting && !device.addresses.is_empty())
                .min_size(Vec2::new(width - 34.0, 36.0))
                .show(ui)
                .clicked();
            ui.add_space(4.0);
            ui.weak("Die Verbindung läuft direkt zum Gerät (LAN oder Tailscale). Der Koordinationsserver stellt nur die Zugangsberechtigung aus.");
        });
        connect
    }
}

/// A favorites-style card: the status line takes the address's place.
fn card_server(device: &Device) -> Server {
    Server {
        name: device.name.clone(),
        host: status(device),
        ..Server::default()
    }
}

fn status(device: &Device) -> String {
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
