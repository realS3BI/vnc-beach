//! Loopback control channel between the viewer and the background process
//! (the one with the tray icon and the server).
//!
//! The background process listens on 127.0.0.1 at a random port and writes
//! the port and a random secret to `control.port` in the config directory
//! (two lines; the file is readable by its user only). A request is two
//! lines, the command and the secret; the reply is a TOML [`Status`]; then
//! the connection closes. Every local process can reach a loopback port,
//! and the status carries the VNC password, so only processes that can
//! read the user's config directory may ask. The Windows service's agent
//! answers the same way on `service.port` (see [`crate::service`]).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, thread};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;

use crate::servers::config_dir;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub pid: u32,
    /// The server is listening.
    pub running: bool,
    /// The server is meant to run (config); false after "Anhalten".
    pub enabled: bool,
    pub port: u16,
    pub password: String,
    pub viewers: Vec<String>,
    pub error: Option<String>,
    pub autostart: bool,
    /// macOS permissions the running server lacks: "accessibility",
    /// "screen_recording".
    pub missing: Vec<String>,
    /// The Windows service runs the server, so it is also reachable on the
    /// lock screen and before anyone signs in.
    pub service: bool,
}

/// Who answers on the control channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// `vnc-beach --background`, the tray icon process.
    Tray,
    /// The Windows service's agent in the console session.
    Service,
}

impl Endpoint {
    fn port_file(self) -> Option<PathBuf> {
        let name = match self {
            Endpoint::Tray => "control.port",
            Endpoint::Service => "service.port",
        };
        config_dir().map(|d| d.join(name))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Status,
    Start,
    Stop,
    /// Re-read `config.toml` and apply it.
    Reload,
    /// End the background process.
    Quit,
}

impl Command {
    fn as_str(self) -> &'static str {
        match self {
            Command::Status => "status",
            Command::Start => "start",
            Command::Stop => "stop",
            Command::Reload => "reload",
            Command::Quit => "quit",
        }
    }

    fn parse(text: &str) -> Option<Command> {
        [
            Command::Status,
            Command::Start,
            Command::Stop,
            Command::Reload,
            Command::Quit,
        ]
        .into_iter()
        .find(|c| c.as_str() == text.trim())
    }
}

/// Sends a command to the background process, or to the Windows service if
/// no background process runs. `None` if neither answers.
pub fn call(cmd: Command) -> Option<Status> {
    call_at(Endpoint::Tray, cmd).or_else(|| call_at(Endpoint::Service, cmd))
}

pub fn call_at(endpoint: Endpoint, cmd: Command) -> Option<Status> {
    let (port, secret) = read_port_file(&endpoint.port_file()?)?;
    call_port(port, &secret, cmd)
}

/// Port and secret from a port file. A file of an older version has no
/// secret; such a process answers without one.
fn read_port_file(path: &Path) -> Option<(u16, String)> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let secret = lines.next().unwrap_or_default().trim().to_string();
    Some((port, secret))
}

fn call_port(port: u16, secret: &str, cmd: Command) -> Option<Status> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    write!(stream, "{}\n{secret}\n", cmd.as_str()).ok()?;
    let mut text = String::new();
    stream.take(1 << 20).read_to_string(&mut text).ok()?;
    // A refused request is closed without a reply; an empty document would
    // otherwise parse as a default status.
    if text.trim().is_empty() {
        return None;
    }
    toml::from_str(&text).ok()
}

/// 32 random bytes as hex: what a caller has to know.
fn new_secret() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `port` and `secret` so that only the directory's user can read
/// them. The macOS login window agent runs as root in the user's config
/// directory: the file goes to the directory's owner, as the user's tray
/// process has to read it.
fn write_port_file(path: &Path, port: u16, secret: &str) -> io::Result<()> {
    let Some(dir) = path.parent() else {
        return Err(io::Error::other("port file without a directory"));
    };
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("port.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let owner = fs::metadata(dir)?;
        if owner.uid() != unsafe { libc::geteuid() } {
            std::os::unix::fs::fchown(&file, Some(owner.uid()), Some(owner.gid()))?;
        }
    }
    write!(file, "{port}\n{secret}\n")?;
    drop(file);
    fs::rename(&tmp, path)
}

/// Starts answering commands on a loopback port and records that port.
/// Returns the port.
pub fn serve<F>(endpoint: Endpoint, handler: F) -> io::Result<u16>
where
    F: Fn(Command) -> Status + Send + 'static,
{
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let secret = new_secret();
    if let Some(path) = endpoint.port_file() {
        write_port_file(&path, port, &secret)?;
    }
    thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Err(e) = answer(stream, &secret, &handler) {
                log::debug!("control request failed: {e}");
            }
        }
    })?;
    Ok(port)
}

/// Answers one request; a wrong or missing secret closes the connection
/// without a reply.
fn answer(stream: TcpStream, secret: &str, handler: &impl Fn(Command) -> Status) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    // Two short lines; a local process must not make this buffer grow.
    let mut reader = BufReader::new(stream.try_clone()?.take(1024));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let Some(cmd) = Command::parse(&line) else {
        return Err(io::Error::other(format!("unknown command {line:?}")));
    };
    let mut given = String::new();
    reader.read_line(&mut given)?;
    if !bool::from(given.trim().as_bytes().ct_eq(secret.as_bytes())) {
        return Err(io::Error::other("wrong control secret"));
    }
    let status = handler(cmd);
    let text = toml::to_string(&status).map_err(io::Error::other)?;
    let mut stream = stream;
    stream.write_all(text.as_bytes())?;
    stream.shutdown(Shutdown::Both)
}

/// Forgets the recorded port (on exit).
pub fn forget(endpoint: Endpoint) {
    if let Some(path) = endpoint.port_file() {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn round_trips_commands_and_status() {
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let c = calls.clone();
        let secret = new_secret();
        let served = secret.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = answer(stream, &served, &|cmd| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Status {
                        running: cmd == Command::Start,
                        viewers: vec!["10.0.0.1:1234".into()],
                        error: Some("x".into()),
                        ..Default::default()
                    }
                });
            }
        });
        let s = call_port(port, &secret, Command::Start).unwrap();
        assert!(s.running);
        assert_eq!(s.viewers, ["10.0.0.1:1234"]);
        assert_eq!(s.error.as_deref(), Some("x"));
        assert!(!call_port(port, &secret, Command::Status).unwrap().running);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Without the secret nothing is answered and nothing is done.
        assert!(call_port(port, "", Command::Quit).is_none());
        assert!(call_port(port, &new_secret(), Command::Quit).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(call_port(1, &secret, Command::Status).is_none());
    }

    #[test]
    fn port_file_holds_port_and_private_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.port");
        write_port_file(&path, 4711, "s3cret").unwrap();
        assert_eq!(read_port_file(&path), Some((4711, "s3cret".into())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Files of older versions carry just the port.
        fs::write(&path, "4712\n").unwrap();
        assert_eq!(read_port_file(&path), Some((4712, String::new())));
        fs::write(&path, "nope").unwrap();
        assert_eq!(read_port_file(&path), None);
        assert_eq!(new_secret().len(), 64);
        assert_ne!(new_secret(), new_secret());
    }
}
