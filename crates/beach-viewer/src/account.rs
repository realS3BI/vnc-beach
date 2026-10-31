//! The coordination server from the app's side: signing in through the
//! browser, the device list, connection tickets, and registering this
//! computer (whose heartbeat the background process sends, see
//! [`crate::background`]). Talks HTTP through the system `curl` like the
//! updater, so no TLS stack is linked in.

use std::io::Write;
use std::net::IpAddr;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use beach_account::loopback::Loopback;
use beach_account::api::{
    ApiError, Favorite, Device, Heartbeat, HeartbeatResponse, Me, RegisterDevice, Registration, TicketRequest,
    TicketResponse, TokenRequest, TokenResponse, UpdateResult,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The signed-in account in `config.toml` (`[account]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountConfig {
    /// The coordination server, e.g. `https://beach.example.com`. Kept
    /// after signing out, so the next sign-in starts there.
    pub url: String,
    /// Session token; empty when signed out.
    pub token: String,
    pub name: String,
    pub login: String,
}

impl AccountConfig {
    pub fn signed_in(&self) -> bool {
        !self.url.is_empty() && !self.token.is_empty()
    }
}

/// This computer's registration (`[server.device]`). The server accepts
/// tickets signed with `key` for device `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLink {
    pub url: String,
    pub id: String,
    pub token: String,
    /// The coordination server's ticket key, base64.
    pub key: String,
    pub name: String,
    /// "Persönlich" or the organization's name, for display.
    pub owner: String,
}

impl DeviceLink {
    pub fn verifier(&self) -> Option<beach_account::ticket::Verifier> {
        Some(beach_account::ticket::Verifier {
            device: self.id.clone(),
            key: beach_account::decode_key(&self.key)?,
        })
    }
}

/// What a viewer needs to connect to an account device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub url: String,
    pub token: String,
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The session or device token is no longer valid.
    Unauthorized,
    /// The server doesn't know the path or the thing (an older
    /// coordination server, a removed device), in words for the user.
    NotFound(String),
    /// Any other answer or a network problem, in words for the user.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized => f.write_str("Die Anmeldung ist abgelaufen. Bitte neu anmelden."),
            Error::NotFound(e) | Error::Failed(e) => f.write_str(e),
        }
    }
}

/// Accepts `beach.example.com` and trailing slashes; HTTPS unless the
/// address says otherwise (plain HTTP only makes sense on a test machine).
pub fn normalize_url(input: &str) -> Result<String, String> {
    let url = input.trim().trim_end_matches('/');
    if url.is_empty() {
        return Err("Bitte die Adresse des Koordinationsservers eingeben.".into());
    }
    let url = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(char::is_whitespace) {
        return Err("Die Adresse muss mit https:// beginnen.".into());
    }
    Ok(url)
}

// HTTP

/// Escapes a value for a double-quoted string in a curl config file.
fn curl_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{escaped}\"")
}

