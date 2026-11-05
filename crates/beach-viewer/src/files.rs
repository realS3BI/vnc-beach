//! File browser, native file drops, and a sequential transfer worker.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use egui::{
    Align, Color32, CornerRadius, CursorIcon, FontId, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, UiBuilder, Vec2,
};

use crate::client::FileChannel;
use crate::theme::{self, Icon, Kind, Palette};
use beach_server::files::Incoming;
use rfb::files::{valid_name, valid_path, Entry, Failure, Request, Response, CHUNK_SIZE};

/// Rows ignore clicks this long after the list changed, so the second half
/// of a double click on a folder doesn't download the file that moved under
/// the pointer.
const SETTLE_SECONDS: f64 = 0.4;
const ROW_HEIGHT: f32 = 36.0;

enum Command {
    List(String),
    Upload { paths: Vec<PathBuf>, directory: String },
    Download { path: String, destination: PathBuf },
}

enum Event {
    Ready { root: String, initial: String },
    Directory { path: String, entries: Vec<Entry> },
    Uploaded { directory: String, names: Vec<String> },
    Saved(PathBuf),
    Failed(String),
    Unavailable(String),
}

/// What the last finished transfer left behind.
enum Done {
    Uploaded { directory: String, names: Vec<String> },
    Saved(PathBuf),
}

#[derive(Default, Clone)]
struct Progress {
    name: String,
    done: u64,
    total: u64,
    /// Position of this file among the files of one upload, from 1.
    index: usize,
    count: usize,
}

enum Support {
    Checking,
    Ready,
    Unavailable(String),
}

pub struct Panel {
    pub open: bool,
    support: Support,
    root: String,
    directory: String,
    /// The address bar shows a text field instead of the folder trail.
    editing: bool,
    focus_path: bool,
    path_input: String,
    entries: Vec<Entry>,
    loaded: bool,
    busy: bool,
    listing: bool,
    uploading: bool,
    /// egui time of the current frame and of the last change to `entries`.
    now: f64,
    settled: f64,
    done: Option<Done>,
    error: Option<String>,
    local: Option<PathBuf>,
    commands: SyncSender<Command>,
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
}

