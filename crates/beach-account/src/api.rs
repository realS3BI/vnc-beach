//! JSON bodies of the coordination server's HTTP API. Viewers send the
//! session token as `Authorization: Bearer`, devices their device token.
//!
//! | Method and path | Auth | Body → answer |
//! | --- | --- | --- |
//! | `GET /api/info` | – | → [`Info`] |
//! | `POST /api/token` | – | [`TokenRequest`] → [`TokenResponse`] |
//! | `POST /api/logout` | session | → 204 |
//! | `GET /api/me` | session | → [`Me`] |
//! | `GET /api/devices` | session | → `Vec<`[`Device`]`>` |
//! | `POST /api/devices` | session | [`RegisterDevice`] → [`Registration`] |
//! | `POST /api/devices/{id}/ticket` | session | [`TicketRequest`] → [`TicketResponse`] |
//! | `POST /api/devices/{id}/update` | session | → 204 |
//! | `GET /api/favorites` | session | → `Vec<`[`Favorite`]`>` |
//! | `PUT /api/favorites/{id}` | session | [`Favorite`] → 204 |
//! | `POST /api/device/heartbeat` | device | [`Heartbeat`] → 204 or [`HeartbeatResponse`] |
//! | `DELETE /api/device` | device | → 204 |
//!
//! The app login opens `GET /login?app_port=PORT&challenge=C` in the
//! browser; after signing in, the browser is sent to
//! `http://127.0.0.1:PORT/callback?code=CODE`, and the app trades the code
//! and the verifier behind `C` (see [`crate::challenge`]) for a session.
//!
//! Remote updates: `POST /api/devices/{id}/update` marks a device; its next
//! heartbeat is answered with [`HeartbeatResponse::update`]. The device then
//! updates itself to the latest GitHub release with its own checks (the
//! coordination server only sends the signal, never a download) and reports
//! a failure or "already up to date" in a later [`Heartbeat::update_result`].

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub name: String,
    /// Sign-in providers this instance offers: `github`, `google`, `dev`.
    pub providers: Vec<String>,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRequest {
    pub code: String,
    pub verifier: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub user: User,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    /// GitHub login or e-mail address, whatever the provider gave.
    pub login: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub user: User,
    pub orgs: Vec<Org>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Member,
    Admin,
    Owner,
}

impl Role {
    pub fn parse(text: &str) -> Option<Role> {
        match text {
            "member" => Some(Role::Member),
            "admin" => Some(Role::Admin),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    /// Manages members, invitations and the organization's devices.
    pub fn can_manage(self) -> bool {
        self >= Role::Admin
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Org {
    pub id: String,
    pub name: String,
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgRef {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// `macos` or `windows`.
    pub platform: String,
    /// `None`: a personal device of the signed-in user.
    pub org: Option<OrgRef>,
    /// Addresses the device reported, most useful first (no port).
    pub addresses: Vec<String>,
    pub port: u16,
    pub online: bool,
    /// Unix seconds of the last heartbeat.
    pub last_seen: Option<i64>,
    pub version: String,
    /// The device can update itself on request (it said so in its heartbeat).
    #[serde(default)]
    pub can_update: bool,
    /// A newer release exists than the device's version, as far as the
    /// coordination server knows.
    #[serde(default)]
    pub outdated: bool,
    #[serde(default)]
    pub update: UpdateState,
}

/// Where a requested remote update stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    #[default]
    Idle,
    /// Waiting for the device's next heartbeat (it may be offline).
    Requested { since: i64 },
    /// The device was told to update and hasn't reported back yet.
    Running { since: i64 },
    Failed { error: String },
}

impl UpdateState {
    /// Requested or running.
    pub fn pending(&self) -> bool {
        matches!(self, UpdateState::Requested { .. } | UpdateState::Running { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterDevice {
    pub name: String,
    pub platform: String,
    /// Organization to register the device for; personal if `None`.
    pub org: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub id: String,
    /// Secret the device authenticates heartbeats with.
    pub token: String,
    /// The coordination server's ticket key, see [`crate::encode_key`].
    pub coordinator_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRequest {
    /// The server's handshake nonce, base64.
    pub nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketResponse {
    pub ticket: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub addresses: Vec<String>,
    pub port: u16,
    pub version: String,
    /// The device updates itself when the answer asks for it.
    #[serde(default)]
    pub remote_update: bool,
    /// How the last requested update ended, if it didn't end in a restart
    /// with a new version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_result: Option<UpdateResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum UpdateResult {
    /// No newer release than the running version.
    UpToDate,
    Failed { error: String },
}

/// A saved connection of the signed-in user, kept in step between their
/// viewers (see [`crate::favorites`]). Passwords stay on each device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub host: String,
    /// Further addresses of the same computer, tried in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(default)]
    pub view_only: bool,
    /// Unix milliseconds of the last change, on whichever device.
    pub updated: i64,
    /// Deleted somewhere; kept so the other viewers delete it too.
    #[serde(default)]
    pub deleted: bool,
}

// Enrollment over a VNC connection (`rfb::enroll`): a viewer that is
// connected to a computer registers it with `POST /api/devices` and hands
// the registration to the computer, which then accepts the account's tickets
// and sends the heartbeat itself.

/// The server's Hello: what it is and whether it is in an account already.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollHello {
    /// Host name, a sensible default for the device name.
    pub name: String,
    /// `macos` or `windows`.
    pub platform: String,
    /// The current registration, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<EnrolledDevice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolledDevice {
    /// The coordination server.
    pub url: String,
    pub id: String,
    pub name: String,
}

/// Sent by the viewer: a fresh registration for this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrollment {
    /// The coordination server.
    pub url: String,
    pub id: String,
    /// The device token for heartbeats.
    pub token: String,
    /// The coordination server's ticket key, see [`crate::encode_key`].
    pub coordinator_key: String,
    pub name: String,
    /// "Persönlich" or the organization's name, for display.
    pub owner: String,
}

/// The server's answer to an [`Enrollment`]; `error` is in words for the user.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Answer to a heartbeat when the coordination server has something to say
/// (otherwise 204).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    /// Update to the latest release now.
    #[serde(default)]
    pub update: bool,
}

/// Whether version `a` is older than `b` (`1.2.3`, `v1.2`, suffixes after
/// `-` ignored). Unparsable versions are never older.
pub fn version_older(a: &str, b: &str) -> bool {
    match (version_key(a), version_key(b)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

fn version_key(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let core = text.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Error body of every non-2xx answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(version_older("0.4.0", "0.4.1"));
        assert!(version_older("v0.9", "0.10.0"));
        assert!(!version_older("0.4.1", "0.4.1"));
        assert!(!version_older("1.0.0", "0.9.9"));
        assert!(!version_older("", "0.4.1"));
    }

    #[test]
    fn old_devices_and_answers_still_parse() {
        let hb: Heartbeat = serde_json::from_str(r#"{"addresses":[],"port":5901,"version":"0.4.0"}"#).unwrap();
        assert!(!hb.remote_update);
        assert_eq!(hb.update_result, None);
        let failed = UpdateState::Failed { error: "x".into() };
        let json = serde_json::to_string(&failed).unwrap();
        assert_eq!(json, r#"{"state":"failed","error":"x"}"#);
        assert_eq!(serde_json::from_str::<UpdateState>(&json).unwrap(), failed);
    }
}
