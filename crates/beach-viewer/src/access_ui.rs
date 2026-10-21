//! The optional capability editor and cancellable connection progress.

use std::sync::mpsc::{Sender, TryRecvError};

use egui::{Margin, RichText};
use zeroize::Zeroizing;

use crate::access::{Access, Answer, Authentication, Event, Prompt, Relay, Shell, Ssh, Task, Wake};
use crate::client::Connection;
use crate::theme::{self, Kind};

pub struct Connecting {
    task: Task,
    status: String,
    prompt: Option<Pending>,
}

struct Pending {
    question: Prompt,
    reply: Sender<Answer>,
    input: Zeroizing<String>,
    show: bool,
    focus: bool,
}

impl Connecting {
    pub fn new(task: Task) -> Self {
        Self {
            task,
            status: "Verbindung wird geprüft …".into(),
            prompt: None,
        }
    }

    pub fn poll(&mut self) -> Option<Result<Connection, String>> {
        loop {
            match self.task.events.try_recv() {
                Ok(Event::Progress(status)) => self.status = status,
                Ok(Event::Prompt(question, reply)) => {
                    self.prompt = Some(Pending {
                        question,
                        reply,
                        input: Zeroizing::new(String::new()),
                        show: false,
                        focus: true,
                    });
                }
                Err(_) => break,
            }
        }
        match self.task.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "Der Verbindungsversuch wurde beendet. Bitte erneut verbinden.".into(),
            )),
        }
    }

    /// Returns true when the user cancels. Inputs remain in the main form.
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let mut cancel = false;
        let mut answer = None;
        ui.add_space(12.0);
        theme::banner(ui, theme::SKY).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(pending) = &mut self.prompt {
                match &pending.question {
                    Prompt::Trust { address, fingerprint } => {
                        ui.label(RichText::new("SSH-Gerät bestätigen").strong());
                        ui.label(format!("Erste Verbindung zu {address}. Prüfe diesen Fingerabdruck am Gerät oder über einen bekannten Zugang:"));
                        ui.add(egui::Label::new(RichText::new(fingerprint).monospace()).wrap().selectable(true));
                        ui.horizontal_wrapped(|ui| {
                            let stop = theme::Button::new("Abbrechen", Kind::Secondary).show(ui);
                            if pending.focus { stop.request_focus(); pending.focus = false; }
                            cancel |= stop.clicked();
                            if theme::Button::new("Vertrauen & fortfahren", Kind::Primary).show(ui).clicked() {
                                answer = Some(Answer::Trust);
                            }
                        });
                    }
                    Prompt::Secret { title, label, echo } => {
                        ui.label(RichText::new(title).strong());
                        theme::field_label(ui, label);
                        let input = ui.add(egui::TextEdit::singleline(&mut *pending.input).password(!(*echo || pending.show)).desired_width(f32::INFINITY));
                        if pending.focus { input.request_focus(); pending.focus = false; }
                        if !echo { ui.checkbox(&mut pending.show, "Anzeigen"); }
                        ui.small("Diese Eingabe wird nur für diesen Versuch verwendet und nicht gespeichert.");
                        ui.horizontal(|ui| {
                            cancel |= theme::Button::new("Abbrechen", Kind::Secondary).show(ui).clicked();
                            if theme::Button::new("Anmelden", Kind::Primary).show(ui).clicked()
                                || (input.has_focus() || input.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                answer = Some(Answer::Secret(Zeroizing::new(std::mem::take(&mut *pending.input))));
                            }
                        });
                    }
                }
            } else {
                ui.horizontal(|ui| { ui.spinner(); ui.label(&self.status); });
                cancel |= theme::Button::new("Abbrechen", Kind::Secondary).show(ui).clicked();
            }
        });
        if let Some(answer) = answer {
            if let Some(pending) = self.prompt.take() {
                let _ = pending.reply.send(answer);
            }
        }
        cancel
    }
}