impl Panel {
    /// `viewport` is the session window that shows the panel; the worker
    /// repaints it when something arrives.
    pub fn new(connection: FileChannel, ctx: egui::Context, viewport: egui::ViewportId) -> Self {
        let (commands, rx) = sync_channel(1);
        let (tx, events) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(Progress::default()));
        let worker = Worker {
            channel: connection,
            cancel: cancel.clone(),
            progress: progress.clone(),
            ctx,
            viewport,
            events: tx,
        };
        std::thread::spawn(move || worker.run(rx));
        Self {
            open: false,
            support: Support::Checking,
            root: String::new(),
            directory: String::new(),
            editing: false,
            focus_path: false,
            path_input: String::new(),
            entries: Vec::new(),
            loaded: false,
            busy: false,
            listing: false,
            uploading: false,
            now: 0.0,
            settled: 0.0,
            done: None,
            error: None,
            local: dirs::download_dir().or_else(dirs::home_dir),
            commands,
            events,
            cancel,
            progress,
        }
    }

    pub fn button_label(&self) -> &'static str {
        if self.busy {
            "Dateien …"
        } else if self.error.is_some() && !self.open {
            "Dateien !"
        } else if self.done.is_some() && !self.open {
            "Dateien ✓"
        } else {
            "Dateien"
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context, view_only: bool) {
        self.now = ctx.input(|i| i.time);
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Ready { root, initial } => {
                    self.support = Support::Ready;
                    self.root = root;
                    self.directory = initial;
                }
                Event::Directory { path, entries } => {
                    self.directory = path;
                    self.entries = entries;
                    self.loaded = true;
                    self.busy = false;
                    self.listing = false;
                    self.settled = self.now;
                }
                Event::Uploaded { directory, names } => {
                    self.busy = false;
                    self.done = Some(Done::Uploaded { directory, names });
                    self.loaded = false;
                }
                Event::Saved(path) => {
                    self.busy = false;
                    self.done = Some(Done::Saved(path));
                }
                Event::Failed(message) => {
                    self.busy = false;
                    self.listing = false;
                    self.error = Some(message);
                    // Files copied before the failure should show up.
                    if self.uploading {
                        self.loaded = false;
                    }
                }
                Event::Unavailable(message) => {
                    self.busy = false;
                    self.listing = false;
                    self.support = Support::Unavailable(message);
                }
            }
        }
        if view_only && self.busy {
            self.cancel.store(true, Ordering::Release);
        }
        let dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        if !dropped.is_empty() {
            self.open = true;
            match self.blocked(view_only) {
                Some(reason) => self.error = Some(reason),
                None => {
                    let paths = dropped.iter().map(|f| f.path().to_path_buf()).collect();
                    self.start(Command::Upload {
                        paths,
                        directory: self.directory.clone(),
                    });
                }
            }
        }
        if self.open
            && !self.loaded
            && !self.busy
            && self.error.is_none()
            && matches!(self.support, Support::Ready)
            && !view_only
        {
            self.start(Command::List(self.directory.clone()));
        }
    }

    fn can_transfer(&self, view_only: bool) -> bool {
        matches!(self.support, Support::Ready) && !view_only && !self.busy
    }

    /// Why no transfer can start right now, if that is so.
    fn blocked(&self, view_only: bool) -> Option<String> {
        if view_only {
            return Some("Dateiübertragung ist bei „Nur ansehen“ deaktiviert.".into());
        }
        match &self.support {
            Support::Checking => {
                Some("Dateiübertragung wird geprüft. Versuche es gleich noch einmal.".into())
            }
            Support::Unavailable(message) => Some(message.clone()),
            Support::Ready if self.busy => {
                Some("Eine Übertragung oder Ordnerabfrage läuft bereits. Warte auf ihren Abschluss.".into())
            }
            Support::Ready => None,
        }
    }

    fn start(&mut self, command: Command) {
        self.cancel.store(false, Ordering::Release);
        self.listing = matches!(command, Command::List(_));
        self.uploading = matches!(command, Command::Upload { .. });
        self.editing = false;
        if !self.listing {
            self.done = None;
        }
        *self.progress.lock().unwrap() = Progress::default();
        match self.commands.try_send(command) {
            Ok(()) => {
                self.busy = true;
                self.error = None;
            }
            Err(_) => {
                self.error = Some("Dateiübertragung nicht verfügbar. Verbinde dich erneut.".into());
            }
        }
    }

    /// `computer` is the name of the session's server, for the title.
    pub fn show(&mut self, ctx: &egui::Context, computer: &str, view_only: bool) {
        if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
            self.drop_target(ctx, computer, view_only);
        }
        if !self.open {
            return;
        }
        // Esc closes the panel, unless it just dropped a typed path.
        let mut open = self.open && !(ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !self.editing);
        egui::Window::new(format!("Dateien auf {computer}"))
            .id(egui::Id::new("files"))
            .open(&mut open)
            .collapsible(false)
            .default_size([580.0, 500.0])
            .min_size([460.0, 360.0])
            .show(ctx, |ui| self.contents(ui, computer, view_only));
        self.open = open;
    }

    /// Shown over the whole session window while files hover above it.
    fn drop_target(&self, ctx: &egui::Context, computer: &str, view_only: bool) {
        let p = Palette::for_context(ctx);
        let shade = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("file-drop-shade"));
        ctx.layer_painter(shade)
            .rect_filled(ctx.content_rect(), 0.0, Color32::from_black_alpha(120));
        let (title, detail) = match self.blocked(view_only) {
            None => {
                let folder = self.directory.rsplit('/').next().filter(|name| !name.is_empty());
                let folder = folder.unwrap_or_else(|| home_name(&self.root));
                (
                    "Zum Hochladen loslassen",
                    format!("Die Dateien landen im Ordner „{folder}“ auf {computer}."),
                )
            }
            Some(reason) => ("Hochladen gerade nicht möglich", reason),
        };
        egui::Area::new(egui::Id::new("file-drop-hint"))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                theme::card_in(&p)
                    .inner_margin(Margin::symmetric(28, 22))
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(title).font(theme::bold(18.0)).color(p.text));
                            ui.label(RichText::new(detail).color(p.weak));
                        });
                    });
            });
    }

    fn contents(&mut self, ui: &mut egui::Ui, computer: &str, view_only: bool) {
        match &self.support {
            Support::Checking => return placeholder(ui, true, "Dateiübertragung wird geprüft …", ""),
            Support::Unavailable(message) => {
                return placeholder(ui, false, "Dateiübertragung nicht verfügbar", message)
            }
            Support::Ready => {}
        }
        if view_only {
            return placeholder(
                ui,
                false,
                "Bei „Nur ansehen“ ausgeschaltet",
                "Schalte „Nur ansehen“ in der Sitzungsleiste aus, um Dateien zu übertragen.",
            );
        }
        let enabled = self.can_transfer(view_only);
        // The footer is placed first, from the bottom, so the list gets
        // exactly what is left. Nothing here may be taller than it claims:
        // a resizable window grows for good when its content overflows.
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            self.save_row(ui);
            self.upload_row(ui, computer, enabled);
            self.status(ui);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                self.address_bar(ui, enabled);
                self.list(ui, enabled);
            });
        });
    }

    /// Up, the folder trail (or a text field for a typed path), refresh.
    fn address_bar(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let p = Palette::of(ui);
        let mut go = None;
        let size = Vec2::new(ui.available_width(), 32.0);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let up = enabled && !self.directory.is_empty();
            if theme::icon_button(ui, Icon::Up, "Übergeordneter Ordner", up).clicked() {
                go = Some(self.directory.rsplit_once('/').map_or("", |(p, _)| p).to_owned());
            }
            let width = (ui.available_width() - 38.0).max(60.0);
            let sense = if enabled && !self.editing {
                Sense::click()
            } else {
                Sense::hover()
            };
            let (rect, bar) = ui.allocate_exact_size(Vec2::new(width, 32.0), sense);
            let outline = if self.editing { p.accent } else { p.card_stroke };
            ui.painter().rect(
                rect,
                CornerRadius::same(8),
                p.input,
                Stroke::new(1.0, outline),
                StrokeKind::Inside,
            );
            let inner = rect.shrink2(Vec2::new(6.0, 0.0));
            let mut child = ui.new_child(
                UiBuilder::new()
                    .id_salt("address")
                    .max_rect(inner)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            child.shrink_clip_rect(inner);
            if self.editing {
                go = go.take().or(self.path_edit(&mut child));
            } else {
                go = go.take().or(self.crumbs(&mut child, enabled));
                // Like the address bar of Explorer: a click next to the
                // trail turns it into a text field.
                if bar.clicked() {
                    let separator = separator(&self.root).to_string();
                    self.path_input = self.directory.replace('/', &separator);
                    self.editing = true;
                    self.focus_path = true;
                }
                if bar.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::Text);
                }
            }
            if self.listing {
                ui.add_sized(Vec2::splat(32.0), egui::Spinner::new().size(16.0));
            } else if theme::icon_button(ui, Icon::Refresh, "Aktualisieren", enabled).clicked() {
                go = Some(self.directory.clone());
            }
        });
        if let Some(path) = go {
            self.start(Command::List(path));
        }
    }

    /// The open folder as a trail of its parents; a click opens that one.
    fn crumbs(&self, ui: &mut egui::Ui, enabled: bool) -> Option<String> {
        let p = Palette::of(ui);
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut items = vec![(home_name(&self.root).to_owned(), String::new())];
        for part in self.directory.split('/').filter(|part| !part.is_empty()) {
            let path = join(&items[items.len() - 1].1, part);
            items.push((part.to_owned(), path));
        }
        let last = items.len() - 1;
        let widths: Vec<f32> = items
            .iter()
            .enumerate()
            .map(|(i, (text, _))| crumb_size(ui, text, i == last).x)
            .collect();
        // Folders in the middle make room first; home and the open folder stay.
        let mut hidden = 0;
        while hidden + 2 < items.len() {
            let shown: f32 = widths
                .iter()
                .enumerate()
                .filter(|(i, _)| *i == 0 || *i > hidden)
                .map(|(_, width)| width + CRUMB_GAP)
                .sum();
            let ellipsis = if hidden > 0 { 18.0 + CRUMB_GAP } else { 0.0 };
            if shown + ellipsis <= ui.available_width() {
                break;
            }
            hidden += 1;
        }
        let mut go = None;
        for (i, (text, path)) in items.iter().enumerate() {
            // One ellipsis stands for all hidden folders.
            if (2..=hidden).contains(&i) {
                continue;
            }
            if i > 0 {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(CRUMB_GAP, 24.0), Sense::hover());
                theme::paint_chevron(ui, rect.center(), 7.0, false, p.weak.gamma_multiply(0.7));
            }
            if i == 1 && hidden > 0 {
                ui.add_sized(
                    Vec2::new(18.0, 24.0),
                    egui::Label::new(RichText::new("…").color(p.weak)).selectable(false),
                );
            } else if crumb(ui, text, i == last, enabled && i != last).clicked() {
                go = Some(path.clone());
            }
        }
        go
    }

    /// The text field of the address bar. Returns the folder to open.
    fn path_edit(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let p = Palette::of(ui);
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(4.0);
        // The home folder's name, as in the trail; the path is relative to it.
        let prefix = format!("{}{}", home_name(&self.root), separator(&self.root));
        ui.add(egui::Label::new(RichText::new(prefix).color(p.weak)).selectable(false));
        let edit = egui::TextEdit::singleline(&mut self.path_input)
            .frame(egui::Frame::NONE)
            .desired_width(f32::INFINITY)
            .hint_text("Ordner, zum Beispiel Documents/Arbeit")
            .show(ui)
            .response;
        if self.focus_path {
            self.focus_path = false;
            edit.request_focus();
        }
        if !edit.lost_focus() {
            return None;
        }
        // Enter opens the folder; Esc or a click elsewhere drops the input.
        self.editing = false;
        if !ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            return None;
        }
        let path = self.path_input.trim().replace('\\', "/");
        let path = path.trim_matches('/');
        if valid_path(path) {
            Some(path.to_owned())
        } else {
            self.error = Some(
                "Gib einen Ordner im Benutzerordner an, zum Beispiel Downloads oder Documents/Arbeit.".into(),
            );
            None
        }
    }

    fn list(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let p = Palette::of(ui);
        let mut action = None;
        let mut no_folder = false;
        egui::Frame::new()
            .fill(p.canvas)
            .stroke(Stroke::new(1.0, p.card_stroke))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(4))
            .show(ui, |ui| {
                if self.entries.is_empty() {
                    if self.loaded {
                        placeholder(
                            ui,
                            false,
                            "Keine übertragbaren Dateien in diesem Ordner",
                            "Ziehe Dateien ins Fenster, um sie hierher zu kopieren.",
                        );
                    } else if self.error.is_some() {
                        placeholder(ui, false, "Der Ordner konnte nicht geladen werden", "");
                    } else {
                        placeholder(ui, true, "Ordner wird geladen …", "");
                    }
                    return;
                }
                let clickable = enabled && self.now >= self.settled + SETTLE_SECONDS;
                let fresh: &[String] = match &self.done {
                    Some(Done::Uploaded { directory, names }) if *directory == self.directory => names,
                    _ => &[],
                };
                let hint = match &self.local {
                    Some(local) => format!("Herunterladen nach {}", local.display()),
                    None => "Herunterladen".into(),
                };
                // A faint handle at rest shows that the list goes on.
                ui.spacing_mut().scroll.dormant_handle_opacity = 0.4;
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    if self.busy && !self.listing {
                        ui.multiply_opacity(0.55);
                    }
                    for entry in &self.entries {
                        if !row(ui, entry, fresh.contains(&entry.name), clickable, &hint).clicked() {
                            continue;
                        }
                        let path = join(&self.directory, &entry.name);
                        if entry.directory {
                            action = Some(Command::List(path));
                        } else if let Some(local) = &self.local {
                            action = Some(Command::Download {
                                path,
                                destination: free_destination(local, &entry.name),
                            });
                        } else {
                            no_folder = true;
                        }
                    }
                });
            });
        if no_folder {
            self.error = Some("Wähle unten zuerst einen Ordner für Downloads.".into());
        }
        if let Some(command) = action {
            self.start(command);
        }
    }

    /// Progress, the last error or the last result, between list and
    /// footer. Every variant allocates its exact height (see `contents`).
    fn status(&mut self, ui: &mut egui::Ui) {
        if self.busy && !self.listing {
            self.transfer(ui);
        } else if let Some(error) = &self.error {
            let label = if self.entries.is_empty() && !self.loaded {
                "Erneut versuchen"
            } else {
                "OK"
            };
            if notice(ui, "error", theme::CORAL, error, &[label]).is_some() {
                self.error = None;
            }
        } else if let Some(done) = &self.done {
            let (text, actions): (String, &[&str]) = match done {
                Done::Saved(path) => (
                    format!("„{}“ gespeichert", file_name(path)),
                    &["Im Ordner anzeigen", "OK"],
                ),
                Done::Uploaded { names, .. } => (
                    match names.as_slice() {
                        [one] => format!("„{one}“ hochgeladen"),
                        many => format!("{} Dateien hochgeladen", many.len()),
                    },
                    &["OK"],
                ),
            };
            match (notice(ui, "done", theme::GREEN, &text, actions), done) {
                (Some(0), Done::Saved(path)) => reveal(path),
                (Some(_), _) => self.done = None,
                (None, _) => {}
            }
        }
    }

    /// The running upload or download with its progress and "Abbrechen".
    fn transfer(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        let progress = self.progress.lock().unwrap().clone();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), Sense::hover());
        ui.painter().rect(
            rect,
            CornerRadius::same(10),
            p.tint(p.accent),
            Stroke::new(1.0, p.accent.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt("transfer")
                .max_rect(rect.shrink2(Vec2::new(10.0, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
        );
        let cancelling = self.cancel.load(Ordering::Acquire);
        let label = if cancelling {
            "Wird abgebrochen …"
        } else {
            "Abbrechen"
        };
        let cancel = theme::Button::new(label, Kind::Secondary)
            .small()
            .enabled(!cancelling)
            .show(&mut child);
        if cancel.clicked() {
            self.cancel.store(true, Ordering::Release);
        }
        let (left, right) = (rect.left() + 14.0, cancel.rect.left() - 14.0);
        let verb = if self.uploading {
            "Hochladen"
        } else {
            "Herunterladen"
        };
        let title = if progress.name.is_empty() {
            format!("{verb} …")
        } else if progress.count > 1 {
            format!(
                "{verb} ({} von {}): {}",
                progress.index, progress.count, progress.name
            )
        } else {
            format!("{verb}: {}", progress.name)
        };
        let amount = if progress.total == 0 {
            String::new()
        } else {
            format!("{} von {}", bytes(progress.done), bytes(progress.total))
        };
        let amount = ui
            .painter()
            .layout_no_wrap(amount, FontId::proportional(12.5), p.weak);
        let title = theme::elided(
            ui,
            &title,
            theme::semibold(13.5),
            p.text,
            right - left - amount.size().x - 12.0,
        );
        let painter = ui.painter();
        let top = rect.top() + 11.0;
        painter.galley(Pos2::new(right - amount.size().x, top + 1.0), amount, p.weak);
        painter.galley(Pos2::new(left, top), title, p.text);
        let track = Rect::from_min_max(
            Pos2::new(left, rect.bottom() - 17.0),
            Pos2::new(right, rect.bottom() - 11.0),
        );
        painter.rect_filled(track, CornerRadius::same(3), p.card_stroke);
        if progress.total > 0 {
            let fraction = (progress.done as f32 / progress.total as f32).clamp(0.0, 1.0);
            let mut filled = track;
            filled.set_width(track.width() * fraction);
            painter.rect_filled(filled, CornerRadius::same(3), p.accent);
        }
    }

    /// The one main action: copy local files into the open server folder.
    fn upload_row(&mut self, ui: &mut egui::Ui, computer: &str, enabled: bool) {
        let p = Palette::of(ui);
        let size = Vec2::new(ui.available_width(), 34.0);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.expand_to_include_rect(ui.max_rect());
            let upload = theme::Button::new("Dateien hochladen …", Kind::Primary)
                .enabled(enabled)
                .show(ui)
                .on_hover_text(format!("In den geöffneten Ordner auf {computer} kopieren"));
            if upload.clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .set_title(format!("Dateien auf {computer} kopieren"))
                    .pick_files()
                {
                    self.start(Command::Upload {
                        paths,
                        directory: self.directory.clone(),
                    });
                }
            }
            ui.label(RichText::new("oder ins Fenster ziehen").size(13.0).color(p.weak))
                .on_hover_text("Ordner bitte vorher als ZIP-Datei packen.");
        });
    }

    /// Where downloads go on this computer.
    fn save_row(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        let size = Vec2::new(ui.available_width(), 26.0);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.expand_to_include_rect(ui.max_rect());
            ui.spacing_mut().item_spacing.x = 6.0;
            let font = FontId::proportional(12.5);
            ui.label(
                RichText::new("Downloads landen in")
                    .font(font.clone())
                    .color(p.weak),
            );
            let change = theme::Button::new("Ändern …", Kind::Ghost)
                .small()
                .enabled(!self.busy);
            let full = match &self.local {
                Some(local) => local.display().to_string(),
                None => "noch keinem Ordner".into(),
            };
            let room = ui.available_width() - change.size(ui).x - 8.0;
            let short = shorten_path(&full, |text| {
                let galley = ui
                    .painter()
                    .layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER);
                galley.size().x <= room
            });
            let label =
                ui.add(egui::Label::new(RichText::new(&short).font(font.clone()).color(p.text)).truncate());
            if short != full {
                label.on_hover_text(full);
            }
            if change.show(ui).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Ordner für Downloads auswählen")
                    .pick_folder()
                {
                    self.local = Some(path);
                }
            }
        });
    }
}

