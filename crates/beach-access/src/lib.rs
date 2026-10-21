//! Optional connection preparation. Capabilities describe the remote device;
//! neither its operating system nor its VPN provider determines the workflow.

#[path = "access/ssh.rs"]
mod ssh;
#[path = "access/wake.rs"]
mod wake;

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Clone, Debug, Default)]
pub struct Target {
    pub host: String,
    pub access: Access,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Access {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh: Option<Ssh>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake: Option<Wake>,
}

impl Access {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_routes()?;
        if let Some(ssh) = &self.ssh {
            ssh.validate()?;
        }
        if let Some(wake) = &self.wake {
            wake::packet(&wake.mac)?;
            wake::destination(&wake.destination)?;
            for relay in &wake.relays {
                relay.ssh.validate()?;
            }
        }
        Ok(())
    }

    fn validate_routes(&self) -> anyhow::Result<()> {
        for address in self
            .alternatives
            .iter()
            .filter(|address| !address.trim().is_empty())
        {
            endpoint(&parse_address(address), None)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ssh {
    /// A host/IP with an optional literal SSH port, never a VNC display number.
    pub address: String,
    pub user: String,
    pub auth: Authentication,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Authentication {
    #[default]
    Password,
    Key {
        path: String,
    },
}

impl Ssh {
    fn validate(&self) -> anyhow::Result<()> {
        endpoint(&self.address, Some(22))?;
        if self.user.trim().is_empty() || self.user.contains(['\0', '\r', '\n']) {
            bail!("SSH-Benutzername fehlt oder enthält einen Zeilenumbruch.");
        }
        if matches!(&self.auth, Authentication::Key { path } if path.trim().is_empty()) {
            bail!("Bitte eine Datei für den privaten SSH-Schlüssel auswählen.");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wake {
    pub mac: String,
    pub destination: String,
    /// Empty means sending from the viewer's own LAN. Each relay sends from
    /// its LAN instead, allowing Wake-on-LAN over a routed VPN.
    pub relays: Vec<Relay>,
}

impl Default for Wake {
    fn default() -> Self {
        Self {
            mac: String::new(),
            destination: "255.255.255.255:9".into(),
            relays: vec![],
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Relay {
    pub ssh: Ssh,
    pub shell: Shell,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shell {
    #[default]
    Posix,
    PowerShell,
}

#[derive(Clone, Copy)]
pub enum Action {
    Connect,
    SignIn,
    Wake,
}

pub enum Prompt {
    Trust {
        address: String,
        fingerprint: String,
    },
    Secret {
        title: String,
        label: String,
        echo: bool,
    },
}

pub enum Answer {
    Trust,
    Secret(Zeroizing<String>),
}

pub enum Event {
    Progress(String),
    Prompt(Prompt, Sender<Answer>),
}

/// Dropping a task cancels outstanding prompts and interrupts its socket.
pub struct Task<T: Send + 'static> {
    pub events: Receiver<Event>,
    pub result: Receiver<Result<T, String>>,
    cancel: Arc<Cancel>,
}

impl<T: Send + 'static> Task<T> {
    pub fn start(
        target: Target,
        action: Action,
        known_hosts: Option<std::path::PathBuf>,
        finish: impl FnOnce(TcpStream) -> anyhow::Result<T> + Send + 'static,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, events) = channel();
        let (completed, result) = channel();
        let cancel = Arc::new(Cancel::default());
        let worker = Worker {
            tx,
            cancel: cancel.clone(),
            repaint: Arc::new(repaint),
            known_hosts,
        };
        std::thread::spawn(move || {
            let result = worker.run(&target, action).and_then(|stream| {
                worker.cancel.check()?;
                worker.progress("VNC-Anmeldung prüfen …");
                finish(stream)
            });
            // The checked socket now belongs to the authenticated session.
            worker.cancel.release_socket();
            if !worker.cancel.stopped.load(Ordering::Acquire) {
                let _ = completed.send(result.map_err(|e| format!("{e:#}")));
                (worker.repaint)();
            }
        });
        Self {
            events,
            result,
            cancel,
        }
    }

    pub fn cancel(&self) {
        self.cancel.stopped.store(true, Ordering::Release);
        if let Some(socket) = self.cancel.socket.lock().unwrap().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

impl<T: Send + 'static> Drop for Task<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Default)]
struct Cancel {
    stopped: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

impl Cancel {
    fn check(&self) -> anyhow::Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            bail!("Verbindungsversuch abgebrochen.");
        }
        Ok(())
    }

    fn watch(&self, stream: &TcpStream) -> anyhow::Result<()> {
        let mut socket = self.socket.lock().unwrap();
        self.check()?;
        *socket = Some(stream.try_clone()?);
        Ok(())
    }

    fn release_socket(&self) {
        self.socket.lock().unwrap().take();
    }

    fn pause(&self, duration: Duration) -> anyhow::Result<()> {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.check()?;
            std::thread::sleep(
                Duration::from_millis(25).min(until.saturating_duration_since(Instant::now())),
            );
        }
        self.check()
    }
}

struct Worker {
    tx: Sender<Event>,
    cancel: Arc<Cancel>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    known_hosts: Option<std::path::PathBuf>,
}

impl Worker {
    fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
        (self.repaint)();
    }

    fn progress(&self, text: impl Into<String>) {
        self.emit(Event::Progress(text.into()));
    }

    fn ask(&self, prompt: Prompt) -> anyhow::Result<Answer> {
        self.cancel.check()?;
        let (tx, rx) = channel();
        self.emit(Event::Prompt(prompt, tx));
        loop {
            self.cancel.check()?;
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(answer) => {
                    self.cancel.check()?;
                    return Ok(answer);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("Anmeldung abgebrochen."),
            }
        }
    }

    fn run(&self, server: &Target, action: Action) -> anyhow::Result<TcpStream> {
        server.access.validate_routes()?;
        let wait = match action {
            Action::Connect => false,
            Action::SignIn => {
                let config = server
                    .access
                    .ssh
                    .as_ref()
                    .context("Für dieses Gerät ist keine SSH-Anmeldung eingerichtet.")?;
                self.progress(format!("SSH-Verbindung zu {} prüfen …", config.address));
                let session = ssh::login(self, config, true)?;
                if let Some(session) = session {
                    let _ = session.disconnect(None, "Connection preparation complete", None);
                    self.progress("SSH-Anmeldung angenommen. Warte auf den VNC-Server …");
                } else {
                    // FileVault can terminate SSH while the data volume mounts.
                    // A disconnect alone is not evidence of a successful unlock.
                    self.progress("SSH-Verbindung beendet. Prüfe, ob VNC erreichbar wird …");
                }
                true
            }
            Action::Wake => {
                let config = server
                    .access
                    .wake
                    .as_ref()
                    .context("Für dieses Gerät ist kein Wake-on-LAN eingerichtet.")?;
                wake::send(self, config)?;
                self.progress("Aufwecksignal gesendet. Warte auf den VNC-Server …");
                true
            }
        };
        let stream = self.wait_for_vnc(
            server,
            if wait {
                Duration::from_secs(90)
            } else {
                Duration::ZERO
            },
        )?;
        self.cancel.check()?;
        Ok(stream)
    }

    fn wait_for_vnc(&self, server: &Target, wait: Duration) -> anyhow::Result<TcpStream> {
        let until = Instant::now() + wait;
        let addresses = addresses(server);
        loop {
            let mut errors = Vec::new();
            for address in &addresses {
                self.cancel.check()?;
                self.progress(format!("VNC über {address} prüfen …"));
                let budget = if wait.is_zero() {
                    Duration::from_secs(3)
                } else {
                    until
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(3))
                };
                if budget.is_zero() {
                    break;
                }
                match self.vnc_socket(address, budget) {
                    Ok(stream) => return Ok(stream),
                    Err(error) => errors.push(format!("{address}: {error:#}")),
                }
            }
            if Instant::now() >= until {
                bail!("VNC ist über keine der eingerichteten Adressen erreichbar. Bitte Netzwerk, Zugangsgerät und VNC-Server prüfen.\n{}", errors.join("\n"));
            }
            self.progress(format!(
                "Warte auf VNC … noch etwa {} Sekunden. Bei Bedarf abbrechen und per SSH anmelden.",
                until.saturating_duration_since(Instant::now()).as_secs()
            ));
            self.cancel
                .pause(Duration::from_secs(2).min(until.saturating_duration_since(Instant::now())))?;
        }
    }

    fn vnc_socket(&self, address: &str, budget: Duration) -> anyhow::Result<TcpStream> {
        let until = Instant::now() + budget;
        let address = parse_address(address);
        endpoint(&address, None)?;
        let stream = self.tcp_until(&address, until)?;
        loop {
            self.cancel.check()?;
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("Keine vollständige VNC-Antwort erhalten.");
            }
            stream.set_read_timeout(Some(Duration::from_millis(250).min(remaining)))?;
            let mut banner = [0; 12];
            match stream.peek(&mut banner) {
                Ok(12) if rfb::handshake::is_server_banner(&banner) => {
                    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
                    return Ok(stream);
                }
                Ok(0) => bail!("Der Server hat die Verbindung geschlossen."),
                Ok(12) => bail!("Dieser Port meldet keinen VNC-Server."),
                Ok(_) => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.into()),
            }
            if Instant::now() >= until {
                bail!("Keine vollständige VNC-Antwort erhalten.");
            }
            self.cancel.pause(Duration::from_millis(25))?;
        }
    }

    fn tcp(&self, address: &str) -> anyhow::Result<TcpStream> {
        self.tcp_until(address, Instant::now() + Duration::from_secs(3))
    }

    fn tcp_until(&self, address: &str, until: Instant) -> anyhow::Result<TcpStream> {
        let resolved = address
            .to_socket_addrs()
            .with_context(|| format!("Adresse {address} konnte nicht aufgelöst werden"))?;
        let mut last = None;
        for addr in resolved {
            self.cancel.check()?;
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "connection timed out").into());
            }
            match TcpStream::connect_timeout(&addr, remaining) {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
                    self.cancel.watch(&stream)?;
                    return Ok(stream);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last
            .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no addresses found"))
            .into())
    }
}

fn addresses(server: &Target) -> Vec<String> {
    let mut addresses = vec![];
    for address in std::iter::once(&server.host).chain(&server.access.alternatives) {
        let address = address.trim();
        if !address.is_empty()
            && !addresses
                .iter()
                .any(|other: &String| parse_address(other) == parse_address(address))
        {
            addresses.push(address.to_owned());
        }
    }
    addresses
}

/// Parses host names and bracketed IPv6 without resolving them. SSH ports are
/// literal; VNC display notation is normalized by `parse_address` beforehand.
fn endpoint(input: &str, default_port: Option<u16>) -> anyhow::Result<(String, u16)> {
    let input = input.trim();
    if input.is_empty() || input.chars().any(char::is_whitespace) || input.contains(['/', '\\', '@', '\0']) {
        bail!("Bitte einen Rechnernamen oder eine IP-Adresse eingeben, optional mit :Port.");
    }
    let (host, port) = if let Some(rest) = input.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .context("IPv6-Adresse mit [ und ] einschließen.")?;
        host.parse::<std::net::Ipv6Addr>()
            .context("Ungültige IPv6-Adresse.")?;
        let port = if tail.is_empty() {
            default_port.context("Port fehlt.")?
        } else {
            tail.strip_prefix(':')
                .context("Port nach ] mit : trennen.")?
                .parse::<u16>()
                .context("Ungültiger Port.")?
        };
        (host.to_owned(), port)
    } else if let Ok(ip) = input.parse::<std::net::Ipv6Addr>() {
        (
            ip.to_string(),
            default_port.context("IPv6-Adresse mit Port in [ und ] einschließen.")?,
        )
    } else if let Some((host, port)) = input.rsplit_once(':') {
        (
            host.to_owned(),
            port.parse::<u16>()
                .context("Port muss zwischen 1 und 65535 liegen.")?,
        )
    } else {
        (input.to_owned(), default_port.context("Port fehlt.")?)
    };
    if host.is_empty()
        || port == 0
        || host.contains(['[', ']', ':']) && host.parse::<std::net::Ipv6Addr>().is_err()
    {
        bail!("Ungültige Adresse oder Port 0. Bitte Adresse und Port prüfen.");
    }
    Ok((host, port))
}

fn socket_address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
#[path = "access/tests.rs"]
mod tests;

/// Accepts `host`, `host:display` (display < 100 → port 5900+display),
/// `host:port`, `host::port` and `[ipv6]:port`.
pub fn parse_address(input: &str) -> String {
    let input = input.trim();
    // A bare IPv6 address takes precedence over the host::port notation.
    if let Ok(ip) = input.parse::<std::net::Ipv6Addr>() {
        return format!("[{ip}]:5900");
    }
    if let Some((host, port)) = input.rsplit_once("::") {
        if !host.is_empty() && port.parse::<u16>().is_ok() {
            return format!("{host}:{port}");
        }
    }
    let (host, n) = match input.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(':') && (h.starts_with('[') || !h.contains(':')) => {
            match p.parse::<u16>() {
                Ok(n) => (h, Some(n)),
                Err(_) => (input, None),
            }
        }
        _ => (input, None),
    };
    let port = match n {
        Some(n) if n < 100 => 5900 + n,
        Some(n) => n,
        None => 5900,
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Keeps VNC authentication errors distinct from SSH preparation failures.
pub fn vnc_error(error: rfb::Error) -> anyhow::Error {
    match error {
        rfb::Error::PasswordRequired => {
            anyhow::anyhow!("Der VNC-Server verlangt ein Passwort. Bitte im Verbindungsformular ergänzen.")
        }
        rfb::Error::AuthFailed(reason) => anyhow::anyhow!("VNC-Anmeldung fehlgeschlagen: {reason}"),
        error => anyhow::anyhow!("VNC-Verbindung fehlgeschlagen: {error}"),
    }
}
