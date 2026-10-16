//! Loopback control channel between the viewer and the background process
//! (the one with the tray icon and the server).
//!
//! The background process listens on 127.0.0.1 at a random port and writes
//! the port to `control.port` in the config directory. A request is one
//! line (the command), the reply is a TOML [`Status`]; then the connection
//! closes. The Windows service's agent answers the same way on
//! `service.port` (see [`crate::service`]).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, thread};

use serde::{Deserialize, Serialize};

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
    let port: u16 = fs::read_to_string(endpoint.port_file()?).ok()?.trim().parse().ok()?;
    call_port(port, cmd)
}

fn call_port(port: u16, cmd: Command) -> Option<Status> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    writeln!(stream, "{}", cmd.as_str()).ok()?;
    let mut text = String::new();
    stream.read_to_string(&mut text).ok()?;
    toml::from_str(&text).ok()
}

/// Starts answering commands on a loopback port and records that port.
/// Returns the port.
pub fn serve<F>(endpoint: Endpoint, handler: F) -> io::Result<u16>
where
    F: Fn(Command) -> Status + Send + 'static,
{
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    if let Some(path) = endpoint.port_file() {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, port.to_string())?;
    }
    thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Err(e) = answer(stream, &handler) {
                log::debug!("control request failed: {e}");
            }
        }
    })?;
    Ok(port)
}

fn answer(stream: TcpStream, handler: &impl Fn(Command) -> Status) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let Some(cmd) = Command::parse(&line) else {
        return Err(io::Error::other(format!("unknown command {line:?}")));
    };
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
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer(stream, &|cmd| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Status {
                        running: cmd == Command::Start,
                        viewers: vec!["10.0.0.1:1234".into()],
                        error: Some("x".into()),
                        ..Default::default()
                    }
                })
                .unwrap();
            }
        });
        let s = call_port(port, Command::Start).unwrap();
        assert!(s.running);
        assert_eq!(s.viewers, ["10.0.0.1:1234"]);
        assert_eq!(s.error.as_deref(), Some("x"));
        assert!(!call_port(port, Command::Status).unwrap().running);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(call_port(1, Command::Status).is_none());
    }
}
