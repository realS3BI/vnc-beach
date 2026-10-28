//! File browser, native file drops, and a sequential transfer worker.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use crate::client::FileChannel;
use beach_server::files::Incoming;
use rfb::files::{valid_name, valid_path, Entry, Failure, Request, Response, CHUNK_SIZE};

enum Command {
    List(String),
    Upload { paths: Vec<PathBuf>, directory: String },
    Download { path: String, destination: PathBuf },
}

enum Event {
    Ready { root: String, initial: String },
    Directory { path: String, entries: Vec<Entry> },
    Finished(String),
    Failed(String),
    Unavailable(String),
}

#[derive(Default, Clone)]
struct Progress {
    name: String,
    done: u64,
    total: u64,
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
    path_input: String,
    entries: Vec<Entry>,
    loaded: bool,
    busy: bool,
    listing: bool,
    message: Option<String>,
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
            path_input: String::new(),
            entries: Vec::new(),
            loaded: false,
            busy: false,
            listing: false,
            message: None,
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
        } else if self.message.is_some() && !self.open {
            "Dateien ✓"
        } else {
            "Dateien"
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context, view_only: bool) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Ready { root, initial } => {
                    self.support = Support::Ready;
                    self.root = root;
                    self.path_input = initial.clone();
                    self.directory = initial;
                }
                Event::Directory { path, entries } => {
                    self.path_input = path.clone();
                    self.directory = path;
                    self.entries = entries;
                    self.loaded = true;
                    self.busy = false;
                    self.listing = false;
                }
                Event::Finished(message) => {
                    self.busy = false;
                    self.message = Some(message);
                    self.loaded = false;
                }
                Event::Failed(message) => {
                    self.busy = false;
                    self.listing = false;
                    self.error = Some(message);
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
            let paths = dropped.iter().map(|f| f.path().to_path_buf()).collect();
            if !self.can_transfer(view_only) {
                self.error = Some(self.drop_hint(view_only));
            } else {
                self.start(Command::Upload {
                    paths,
                    directory: self.directory.clone(),
                });
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

    fn drop_hint(&self, view_only: bool) -> String {
        if view_only {
            return "Dateiübertragung ist bei „Nur ansehen“ deaktiviert.".into();
        }
        match &self.support {
            Support::Checking => "Dateiübertragung wird geprüft. Versuche es gleich noch einmal.".into(),
            Support::Unavailable(message) => message.clone(),
            Support::Ready if self.busy => {
                "Eine Übertragung oder Ordnerabfrage läuft bereits. Warte auf ihren Abschluss.".into()
            }
            Support::Ready => format!("Zum Server kopieren: {}", self.remote_label()),
        }
    }

    fn remote_label(&self) -> String {
        if self.directory.is_empty() {
            self.root.clone()
        } else {
            format!("{}/{}", self.root, self.directory)
        }
    }

    fn start(&mut self, command: Command) {
        self.cancel.store(false, Ordering::Release);
        self.listing = matches!(command, Command::List(_));
        if !self.listing {
            self.message = None;
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

    pub fn show(&mut self, ctx: &egui::Context, view_only: bool) {
        if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
            egui::Area::new(egui::Id::new("file-drop-hint"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).inner_margin(20.0).show(ui, |ui| {
                        ui.label(self.drop_hint(view_only));
                    });
                });
        }
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Dateien")
            .open(&mut open)
            .default_width(620.0)
            .default_height(440.0)
            .show(ctx, |ui| {
                match &self.support {
                    Support::Checking => {
                        ui.spinner();
                        ui.label("Dateiübertragung wird geprüft …");
                        return;
                    }
                    Support::Unavailable(message) => {
                        ui.label(message);
                        return;
                    }
                    Support::Ready => {}
                }
                if view_only {
                    ui.label("Dateiübertragung ist bei „Nur ansehen“ deaktiviert.");
                }
                ui.label(format!("Server: {}", self.remote_label()));
                ui.label("Dateien werden in diesen Ordner kopiert. Originale bleiben erhalten.");
                ui.add_enabled_ui(self.can_transfer(view_only), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Ordner im Benutzerverzeichnis:");
                        let edit = ui.text_edit_singleline(&mut self.path_input);
                        if ui.button("Öffnen").clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                            if valid_path(&self.path_input) {
                                self.start(Command::List(self.path_input.clone()));
                            } else {
                                self.error = Some("Gib einen relativen Ordner an, zum Beispiel Downloads oder Dokumente/Arbeit.".into());
                            }
                        }
                    });
                });
                ui.add_enabled_ui(self.can_transfer(view_only), |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!self.directory.is_empty(), egui::Button::new("Übergeordnet"))
                            .clicked()
                        {
                            let parent = self.directory.rsplit_once('/').map_or("", |(p, _)| p).to_owned();
                            self.start(Command::List(parent));
                        }
                        if ui.button("Aktualisieren").clicked() {
                            self.start(Command::List(self.directory.clone()));
                        }
                        if ui.button("Dateien auswählen …").clicked() {
                            if let Some(paths) = rfd::FileDialog::new()
                                .set_title("Dateien zum Server kopieren")
                                .pick_files()
                            {
                                self.start(Command::Upload {
                                    paths,
                                    directory: self.directory.clone(),
                                });
                            }
                        }
                    });
                });
                ui.small("Dateien ins VNC-Fenster ziehen. Ordner bitte vorher als ZIP-Datei packen.");
                ui.separator();
                if self.listing {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Ordner wird geladen …");
                    });
                }
                if self.loaded && self.entries.is_empty() {
                    ui.label("Keine übertragbaren Dateien oder Ordner vorhanden. Ziehe Dateien ins Fenster, um sie hier abzulegen.");
                }
                let enabled = self.can_transfer(view_only);
                let mut action = None;
                egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                    for entry in &self.entries {
                        ui.push_id(&entry.name, |ui| {
                            ui.horizontal(|ui| {
                                if entry.directory {
                                    if ui
                                        .add_enabled(
                                            enabled,
                                            egui::Button::new(format!("{} öffnen", entry.name)),
                                        )
                                        .clicked()
                                    {
                                        action = Some(Command::List(join(&self.directory, &entry.name)));
                                    }
                                } else {
                                    ui.label(&entry.name);
                                    ui.weak(bytes(entry.size));
                                    if ui
                                        .add_enabled(
                                            enabled && self.local.is_some(),
                                            egui::Button::new("Herunterladen"),
                                        )
                                        .clicked()
                                    {
                                        if let Some(local) = &self.local {
                                            action = Some(Command::Download {
                                                path: join(&self.directory, &entry.name),
                                                destination: local.join(&entry.name),
                                            });
                                        }
                                    }
                                }
                            });
                        });
                    }
                });
                if let Some(command) = action {
                    self.start(command);
                }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "Lokal speichern: {}",
                        self.local
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "Ordner auswählen".into())
                    ));
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Ändern …"))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Download-Ordner auswählen")
                            .pick_folder()
                        {
                            self.local = Some(path);
                        }
                    }
                });
                ui.small("Vorhandene Dateien werden nicht überschrieben.");
                if self.busy && !self.listing {
                    let p = self.progress.lock().unwrap().clone();
                    let fraction = if p.total == 0 {
                        0.0
                    } else {
                        p.done as f32 / p.total as f32
                    };
                    ui.add(egui::ProgressBar::new(fraction).text(format!(
                        "{}: {} von {}",
                        p.name,
                        bytes(p.done),
                        bytes(p.total)
                    )));
                }
                if self.busy {
                    let cancelling = self.cancel.load(Ordering::Acquire);
                    if ui
                        .add_enabled(
                            !cancelling,
                            egui::Button::new(if cancelling {
                                "Wird abgebrochen …"
                            } else {
                                "Abbrechen"
                            }),
                        )
                        .clicked()
                    {
                        self.cancel.store(true, Ordering::Release);
                    }
                }
                if let Some(message) = &self.message {
                    ui.label(message);
                }
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.button("Hinweis schließen").clicked() {
                        self.error = None;
                    }
                }
            });
        self.open = open;
    }
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
fn bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
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

    fn progress(&self, name: &str, done: u64, total: u64) {
        *self.progress.lock().unwrap() = Progress {
            name: name.into(),
            done,
            total,
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
        for (index, path) in paths.into_iter().enumerate() {
            if let Err(error) = self.upload_file(&path, directory) {
                return Err(match error {
                    TransferError::Failed(message) => {
                        TransferError::Failed(format!("{index} von {total} Dateien kopiert. {message}"))
                    }
                    error => error,
                });
            }
        }
        self.emit(Event::Finished(if total == 1 {
            "Eine Datei zum Server kopiert.".into()
        } else {
            format!("{total} Dateien zum Server kopiert.")
        }));
        Ok(())
    }

    fn upload_file(&self, path: &std::path::Path, directory: &str) -> Result<()> {
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
        self.progress(name, 0, size);
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
            self.progress(name, done, size);
        }
        self.check_cancel()?;
        self.ack(Request::WriteFinish)?;
        Ok(())
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
        self.progress(name, 0, size);
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
            self.progress(name, done, size);
        }
        self.check_cancel()?;
        self.ack(Request::Cancel)?;
        incoming.finish()?;
        self.emit(Event::Finished(format!("Gespeichert: {}", destination.display())));
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
        let mut panel = Panel::new(connection.files.take().unwrap(), ctx.clone(), egui::ViewportId::ROOT);
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
            let mut panel = Panel::new(connection.files.take().unwrap(), ctx.clone(), egui::ViewportId::ROOT);
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
        let mut panel = Panel::new(connection.files.take().unwrap(), ctx.clone(), egui::ViewportId::ROOT);
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
