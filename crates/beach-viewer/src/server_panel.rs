//! "Dieser Computer": status and settings of the built-in server, which runs
//! in the background process (see [`crate::background`]).

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use beach_server::Display;
use egui::{CornerRadius, Margin, RichText, Vec2};

use crate::theme::{self, Kind, Palette};

use crate::config::{self, Config, ServerConfig};
use crate::{autostart, service};
use crate::control::{self, Command, Endpoint, Status};

pub struct ServerPanel {
    /// Latest poll: `None` if no background process answers.
    status: Option<Status>,
    polls: Receiver<Option<Status>>,
    /// The settings window's draft.
    draft: Option<ServerConfig>,
    displays: Vec<String>,
    show_password: bool,
    /// "Dienst neu starten" was clicked.
    restart: bool,
    error: Option<String>,
    #[cfg(target_os = "macos")]
    mac_status: crate::mac_server::Status,
    #[cfg(target_os = "macos")]
    mac_polls: Receiver<crate::mac_server::Status>,
    #[cfg(target_os = "macos")]
    mac_setup: Option<Receiver<Result<(), String>>>,
    #[cfg(target_os = "macos")]
    mac_consent: bool,
    #[cfg(target_os = "macos")]
    mac_auto_login: bool,
    #[cfg(target_os = "macos")]
    mac_finished: bool,
    /// The virtual display driver is installed (checked when the settings open).
    #[cfg(windows)]
    driver_installed: bool,
    #[cfg(windows)]
    driver_setup: Option<Receiver<Result<(), String>>>,
}