/// Sends a request; the token and body go through curl's stdin so they
/// don't show up in the process list. `None` for an empty answer.
fn call<T: DeserializeOwned>(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<String>,
) -> Result<Option<T>, Error> {
    let mut config = format!(
        "url = {}\nrequest = {}\nwrite-out = \"\\n%{{http_code}}\"\nheader = \"Accept: application/json\"\n",
        curl_quote(url),
        curl_quote(method)
    );
    if let Some(token) = token {
        config += &format!(
            "header = {}\n",
            curl_quote(&format!("Authorization: Bearer {token}"))
        );
    }
    if let Some(body) = body {
        config += "header = \"Content-Type: application/json\"\n";
        config += &format!("data-binary = {}\n", curl_quote(&body));
    }
    let mut cmd = Command::new(crate::update::curl_path());
    cmd.args([
        "-sS",
        "--connect-timeout",
        "10",
        "--max-time",
        "30",
        "-A",
        "VNC-Beach",
        "--config",
        "-",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Failed(format!("curl nicht verfügbar: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(config.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::Failed(format!("curl: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(Error::Failed(format!(
            "Koordinationsserver nicht erreichbar: {err}"
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
    let status: u16 = status.trim().parse().unwrap_or(0);
    match status {
        200..=299 if body.trim().is_empty() => Ok(None),
        200..=299 => serde_json::from_str(body)
            .map(Some)
            .map_err(|e| Error::Failed(format!("Unerwartete Antwort des Koordinationsservers: {e}"))),
        401 => Err(Error::Unauthorized),
        _ => {
            let reason = serde_json::from_str::<ApiError>(body)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("HTTP {status}"));
            let reason = format!("Koordinationsserver: {reason}");
            Err(if status == 404 {
                Error::NotFound(reason)
            } else {
                Error::Failed(reason)
            })
        }
    }
}

fn required<T>(answer: Result<Option<T>, Error>) -> Result<T, Error> {
    answer?.ok_or_else(|| Error::Failed("Leere Antwort des Koordinationsservers.".into()))
}

fn json(value: &impl Serialize) -> Option<String> {
    Some(serde_json::to_string(value).expect("request serializes"))
}

pub fn me(account: &AccountConfig) -> Result<Me, Error> {
    required(call(
        "GET",
        &format!("{}/api/me", account.url),
        Some(&account.token),
        None,
    ))
}

pub fn devices(account: &AccountConfig) -> Result<Vec<Device>, Error> {
    required(call(
        "GET",
        &format!("{}/api/devices", account.url),
        Some(&account.token),
        None,
    ))
}

/// Keeps the favorites in step with the account (see
/// [`beach_account::favorites`]): sends what is newer here and returns what
/// is newer there, for [`crate::servers::Directory::apply`]. An older
/// coordination server without favorites changes nothing.
pub fn sync_favorites(account: &AccountConfig, local: &[Favorite]) -> Result<Vec<Favorite>, Error> {
    let remote: Vec<Favorite> = match call("GET", &format!("{}/api/favorites", account.url), Some(&account.token), None)
    {
        Ok(list) => list.unwrap_or_default(),
        Err(Error::NotFound(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    let merged = beach_account::favorites::merge(local, &remote, now);
    for f in &merged.push {
        let url = format!("{}/api/favorites/{}", account.url, f.id);
        call::<()>("PUT", &url, Some(&account.token), json(f))?;
    }
    if !merged.push.is_empty() || !merged.apply.is_empty() {
        log::info!("favorites: sent {}, received {}", merged.push.len(), merged.apply.len());
    }
    Ok(merged.apply)
}

/// Asks the device to update itself to the latest release.
pub fn request_update(account: &AccountConfig, id: &str) -> Result<(), Error> {
    call::<()>(
        "POST",
        &format!("{}/api/devices/{id}/update", account.url),
        Some(&account.token),
        None,
    )
    .map(|_| ())
}

pub fn logout(account: &AccountConfig) {
    let _ = call::<()>(
        "POST",
        &format!("{}/api/logout", account.url),
        Some(&account.token),
        None,
    );
}

/// A ticket for the device the server announced, bound to its nonce.
pub fn ticket(target: &Target, device: &str, nonce: &[u8]) -> Result<Vec<u8>, String> {
    if device != target.device {
        return Err("An dieser Adresse antwortet ein anderes Gerät des Kontos.".into());
    }
    let req = TicketRequest {
        nonce: beach_account::b64(nonce),
    };
    let url = format!("{}/api/devices/{}/ticket", target.url, target.device);
    let res: TicketResponse =
        required(call("POST", &url, Some(&target.token), json(&req))).map_err(|e| e.to_string())?;
    Ok(res.ticket.into_bytes())
}

/// Registers this computer. `org: None` makes it a personal device.
pub fn register(account: &AccountConfig, name: &str, org: Option<(&str, &str)>) -> Result<DeviceLink, Error> {
    let req = RegisterDevice {
        name: name.trim().to_string(),
        platform: platform().into(),
        org: org.map(|(id, _)| id.to_string()),
    };
    let reg: Registration = required(call(
        "POST",
        &format!("{}/api/devices", account.url),
        Some(&account.token),
        json(&req),
    ))?;
    if beach_account::decode_key(&reg.coordinator_key).is_none() {
        return Err(Error::Failed(
            "Der Koordinationsserver hat keinen gültigen Schlüssel geschickt.".into(),
        ));
    }
    Ok(DeviceLink {
        url: account.url.clone(),
        id: reg.id,
        token: reg.token,
        key: reg.coordinator_key,
        name: req.name,
        owner: org
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| "Persönlich".into()),
    })
}

/// Removes this computer from the account.
pub fn unregister(link: &DeviceLink) -> Result<(), Error> {
    match call::<()>(
        "DELETE",
        &format!("{}/api/device", link.url),
        Some(&link.token),
        None,
    ) {
        Ok(_) | Err(Error::Unauthorized) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Sends the heartbeat with the outcome of the last remote update, if
/// any. Returns whether the coordination server asks for an update.
pub fn heartbeat(
    link: &DeviceLink,
    port: u16,
    remote_update: bool,
    update_result: Option<UpdateResult>,
) -> Result<bool, Error> {
    let hb = Heartbeat {
        addresses: local_addresses(),
        port,
        version: env!("CARGO_PKG_VERSION").into(),
        remote_update,
        update_result,
    };
    let answer = call::<HeartbeatResponse>(
        "POST",
        &format!("{}/api/device/heartbeat", link.url),
        Some(&link.token),
        json(&hb),
    )?;
    Ok(answer.is_some_and(|a| a.update))
}

pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// This computer's addresses, most useful first: Tailscale (100.64.0.0/10),
/// then private LAN, then the rest. IPv6 only for Tailscale: the others
/// are mostly short-lived privacy addresses.
pub fn local_addresses() -> Vec<String> {
    let ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .map(|list| list.into_iter().map(|i| i.ip()).collect())
        .unwrap_or_default();
    rank_addresses(ips)
}

fn rank_addresses(mut ips: Vec<IpAddr>) -> Vec<String> {
    let rank = |ip: &IpAddr| match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if a == 100 && (64..128).contains(&b) {
                0
            } else if v4.is_private() {
                1
            } else {
                2
            }
        }
        IpAddr::V6(_) => 3,
    };
    ips.retain(|ip| match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        // Tailscale's ULA prefix fd7a:115c:a1e0::/48.
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    });
    ips.sort_by_key(rank);
    ips.dedup();
    ips.iter().map(IpAddr::to_string).collect()
}

/// `host:port` for an address the device reported.
pub fn endpoint(address: &str, port: u16) -> String {
    if address.contains(':') {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}

// Signing in

/// A sign-in in progress: the browser was sent to the coordination server,
/// which sends it back to the loopback listener on this machine with a code.
pub struct Login {
    loopback: Loopback,
    url: String,
    verifier: String,
}

impl Login {
    pub fn start(url: &str) -> Result<Login, String> {
        let loopback = Loopback::bind().map_err(|e| e.to_string())?;
        let bytes: [u8; 32] = rand::random();
        Ok(Login {
            loopback,
            url: url.to_string(),
            verifier: beach_account::b64(&bytes),
        })
    }

    /// The page to open in the browser.
    pub fn browser_url(&self) -> String {
        format!(
            "{}/login?app_port={}&challenge={}",
            self.url,
            self.loopback.port(),
            beach_account::challenge(&self.verifier)
        )
    }

    /// Waits up to ten minutes for the browser, then trades the code for a
    /// session. Blocking.
    pub fn finish(self, cancel: &AtomicBool) -> Result<AccountConfig, String> {
        let code = self.wait_for_code(cancel)?;
        let req = TokenRequest {
            code,
            verifier: self.verifier.clone(),
        };
        let res: TokenResponse = required(call("POST", &format!("{}/api/token", self.url), None, json(&req)))
            .map_err(|e| e.to_string())?;
        Ok(AccountConfig {
            url: self.url,
            token: res.token,
            name: res.user.name,
            login: res.user.login,
        })
    }

    fn wait_for_code(&self, cancel: &AtomicBool) -> Result<String, String> {
        let until = Instant::now() + Duration::from_secs(600);
        while Instant::now() < until {
            if cancel.load(Ordering::Acquire) {
                return Err("Anmeldung abgebrochen.".into());
            }
            if let Some(code) = self.loopback.accept_code(Duration::from_millis(200))? {
                return Ok(code);
            }
        }
        Err("Die Anmeldung im Browser hat zu lange gedauert.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_default_to_https() {
        assert_eq!(
            normalize_url(" beach.example.com/ ").unwrap(),
            "https://beach.example.com"
        );
        assert_eq!(
            normalize_url("http://localhost:8080").unwrap(),
            "http://localhost:8080"
        );
        assert!(normalize_url("").is_err());
        assert!(normalize_url("ftp://x").is_err());
    }

    #[test]
    fn ranks_tailscale_then_lan() {
        let ips = [
            "192.168.1.5",
            "127.0.0.1",
            "100.101.1.2",
            "169.254.3.3",
            "203.0.113.9",
            "fe80::1",
            "2001:db8::5",
            "fd7a:115c:a1e0::1",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        assert_eq!(
            rank_addresses(ips),
            ["100.101.1.2", "192.168.1.5", "203.0.113.9", "fd7a:115c:a1e0::1"]
        );
        assert_eq!(endpoint("fd7a::1", 5901), "[fd7a::1]:5901");
        assert_eq!(endpoint("10.0.0.1", 5901), "10.0.0.1:5901");
    }

    #[test]
    fn curl_config_values_are_quoted() {
        assert_eq!(curl_quote(r#"{"a":"b\c"}"#), r#""{\"a\":\"b\\c\"}""#);
    }
}
