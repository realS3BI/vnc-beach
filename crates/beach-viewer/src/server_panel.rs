//! "Dieser Computer": status and settings of the built-in server, which runs
//! in the background process (see [`crate::background`]).

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use egui::{Color32, RichText, Vec2};

use crate::autostart;
use crate::config::{self, Config, ServerConfig};
use crate::control::{self, Command, Status};

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
    /// Starts polling the background process every two seconds.
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, polls) = channel();
        std::thread::Builder::new()
            .name("server-status".into())
            .spawn(move || loop {
                if tx.send(control::call(Command::Status)).is_err() {
                    break;
                }
                ctx.request_repaint();
                std::thread::sleep(Duration::from_secs(2));
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
        if control::call(Command::Quit).is_some() {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < until && control::call(Command::Status).is_some() {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        self.status = None;
        self.error = Self::start_background().err();
    }

    /// The status block for the sidebar.
    pub fn show(&mut self, ui: &mut egui::Ui, config: &Config) {
        self.poll();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Dieser Computer").strong().size(16.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Einstellungen …").clicked() {
                    self.draft = Some(config.server.clone());
                    self.displays = beach_server::displays().unwrap_or_default();
                }
            });
        });
        ui.add_space(4.0);
        let online = Color32::from_rgb(90, 200, 120);
        let off = ui.visuals().weak_text_color();
        match &self.status {
            Some(s) if s.running => {
                let viewers = match s.viewers.len() {
                    0 => "kein Zuschauer".to_string(),
                    1 => format!("1 Zuschauer ({})", s.viewers[0]),
                    n => format!("{n} Zuschauer"),
                };
                let warn = Color32::from_rgb(240, 180, 60);
                let missing = |what: &str| s.missing.iter().any(|m| m == what);
                ui.horizontal_wrapped(|ui| {
                    dot(ui, if s.missing.is_empty() { online } else { warn });
                    ui.label(format!("Server online, Port {}, {viewers}", s.port));
                });
                if missing("accessibility") {
                    ui.add_space(4.0);
                    ui.colored_label(
                        warn,
                        "Zuschauer können nur zusehen: VNC Beach fehlt die Freigabe für \
                         Bedienungshilfen. Nach dem Erlauben gehen Maus und Tastatur von selbst.",
                    );
                    if ui.button("Bedienungshilfen öffnen").clicked() {
                        open_privacy_pane("Privacy_Accessibility");
                    }
                }
                if missing("screen_recording") {
                    ui.add_space(4.0);
                    ui.colored_label(
                        warn,
                        "Zuschauer sehen nur den Schreibtischhintergrund: VNC Beach fehlt die \
                         Freigabe für Bildschirmaufnahme. Nach dem Erlauben den Dienst neu starten.",
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Bildschirmaufnahme öffnen").clicked() {
                            open_privacy_pane("Privacy_ScreenCapture");
                        }
                        if ui.button("Dienst neu starten").clicked() {
                            self.restart = true;
                        }
                    });
                }
            }
            Some(s) => {
                ui.horizontal_wrapped(|ui| {
                    dot(ui, off);
                    match &s.error {
                        Some(e) => ui.label(friendly_error(e, s.port)),
                        None => ui.label("Server aus"),
                    };
                });
            }
            None => {
                ui.horizontal_wrapped(|ui| {
                    dot(ui, off);
                    ui.label("Hintergrunddienst läuft nicht");
                });
            }
        }
        let password = self
            .status
            .as_ref()
            .map(|s| s.password.clone())
            .unwrap_or_else(|| config.server.password.clone());
        if !password.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Passwort:");
                if self.show_password {
                    ui.monospace(&password);
                } else {
                    ui.monospace("••••••••");
                }
                let eye = if self.show_password {
                    "verbergen"
                } else {
                    "anzeigen"
                };
                if ui.small_button(eye).clicked() {
                    self.show_password = !self.show_password;
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| match &self.status {
            Some(s) if s.enabled => {
                if ui.button("Server anhalten").clicked() {
                    self.command(Command::Stop);
                }
            }
            Some(_) => {
                if ui.button("Server starten").clicked() {
                    self.command(Command::Start);
                }
            }
            None => {
                if ui.button("Hintergrunddienst starten").clicked() {
                    self.error = Self::start_background().err();
                }
            }
        });
        if std::mem::take(&mut self.restart) {
            self.restart_background();
        }
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(230, 90, 80), e);
        }
        ui.add_space(6.0);
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
                        ui.label("Bildschirm");
                        let name = |d: Option<usize>| match d {
                            None => "Hauptbildschirm".to_string(),
                            Some(i) => self
                                .displays
                                .get(i)
                                .cloned()
                                .unwrap_or_else(|| format!("Bildschirm {i}")),
                        };
                        egui::ComboBox::from_id_salt("display")
                            .selected_text(name(draft.display))
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut draft.display, None, name(None));
                                for i in 0..self.displays.len() {
                                    ui.selectable_value(&mut draft.display, Some(i), name(Some(i)));
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

/// A small status dot (the default font has no "●").
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
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
