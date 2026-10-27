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
//! | `POST /api/device/heartbeat` | device | [`Heartbeat`] → 204 |
//! | `DELETE /api/device` | device | → 204 |
//!
//! The app login opens `GET /login?app_port=PORT&challenge=C` in the
//! browser; after signing in, the browser is sent to
//! `http://127.0.0.1:PORT/callback?code=CODE`, and the app trades the code
//! and the verifier behind `C` (see [`crate::challenge`]) for a session.

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
}

/// Error body of every non-2xx answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
