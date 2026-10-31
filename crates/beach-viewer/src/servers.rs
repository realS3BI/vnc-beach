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
//!
//! [[deleted]]                      # removed favorites, so synced viewers remove them too
//! id = "5b0c…"
//! updated = 1793466620000
//! ```
//!
//! With an account, favorites are kept in step with the coordination server
//! (`beach_account::favorites`): `id` and `updated` (Unix milliseconds of
//! the last change) identify them across viewers. Passwords stay here.

use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};

use beach_account::api::Favorite;

use crate::settings::Overrides;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Server {
    /// Stable across viewers; empty until the directory assigns one.
    pub id: String,
    /// Unix milliseconds of the last change; 0 if never synced.
    pub updated: i64,
    pub name: String,
    pub host: String,
    /// Plain password in memory; obfuscated on disk.
    pub password: String,
    pub settings: Overrides,
    pub access: crate::access::Access,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "server")]
    servers: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deleted: Vec<Tombstone>,
}

/// A removed favorite, remembered for the sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub id: String,
    pub updated: i64,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    updated: i64,
    name: String,
    host: String,
    #[serde(default, with = "obfuscated", skip_serializing_if = "String::is_empty")]
    password: String,
    #[serde(default, skip_serializing_if = "Overrides::is_empty")]
    settings: Overrides,
    #[serde(default, skip_serializing_if = "crate::access::Access::is_empty")]
    access: crate::access::Access,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl From<&Server> for Entry {
    fn from(s: &Server) -> Self {
        Entry {
            id: s.id.clone(),
            updated: s.updated,
            name: s.name.clone(),
            host: s.host.clone(),
            password: s.password.clone(),
            settings: s.settings,
            access: s.access.clone(),
        }
    }
}

