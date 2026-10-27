//! Renders the actual access editor without starting a server, tray, updater
//! or autostart setup. Optional first argument: save a PNG and exit.
#![allow(dead_code, unused_imports)]

#[path = "../src/access.rs"]
mod access;
#[path = "../src/access_ui.rs"]
mod access_ui;
#[path = "../src/account.rs"]
mod account;
#[path = "../src/client.rs"]
mod client;
#[path = "../src/servers.rs"]
mod servers;
#[path = "../src/settings.rs"]
mod settings;
#[path = "../src/theme.rs"]
mod theme;
mod update {
    pub(crate) fn curl_path() -> std::path::PathBuf {
        "curl".into()
    }
}

struct Preview {
    access: access::Access,
    screenshot: Option<std::path::PathBuf>,
    frames: usize,
}

impl eframe::App for Preview {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = event {
                let bytes: Vec<_> = image.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
                image::save_buffer(
                    self.screenshot.as_ref().unwrap(),
                    &bytes,
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(440.0);
                ui.heading("VNC Beach");
                ui.label("Erreichbarkeit & Start für einen gespeicherten Computer");
                ui.add_space(12.0);
                theme::card(ui).show(ui, |ui| {
                    ui.set_width(406.0);
                    let response = access_ui::configuration(ui, &mut self.access);
                    let id = response.header_response.id;
                    let mut state =
                        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
                    state.set_open(true);
                    state.store(ui.ctx());
                });
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    theme::Button::new("Verbinden", theme::Kind::Primary).show(ui);
                    theme::Button::new("Per SSH anmelden", theme::Kind::Secondary).show(ui);
                    theme::Button::new("Aufwecken", theme::Kind::Secondary).show(ui);
                });
            });
        });
        self.frames += 1;
        if self.screenshot.is_some() && self.frames == 12 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        if self.screenshot.is_some() {
            ctx.request_repaint();
        }
    }
}

fn main() -> eframe::Result {
    let screenshot = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    eframe::run_native(
        "VNC Beach · Vorschau der Verbindungseinstellungen",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([480.0, 880.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(Preview {
                access: access::Access {
                    alternatives: vec!["192.168.1.20::5901".into()],
                    ssh: Some(access::Ssh {
                        address: "192.168.1.20".into(),
                        user: "remote".into(),
                        ..Default::default()
                    }),
                    wake: Some(access::Wake {
                        mac: "00:11:22:33:44:55".into(),
                        ..Default::default()
                    }),
                },
                screenshot,
                frames: 0,
            }))
        }),
    )
}