const CRUMB_GAP: f32 = 14.0;

fn crumb_font(current: bool) -> FontId {
    if current {
        theme::semibold(13.5)
    } else {
        FontId::proportional(13.5)
    }
}

fn crumb_size(ui: &egui::Ui, text: &str, current: bool) -> Vec2 {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), crumb_font(current), Color32::PLACEHOLDER);
    Vec2::new(galley.size().x + 12.0, 24.0)
}

/// One folder of the trail in the address bar.
fn crumb(ui: &mut egui::Ui, text: &str, current: bool, clickable: bool) -> egui::Response {
    let p = Palette::of(ui);
    let mut size = crumb_size(ui, text, current);
    size.x = size.x.min(ui.available_width().max(30.0));
    let sense = if clickable { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            clickable,
            format!("Ordner {text} öffnen"),
        )
    });
    let hovered = clickable && response.hovered();
    if hovered || (clickable && response.has_focus()) {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), p.button_hover);
    }
    let color = if current || hovered { p.text } else { p.weak };
    let galley = theme::elided(ui, text, crumb_font(current), color, size.x - 12.0);
    ui.painter().galley(
        Pos2::new(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if clickable {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// One folder or file of the list. The whole row is the click target:
/// a folder opens, a file downloads.
fn row(ui: &mut egui::Ui, entry: &Entry, fresh: bool, clickable: bool, hint: &str) -> egui::Response {
    let p = Palette::of(ui);
    let sense = if clickable { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), sense);
    response.widget_info(|| {
        let label = if entry.directory {
            format!("Ordner {} öffnen", entry.name)
        } else {
            format!("{} herunterladen, {}", entry.name, bytes(entry.size))
        };
        egui::WidgetInfo::labeled(egui::WidgetType::Button, clickable, label)
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = clickable && response.hovered();
    let focused = clickable && response.has_focus();
    let painter = ui.painter();
    if hovered || focused {
        let fill = if response.is_pointer_button_down_on() {
            p.button_hover
        } else {
            p.button
        };
        painter.rect_filled(rect, CornerRadius::same(7), fill);
    }
    if focused {
        painter.rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(2.0, p.accent),
            StrokeKind::Inside,
        );
    }
    let middle = rect.center().y;
    let mut right = rect.right() - 12.0;
    if entry.directory {
        let folder = if p.dark { theme::SUN } else { theme::AMBER };
        theme::paint_icon(
            painter,
            Icon::Folder,
            Pos2::new(rect.left() + 20.0, middle),
            folder,
        );
        let chevron = if hovered { p.text } else { p.weak };
        theme::paint_chevron(ui, Pos2::new(right - 5.0, middle), 8.0, false, chevron);
        right -= 22.0;
    } else {
        theme::paint_icon(painter, Icon::File, Pos2::new(rect.left() + 20.0, middle), p.weak);
        let download = if hovered { p.accent } else { p.weak };
        theme::paint_icon(painter, Icon::Download, Pos2::new(right - 8.0, middle), download);
        right -= 30.0;
        let size = painter.layout_no_wrap(bytes(entry.size), FontId::proportional(12.5), p.weak);
        painter.galley(
            Pos2::new(right - size.size().x, middle - size.size().y / 2.0),
            size,
            p.weak,
        );
        // A fixed column, so that all names end at the same place.
        right -= 72.0;
    }
    if fresh {
        let tag = painter.layout_no_wrap("Neu".into(), theme::semibold(11.0), theme::GREEN);
        let size = tag.size() + Vec2::new(14.0, 6.0);
        let pill = Rect::from_min_size(Pos2::new(right - size.x, middle - size.y / 2.0), size);
        painter.rect_filled(pill, CornerRadius::same(99), p.tint(theme::GREEN));
        painter.galley(pill.center() - tag.size() / 2.0, tag, theme::GREEN);
        right = pill.left() - 8.0;
    }
    let left = rect.left() + 40.0;
    let name = theme::elided(ui, &entry.name, FontId::proportional(14.0), p.text, right - left);
    let cut = name.elided;
    painter.galley(Pos2::new(left, middle - name.size().y / 2.0), name, p.text);
    if !clickable {
        return response;
    }
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    match (entry.directory, cut) {
        (true, false) => response,
        (true, true) => response.on_hover_text(&entry.name),
        (false, false) => response.on_hover_text(hint),
        (false, true) => response.on_hover_text(format!("{}\n{hint}", entry.name)),
    }
}

/// Centred text for a state without a list: checking, empty, unavailable.
/// Takes all the space that is left.
fn placeholder(ui: &mut egui::Ui, spinner: bool, title: &str, detail: &str) {
    let p = Palette::of(ui);
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let width = (rect.width() - 48.0).clamp(80.0, 380.0);
    let text = |text: &str, font: FontId, color: Color32| {
        let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, width);
        job.halign = Align::Center;
        ui.painter().layout_job(job)
    };
    let title = text(title, theme::semibold(15.0), p.text);
    let detail = (!detail.is_empty()).then(|| text(detail, FontId::proportional(13.0), p.weak));
    let wheel = if spinner { 30.0 } else { 0.0 };
    let height = wheel + title.size().y + detail.as_ref().map_or(0.0, |d| 6.0 + d.size().y);
    let mut y = rect.center().y - height / 2.0;
    let x = rect.center().x;
    if spinner {
        let wheel_rect = Rect::from_center_size(Pos2::new(x, y + 10.0), Vec2::splat(18.0));
        ui.put(wheel_rect, egui::Spinner::new().size(18.0));
        y += wheel;
    }
    let step = title.size().y + 6.0;
    ui.painter().galley(Pos2::new(x, y), title, p.text);
    if let Some(detail) = detail {
        ui.painter().galley(Pos2::new(x, y + step), detail, p.weak);
    }
}

fn ghost(text: &str) -> theme::Button<'_> {
    theme::Button::new(text, Kind::Ghost).small()
}

/// A tinted note with buttons on its right; returns the index of the one
/// that was clicked. The height is known before anything is placed, which
/// the bottom-up footer needs.
fn notice(ui: &mut egui::Ui, id: &str, color: Color32, text: &str, actions: &[&str]) -> Option<usize> {
    let p = Palette::of(ui);
    let width = ui.available_width();
    let buttons: f32 = actions.iter().map(|action| ghost(action).size(ui).x + 6.0).sum();
    let galley = ui.painter().layout(
        text.to_owned(),
        FontId::proportional(13.5),
        p.text,
        (width - 36.0 - buttons).max(80.0),
    );
    let height = (galley.size().y + 22.0).max(42.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        p.tint(color),
        Stroke::new(1.0, color.gamma_multiply(0.6)),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        Pos2::new(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(id)
            .max_rect(rect.shrink2(Vec2::new(8.0, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 6.0;
    let mut clicked = None;
    for (index, action) in actions.iter().enumerate().rev() {
        if ghost(action).show(&mut child).clicked() {
            clicked = Some(index);
        }
    }
    clicked
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.into()
    } else {
        format!("{directory}/{name}")
    }
}

/// The separator the server's own paths use, judged by its home folder.
fn separator(root: &str) -> char {
    if root.contains('\\') {
        '\\'
    } else {
        '/'
    }
}

/// The name of the server's home folder, the first step of the trail.
fn home_name(root: &str) -> &str {
    root.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("Benutzerordner")
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The longest tail of `path` that `fits`, with an ellipsis for the rest.
fn shorten_path(path: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(path) {
        return path.to_owned();
    }
    let separator = separator(path);
    let parts: Vec<&str> = path.split(separator).filter(|part| !part.is_empty()).collect();
    for start in 1..parts.len() {
        let tail = format!("…{separator}{}", parts[start..].join(&separator.to_string()));
        if fits(&tail) {
            return tail;
        }
    }
    parts
        .last()
        .map_or_else(|| path.to_owned(), |last| (*last).to_owned())
}

/// Where a download called `name` can go in `folder` without replacing
/// anything: the name itself, or "name (2).ext" and so on.
fn free_destination(folder: &Path, name: &str) -> PathBuf {
    let free = |path: &PathBuf| path.symlink_metadata().is_err();
    let plain = folder.join(name);
    if free(&plain) {
        return plain;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    (2..1000)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(free)
        // The transfer refuses an existing file, so this stays safe.
        .unwrap_or(plain)
}

/// Shows a saved file in Explorer or Finder.
fn reveal(path: &Path) {
    #[cfg(windows)]
    let result = {
        use std::os::windows::process::CommandExt;
        // Explorer wants quotes around the path alone, which `arg` can't write.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
    };
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
    if let Err(error) = result {
        log::warn!("could not show {}: {error}", path.display());
    }
}

fn bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < 3 {
        value /= 1024.0;
        unit += 1;
    }
    // German decimal comma, like the rest of the interface.
    format!("{value:.1} {}", ["KB", "MB", "GB", "TB"][unit]).replace('.', ",")
}

enum TransferError {
    Failed(String),
    Channel(rfb::Error),
}
type Result<T> = std::result::Result<T, TransferError>;

impl From<std::io::Error> for TransferError {
    fn from(error: std::io::Error) -> Self {
        log::debug!("local file transfer: {error}");
        Self::Failed(match error.kind() {
            std::io::ErrorKind::AlreadyExists => "Die Zieldatei existiert bereits. Benenne sie um oder wähle einen anderen Ordner.",
            std::io::ErrorKind::PermissionDenied => "Kein Zugriff auf die lokale Datei. Prüfe die Berechtigungen oder wähle einen anderen Ordner.",
            std::io::ErrorKind::NotFound => "Die lokale Datei oder der Zielordner existiert nicht mehr. Wähle sie erneut aus.",
            _ => "Die lokale Datei konnte nicht gelesen oder gespeichert werden. Prüfe den freien Speicher und versuche es erneut.",
        }.into())
    }
}

struct Worker {
    channel: FileChannel,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    ctx: egui::Context,
    viewport: egui::ViewportId,
    events: std::sync::mpsc::Sender<Event>,
}

impl Worker {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        self.ctx.request_repaint_of(self.viewport);
    }

    fn run(self, commands: Receiver<Command>) {
        match self.channel.hello() {
            Ok(Response::Hello { root, initial }) if valid_path(&initial) => {
                self.emit(Event::Ready { root, initial })
            }
            _ => {
                self.emit(Event::Unavailable("Dieser Server bietet keine Dateiübertragung an. Dafür wird ein aktueller Server von VNC Beach ohne „Nur ansehen“ benötigt.".into()));
                return;
            }
        }
        while let Ok(command) = commands.recv() {
            let result = match command {
                Command::List(path) => self.list(path),
                Command::Upload { paths, directory } => self.upload(paths, &directory),
                Command::Download { path, destination } => self.download(path, destination),
            };
            if let Err(error) = result {
                match error {
                    TransferError::Failed(message) => {
                        // Drain the response before accepting another command, even
                        // when cancellation was requested during an in-flight chunk.
                        if self.request(Request::Cancel).is_err() {
                            self.emit(Event::Unavailable(
                                "Dateiübertragung unterbrochen. Verbinde dich erneut.".into(),
                            ));
                            return;
                        }
                        self.emit(Event::Failed(message));
                    }
                    TransferError::Channel(error) => {
                        log::debug!("file transfer channel failed: {error}");
                        self.channel.cancel_pending();
                        self.emit(Event::Unavailable(
                            "Keine Antwort auf die Dateiübertragung. Verbinde dich erneut.".into(),
                        ));
                        return;
                    }
                }
            }
        }
    }

    fn request(&self, request: Request) -> Result<Response> {
        let response = self.channel.request(request).map_err(TransferError::Channel)?;
        if let Response::Failed(failure) = response {
            return Err(TransferError::Failed(match failure {
                Failure::Denied => "Kein Zugriff auf diesen Serverordner. Wähle einen Ordner innerhalb des Benutzerordners.",
                Failure::NotFound => "Die Serverdatei oder der Ordner existiert nicht mehr. Aktualisiere die Liste.",
                Failure::Exists => "Die Zieldatei existiert bereits. Benenne sie um oder wähle einen anderen Ordner.",
                Failure::Invalid => "Die Datei oder ihr Name kann nicht übertragen werden. Prüfe die Datei und versuche es erneut.",
                Failure::Io => "Der Server konnte die Datei nicht lesen oder speichern. Prüfe den freien Speicher und versuche es erneut.",
                Failure::Busy => "Auf dem Server läuft noch eine Übertragung. Versuche es erneut.",
                Failure::TooLarge => "Der Ordner enthält zu viele Einträge. Verwende einen kleineren Unterordner.",
            }.into()));
        }
        Ok(response)
    }

    fn ack(&self, request: Request) -> Result<()> {
        match self.request(request)? {
            Response::Ok => Ok(()),
            _ => Err(TransferError::Channel(rfb::Error::Protocol(
                "unexpected file transfer response".into(),
            ))),
        }
    }

    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err(TransferError::Failed(
                "Übertragung abgebrochen. Bereits vollständig kopierte Dateien bleiben erhalten.".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// `position` is the file's number among `count` files, from 1.
    fn progress(&self, name: &str, done: u64, total: u64, (index, count): (usize, usize)) {
        *self.progress.lock().unwrap() = Progress {
            name: name.into(),
            done,
            total,
            index,
            count,
        };
        self.ctx.request_repaint_of(self.viewport);
    }

    fn list(&self, path: String) -> Result<()> {
        self.check_cancel()?;
        match self.request(Request::List(path.clone()))? {
            Response::Directory {
                path: returned,
                entries,
            } if returned == path && entries.iter().all(|e| valid_name(&e.name)) => {
                self.check_cancel()?;
                self.emit(Event::Directory { path, entries });
                Ok(())
            }
            _ => Err(TransferError::Channel(rfb::Error::Protocol(
                "invalid directory response".into(),
            ))),
        }
    }

    fn upload(&self, paths: Vec<PathBuf>, directory: &str) -> Result<()> {
        let total = paths.len();
        let mut names = Vec::with_capacity(total);
        for (index, path) in paths.into_iter().enumerate() {
            match self.upload_file(&path, directory, (index + 1, total)) {
                Ok(name) => names.push(name),
                Err(TransferError::Failed(message)) => {
                    return Err(TransferError::Failed(format!(
                        "{index} von {total} Dateien kopiert. {message}"
                    )));
                }
                Err(error) => return Err(error),
            }
        }
        self.emit(Event::Uploaded {
            directory: directory.to_owned(),
            names,
        });
        Ok(())
    }

    /// Copies one file to the server and returns its name there.
    fn upload_file(&self, path: &Path, directory: &str, position: (usize, usize)) -> Result<String> {
        self.check_cancel()?;
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| valid_name(n))
        else {
            return Err(TransferError::Failed(
                "Dieser Dateiname kann nicht übertragen werden. Benenne die Datei um und versuche es erneut."
                    .into(),
            ));
        };
        // Inspect before open: opening a FIFO could otherwise block forever.
        if !path.symlink_metadata()?.is_file() {
            return Err(TransferError::Failed(format!(
                "„{name}“ ist keine reguläre Datei. Packe Ordner vorher als ZIP-Datei."
            )));
        }
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        self.progress(name, 0, size, position);
        self.ack(Request::WriteStart {
            path: join(directory, name),
            size,
        })?;
        let mut done = 0;
        let mut buffer = vec![0; CHUNK_SIZE];
        while done < size {
            self.check_cancel()?;
            let n = ((size - done).min(CHUNK_SIZE as u64)) as usize;
            file.read_exact(&mut buffer[..n])?;
            self.ack(Request::WriteChunk(buffer[..n].to_vec()))?;
            done += n as u64;
            self.progress(name, done, size, position);
        }
        self.check_cancel()?;
        self.ack(Request::WriteFinish)?;
        Ok(name.to_owned())
    }

    fn download(&self, path: String, destination: PathBuf) -> Result<()> {
        self.check_cancel()?;
        let size = match self.request(Request::ReadStart(path.clone()))? {
            Response::Started { size } => size,
            _ => {
                return Err(TransferError::Channel(rfb::Error::Protocol(
                    "missing file size".into(),
                )))
            }
        };
        let mut incoming = Incoming::create(destination.clone(), size)?;
        let name = path.rsplit('/').next().unwrap_or(&path);
        self.progress(name, 0, size, (1, 1));
        let mut done = 0;
        while done < size {
            self.check_cancel()?;
            let Response::Data(data) = self.request(Request::ReadChunk)? else {
                return Err(TransferError::Channel(rfb::Error::Protocol(
                    "missing file data".into(),
                )));
            };
            if data.is_empty() || data.len() as u64 > size - done {
                return Err(TransferError::Channel(rfb::Error::Protocol(
                    "invalid file data length".into(),
                )));
            }
            incoming.write(&data)?;
            done += data.len() as u64;
            self.progress(name, done, size, (1, 1));
        }
        self.check_cancel()?;
        self.ack(Request::Cancel)?;
        incoming.finish()?;
        self.emit(Event::Saved(destination));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Connection;
    use rfb::{
        encoding, handshake,
        messages::{self, ClientMessage, Rect},
        PixelFormat,
    };
    use std::io::{BufReader, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    // Real RFB handshake, viewer reader/writer, and server file service, without
    // capturing or injecting input into the machine running the tests.
    fn peer(root: PathBuf, supports_files: bool) -> (Connection, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_nodelay(true).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            handshake::server_handshake(
                &mut socket,
                Some("test"),
                &handshake::ServerInit {
                    width: 1,
                    height: 1,
                    pixel_format: PixelFormat::BGRX,
                    name: "file-test".into(),
                },
            )
            .unwrap();
            let mut input = BufReader::new(socket.try_clone().unwrap());
            let mut files = beach_server::files::Session::new(root).unwrap();
            let mut negotiated = false;
            let mut frame_sent = false;
            while let Ok(message) = ClientMessage::read(&mut input) {
                match message {
                    ClientMessage::SetEncodings(encodings)
                        if supports_files && encodings.contains(&encoding::BEACH_FILE_TRANSFER) =>
                    {
                        negotiated = true;
                        files.hello().write(&mut socket).unwrap();
                    }
                    ClientMessage::FileTransfer(request) => {
                        assert!(negotiated, "file request before capability confirmation");
                        files.handle(request).write(&mut socket).unwrap();
                    }
                    ClientMessage::FramebufferUpdateRequest { .. } if !frame_sent => {
                        messages::write_framebuffer_update_header(&mut socket, 1).unwrap();
                        Rect {
                            x: 0,
                            y: 0,
                            width: 1,
                            height: 1,
                            encoding: encoding::RAW,
                        }
                        .write(&mut socket)
                        .unwrap();
                        socket.write_all(&[13, 27, 41, 0]).unwrap();
                        frame_sent = true;
                    }
                    _ => {}
                }
            }
        });
        let connection = Connection::from_stream(
            std::net::TcpStream::connect(address).unwrap(),
            Some("test"),
            None,
            PixelFormat::BGRX,
            || {},
        )
        .unwrap();
        (connection, task)
    }

    fn pump(panel: &mut Panel, ctx: &egui::Context, done: impl Fn(&Panel) -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            ctx.run_ui(egui::RawInput::default(), |_| panel.tick(ctx, false))
                .textures_delta
                .clear();
            if done(panel) {
                return;
            }
            assert!(
                Instant::now() < until,
                "file operation timed out: {:?}",
                panel.error
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[derive(Debug)]
    struct DropFile(PathBuf);
    impl egui::DroppedFile for DropFile {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn bytes(&self) -> std::result::Result<Vec<u8>, String> {
            panic!("must stream the dropped path")
        }
    }

    /// One frame of the open panel at egui time `time`. Returns where the
    /// text `find` was painted.
    fn frame(
        panel: &mut Panel,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        find: &str,
    ) -> Option<Pos2> {
        fn search(shape: &egui::epaint::Shape, find: &str) -> Option<Pos2> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == find => Some(text.pos),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| search(s, find)),
                _ => None,
            }
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |_| {
            panel.tick(ctx, false);
            panel.show(ctx, "Test", false);
        });
        output.textures_delta.clear();
        output.shapes.iter().find_map(|s| search(&s.shape, find))
    }

    fn click(panel: &mut Panel, ctx: &egui::Context, time: f64, pos: Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(panel, ctx, time, vec![egui::Event::PointerMoved(pos)], "");
        frame(panel, ctx, time + 0.01, vec![button(true)], "");
        frame(panel, ctx, time + 0.02, vec![button(false)], "");
    }

    #[test]
    fn rows_open_folders_and_download_but_not_on_a_double_click() {
        let remote = tempfile::tempdir().unwrap();
        let downloads = tempfile::tempdir().unwrap();
        std::fs::create_dir(remote.path().join("Ordner")).unwrap();
        std::fs::write(remote.path().join("Ordner/innen.txt"), b"innen").unwrap();
        let (mut connection, task) = peer(remote.path().into(), true);
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut panel = Panel::new(
            connection.files.take().unwrap(),
            ctx.clone(),
            egui::ViewportId::ROOT,
        );
        panel.open = true;
        panel.local = Some(downloads.path().into());
        // Poll at one egui time until `done`, so no click cool-down runs out.
        let wait = |panel: &mut Panel, time: f64, done: &dyn Fn(&Panel) -> bool| {
            let until = Instant::now() + Duration::from_secs(5);
            while !done(panel) {
                assert!(Instant::now() < until, "timed out: {:?}", panel.error);
                std::thread::sleep(Duration::from_millis(5));
                frame(panel, &ctx, time, vec![], "");
            }
        };
        wait(&mut panel, 1.0, &|p| p.loaded && !p.busy);
        let folder = frame(&mut panel, &ctx, 2.0, vec![], "Ordner").expect("folder row");
        let spot = folder + Vec2::new(30.0, 6.0);
        click(&mut panel, &ctx, 2.0, spot);
        wait(&mut panel, 2.03, &|p| p.directory == "Ordner" && !p.busy);
        let file = frame(&mut panel, &ctx, 2.04, vec![], "innen.txt").expect("file row");
        assert_eq!(file, folder, "the file took the folder's place");
        // The second half of a double click hits the file and must do nothing.
        click(&mut panel, &ctx, 2.1, spot);
        assert!(!panel.busy);
        // A deliberate click later downloads it.
        click(&mut panel, &ctx, 3.0, spot);
        wait(&mut panel, 3.03, &|p| !p.busy);
        assert!(panel.error.is_none(), "{:?}", panel.error);
        assert!(matches!(&panel.done, Some(Done::Saved(path)) if path.ends_with("innen.txt")));
        assert_eq!(
            std::fs::read(downloads.path().join("innen.txt")).unwrap(),
            b"innen"
        );
        // The same file again gets a new name instead of an error.
        click(&mut panel, &ctx, 4.0, spot);
        wait(&mut panel, 4.03, &|p| !p.busy);
        assert!(panel.error.is_none(), "{:?}", panel.error);
        assert_eq!(
            std::fs::read(downloads.path().join("innen (2).txt")).unwrap(),
            b"innen"
        );
        drop(panel);
        drop(connection);
        task.join().unwrap();
    }

    #[test]
    fn downloads_get_a_free_name() {
        let folder = tempfile::tempdir().unwrap();
        let path = |name: &str| folder.path().join(name);
        assert_eq!(free_destination(folder.path(), "a.pdf"), path("a.pdf"));
        std::fs::write(path("a.pdf"), b"1").unwrap();
        assert_eq!(free_destination(folder.path(), "a.pdf"), path("a (2).pdf"));
        std::fs::write(path("a (2).pdf"), b"2").unwrap();
        assert_eq!(free_destination(folder.path(), "a.pdf"), path("a (3).pdf"));
        std::fs::write(path("Makefile"), b"3").unwrap();
        assert_eq!(free_destination(folder.path(), "Makefile"), path("Makefile (2)"));
        std::fs::write(path(".profile"), b"4").unwrap();
        assert_eq!(free_destination(folder.path(), ".profile"), path(".profile (2)"));
    }

    #[test]
    fn paths_and_sizes_read_well() {
        let short = |limit: usize| move |text: &str| text.chars().count() <= limit;
        let windows = r"C:\Users\sebas\Downloads";
        assert_eq!(shorten_path(windows, short(40)), windows);
        assert_eq!(shorten_path(windows, short(18)), r"…\sebas\Downloads");
        assert_eq!(shorten_path(windows, short(12)), r"…\Downloads");
        assert_eq!(shorten_path(windows, short(3)), "Downloads");
        assert_eq!(shorten_path("/Users/samuel/Downloads", short(12)), "…/Downloads");
        assert_eq!(home_name("/Users/samuel"), "samuel");
        assert_eq!(home_name(r"C:\Users\sebas"), "sebas");
        assert_eq!(home_name("/"), "Benutzerordner");
        assert_eq!(separator(r"C:\Users\sebas"), '\\');
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1640), "1,6 KB");
        assert_eq!(bytes(2_830_000), "2,7 MB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5,0 GB");
    }

    #[test]
    fn native_drop_upload_and_download_over_rfb() {
        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        let downloads = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..CHUNK_SIZE * 3 + 29).map(|n| (n % 251) as u8).collect();
        let source = local.path().join("Grüße.bin");
        std::fs::write(&source, &data).unwrap();
        let (mut connection, task) = peer(remote.path().into(), true);
        let ctx = egui::Context::default();
        let mut panel = Panel::new(
            connection.files.take().unwrap(),
            ctx.clone(),
            egui::ViewportId::ROOT,
        );
        pump(&mut panel, &ctx, |p| matches!(p.support, Support::Ready));
        let input = egui::RawInput {
            dropped_files: vec![Arc::new(DropFile(source.clone()))],
            ..Default::default()
        };
        ctx.run_ui(input, |_| panel.tick(&ctx, false))
            .textures_delta
            .clear();
        assert!(panel.open);
        pump(&mut panel, &ctx, |p| !p.busy && p.loaded);
        assert!(panel.error.is_none(), "{:?}", panel.error);
        assert_eq!(std::fs::read(remote.path().join("Grüße.bin")).unwrap(), data);
        assert_eq!(std::fs::read(&source).unwrap(), data, "source must remain intact");
        assert!(panel.entries.iter().any(|e| e.name == "Grüße.bin"));
        let destination = downloads.path().join("Grüße.bin");
        panel.start(Command::Download {
            path: "Grüße.bin".into(),
            destination: destination.clone(),
        });
        pump(&mut panel, &ctx, |p| !p.busy && p.loaded);
        assert!(panel.error.is_none(), "{:?}", panel.error);
        assert_eq!(std::fs::read(&destination).unwrap(), data);
        // Existing local destinations must remain unchanged.
        std::fs::write(&destination, b"keep").unwrap();
        panel.start(Command::Download {
            path: "Grüße.bin".into(),
            destination: destination.clone(),
        });
        pump(&mut panel, &ctx, |p| !p.busy);
        assert!(panel.error.as_deref().unwrap().contains("existiert"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep");
        assert_eq!(connection.fb.lock().unwrap().rgba, vec![41, 27, 13, 255]);
        drop(panel);
        drop(connection);
        task.join().unwrap();
    }

    #[test]
    fn unsupported_server_and_view_only_never_receive_file_requests() {
        let remote = tempfile::tempdir().unwrap();
        let source = remote.path().join("local.txt");
        std::fs::write(&source, b"local").unwrap();
        for supports in [false, true] {
            let (mut connection, task) = peer(remote.path().into(), supports);
            let ctx = egui::Context::default();
            let mut panel = Panel::new(
                connection.files.take().unwrap(),
                ctx.clone(),
                egui::ViewportId::ROOT,
            );
            if supports {
                pump(&mut panel, &ctx, |p| matches!(p.support, Support::Ready));
            }
            let input = egui::RawInput {
                dropped_files: vec![Arc::new(DropFile(source.clone()))],
                ..Default::default()
            };
            ctx.run_ui(input, |_| panel.tick(&ctx, supports))
                .textures_delta
                .clear();
            assert!(!panel.busy);
            assert!(panel.error.is_some());
            assert_eq!(std::fs::read_dir(remote.path()).unwrap().count(), 1);
            drop(panel);
            drop(connection);
            task.join().unwrap();
        }
    }

    #[test]
    fn cancelled_upload_removes_partial_server_file() {
        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("large.bin");
        let file = File::create(&source).unwrap();
        file.set_len(128 * 1024 * 1024).unwrap();
        let (mut connection, task) = peer(remote.path().into(), true);
        let ctx = egui::Context::default();
        let mut panel = Panel::new(
            connection.files.take().unwrap(),
            ctx.clone(),
            egui::ViewportId::ROOT,
        );
        pump(&mut panel, &ctx, |p| matches!(p.support, Support::Ready));
        panel.start(Command::Upload {
            paths: vec![source],
            directory: String::new(),
        });
        pump(&mut panel, &ctx, |p| p.progress.lock().unwrap().done > 0);
        panel.cancel.store(true, Ordering::Release);
        pump(&mut panel, &ctx, |p| !p.busy);
        assert!(panel.error.as_deref().unwrap().contains("abgebrochen"));
        assert_eq!(std::fs::read_dir(remote.path()).unwrap().count(), 0);
        drop(panel);
        drop(connection);
        task.join().unwrap();
    }
}