impl From<Entry> for Server {
    fn from(e: Entry) -> Self {
        Server {
            id: e.id,
            updated: e.updated,
            name: e.name,
            host: e.host,
            password: e.password,
            settings: e.settings,
            access: e.access,
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
    deleted: Vec<Tombstone>,
    path: Option<PathBuf>,
}

impl Directory {
    /// Reads the directory; a missing or unreadable file gives an empty one.
    pub fn load() -> Directory {
        Directory::load_from(default_path())
    }

    fn load_from(path: Option<PathBuf>) -> Directory {
        let file = path
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
                toml::from_str::<File>(&text).unwrap_or_else(|e| {
                    log::warn!("ignoring server directory: {e}");
                    File::default()
                })
            })
            .unwrap_or_default();
        let mut directory = Directory {
            servers: file.servers.into_iter().map(Server::from).collect(),
            deleted: file.deleted,
            path,
        };
        // Favorites from before the sync get their ID once.
        if directory.servers.iter().any(|s| s.id.is_empty()) {
            for server in directory.servers.iter_mut().filter(|s| s.id.is_empty()) {
                server.id = new_id();
            }
            if let Err(e) = directory.save() {
                log::warn!("cannot save favorite IDs: {e}");
            }
        }
        directory
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Err(io::Error::other("no config directory (HOME/APPDATA not set)"));
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serialize(&self.servers, &self.deleted);
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
    /// Saves `server` as the favorite at `index` (or the one with its
    /// name), or adds it; either way it counts as changed now.
    pub fn upsert(&mut self, index: Option<usize>, mut server: Server) -> usize {
        let index = index
            .filter(|&i| i < self.servers.len())
            .or_else(|| self.find(&server.name));
        server.updated = now_ms();
        match index {
            Some(i) => {
                if server.id.is_empty() {
                    server.id = self.servers[i].id.clone();
                }
                if server.id.is_empty() {
                    server.id = new_id();
                }
                self.servers[i] = server;
                i
            }
            None => {
                if server.id.is_empty() {
                    server.id = new_id();
                }
                self.servers.push(server);
                self.servers.len() - 1
            }
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.servers.len() {
            let server = self.servers.remove(index);
            self.deleted.retain(|t| t.id != server.id);
            self.deleted.push(Tombstone {
                id: server.id,
                updated: now_ms(),
            });
        }
    }

    /// Everything the sync compares: the favorites and the tombstones.
    pub fn favorites(&self) -> Vec<Favorite> {
        let servers = self.servers.iter().map(|s| Favorite {
            id: s.id.clone(),
            name: s.name.clone(),
            host: s.host.clone(),
            alternatives: s.access.alternatives.clone(),
            view_only: s.settings.view_only == Some(true),
            updated: s.updated,
            deleted: false,
        });
        let deleted = self.deleted.iter().map(|t| Favorite {
            id: t.id.clone(),
            name: String::new(),
            host: String::new(),
            alternatives: Vec::new(),
            view_only: false,
            updated: t.updated,
            deleted: true,
        });
        servers.chain(deleted).collect()
    }

    /// Takes over newer favorites from the server (see
    /// [`beach_account::favorites::Merged::apply`]); passwords and other
    /// local settings stay. Returns whether anything changed.
    pub fn apply(&mut self, remote: &[Favorite]) -> bool {
        let mut changed = false;
        for r in remote {
            let index = self.servers.iter().position(|s| s.id == r.id).or_else(|| {
                let favorites = self.favorites();
                favorites
                    .iter()
                    .position(|l| beach_account::favorites::same_host(r, l))
                    .filter(|&i| i < self.servers.len())
            });
            if r.deleted {
                if let Some(i) = index {
                    self.servers.remove(i);
                }
                self.deleted.retain(|t| t.id != r.id);
                self.deleted.push(Tombstone {
                    id: r.id.clone(),
                    updated: r.updated,
                });
                changed = true;
                continue;
            }
            if self.deleted.iter().any(|t| t.id == r.id && t.updated >= r.updated) {
                continue;
            }
            self.deleted.retain(|t| t.id != r.id);
            let server = match index {
                Some(i) => &mut self.servers[i],
                None => {
                    self.servers.push(Server::default());
                    self.servers.last_mut().unwrap()
                }
            };
            server.id = r.id.clone();
            server.updated = r.updated;
            server.name = r.name.clone();
            server.host = r.host.clone();
            server.access.alternatives = r.alternatives.clone();
            server.settings.view_only = r.view_only.then_some(true);
            changed = true;
        }
        changed
    }
}

/// A new favorite ID, formatted like a UUID (the iOS app uses those).
fn new_id() -> String {
    let b: [u8; 16] = rand::random();
    let hex: String = b.iter().map(|b| format!("{b:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
fn parse(text: &str) -> Result<Vec<Server>, toml::de::Error> {
    let file: File = toml::from_str(text)?;
    Ok(file.servers.into_iter().map(Server::from).collect())
}

fn serialize(servers: &[Server], deleted: &[Tombstone]) -> String {
    let file = File {
        servers: servers.iter().map(Entry::from).collect(),
        deleted: deleted.to_vec(),
    };
    let mut text = String::from("# VNC Beach favorites. Passwords are stored in the vncpasswd format,\n# which hides them from a glance but is not encryption.\n\n");
    text.push_str(&toml::to_string(&file).expect("servers serialize"));
    text
}

fn default_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("servers.toml"))
}

/// Set by the Windows service: it runs as SYSTEM but serves the
/// configuration of the user who set it up.
static CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_config_dir(dir: PathBuf) {
    let _ = CONFIG_DIR.set(dir);
}

/// `~/Library/Application Support/vnc-beach` on macOS,
/// `%APPDATA%\vnc-beach` on Windows, XDG config dir elsewhere.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = CONFIG_DIR.get() {
        return Some(dir.clone());
    }
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
                id: String::new(),
                updated: 0,
                name: "Mac Büro".into(),
                host: "192.168.1.20".into(),
                password: "password".into(),
                settings: Overrides::default(),
                access: Default::default(),
            },
            Server {
                id: String::new(),
                updated: 0,
                name: "Ohne Passwort".into(),
                host: "mac.local:1".into(),
                password: String::new(),
                settings: Overrides {
                    colors: Some(crate::settings::Colors::Low),
                    view_only: Some(true),
                    ..Default::default()
                },
                access: Default::default(),
            },
        ]
    }

    #[test]
    fn round_trips_through_toml() {
        let text = serialize(&sample(), &[]);
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
        assert!(servers[0].access.is_empty());
        assert_eq!(parse("").unwrap(), Vec::<Server>::new());
        assert!(parse("[[server]]\nname = \"a\"\n").is_err());
    }

    #[test]
    fn preserves_optional_access_capabilities_without_storing_login_passwords() {
        use crate::access::{Access, Authentication, Relay, Shell, Ssh, Wake};
        let mut servers = sample();
        servers[0].access = Access {
            alternatives: vec!["192.168.1.20::5901".into()],
            ssh: Some(Ssh {
                address: "192.168.1.20".into(),
                user: "remote".into(),
                auth: Authentication::Password,
            }),
            wake: Some(Wake {
                mac: "00:11:22:33:44:55".into(),
                relays: vec![Relay {
                    ssh: Ssh {
                        address: "relay:22".into(),
                        user: "operator".into(),
                        auth: Authentication::Key {
                            path: "/path/to/key".into(),
                        },
                    },
                    shell: Shell::PowerShell,
                }],
                ..Default::default()
            }),
        };
        let text = serialize(&servers, &[]);
        assert_eq!(parse(&text).unwrap(), servers);
        assert_eq!(
            text.matches("password =").count(),
            1,
            "only the existing VNC password is serialized"
        );
        assert!(text.contains("power_shell"));
        let legacy = serialize(&sample(), &[]);
        assert!(!legacy.contains("access"));
    }

    #[test]
    fn upsert_and_find() {
        let mut dir = Directory {
            servers: sample(),
            deleted: Vec::new(),
            path: None,
        };
        assert_eq!(dir.find("mac büro"), Some(0));
        assert_eq!(dir.find("nope"), None);
        let i = dir.upsert(
            None,
            Server {
                id: String::new(),
                updated: 0,
                name: "Mac Büro".into(),
                host: "10.0.0.1".into(),
                password: String::new(),
                settings: Overrides::default(),
                access: Default::default(),
            },
        );
        assert_eq!(
            (i, dir.servers.len(), dir.servers[0].host.as_str()),
            (0, 2, "10.0.0.1")
        );
        let i = dir.upsert(
            Some(1),
            Server {
                id: String::new(),
                updated: 0,
                name: "Umbenannt".into(),
                host: "x".into(),
                password: String::new(),
                settings: Overrides::default(),
                access: Default::default(),
            },
        );
        assert_eq!((i, dir.servers[1].name.as_str()), (1, "Umbenannt"));
        let i = dir.upsert(
            Some(99),
            Server {
                id: String::new(),
                updated: 0,
                name: "Neu".into(),
                host: "y".into(),
                password: String::new(),
                settings: Overrides::default(),
                access: Default::default(),
            },
        );
        assert_eq!((i, dir.servers.len()), (2, 3));
        dir.remove(0);
        assert_eq!(dir.servers[0].name, "Umbenannt");
        assert!(dir.save().is_err());
    }

    #[test]
    fn syncs_favorites_by_id_and_keeps_passwords() {
        let mut dir = Directory {
            servers: sample(),
            deleted: Vec::new(),
            path: None,
        };
        let i = dir.upsert(Some(0), dir.servers[0].clone());
        let id = dir.servers[i].id.clone();
        assert!(!id.is_empty() && dir.servers[i].updated > 0);
        let mut ours = dir.favorites();
        assert_eq!(ours.len(), 2);
        assert!(!ours[0].view_only && ours[1].view_only);

        // A rename from another viewer keeps the local password.
        ours[0].name = "Mac Büro 2".into();
        ours[0].updated += 1;
        assert!(dir.apply(&ours[..1]));
        assert_eq!((dir.servers[0].name.as_str(), dir.servers[0].password.as_str()), ("Mac Büro 2", "password"));

        // A favorite from another viewer that never synced here matches by
        // address instead of doubling.
        let mut theirs = ours[1].clone();
        theirs.id = "OTHER".into();
        theirs.updated = 99;
        dir.apply(std::slice::from_ref(&theirs));
        assert_eq!(dir.servers.len(), 2);
        assert_eq!(dir.servers[1].id, "OTHER");

        // Deleting leaves a tombstone, and a remote one deletes here.
        dir.remove(0);
        assert!(dir.favorites().iter().any(|f| f.id == id && f.deleted));
        theirs.deleted = true;
        theirs.updated = 100;
        dir.apply(&[theirs]);
        assert!(dir.servers.is_empty());
        assert_eq!(dir.favorites().iter().filter(|f| f.deleted).count(), 2);
    }

    #[test]
    fn saves_to_disk_and_overwrites() {
        let base = std::env::temp_dir().join(format!("vnc-beach-test-{}", std::process::id()));
        let path = base.join("sub").join("servers.toml");
        let mut servers = sample();
        for (i, s) in servers.iter_mut().enumerate() {
            s.id = format!("id-{i}");
        }
        let mut dir = Directory {
            servers: servers.clone(),
            deleted: Vec::new(),
            path: Some(path.clone()),
        };
        dir.save().unwrap();
        assert_eq!(Directory::load_from(Some(path.clone())).servers, servers);
        dir.remove(1);
        dir.save().unwrap();
        let loaded = Directory::load_from(Some(path.clone()));
        assert_eq!(loaded.servers, servers[..1]);
        assert!(loaded.favorites().iter().any(|f| f.id == "id-1" && f.deleted));
        // Favorites from before the sync get IDs when loaded, once.
        fs::write(&path, serialize(&sample(), &[])).unwrap();
        let first = Directory::load_from(Some(path.clone())).servers;
        assert!(first.iter().all(|s| !s.id.is_empty()));
        assert_eq!(Directory::load_from(Some(path.clone())).servers, first);
        assert!(Directory::load_from(Some(base.join("missing.toml")))
            .servers
            .is_empty());
        fs::remove_dir_all(&base).unwrap();
    }
}
