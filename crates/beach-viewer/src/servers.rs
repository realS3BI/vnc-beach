//! The server directory: saved connections in a TOML file in the user's
//! config directory.
//!
//! ```toml
//! [[server]]
//! name = "Mac Büro"
//! host = "192.168.1.20"
//! password = "dbd83cfd727a1458"   # vncpasswd format, hex; absent if none saved
//!
//! [server.settings]                # only the fields that differ from the defaults
//! colors = "low"
//! ```

use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};

use crate::settings::Overrides;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub host: String,
    /// Plain password in memory; obfuscated on disk.
    pub password: String,
    pub settings: Overrides,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "server")]
    servers: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    name: String,
    host: String,
    #[serde(default, with = "obfuscated", skip_serializing_if = "String::is_empty")]
    password: String,
    #[serde(default, skip_serializing_if = "Overrides::is_empty")]
    settings: Overrides,
}

impl From<&Server> for Entry {
    fn from(s: &Server) -> Self {
        Entry {
            name: s.name.clone(),
            host: s.host.clone(),
            password: s.password.clone(),
            settings: s.settings,
        }
    }
}

impl From<Entry> for Server {
    fn from(e: Entry) -> Self {
        Server {
            name: e.name,
            host: e.host,
            password: e.password,
            settings: e.settings,
        }
    }
}

/// Serde adapter: a plain password in memory, vncpasswd-obfuscated hex on
/// disk, and no key at all when empty.
pub mod obfuscated {
    use super::{hex_decode, hex_encode};
    use rfb::auth;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(password: &str, s: S) -> Result<S::Ok, S::Error> {
        if password.is_empty() {
            s.serialize_str("")
        } else {
            s.serialize_str(&hex_encode(&auth::obfuscate_password(password)))
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let text = String::deserialize(d)?;
        Ok(hex_decode(&text)
            .map(|block| auth::deobfuscate_password(&block))
            .unwrap_or_default())
    }
}

pub struct Directory {
    pub servers: Vec<Server>,
    path: Option<PathBuf>,
}

impl Directory {
    /// Reads the directory; a missing or unreadable file gives an empty one.
    pub fn load() -> Directory {
        Directory::load_from(default_path())
    }

