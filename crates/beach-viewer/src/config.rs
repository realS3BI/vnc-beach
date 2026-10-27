//! `config.toml`: default connection settings and the built-in server's
//! configuration. Lives next to `servers.toml`.
//!
//! ```toml
//! [viewer]
//! colors = "full"
//!
//! [server]
//! enabled = true
//! port = 5901
//! password = "dbd83cfd727a1458"   # vncpasswd format, hex
//! ```

use std::path::{Path, PathBuf};
use std::{fs, io};

use beach_server::Display;
use serde::{Deserialize, Serialize};

use crate::servers::{self, config_dir};
use crate::account::{AccountConfig, DeviceLink};
use crate::settings::Settings;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub viewer: Settings,
    pub server: ServerConfig,
    /// The account signed in to in the viewer.
    pub account: AccountConfig,
}

/// The built-in server (Windows). The password is obfuscated on disk like
/// the favorites' passwords and plain in memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Start the server with the background process.
    pub enabled: bool,
    /// Start the background process at login.
    pub autostart: bool,
    pub port: u16,
    #[serde(with = "servers::obfuscated")]
    pub password: String,
    pub view_only: bool,
    /// Also reachable before sign-in: the Windows service, or the macOS
    /// login window agent. Set up on its own at start (see
    /// [`crate::service::ensure`]).
    pub service: bool,
    /// Version for which the administrator prompt was declined; not asked
    /// again until the next version.
    pub service_declined: String,
    /// `"all"` (default), `"primary"` or an index as listed by
    /// `beach-server --list-displays`.
    #[serde(with = "display_toml")]
    pub display: Display,
    pub fps: u32,
    /// Registered with a coordination server: accept its tickets and
    /// report addresses there.
    pub device: Option<DeviceLink>,
}

/// `display = "all"`, `display = "primary"` or `display = 1`.
mod display_toml {
    use beach_server::Display;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Index(usize),
        Name(String),
    }

    pub fn serialize<S: Serializer>(d: &Display, s: S) -> Result<S::Ok, S::Error> {
        match *d {
            Display::Index(i) => Repr::Index(i),
            other => Repr::Name(other.to_string()),
        }
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Display, D::Error> {
        Ok(match Repr::deserialize(d)? {
            Repr::Index(i) => Display::Index(i),
            Repr::Name(name) => name.parse().map_err(serde::de::Error::custom)?,
        })
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            enabled: true,
            autostart: true,
            // 5900 is the VNC standard, but macOS Screen Sharing usually
            // holds it already.
            port: 5901,
            password: String::new(),
            view_only: false,
            service: true,
            service_declined: String::new(),
            display: Display::All,
            fps: 60,
            device: None,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Reads the config; a missing or broken file gives the defaults.
pub fn load() -> Config {
    load_from(path().as_deref())
}

fn load_from(path: Option<&Path>) -> Config {
    let Some(path) = path else {
        return Config::default();
    };
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            log::warn!("ignoring {}: {e}", path.display());
            Config::default()
        }),
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                log::warn!("cannot read {}: {e}", path.display());
            }
            Config::default()
        }
    }
}

pub fn save(config: &Config) -> io::Result<()> {
    let Some(path) = path() else {
        return Err(io::Error::other("no config directory (HOME/APPDATA not set)"));
    };
    save_to(config, &path)
}

fn save_to(config: &Config, path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut text =
        String::from("# VNC Beach configuration: default connection settings and the built-in server.\n\n");
    text.push_str(&toml::to_string(config).map_err(io::Error::other)?);
    let tmp = path.with_extension("toml.tmp");
    // Holds the account's session and device tokens: private from the start.
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    // `mode` only applies to new files; a leftover tmp file is tightened too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    drop(file);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Colors;

    #[test]
    fn round_trips_and_tolerates_missing_file() {
        let base = std::env::temp_dir().join(format!("vnc-beach-config-{}", std::process::id()));
        let path = base.join("config.toml");
        assert_eq!(load_from(Some(&path)), Config::default());
        let mut config = Config::default();
        config.viewer.colors = Colors::Medium;
        config.server.password = "geheim".into();
        config.server.port = 5999;
        config.account = AccountConfig {
            url: "https://beach.example.com".into(),
            token: "session".into(),
            name: "Alice".into(),
            login: "alice".into(),
        };
        config.server.device = Some(DeviceLink {
            url: "https://beach.example.com".into(),
            id: "dev".into(),
            token: "device-token".into(),
            key: "key".into(),
            name: "Büro".into(),
            owner: "Persönlich".into(),
        });
        save_to(&config, &path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("geheim"), "{text}");
        assert_eq!(load_from(Some(&path)), config);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn display_choice_in_toml() {
        let mut config = Config::default();
        assert!(toml::to_string(&config).unwrap().contains("display = \"all\""));
        config.server.display = Display::Index(1);
        let text = toml::to_string(&config).unwrap();
        assert!(text.contains("display = 1"), "{text}");
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
        let parsed: Config = toml::from_str("[server]\ndisplay = \"primary\"").unwrap();
        assert_eq!(parsed.server.display, Display::Primary);
        assert!(toml::from_str::<Config>("[server]\ndisplay = \"left\"").is_err());
    }
}