impl ServerPanel {
    /// Starts the background process if none runs (it sets up autostart and
    /// the service on a fresh installation), then polls it every two seconds.
    pub fn new(ctx: egui::Context) -> Self {
        #[cfg(target_os = "macos")]
        let mac_polls = {
            let (send, receive) = channel();
            let context = ctx.clone();
            std::thread::spawn(move || loop {
                if send.send(crate::mac_server::status()).is_err() { break; }
                context.request_repaint();
                std::thread::sleep(Duration::from_secs(10));
            });
            receive
        };
        let (tx, polls) = channel();
        std::thread::Builder::new()
            .name("server-status".into())
            .spawn(move || {
                if control::call_at(Endpoint::Tray, Command::Status).is_none() {
                    if let Err(e) = Self::start_background() {
                        log::warn!("cannot start the background process: {e}");
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                loop {
                    if tx.send(control::call(Command::Status)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    std::thread::sleep(Duration::from_secs(2));
                }
            })
            .expect("spawn status thread");
        ServerPanel {
            status: None,
            polls,
            draft: None,
            displays: Vec::new(),
            show_password: false,
            restart: false,
            error: None,
            #[cfg(target_os = "macos")]
            mac_status: crate::mac_server::Status::default(),
            #[cfg(target_os = "macos")]
            mac_polls,
            #[cfg(target_os = "macos")]
            mac_setup: None,
            #[cfg(target_os = "macos")]
            mac_consent: false,
            #[cfg(target_os = "macos")]
            mac_auto_login: false,
            #[cfg(target_os = "macos")]
            mac_finished: false,
            #[cfg(windows)]
            driver_installed: false,
            #[cfg(windows)]
            driver_setup: None,
        }
    }

    /// Takes the results of background status checks and setup runs.
    fn poll(&mut self) {
        #[cfg(target_os = "macos")]
        {
            while let Ok(status) = self.mac_polls.try_recv() {
                if self.mac_status.message.is_empty() { self.mac_auto_login = status.automatic_login; }
                self.mac_status = status;
            }
            if let Some(result) = self.mac_setup.as_ref().and_then(|receive| receive.try_recv().ok()) {
                self.mac_setup = None;
                match result {
                    Ok(()) => {
                        self.error = None;
                        self.mac_finished = true;
                        self.mac_consent = false;
                        self.mac_status.message = "Eingerichtet. Status wird geprüft; ein Neustarttest steht noch aus.".into();
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        #[cfg(windows)]
        if let Some(result) = self.driver_setup.as_ref().and_then(|receive| receive.try_recv().ok()) {
            self.driver_setup = None;
            self.driver_installed = crate::display_driver::installed();
            if let Err(e) = result {
                self.error = Some(e);
            }
        }
        while let Ok(status) = self.polls.try_recv() {
            self.status = status;
        }
    }

    /// Windows: status of the virtual display driver and its setup button.
    #[cfg(windows)]
    fn virtual_display_row(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.strong("Virtueller Bildschirm");
        if self.driver_installed {
            ui.weak(
                "Eingerichtet: Sind alle Monitore aus, stellt VNC Beach einen Bildschirm mit 2560 × 1440 \
                 bereit und entfernt ihn wieder, sobald ein Monitor zurück ist.",
            );
            return;
        }
        ui.weak(
            "Sind alle Monitore aus, zeigt Windows nur einen kleinen Ersatzbildschirm. Mit dem kostenlosen \
             Treiber „usbmmidd_v2“ von Amyuni Technologies stellt VNC Beach dann einen Bildschirm mit \
             2560 × 1440 bereit. Der Treiber wird von amyuni.com geladen, steht unter der Lizenz des \
             Herstellers und braucht Administratorrechte.",
        );
        if self.driver_setup.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Treiber wird eingerichtet …");
            });
        } else if theme::Button::new("Treiber einrichten", Kind::Secondary)
            .small()
            .show(ui)
            .clicked()
        {
            let (send, receive) = channel();
            let context = ui.ctx().clone();
            self.driver_setup = Some(receive);
            self.error = None;
            std::thread::spawn(move || {
                let result = service::request_display_driver().map_err(|e| match e {
                    service::SetupError::Cancelled => {
                        "Ohne Administratorrechte lässt sich der Treiber nicht einrichten.".to_string()
                    }
                    service::SetupError::Failed(e) => e,
                });
                let _ = send.send(result);
                context.request_repaint();
            });
        }
    }

    fn command(&mut self, cmd: Command) {
        match control::call(cmd) {
            Some(status) => self.status = Some(status),
            None => {
                self.status = None;
                self.error = Some("Der Hintergrunddienst antwortet nicht.".into());
            }
        }
    }

    fn start_background() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::process::Command::new(exe)
            .arg("--background")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Quits the background process and starts a fresh one, which picks up
    /// permissions macOS only grants to new processes.
    fn restart_background(&mut self) {
        if control::call_at(Endpoint::Tray, Command::Quit).is_some() {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < until && control::call_at(Endpoint::Tray, Command::Status).is_some() {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        self.status = None;
        self.error = Self::start_background().err();
    }

    /// Sets the service up or removes it to match the settings. Asks for
    /// administrator rights.
    fn apply_service(config: &mut Config) -> Result<(), String> {
        if !service::supported() {
            return Ok(());
        }
        if !config.server.service {
            if service::installed() {
                service::request_uninstall().map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        if service::needs_setup().is_none() {
            return Ok(());
        }
        // Asked for explicitly: forget an earlier "no".
        if !config.server.service_declined.is_empty() {
            config.server.service_declined.clear();
            let _ = config::save(config);
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        service::request_install(&exe).map_err(|e| e.to_string())
    }

    /// Explicit opt-in: never silently disable FileVault on first launch.
    #[cfg(target_os = "macos")]
    fn mac_server_row(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.strong("Servermodus nach Stromausfall");
        ui.label(&self.mac_status.message);
        ui.weak("Startet SSH, Tailscale und VNC Beach vor der Anmeldung. Unterstützt die eigenständige Tailscale-App und einen vorhandenen tailscaled-Dienst. Tailscale muss bereits angemeldet sein.");
        ui.add_enabled_ui(self.mac_setup.is_none(), |ui| {
            ui.checkbox(&mut self.mac_consent, "FileVault für diesen Mac deaktivieren");
            ui.weak("Ohne FileVault schützt das Anmeldepasswort die Daten bei physischem Zugriff nicht mehr wie zuvor. Die Auswahl gilt dauerhaft für diesen Mac und bleibt bei VNC-Beach-Updates erhalten.");
            ui.checkbox(&mut self.mac_auto_login, "Auch Benutzerdienste automatisch starten");
            ui.weak("Meldet den Benutzer automatisch an und sperrt den Bildschirm nach 15 Sekunden. macOS speichert dafür das Anmeldepasswort. Ohne diese Option starten nur die Dienste vor der Anmeldung.");
            let label = if self.mac_status.enabled { "Servermodus reparieren …" } else { "Servermodus einrichten …" };
            if ui.add_enabled(self.mac_consent, egui::Button::new(label)).clicked() {
                let (send, receive) = channel();
                let automatic_login = self.mac_auto_login;
                let context = ui.ctx().clone();
                self.mac_setup = Some(receive);
                self.error = None;
                std::thread::spawn(move || {
                    let _ = send.send(crate::mac_server::request_setup(automatic_login));
                    context.request_repaint();
                });
            }
        });
        if self.mac_setup.is_some() {
            ui.horizontal(|ui| { ui.spinner(); ui.label("Servermodus wird eingerichtet. Passwortabfrage von VNC Beach beachten …"); });
        }
        if let Some(error) = &self.error {
            ui.colored_label(Palette::of(ui).text, error);
        }
    }

    /// The status block for the sidebar.
    pub fn show(&mut self, ui: &mut egui::Ui, config: &Config) {
        self.poll();
        let p = Palette::of(ui);
        theme::card(ui)
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Dieser Computer")
                            .font(theme::semibold(14.0))
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::Button::new("Einstellungen", Kind::Ghost)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            self.draft = Some(config.server.clone());
                            self.displays = beach_server::displays().unwrap_or_default();
                            #[cfg(windows)]
                            {
                                self.driver_installed = crate::display_driver::installed();
                            }
                        }
                    });
                });
                ui.add_space(2.0);
                self.status_rows(ui);
                let password = self
                    .status
                    .as_ref()
                    .map(|s| s.password.clone())
                    .unwrap_or_else(|| config.server.password.clone());
                if !password.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Passwort").color(p.weak));
                        let shown = if self.show_password {
                            password.as_str()
                        } else {
                            "••••••••"
                        };
                        egui::Frame::new()
                            .fill(p.input)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.monospace(shown);
                            });
                        let eye = if self.show_password {
                            "Verbergen"
                        } else {
                            "Anzeigen"
                        };
                        if theme::Button::new(eye, Kind::Ghost).small().show(ui).clicked() {
                            self.show_password = !self.show_password;
                        }
                    });
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| match &self.status {
                    Some(s) if s.enabled => {
                        if theme::Button::new("Server anhalten", Kind::Secondary)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            self.command(Command::Stop);
                        }
                    }
                    Some(_) => {
                        if theme::Button::new("Server starten", Kind::Secondary)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            self.command(Command::Start);
                        }
                    }
                    None => {
                        if theme::Button::new("Hintergrunddienst starten", Kind::Secondary)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            self.error = Self::start_background().err();
                        }
                    }
                });
                if std::mem::take(&mut self.restart) {
                    self.restart_background();
                }
                if let Some(e) = &self.error {
                    ui.add_space(2.0);
                    ui.label(RichText::new(e).color(theme::CORAL).size(12.5));
                }
            });
    }

    /// Status dot and line, plus warnings about missing macOS permissions.
    fn status_rows(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        let off = p.weak;
        match &self.status {
            Some(s) if s.running => {
                let viewers = match s.viewers.len() {
                    0 => "kein Zuschauer".to_string(),
                    1 => format!("1 Zuschauer ({})", s.viewers[0]),
                    n => format!("{n} Zuschauer"),
                };
                let missing = |what: &str| s.missing.iter().any(|m| m == what);
                ui.horizontal_wrapped(|ui| {
                    theme::dot(
                        ui,
                        if s.missing.is_empty() {
                            theme::GREEN
                        } else {
                            theme::AMBER
                        },
                    );
                    ui.label(format!("Online auf Port {}, {viewers}", s.port));
                });
                if s.service {
                    ui.weak("Läuft als Windows-Dienst, auch im Sperrbildschirm erreichbar.");
                }
                if missing("accessibility") {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Zuschauer können nur zusehen: VNC Beach fehlt die Freigabe für \
                             Bedienungshilfen. Nach dem Erlauben gehen Maus und Tastatur von selbst.",
                        )
                        .color(theme::AMBER)
                        .size(12.5),
                    );
                    if theme::Button::new("Bedienungshilfen öffnen", Kind::Secondary)
                        .small()
                        .show(ui)
                        .clicked()
                    {
                        open_privacy_pane("Privacy_Accessibility");
                    }
                }
                if missing("screen_recording") {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Zuschauer sehen nur den Schreibtischhintergrund: VNC Beach fehlt die \
                             Freigabe für Bildschirmaufnahme. Nach dem Erlauben den Dienst neu starten.",
                        )
                        .color(theme::AMBER)
                        .size(12.5),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if theme::Button::new("Bildschirmaufnahme öffnen", Kind::Secondary)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            open_privacy_pane("Privacy_ScreenCapture");
                        }
                        if theme::Button::new("Dienst neu starten", Kind::Secondary)
                            .small()
                            .show(ui)
                            .clicked()
                        {
                            self.restart = true;
                        }
                    });
                }
            }
            Some(s) => {
                ui.horizontal_wrapped(|ui| {
                    theme::dot(ui, off);
                    match &s.error {
                        Some(e) => ui.label(friendly_error(e, s.port)),
                        None => ui.label("Server aus"),
                    };
                });
            }
            None => {
                ui.horizontal_wrapped(|ui| {
                    theme::dot(ui, off);
                    ui.label("Hintergrunddienst läuft nicht");
                });
            }
        }
    }

    /// The settings window. Returns true if the config was changed and saved.
    pub fn window(&mut self, ctx: &egui::Context, config: &mut Config) -> bool {
        #[cfg(target_os = "macos")]
        if self.mac_finished {
            self.mac_finished = false;
            config.server = config::load().server;
            if let Some(draft) = self.draft.as_mut() {
                draft.enabled = true;
                draft.autostart = true;
                draft.service = true;
            }
        }
        let Some(mut draft) = self.draft.take() else {
            return false;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Server-Einstellungen")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(380.0);
                egui::ScrollArea::vertical()
                    .max_height((ctx.content_rect().height() - 150.0).max(200.0))
                    .show(ui, |ui| {
                ui.checkbox(&mut draft.enabled, "Bildschirm dieses Computers freigeben");
                ui.add_enabled(
                    autostart::supported(),
                    egui::Checkbox::new(&mut draft.autostart, "Hintergrunddienst beim Anmelden starten"),
                );
                if service::supported() {
                    if cfg!(windows) {
                        ui.checkbox(
                            &mut draft.service,
                            "Auch im Sperrbildschirm und vor der Anmeldung erreichbar",
                        );
                        ui.weak(
                            "Richtet einen Windows-Dienst ein, der mit Windows startet, und stellt \
                             Tailscale so ein, dass es ohne Anmeldung läuft. Windows fragt dafür nach \
                             Administratorrechten.",
                        );
                    } else {
                        ui.checkbox(&mut draft.service, "Auch am Anmeldefenster erreichbar");
                        ui.weak(
                            "Startet den Server auch am Anmeldefenster des Macs. macOS fragt dafür einmal \
                             nach dem Passwort eines Administrators.",
                        );
                    }
                }
                #[cfg(target_os = "macos")]
                self.mac_server_row(ui);
                #[cfg(windows)]
                self.virtual_display_row(ui);
                ui.add_space(8.0);
                egui::Grid::new("server-settings")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut draft.port).range(1..=65535));
                        ui.end_row();
                        ui.label("Passwort");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut draft.password).desired_width(140.0));
                            if ui.small_button("Neu erzeugen").clicked() {
                                draft.password = beach_server::random_password();
                            }
                        });
                        ui.end_row();
                        ui.label("Bildschirme");
                        let name = |d: Display| match d {
                            Display::All => "Alle Bildschirme".to_string(),
                            Display::Primary => "Nur Hauptbildschirm".to_string(),
                            Display::Index(i) => self
                                .displays
                                .get(i)
                                .cloned()
                                .unwrap_or_else(|| format!("Bildschirm {i}")),
                        };
                        egui::ComboBox::from_id_salt("display")
                            .selected_text(name(draft.display))
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut draft.display, Display::All, name(Display::All));
                                ui.selectable_value(
                                    &mut draft.display,
                                    Display::Primary,
                                    name(Display::Primary),
                                );
                                for i in 0..self.displays.len() {
                                    ui.selectable_value(
                                        &mut draft.display,
                                        Display::Index(i),
                                        name(Display::Index(i)),
                                    );
                                }
                            });
                        ui.end_row();
                        ui.label("Bilder pro Sekunde");
                        ui.add(egui::DragValue::new(&mut draft.fps).range(1..=60));
                        ui.end_row();
                        ui.label("Eingabe");
                        ui.checkbox(&mut draft.view_only, "Nur ansehen, keine Eingaben annehmen");
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.weak("Das Passwort gilt nur für die ersten 8 Zeichen (VNC-Standard).");
                ui.weak(
                    "Bei „Alle Bildschirme“ sehen Zuschauer alle Bildschirme nebeneinander und können im \
                     Viewer auf einen einzelnen umschalten.",
                );
                if cfg!(target_os = "macos") {
                    ui.weak(
                        "Port 5900 gehört meist der Bildschirmfreigabe von macOS, deshalb nimmt \
                         VNC Beach 5901. Der Hintergrunddienst braucht Bildschirmaufnahme und Bedienungshilfen \
                         unter Datenschutz & Sicherheit.",
                    );
                }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    #[cfg(target_os = "macos")]
                    let ready = self.mac_setup.is_none();
                    #[cfg(not(target_os = "macos"))]
                    let ready = true;
                    save = ui.add_enabled(ready, egui::Button::new("Speichern")).clicked();
                    cancel = ui.button("Abbrechen").clicked();
                });
            });
        if save {
            draft.password = draft.password.chars().take(8).collect();
            if draft.password.is_empty() {
                draft.password = beach_server::random_password();
            }
            // Linked to the account from the sidebar in the meantime.
            draft.device = config.server.device.clone();
            config.server = draft;
            self.error = config::save(config)
                .map_err(|e| format!("Speichern fehlgeschlagen: {e}"))
                .err();
            if autostart::supported() && autostart::enabled() != config.server.autostart {
                if let Err(e) = autostart::set(config.server.autostart) {
                    self.error = Some(format!("Autostart: {e}"));
                }
            }
            if let Err(e) = Self::apply_service(config) {
                self.error = Some(e);
            }
            if self.status.is_some() {
                self.command(Command::Reload);
            } else if config.server.enabled {
                self.error = Self::start_background().err();
            }
            true
        } else {
            if open && !cancel {
                self.draft = Some(draft);
            }
            false
        }
    }
}

