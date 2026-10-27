//! SQLite storage. Secrets (session, device, invitation and login codes)
//! are kept only as SHA-256 hashes.

use std::path::Path;

use anyhow::Context as _;
use beach_account::api::{Device, Heartbeat, Org, OrgRef, Role, User};
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

/// Heartbeats come every minute; a device is online while the last one is
/// younger than this.
pub const ONLINE_FOR: i64 = 150;
const DAY: i64 = 86_400;
pub const WEB_SESSION_LIFETIME: i64 = 30 * DAY;
const APP_SESSION_LIFETIME: i64 = 365 * DAY;
const LOGIN_LIFETIME: i64 = 600;
const APP_CODE_LIFETIME: i64 = 120;
pub const INVITE_LIFETIME: i64 = 7 * DAY;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    subject TEXT NOT NULL,
    name TEXT NOT NULL,
    login TEXT NOT NULL,
    created INTEGER NOT NULL,
    UNIQUE (provider, subject)
);
CREATE TABLE IF NOT EXISTS sessions (
    hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    expires INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS logins (
    state TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    app_port INTEGER,
    challenge TEXT,
    next TEXT,
    expires INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_codes (
    hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    challenge TEXT NOT NULL,
    expires INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS orgs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS members (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    PRIMARY KEY (org_id, user_id)
);
CREATE TABLE IF NOT EXISTS invites (
    id TEXT PRIMARY KEY,
    hash TEXT NOT NULL UNIQUE,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    created_by TEXT NOT NULL,
    expires INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    platform TEXT NOT NULL,
    user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    org_id TEXT REFERENCES orgs(id) ON DELETE CASCADE,
    registered_by TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    addresses TEXT NOT NULL DEFAULT '[]',
    port INTEGER NOT NULL DEFAULT 5901,
    version TEXT NOT NULL DEFAULT '',
    last_seen INTEGER,
    created INTEGER NOT NULL,
    CHECK ((user_id IS NULL) != (org_id IS NULL))
);
";

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// A random secret for tokens and codes: 32 bytes, base64.
pub fn secret() -> String {
    let bytes: [u8; 32] = rand::random();
    beach_account::b64(&bytes)
}

/// A short random ID for users, organizations, devices and invitations.
fn new_id() -> String {
    let bytes: [u8; 9] = rand::random();
    beach_account::b64(&bytes)
}

fn hash(secret: &str) -> String {
    beach_account::b64(&Sha256::digest(secret.as_bytes()))
}

/// Who signed in, as the identity provider reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub provider: String,
    /// The provider's immutable account ID (GitHub user ID, Google `sub`).
    pub subject: String,
    pub name: String,
    pub login: String,
}

/// A pending sign-in between redirect and provider callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub provider: String,
    /// Set when the desktop app started the login.
    pub app: Option<(u16, String)>,
    /// Where the browser goes afterwards (a local path).
    pub next: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub user: User,
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub id: String,
    pub role: Role,
    pub expires: i64,
}

/// A device with its owner, for permission checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceOwner {
    pub user_id: Option<String>,
    pub org_id: Option<String>,
    pub registered_by: String,
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Db::init(conn)
    }

    #[cfg(test)]
    pub fn memory() -> Db {
        Db::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(conn: Connection) -> anyhow::Result<Db> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db { conn })
    }

    /// Drops expired sessions, logins, codes and invitations.
    pub fn clean_up(&self) -> rusqlite::Result<()> {
        let now = now();
        for table in ["sessions", "logins", "app_codes", "invites"] {
            self.conn
                .execute(&format!("DELETE FROM {table} WHERE expires < ?1"), [now])?;
        }
        Ok(())
    }

    // Users and sessions

    /// Finds or creates the user; name and login follow the provider.
    pub fn sign_in(&self, id: &Identity) -> rusqlite::Result<User> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM users WHERE provider = ?1 AND subject = ?2",
                params![id.provider, id.subject],
                |r| r.get(0),
            )
            .optional()?;
        let user_id = match existing {
            Some(user_id) => {
                self.conn.execute(
                    "UPDATE users SET name = ?2, login = ?3 WHERE id = ?1",
                    params![user_id, id.name, id.login],
                )?;
                user_id
            }
            None => {
                let user_id = new_id();
                self.conn.execute(
                    "INSERT INTO users (id, provider, subject, name, login, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![user_id, id.provider, id.subject, id.name, id.login, now()],
                )?;
                user_id
            }
        };
        Ok(User {
            id: user_id,
            name: id.name.clone(),
            login: id.login.clone(),
        })
    }

    pub fn user(&self, id: &str) -> rusqlite::Result<Option<User>> {
        self.conn
            .query_row("SELECT id, name, login FROM users WHERE id = ?1", [id], user_row)
            .optional()
    }

    /// `kind`: `web` (browser cookie) or `app`.
    pub fn create_session(&self, user_id: &str, kind: &str) -> rusqlite::Result<String> {
        let token = secret();
        let lifetime = if kind == "app" {
            APP_SESSION_LIFETIME
        } else {
            WEB_SESSION_LIFETIME
        };
        self.conn.execute(
            "INSERT INTO sessions (hash, user_id, kind, expires) VALUES (?1, ?2, ?3, ?4)",
            params![hash(&token), user_id, kind, now() + lifetime],
        )?;
        Ok(token)
    }

    pub fn session_user(&self, token: &str) -> rusqlite::Result<Option<User>> {
        self.conn
            .query_row(
                "SELECT u.id, u.name, u.login FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.hash = ?1 AND s.expires >= ?2",
                params![hash(token), now()],
                user_row,
            )
            .optional()
    }

    pub fn delete_session(&self, token: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE hash = ?1", [hash(token)])?;
        Ok(())
    }

    // Sign-in flow

    pub fn create_login(&self, login: &Login) -> rusqlite::Result<String> {
        let state = secret();
        let (port, challenge) = match &login.app {
            Some((port, challenge)) => (Some(*port), Some(challenge.clone())),
            None => (None, None),
        };
        self.conn.execute(
            "INSERT INTO logins (state, provider, app_port, challenge, next, expires) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![state, login.provider, port, challenge, login.next, now() + LOGIN_LIFETIME],
        )?;
        Ok(state)
    }

    /// Returns and removes the pending login, if it is still valid.
    pub fn take_login(&self, state: &str) -> rusqlite::Result<Option<Login>> {
        let login = self
            .conn
            .query_row(
                "SELECT provider, app_port, challenge, next FROM logins WHERE state = ?1 AND expires >= ?2",
                params![state, now()],
                |r| {
                    let port: Option<u16> = r.get(1)?;
                    let challenge: Option<String> = r.get(2)?;
                    Ok(Login {
                        provider: r.get(0)?,
                        app: port.zip(challenge),
                        next: r.get(3)?,
                    })
                },
            )
            .optional()?;
        self.conn
            .execute("DELETE FROM logins WHERE state = ?1", [state])?;
        Ok(login)
    }

    pub fn create_app_code(&self, user_id: &str, challenge: &str) -> rusqlite::Result<String> {
        let code = secret();
        self.conn.execute(
            "INSERT INTO app_codes (hash, user_id, challenge, expires) VALUES (?1, ?2, ?3, ?4)",
            params![hash(&code), user_id, challenge, now() + APP_CODE_LIFETIME],
        )?;
        Ok(code)
    }

    /// Trades a login code for its user if `verifier` matches the
    /// challenge. The code is used up either way.
    pub fn take_app_code(&self, code: &str, verifier: &str) -> rusqlite::Result<Option<String>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT user_id, challenge FROM app_codes WHERE hash = ?1 AND expires >= ?2",
                params![hash(code), now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        self.conn
            .execute("DELETE FROM app_codes WHERE hash = ?1", [hash(code)])?;
        Ok(row
            .and_then(|(user, challenge)| (beach_account::challenge(verifier) == challenge).then_some(user)))
    }

    // Organizations

    pub fn create_org(&self, name: &str, owner: &str) -> rusqlite::Result<String> {
        // Rolled back on drop if a step fails.
        let tx = self.conn.unchecked_transaction()?;
        let id = new_id();
        self.conn.execute(
            "INSERT INTO orgs (id, name, created) VALUES (?1, ?2, ?3)",
            params![id, name, now()],
        )?;
        self.conn.execute(
            "INSERT INTO members (org_id, user_id, role) VALUES (?1, ?2, 'owner')",
            params![id, owner],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn delete_org(&self, id: &str) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM orgs WHERE id = ?1", [id])?;
        Ok(())
    }

    /// The user's organizations, by name.
    pub fn orgs(&self, user_id: &str) -> rusqlite::Result<Vec<Org>> {
        let mut stmt = self.conn.prepare(
            "SELECT o.id, o.name, m.role FROM orgs o JOIN members m ON m.org_id = o.id
             WHERE m.user_id = ?1 ORDER BY o.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([user_id], |r| {
            Ok(Org {
                id: r.get(0)?,
                name: r.get(1)?,
                role: role(r, 2)?,
            })
        })?;
        rows.collect()
    }

    /// The organization as the user sees it; `None` if they aren't a member.
    pub fn org(&self, id: &str, user_id: &str) -> rusqlite::Result<Option<Org>> {
        Ok(self.orgs(user_id)?.into_iter().find(|o| o.id == id))
    }

    pub fn members(&self, org_id: &str) -> rusqlite::Result<Vec<Member>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id, u.name, u.login, m.role FROM members m JOIN users u ON u.id = m.user_id
             WHERE m.org_id = ?1 ORDER BY u.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([org_id], |r| {
            Ok(Member {
                user: user_row(r)?,
                role: role(r, 3)?,
            })
        })?;
        rows.collect()
    }

    pub fn set_role(&self, org_id: &str, user_id: &str, role: Role) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE members SET role = ?3 WHERE org_id = ?1 AND user_id = ?2",
            params![org_id, user_id, role.as_str()],
        )?;
        Ok(())
    }

    pub fn remove_member(&self, org_id: &str, user_id: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM members WHERE org_id = ?1 AND user_id = ?2",
            params![org_id, user_id],
        )?;
        Ok(())
    }

    // Invitations

    /// Returns the link token; only its hash is stored.
    pub fn create_invite(&self, org_id: &str, role: Role, by: &str) -> rusqlite::Result<String> {
        let token = secret();
        self.conn.execute(
            "INSERT INTO invites (id, hash, org_id, role, created_by, expires) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_id(), hash(&token), org_id, role.as_str(), by, now() + INVITE_LIFETIME],
        )?;
        Ok(token)
    }

    pub fn invites(&self, org_id: &str) -> rusqlite::Result<Vec<Invite>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, expires FROM invites WHERE org_id = ?1 AND expires >= ?2 ORDER BY expires",
        )?;
        let rows = stmt.query_map(params![org_id, now()], |r| {
            Ok(Invite {
                id: r.get(0)?,
                role: role(r, 1)?,
                expires: r.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn delete_invite(&self, org_id: &str, id: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM invites WHERE org_id = ?1 AND id = ?2",
            params![org_id, id],
        )?;
        Ok(())
    }

    /// The organization an invitation link leads to: `(id, name)`.
    pub fn invite_org(&self, token: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT o.id, o.name FROM invites i JOIN orgs o ON o.id = i.org_id
                 WHERE i.hash = ?1 AND i.expires >= ?2",
                params![hash(token), now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    /// Adds the user and uses the link up. Members keep a higher role they
    /// already have. Returns the organization ID.
    pub fn accept_invite(&self, token: &str, user_id: &str) -> rusqlite::Result<Option<String>> {
        let tx = self.conn.unchecked_transaction()?;
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT org_id, role FROM invites WHERE hash = ?1 AND expires >= ?2",
                params![hash(token), now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((org_id, invited)) = row else {
            return Ok(None);
        };
        self.conn
            .execute("DELETE FROM invites WHERE hash = ?1", [hash(token)])?;
        let invited = Role::parse(&invited).unwrap_or(Role::Member);
        let current = self.org(&org_id, user_id)?.map(|o| o.role);
        if current.is_none_or(|current| current < invited) {
            self.conn.execute(
                "INSERT INTO members (org_id, user_id, role) VALUES (?1, ?2, ?3)
                 ON CONFLICT (org_id, user_id) DO UPDATE SET role = excluded.role",
                params![org_id, user_id, invited.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(Some(org_id))
    }

    // Devices

    /// Registers a device for the user (`org: None`) or an organization.
    /// Returns its ID and secret token.
    pub fn register_device(
        &self,
        name: &str,
        platform: &str,
        org: Option<&str>,
        by: &str,
    ) -> rusqlite::Result<(String, String)> {
        let (id, token) = (new_id(), secret());
        let user = org.is_none().then_some(by);
        self.conn.execute(
            "INSERT INTO devices (id, name, platform, user_id, org_id, registered_by, token_hash, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, name, platform, user, org, by, hash(&token), now()],
        )?;
        Ok((id, token))
    }

    /// The device a device token belongs to.
    pub fn device_by_token(&self, token: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT id FROM devices WHERE token_hash = ?1",
                [hash(token)],
                |r| r.get(0),
            )
            .optional()
    }

    pub fn heartbeat(&self, id: &str, hb: &Heartbeat) -> rusqlite::Result<()> {
        let addresses = serde_json::to_string(&hb.addresses).unwrap_or_else(|_| "[]".into());
        self.conn.execute(
            "UPDATE devices SET addresses = ?2, port = ?3, version = ?4, last_seen = ?5 WHERE id = ?1",
            params![id, addresses, hb.port, hb.version, now()],
        )?;
        Ok(())
    }

    pub fn delete_device(&self, id: &str) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM devices WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn device_owner(&self, id: &str) -> rusqlite::Result<Option<DeviceOwner>> {
        self.conn
            .query_row(
                "SELECT user_id, org_id, registered_by FROM devices WHERE id = ?1",
                [id],
                |r| {
                    Ok(DeviceOwner {
                        user_id: r.get(0)?,
                        org_id: r.get(1)?,
                        registered_by: r.get(2)?,
                    })
                },
            )
            .optional()
    }

    /// Devices the user may connect to: their own and those of their
    /// organizations. Personal ones first, then by organization and name.
    pub fn visible_devices(&self, user_id: &str) -> rusqlite::Result<Vec<Device>> {
        self.devices_where(
            "d.user_id = ?1 OR d.org_id IN (SELECT org_id FROM members WHERE user_id = ?1)",
            user_id,
        )
    }

    pub fn org_devices(&self, org_id: &str) -> rusqlite::Result<Vec<Device>> {
        self.devices_where("d.org_id = ?1", org_id)
    }

    fn devices_where(&self, condition: &str, param: &str) -> rusqlite::Result<Vec<Device>> {
        let sql = format!(
            "SELECT d.id, d.name, d.platform, d.org_id, o.name, d.addresses, d.port, d.last_seen, d.version
             FROM devices d LEFT JOIN orgs o ON o.id = d.org_id
             WHERE {condition}
             ORDER BY d.org_id IS NOT NULL, o.name COLLATE NOCASE, d.name COLLATE NOCASE"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let now = now();
        let rows = stmt.query_map([param], |r| {
            let org_id: Option<String> = r.get(3)?;
            let org_name: Option<String> = r.get(4)?;
            let addresses: String = r.get(5)?;
            let last_seen: Option<i64> = r.get(7)?;
            Ok(Device {
                id: r.get(0)?,
                name: r.get(1)?,
                platform: r.get(2)?,
                org: org_id.zip(org_name).map(|(id, name)| OrgRef { id, name }),
                addresses: serde_json::from_str(&addresses).unwrap_or_default(),
                port: r.get(6)?,
                online: last_seen.is_some_and(|t| now - t <= ONLINE_FOR),
                last_seen,
                version: r.get(8)?,
            })
        })?;
        rows.collect()
    }
}

fn user_row(r: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        name: r.get(1)?,
        login: r.get(2)?,
    })
}

fn role(r: &Row, index: usize) -> rusqlite::Result<Role> {
    let text: String = r.get(index)?;
    Role::parse(&text)
        .ok_or_else(|| rusqlite::Error::InvalidColumnType(index, text, rusqlite::types::Type::Text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> Identity {
        Identity {
            provider: "github".into(),
            subject: format!("id-{name}"),
            name: name.into(),
            login: name.to_lowercase(),
        }
    }

    #[test]
    fn sign_in_reuses_the_account_and_follows_renames() {
        let db = Db::memory();
        let first = db.sign_in(&identity("Alice")).unwrap();
        let mut renamed = identity("Alice");
        renamed.name = "Alice B.".into();
        let second = db.sign_in(&renamed).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(db.user(&first.id).unwrap().unwrap().name, "Alice B.");
    }

    #[test]
    fn sessions_and_app_codes() {
        let db = Db::memory();
        let user = db.sign_in(&identity("Alice")).unwrap();
        let token = db.create_session(&user.id, "app").unwrap();
        assert_eq!(db.session_user(&token).unwrap(), Some(user.clone()));
        db.delete_session(&token).unwrap();
        assert_eq!(db.session_user(&token).unwrap(), None);

        let verifier = "a-long-random-verifier";
        let code = db
            .create_app_code(&user.id, &beach_account::challenge(verifier))
            .unwrap();
        assert_eq!(db.take_app_code(&code, "wrong").unwrap(), None);
        // Used up by the failed attempt.
        assert_eq!(db.take_app_code(&code, verifier).unwrap(), None);
        let code = db
            .create_app_code(&user.id, &beach_account::challenge(verifier))
            .unwrap();
        assert_eq!(db.take_app_code(&code, verifier).unwrap(), Some(user.id));
    }

    #[test]
    fn logins_are_single_use() {
        let db = Db::memory();
        let login = Login {
            provider: "google".into(),
            app: Some((4711, "challenge".into())),
            next: None,
        };
        let state = db.create_login(&login).unwrap();
        assert_eq!(db.take_login(&state).unwrap(), Some(login));
        assert_eq!(db.take_login(&state).unwrap(), None);
    }

    #[test]
    fn personal_and_org_devices_are_visible_to_the_right_people() {
        let db = Db::memory();
        let alice = db.sign_in(&identity("Alice")).unwrap();
        let bob = db.sign_in(&identity("Bob")).unwrap();
        let org = db.create_org("Büro", &alice.id).unwrap();
        db.register_device("Alices Mac", "macos", None, &alice.id)
            .unwrap();
        let (office, token) = db
            .register_device("Empfang", "windows", Some(&org), &alice.id)
            .unwrap();

        let names = |user: &str| -> Vec<String> {
            db.visible_devices(user)
                .unwrap()
                .into_iter()
                .map(|d| d.name)
                .collect()
        };
        assert_eq!(names(&alice.id), ["Alices Mac", "Empfang"]);
        assert!(names(&bob.id).is_empty());

        let invite = db.create_invite(&org, Role::Member, &alice.id).unwrap();
        assert_eq!(
            db.invite_org(&invite).unwrap(),
            Some((org.clone(), "Büro".into()))
        );
        assert_eq!(db.accept_invite(&invite, &bob.id).unwrap(), Some(org.clone()));
        assert_eq!(
            db.accept_invite(&invite, &bob.id).unwrap(),
            None,
            "links work once"
        );
        assert_eq!(names(&bob.id), ["Empfang"]);

        assert_eq!(db.device_by_token(&token).unwrap(), Some(office.clone()));
        db.heartbeat(
            &office,
            &Heartbeat {
                addresses: vec!["100.64.0.7".into()],
                port: 5901,
                version: "0.3.0".into(),
            },
        )
        .unwrap();
        let device = &db.org_devices(&org).unwrap()[0];
        assert!(device.online);
        assert_eq!(device.addresses, ["100.64.0.7"]);
        assert_eq!(device.org.as_ref().unwrap().name, "Büro");

        db.remove_member(&org, &bob.id).unwrap();
        assert!(names(&bob.id).is_empty());
        db.delete_org(&org).unwrap();
        assert_eq!(names(&alice.id), ["Alices Mac"], "org devices go with the org");
    }

    #[test]
    fn invitations_never_demote() {
        let db = Db::memory();
        let alice = db.sign_in(&identity("Alice")).unwrap();
        let org = db.create_org("Team", &alice.id).unwrap();
        let invite = db.create_invite(&org, Role::Member, &alice.id).unwrap();
        db.accept_invite(&invite, &alice.id).unwrap();
        assert_eq!(db.org(&org, &alice.id).unwrap().unwrap().role, Role::Owner);
    }
}
