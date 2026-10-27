//! The coordination server from the app's side: signing in through the
//! browser, the device list, connection tickets, and registering this
//! computer (whose heartbeat the background process sends, see
//! [`crate::background`]). Talks HTTP through the system `curl` like the
//! updater, so no TLS stack is linked in.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use beach_account::api::{
    ApiError, Device, Heartbeat, Me, RegisterDevice, Registration, TicketRequest, TicketResponse,
    TokenRequest, TokenResponse,
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
    /// Any other answer or a network problem, in words for the user.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized => f.write_str("Die Anmeldung ist abgelaufen. Bitte neu anmelden."),
            Error::Failed(e) => f.write_str(e),
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
            Err(Error::Failed(format!("Koordinationsserver: {reason}")))
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

pub fn heartbeat(link: &DeviceLink, port: u16) -> Result<(), Error> {
    let hb = Heartbeat {
        addresses: local_addresses(),
        port,
        version: env!("CARGO_PKG_VERSION").into(),
    };
    call::<()>(
        "POST",
        &format!("{}/api/device/heartbeat", link.url),
        Some(&link.token),
        json(&hb),
    )
    .map(|_| ())
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
/// which sends it back to [`Self::port`] on this machine with a code.
pub struct Login {
    listener: TcpListener,
    url: String,
    verifier: String,
}

impl Login {
    pub fn start(url: &str) -> Result<Login, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let bytes: [u8; 32] = rand::random();
        Ok(Login {
            listener,
            url: url.to_string(),
            verifier: beach_account::b64(&bytes),
        })
    }

    /// The page to open in the browser.
    pub fn browser_url(&self) -> String {
        let port = self.listener.local_addr().map(|a| a.port()).unwrap_or_default();
        format!(
            "{}/login?app_port={port}&challenge={}",
            self.url,
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
        self.listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let until = Instant::now() + Duration::from_secs(600);
        while Instant::now() < until {
            if cancel.load(Ordering::Acquire) {
                return Err("Anmeldung abgebrochen.".into());
            }
            let mut stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            match callback_code(&request) {
                Some(code) => {
                    let _ = stream.write_all(DONE_PAGE.as_bytes());
                    return Ok(code);
                }
                None => {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
        Err("Die Anmeldung im Browser hat zu lange gedauert.".into())
    }
}

const DONE_PAGE: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
<!doctype html><html lang=\"de\"><meta charset=\"utf-8\"><title>VNC Beach</title>\
<body style=\"font:16px system-ui;background:#FFF8EE;color:#17364A;display:grid;place-items:center;height:90vh\">\
<div><h1>Angemeldet</h1><p>Du kannst dieses Fenster schließen und zu VNC Beach zurückkehren.</p></div></body></html>";

/// The `code` of `GET /callback?code=… HTTP/1.1`.
fn callback_code(request: &str) -> Option<String> {
    let line = request.lines().next()?;
    let target = line.strip_prefix("GET ")?.split(' ').next()?;
    let query = target.strip_prefix("/callback?")?;
    query.split('&').find_map(|pair| {
        let value = pair.strip_prefix("code=")?;
        percent_decode(value)
    })
}

fn percent_decode(text: &str) -> Option<String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hex = [bytes.next()?, bytes.next()?];
                out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
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
    fn reads_the_callback_code() {
        assert_eq!(
            callback_code("GET /callback?code=ab%2Bc-_ HTTP/1.1\r\nHost: x\r\n\r\n").as_deref(),
            Some("ab+c-_")
        );
        assert_eq!(callback_code("GET /favicon.ico HTTP/1.1\r\n"), None);
        assert_eq!(callback_code("POST /callback?code=x HTTP/1.1\r\n"), None);
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
