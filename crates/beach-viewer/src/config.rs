//! `config.toml`: default connection settings and the built-in server's
//! configuration. Lives next to `servers.toml`.
//!
//! ```toml
//! [viewer]
//! colors = "full"
//!
//! [server]
//! enabled = true
//! port = 5900
//! password = "dbd83cfd727a1458"   # vncpasswd format, hex
//! ```

use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};

use crate::servers::{self, config_dir};
use crate::settings::Settings;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub viewer: Settings,
    pub server: ServerConfig,
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
    /// Display index as listed by `beach-server --list-displays`; `None` is
    /// the primary one.
    pub display: Option<usize>,
    pub fps: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            enabled: true,
            autostart: true,
            port: 5900,
            password: String::new(),
            view_only: false,
            display: None,
            fps: 60,
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
    fs::write(&tmp, text)?;
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
        config.server.port = 5901;
        save_to(&config, &path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("geheim"), "{text}");
        assert_eq!(load_from(Some(&path)), config);
        fs::remove_dir_all(&base).unwrap();
    }
}