/// The server's start error in words a user can act on.
fn friendly_error(error: &str, port: u16) -> String {
    let lower = error.to_lowercase();
    if lower.contains("binding") && (lower.contains("in use") || lower.contains("10048")) {
        if cfg!(target_os = "macos") {
            format!(
                "Port {port} ist belegt, vermutlich von der Bildschirmfreigabe von macOS. \
                 In den Einstellungen einen anderen Port wählen, etwa {}.",
                port + 1
            )
        } else {
            format!("Port {port} ist belegt. In den Einstellungen einen anderen Port wählen.")
        }
    } else if lower.contains("screen capture") || lower.contains("capturing the screen") {
        format!("Der Bildschirm lässt sich nicht aufnehmen: {error}")
    } else {
        format!("Server-Fehler: {error}")
    }
}

/// Opens a page of System Settings > Privacy & Security (macOS).
fn open_privacy_pane(anchor: &str) {
    #[cfg(target_os = "macos")]
    {
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
            log::warn!("cannot open System Settings: {e}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = anchor;
}

#[cfg(test)]
mod tests {
    use super::friendly_error;

    #[test]
    fn explains_busy_port() {
        let e = "binding 0.0.0.0:5900: Address already in use (os error 48)";
        assert!(friendly_error(e, 5900).contains("Port 5900 ist belegt"));
        let e = "binding 0.0.0.0:5900: Normalerweise ... (os error 10048)";
        assert!(friendly_error(e, 5900).contains("belegt"));
        assert_eq!(friendly_error("boom", 5900), "Server-Fehler: boom");
    }
}