    fn load_from(path: Option<PathBuf>) -> Directory {
        let servers = path
            .as_deref()
            .and_then(|p| match fs::read_to_string(p) {
                Ok(text) => Some(text),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => {
                    log::warn!("cannot read {}: {e}", p.display());
                    None
                }
            })
            .map(|text| {
                parse(&text).unwrap_or_else(|e| {
                    log::warn!("ignoring server directory: {e}");
                    Vec::new()
                })
            })
            .unwrap_or_default();
        Directory { servers, path }
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Err(io::Error::other("no config directory (HOME/APPDATA not set)"));
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serialize(&self.servers);
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        self.servers
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// Replaces the server at `index` or, without one, the server with the same
    /// name; otherwise appends. Returns the final index.
    pub fn upsert(&mut self, index: Option<usize>, server: Server) -> usize {
        let index = index
            .filter(|&i| i < self.servers.len())
            .or_else(|| self.find(&server.name));
        match index {
            Some(i) => {
                self.servers[i] = server;
                i
            }
            None => {
                self.servers.push(server);
                self.servers.len() - 1
            }
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.servers.len() {
            self.servers.remove(index);
        }
    }
}

fn parse(text: &str) -> Result<Vec<Server>, toml::de::Error> {
    let file: File = toml::from_str(text)?;
    Ok(file.servers.into_iter().map(Server::from).collect())
}

fn serialize(servers: &[Server]) -> String {
    let file = File {
        servers: servers.iter().map(Entry::from).collect(),
    };
    let mut text = String::from("# VNC Beach favorites. Passwords are stored in the vncpasswd format,\n# which hides them from a glance but is not encryption.\n\n");
    text.push_str(&toml::to_string(&file).expect("servers serialize"));
    text
}

fn default_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("servers.toml"))
}

/// `~/Library/Application Support/vnc-beach` on macOS,
/// `%APPDATA%\vnc-beach` on Windows, XDG config dir elsewhere.
pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else {
        match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        }
    };
    Some(base.join("vnc-beach"))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(text: &str) -> Option<[u8; 8]> {
    if text.len() != 16 {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, chunk) in text.as_bytes().chunks_exact(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Server> {
        vec![
            Server {
                name: "Mac Büro".into(),
                host: "192.168.1.20".into(),
                password: "password".into(),
                settings: Overrides::default(),
            },
            Server {
                name: "Ohne Passwort".into(),
                host: "mac.local:1".into(),
                password: String::new(),
                settings: Overrides {
                    colors: Some(crate::settings::Colors::Low),
                    view_only: Some(true),
                    ..Default::default()
                },
            },
        ]
    }

    #[test]
    fn round_trips_through_toml() {
        let text = serialize(&sample());
        assert!(text.contains("password = \"dbd83cfd727a1458\""), "{text}");
        assert_eq!(text.matches("password =").count(), 1, "{text}");
        assert_eq!(text.matches("[server.settings]").count(), 1, "{text}");
        assert!(text.contains("colors = \"low\""), "{text}");
        assert!(!text.contains("scaling"), "{text}");
        assert_eq!(parse(&text).unwrap(), sample());
    }

    #[test]
    fn tolerates_missing_and_broken_fields() {
        let text = "[[server]]\nname = \"a\"\nhost = \"b\"\npassword = \"nicht hex\"\n";
        let servers = parse(text).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].password, "");
        assert!(servers[0].settings.is_empty());
        assert_eq!(parse("").unwrap(), Vec::<Server>::new());
        assert!(parse("[[server]]\nname = \"a\"\n").is_err());
    }

    #[test]
    fn upsert_and_find() {
        let mut dir = Directory {
            servers: sample(),
            path: None,
        };
        assert_eq!(dir.find("mac büro"), Some(0));
        assert_eq!(dir.find("nope"), None);
        let i = dir.upsert(
            None,
            Server {
                name: "Mac Büro".into(),
                host: "10.0.0.1".into(),
                password: String::new(),
                settings: Overrides::default(),
            },
        );
        assert_eq!(
            (i, dir.servers.len(), dir.servers[0].host.as_str()),
            (0, 2, "10.0.0.1")
        );
        let i = dir.upsert(
            Some(1),
            Server {
                name: "Umbenannt".into(),
                host: "x".into(),
                password: String::new(),
                settings: Overrides::default(),
            },
        );
        assert_eq!((i, dir.servers[1].name.as_str()), (1, "Umbenannt"));
        let i = dir.upsert(
            Some(99),
            Server {
                name: "Neu".into(),
                host: "y".into(),
                password: String::new(),
                settings: Overrides::default(),
            },
        );
        assert_eq!((i, dir.servers.len()), (2, 3));
        dir.remove(0);
        assert_eq!(dir.servers[0].name, "Umbenannt");
        assert!(dir.save().is_err());
    }

    #[test]
    fn saves_to_disk_and_overwrites() {
        let base = std::env::temp_dir().join(format!("vnc-beach-test-{}", std::process::id()));
        let path = base.join("sub").join("servers.toml");
        let mut dir = Directory {
            servers: sample(),
            path: Some(path.clone()),
        };
        dir.save().unwrap();
        assert_eq!(Directory::load_from(Some(path.clone())).servers, sample());
        dir.remove(1);
        dir.save().unwrap();
        assert_eq!(Directory::load_from(Some(path.clone())).servers, sample()[..1]);
        assert!(Directory::load_from(Some(base.join("missing.toml")))
            .servers
            .is_empty());
        fs::remove_dir_all(&base).unwrap();
    }
}
