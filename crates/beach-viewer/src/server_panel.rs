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
}

impl ServerPanel {
    /// Starts the background process if none runs (it sets up autostart and
    /// the service on a fresh installation), then polls it every two seconds.
    pub fn new(ctx: egui::Context) -> Self {
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
        }
    }

    fn poll(&mut self) {
        while let Ok(status) = self.polls.try_recv() {
            self.status = status;
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

    /// macOS: Tailscale's apps only run after sign-in; offer the daemon.
    #[cfg(target_os = "macos")]
    fn tailscale_row(ui: &mut egui::Ui, error: &mut Option<String>) {
        ui.add_space(6.0);
        if service::tailscale_daemon_installed() {
            ui.weak("Tailscale läuft als Dienst und ist schon vor der Anmeldung verbunden.");
            return;
        }
        ui.weak(
            "Die Tailscale-App verbindet sich erst nach der Anmeldung. Für den Zugriff nach einem \
             Neustart braucht es den Tailscale-Dienst; die Umstellung läuft im Terminal und nur \
             direkt am Mac.",
        );
        if ui.small_button("Tailscale-Dienst einrichten …").clicked() {
            if let Err(e) = service::tailscale_daemon_script() {
                *error = Some(format!("Terminal lässt sich nicht öffnen: {e}"));
            }
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
                Self::tailscale_row(ui, &mut self.error);
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
                        "Läuft die Bildschirmfreigabe von macOS, ist Port 5900 belegt: hier z. B. 5901 \
                         eintragen. Der Hintergrunddienst braucht Bildschirmaufnahme und Bedienungshilfen \
                         unter Datenschutz & Sicherheit.",
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    save = ui.button("Speichern").clicked();
                    cancel = ui.button("Abbrechen").clicked();
                });
            });
        if save {
            draft.password = draft.password.chars().take(8).collect();
            if draft.password.is_empty() {
                draft.password = beach_server::random_password();
            }
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