pub fn configuration(
    ui: &mut egui::Ui,
    access: &mut Access,
) -> egui::collapsing_header::CollapsingResponse<()> {
    egui::CollapsingHeader::new("Erreichbarkeit & Start")
        .id_salt("connection-access")
        .show(ui, |ui| {
            ui.small("Optionale Wege und Fähigkeiten für dieses Gerät. Die normale VNC-Verbindung funktioniert auch ohne diese Angaben.");
            egui::CollapsingHeader::new("Alternative Adressen")
                .id_salt("alternative-addresses")
                .show(ui, |ui| {
                    ui.small("Weitere VNC-Adressen, etwa im lokalen Netz. Sie werden der Reihe nach geprüft. Für den Zugriff von unterwegs muss der Netzwerkweg bereits eingerichtet sein, etwa über einen Tailscale-Subnet-Router.");
                    let mut remove = None;
                    for (index, address) in access.alternatives.iter_mut().enumerate() {
                        ui.push_id(index, |ui| {
                            theme::field_label(ui, &format!("Adresse {}", index + 2));
                            ui.horizontal(|ui| {
                                text(ui, address, "Rechnername oder IP-Adresse mit VNC-Port");
                                if theme::Button::new("Entfernen", Kind::Ghost).small().show(ui).clicked() { remove = Some(index); }
                            });
                        });
                    }
                    if let Some(index) = remove { access.alternatives.remove(index); }
                    if theme::Button::new("+ Adresse", Kind::Secondary).small().show(ui).clicked() { access.alternatives.push(String::new()); }
                });
            let mut enabled = access.ssh.is_some();
            if ui.checkbox(&mut enabled, "SSH-Anmeldung anbieten").changed() {
                access.ssh = enabled.then(Ssh::default);
            }
            if let Some(ssh) = &mut access.ssh {
                egui::CollapsingHeader::new("SSH-Zugang")
                    .default_open(true).id_salt("target-ssh")
                    .show(ui, |ui| {
                        ssh_fields(ui, ssh);
                        ui.small("Meldet dich per SSH an und wartet anschließend auf VNC. Kann FileVault auf unterstützten Macs entsperren. Ein erreichbarer SSH-Port allein bedeutet nicht, dass das Gerät gesperrt ist.");
                    });
            }
            let mut enabled = access.wake.is_some();
            if ui.checkbox(&mut enabled, "Wake-on-LAN anbieten").changed() {
                access.wake = enabled.then(Wake::default);
            }
            if let Some(wake) = &mut access.wake {
                egui::CollapsingHeader::new("Aufwecken")
                    .default_open(true).id_salt("wake-settings")
                    .show(ui, |ui| {
                        theme::field_label(ui, "MAC-Adresse der Netzwerkkarte");
                        text(ui, &mut wake.mac, "00:11:22:33:44:55");
                        theme::field_label(ui, "Aufweckziel im lokalen Netz");
                        text(ui, &mut wake.destination, "192.168.1.255:9");
                        ui.small("Das Zielgerät muss Wake-on-LAN unterstützen und dafür eingerichtet sein. Ohne Zugangsgerät wird das Signal aus deinem aktuellen Netz gesendet. Für unterwegs ein SSH-Zugangsgerät im Netz des Zielgeräts hinzufügen.");
                        let mut remove = None;
                        for (index, relay) in wake.relays.iter_mut().enumerate() {
                            egui::CollapsingHeader::new(format!("Zugangsgerät {}", index + 1))
                                .default_open(true).id_salt(("wake-relay", index))
                                .show(ui, |ui| {
                                    ssh_fields(ui, &mut relay.ssh);
                                    theme::field_label(ui, "Befehlsumgebung");
                                    ui.radio_value(&mut relay.shell, Shell::Posix, "Linux, macOS oder NAS mit POSIX-Shell");
                                    ui.radio_value(&mut relay.shell, Shell::PowerShell, "Windows mit PowerShell");
                                    if relay.shell == Shell::Posix { ui.small("Benötigt Python 3 oder Perl auf dem Zugangsgerät."); }
                                    if theme::Button::new("Zugangsgerät entfernen", Kind::Ghost).small().show(ui).clicked() { remove = Some(index); }
                                });
                        }
                        if let Some(index) = remove { wake.relays.remove(index); }
                        if theme::Button::new("+ SSH-Zugangsgerät", Kind::Secondary).small().show(ui).clicked() { wake.relays.push(Relay::default()); }
                        if wake.relays.len() > 1 { ui.small("Falls ein Zugangsgerät nicht verfügbar ist, wird das nächste versucht."); }
                    });
            }
        })
}

fn ssh_fields(ui: &mut egui::Ui, config: &mut Ssh) {
    theme::field_label(ui, "SSH-Adresse");
    text(
        ui,
        &mut config.address,
        "Rechnername oder IP-Adresse, optional mit :22",
    );
    theme::field_label(ui, "SSH-Benutzername");
    text(ui, &mut config.user, "Benutzer auf diesem Gerät");
    let mut key = matches!(config.auth, Authentication::Key { .. });
    if ui
        .checkbox(&mut key, "Privaten SSH-Schlüssel verwenden")
        .changed()
    {
        config.auth = if key {
            Authentication::Key { path: String::new() }
        } else {
            Authentication::Password
        };
    }
    if let Authentication::Key { path } = &mut config.auth {
        theme::field_label(ui, "Datei des privaten SSH-Schlüssels");
        ui.horizontal(|ui| {
            text(ui, path, "Vollständiger Pfad zur Schlüsseldatei");
            if theme::Button::new("Auswählen …", Kind::Secondary)
                .small()
                .show(ui)
                .clicked()
            {
                if let Some(file) = rfd::FileDialog::new()
                    .set_title("Privaten SSH-Schlüssel auswählen")
                    .pick_file()
                {
                    *path = file.display().to_string();
                }
            }
        });
    }
}

fn text(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .margin(Margin::symmetric(8, 6))
            .desired_width((ui.available_width() - 95.0).max(120.0)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn frame(ctx: &egui::Context, connecting: &mut Connecting, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(480.0, 640.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(420.0);
                connecting.show(ui);
            },
        );
        // These tests inspect layout and interactions without a GPU renderer.
        output.textures_delta.clear();
        output
    }

    fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn append(shape: &egui::epaint::Shape, output: &mut String) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    output.push_str(text.galley.text());
                    output.push('\n');
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        append(shape, output);
                    }
                }
                _ => {}
            }
        }
        let mut text = String::new();
        for shape in shapes {
            append(&shape.shape, &mut text);
        }
        text
    }

    #[test]
    fn password_is_masked_and_enter_submits_it_once() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let (reply, answers) = channel();
        let question = Prompt::Secret {
            title: "SSH-Anmeldung".into(),
            label: "Passwort".into(),
            echo: false,
        };
        let mut connecting = Connecting::new(crate::access::start(
            crate::servers::Server::default(),
            crate::access::Action::Connect,
            rfb::PixelFormat::BGRX,
            || {},
        ));
        connecting.prompt = Some(Pending {
            question,
            reply,
            input: Zeroizing::new(String::new()),
            show: false,
            focus: true,
        });
        connecting.prompt.as_mut().unwrap().input = Zeroizing::new("temporary secret".into());
        let output = frame(&ctx, &mut connecting, vec![]);
        let text = painted_text(&output.shapes);
        assert!(text.contains("SSH-Anmeldung"));
        assert!(!text.contains("temporary secret"));
        frame(
            &ctx,
            &mut connecting,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let Answer::Secret(value) = answers.recv_timeout(std::time::Duration::from_secs(1)).unwrap() else {
            panic!("missing password answer");
        };
        assert_eq!(&*value, "temporary secret");
        assert!(connecting.prompt.is_none());
        assert!(answers.try_recv().is_err());
    }

    #[test]
    fn trust_prompt_shows_identity_and_does_not_trust_automatically() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let (reply, answers) = channel();
        let question = Prompt::Trust {
            address: "relay:22".into(),
            fingerprint: "SHA256:fixture".into(),
        };
        let mut connecting = Connecting::new(crate::access::start(
            crate::servers::Server::default(),
            crate::access::Action::Connect,
            rfb::PixelFormat::BGRX,
            || {},
        ));
        connecting.prompt = Some(Pending {
            question,
            reply,
            input: Zeroizing::new(String::new()),
            show: false,
            focus: true,
        });
        let output = frame(&ctx, &mut connecting, vec![]);
        let text = painted_text(&output.shapes);
        assert!(text.contains("relay:22") && text.contains("SHA256:fixture"));
        assert!(text.contains("Abbrechen") && text.contains("Vertrauen & fortfahren"));
        assert!(answers.try_recv().is_err());
    }
}
