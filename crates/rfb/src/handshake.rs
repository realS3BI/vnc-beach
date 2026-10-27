//! Protocol version, security and initialisation phases (RFC 6143 §7.1–7.3).

use std::io::{Read, Write};

use crate::auth::{random_challenge, vnc_response, CHALLENGE_LEN};
use crate::io::{ReadExt, WriteExt};
use crate::{proto, Error, PixelFormat, Result};

pub const SECURITY_NONE: u8 = 1;
pub const SECURITY_VNC: u8 = 2;
/// VNC Beach account ticket, offered next to [`SECURITY_VNC`] by servers
/// registered with a coordination server. Not assigned by IANA; 66 (`B`)
/// lies in the unused range 36–112. After the client picks it:
///
/// - server: 32-byte nonce, `u8` length, device ID (UTF-8)
/// - client: `u16` length, ticket
/// - server: SecurityResult as for VNC authentication
pub const SECURITY_BEACH_TICKET: u8 = 66;
pub const TICKET_NONCE_LEN: usize = 32;

/// Whether a complete server banner can be negotiated by this client. Useful
/// when checking alternate routes before sending any authentication data.
pub fn is_server_banner(banner: &[u8; 12]) -> bool {
    Version::parse(banner).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    V3_3,
    V3_7,
    V3_8,
}

impl Version {
    fn banner(self) -> &'static [u8; 12] {
        match self {
            Version::V3_3 => b"RFB 003.003\n",
            Version::V3_7 => b"RFB 003.007\n",
            Version::V3_8 => b"RFB 003.008\n",
        }
    }

    /// Maps any banner onto the closest version we speak. Apple sends 003.889,
    /// some UltraVNC builds 003.004/003.006; those behave like 3.8 and 3.3.
    fn parse(b: &[u8; 12]) -> Result<Self> {
        let s = std::str::from_utf8(b).map_err(|_| Error::Protocol("bad version banner".into()))?;
        let rest = s.strip_prefix("RFB ").and_then(|r| r.strip_suffix('\n'));
        let Some((major, minor)) = rest.and_then(|r| r.split_once('.')) else {
            return proto(format!("bad version banner {s:?}"));
        };
        let (major, minor): (u32, u32) = match (major.parse(), minor.parse()) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return proto(format!("bad version banner {s:?}")),
        };
        Ok(match (major, minor) {
            (3, 7) => Version::V3_7,
            (3, m) if m >= 8 => Version::V3_8,
            (m, _) if m > 3 => Version::V3_8,
            _ => Version::V3_3,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub pixel_format: PixelFormat,
    pub name: String,
}

impl ServerInit {
    fn read(r: &mut impl Read) -> Result<Self> {
        let width = r.u16()?;
        let height = r.u16()?;
        let pixel_format = PixelFormat::read(r)?;
        let len = r.u32()? as usize;
        if len > 1 << 16 {
            return proto("desktop name too long");
        }
        let name = String::from_utf8_lossy(&r.read_vec(len)?).into_owned();
        Ok(ServerInit {
            width,
            height,
            pixel_format,
            name,
        })
    }

    fn write(&self, w: &mut impl Write) -> Result<()> {
        let mut b = Vec::new();
        b.put_u16(self.width)?;
        b.put_u16(self.height)?;
        self.pixel_format.write(&mut b)?;
        b.put_u32(self.name.len() as u32)?;
        b.extend_from_slice(self.name.as_bytes());
        w.write_all(&b)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientInit {
    pub shared: bool,
}

/// Runs the server side of the handshake. `password: None` means no authentication.
pub fn server_handshake<S: Read + Write>(
    s: &mut S,
    password: Option<&str>,
    init: &ServerInit,
) -> Result<(Version, ClientInit)> {
    let security = ServerSecurity { password, tickets: None };
    server_handshake_secure(s, &security, init).map(|(version, client, _)| (version, client))
}

/// What the server accepts: the VNC password (or nothing), and VNC Beach
/// account tickets if `tickets` is set.
#[derive(Clone, Copy, Default)]
pub struct ServerSecurity<'a> {
    /// `None` accepts viewers without authentication.
    pub password: Option<&'a str>,
    pub tickets: Option<&'a dyn TicketVerifier>,
}

/// Checks VNC Beach account tickets (security type
/// [`SECURITY_BEACH_TICKET`]).
pub trait TicketVerifier {
    /// The device ID the server announces, so the viewer asks for a ticket
    /// for the right machine.
    fn device(&self) -> &str;
    /// Accepts `ticket` if it was issued for this device and `nonce`.
    fn verify(&self, nonce: &[u8; TICKET_NONCE_LEN], ticket: &[u8]) -> std::result::Result<Grant, String>;
}

/// Who a ticket let in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub user: String,
    pub view_only: bool,
}

/// Like [`server_handshake`], with account tickets. Returns the ticket's
/// [`Grant`] if the viewer authenticated with one.
pub fn server_handshake_secure<S: Read + Write>(
    s: &mut S,
    security: &ServerSecurity,
    init: &ServerInit,
) -> Result<(Version, ClientInit, Option<Grant>)> {
    s.write_all(Version::V3_8.banner())?;
    s.flush()?;
    let mut banner = [0; 12];
    s.read_exact(&mut banner)?;
    let version = Version::parse(&banner)?;

    let classic = if security.password.is_some() {
        SECURITY_VNC
    } else {
        SECURITY_NONE
    };
    let chosen = if version == Version::V3_3 {
        // 3.3 lets the server pick exactly one type; tickets need 3.7+.
        s.put_u32(classic as u32)?;
        classic
    } else {
        // The standard type comes first for viewers that take the first
        // entry; ours picks the ticket by preference.
        let mut offered = vec![classic];
        if security.tickets.is_some() {
            offered.push(SECURITY_BEACH_TICKET);
        }
        s.put_u8(offered.len() as u8)?;
        s.write_all(&offered)?;
        s.flush()?;
        let chosen = s.u8()?;
        if !offered.contains(&chosen) {
            fail(s, version, "unsupported security type")?;
            return Err(Error::AuthFailed(format!("client chose security type {chosen}")));
        }
        chosen
    };

    let mut grant = None;
    match (chosen, security.password, security.tickets) {
        (SECURITY_VNC, Some(password), _) => {
            let challenge = random_challenge();
            s.write_all(&challenge)?;
            s.flush()?;
            let mut response = [0; CHALLENGE_LEN];
            s.read_exact(&mut response)?;
            if response != vnc_response(password, &challenge) {
                fail(s, version, "wrong password")?;
                return Err(Error::AuthFailed("wrong password".into()));
            }
            s.put_u32(0)?;
        }
        (SECURITY_BEACH_TICKET, _, Some(tickets)) => {
            let nonce: [u8; TICKET_NONCE_LEN] = rand::random();
            let device = tickets.device().as_bytes();
            if device.len() > u8::MAX as usize {
                return proto("device id too long");
            }
            s.write_all(&nonce)?;
            s.put_u8(device.len() as u8)?;
            s.write_all(device)?;
            s.flush()?;
            let len = s.u16()? as usize;
            let ticket = s.read_vec(len)?;
            match tickets.verify(&nonce, &ticket) {
                Ok(g) => grant = Some(g),
                Err(reason) => {
                    fail(s, version, &reason)?;
                    return Err(Error::AuthFailed(reason));
                }
            }
            s.put_u32(0)?;
        }
        _ => {
            if version == Version::V3_8 {
                s.put_u32(0)?;
            }
        }
    }
    s.flush()?;

    let shared = s.u8()? != 0;
    init.write(s)?;
    s.flush()?;
    Ok((version, ClientInit { shared }, grant))
}

fn fail(w: &mut impl Write, version: Version, reason: &str) -> Result<()> {
    w.put_u32(1)?;
    if version == Version::V3_8 {
        w.put_u32(reason.len() as u32)?;
        w.write_all(reason.as_bytes())?;
    }
    w.flush()?;
    Ok(())
}

/// Runs the client side of the handshake. Returns [`Error::PasswordRequired`]
/// if the server wants VNC authentication and `password` is `None`.
pub fn client_handshake<S: Read + Write>(s: &mut S, password: Option<&str>) -> Result<ServerInit> {
    client_handshake_with_banner(s, password).map(|(init, _)| init)
}

/// Like [`client_handshake`], but also returns the server's version banner
/// (e.g. `RFB 003.889` for macOS Screen Sharing), which tells a viewer what
/// it is talking to.
pub fn client_handshake_with_banner<S: Read + Write>(
    s: &mut S,
    password: Option<&str>,
) -> Result<(ServerInit, String)> {
    client_handshake_secure(s, password, None)
}

/// Fetches an account ticket for the device the server announced and the
/// server's nonce, e.g. from the coordination server.
pub type TicketSource<'a> = &'a mut dyn FnMut(&str, &[u8; TICKET_NONCE_LEN]) -> std::result::Result<Vec<u8>, String>;

/// Like [`client_handshake_with_banner`]; with `tickets`, an account ticket
/// is preferred whenever the server offers that.
pub fn client_handshake_secure<S: Read + Write>(
    s: &mut S,
    password: Option<&str>,
    tickets: Option<TicketSource>,
) -> Result<(ServerInit, String)> {
    let mut banner = [0; 12];
    s.read_exact(&mut banner)?;
    let version = Version::parse(&banner)?;
    s.write_all(version.banner())?;
    s.flush()?;

    let security = if version == Version::V3_3 {
        match s.u32()? {
            0 => return Err(Error::AuthFailed(read_reason(s)?)),
            t => u8::try_from(t).map_err(|_| Error::UnsupportedSecurity(vec![]))?,
        }
    } else {
        let n = s.u8()? as usize;
        if n == 0 {
            return Err(Error::AuthFailed(read_reason(s)?));
        }
        let types = s.read_vec(n)?;
        let chosen = if tickets.is_some() && types.contains(&SECURITY_BEACH_TICKET) {
            SECURITY_BEACH_TICKET
        } else if types.contains(&SECURITY_NONE) {
            SECURITY_NONE
        } else if types.contains(&SECURITY_VNC) {
            SECURITY_VNC
        } else {
            return Err(Error::UnsupportedSecurity(types));
        };
        s.put_u8(chosen)?;
        s.flush()?;
        chosen
    };

    match security {
        SECURITY_NONE => {}
        SECURITY_VNC => {
            let password = password.ok_or(Error::PasswordRequired)?;
            let mut challenge = [0; CHALLENGE_LEN];
            s.read_exact(&mut challenge)?;
            s.write_all(&vnc_response(password, &challenge))?;
            s.flush()?;
        }
        SECURITY_BEACH_TICKET => {
            let tickets = tickets.expect("chosen only with a ticket source");
            let mut nonce = [0; TICKET_NONCE_LEN];
            s.read_exact(&mut nonce)?;
            let len = s.u8()? as usize;
            let device = String::from_utf8_lossy(&s.read_vec(len)?).into_owned();
            let ticket = tickets(&device, &nonce).map_err(Error::AuthFailed)?;
            let len = u16::try_from(ticket.len()).map_err(|_| Error::Protocol("ticket too long".into()))?;
            s.put_u16(len)?;
            s.write_all(&ticket)?;
            s.flush()?;
        }
        t => return Err(Error::UnsupportedSecurity(vec![t])),
    }

    let has_result = security != SECURITY_NONE || version == Version::V3_8;
    if has_result && s.u32()? != 0 {
        let reason = if version == Version::V3_8 {
            read_reason(s)?
        } else {
            "rejected".into()
        };
        return Err(Error::AuthFailed(reason));
    }

    s.put_u8(1)?; // shared: don't kick other viewers
    s.flush()?;
    let init = ServerInit::read(s)?;
    Ok((init, String::from_utf8_lossy(&banner).trim().to_string()))
}

fn read_reason(r: &mut impl Read) -> Result<String> {
    let len = r.u32()? as usize;
    if len > 1 << 16 {
        return proto("failure reason too long");
    }
    Ok(String::from_utf8_lossy(&r.read_vec(len)?).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_standard_and_apple_banners() {
        for banner in [
            b"RFB 003.003\n",
            b"RFB 003.007\n",
            b"RFB 003.008\n",
            b"RFB 003.889\n",
        ] {
            assert!(is_server_banner(banner));
        }
        for banner in [
            b"SSH-2.0-test",
            b"HTTP/1.1 200",
            b"RFB invalid\n",
            b"RFB 003.008 ",
        ] {
            assert!(!is_server_banner(banner));
        }
    }
    use std::net::{TcpListener, TcpStream};

    fn init() -> ServerInit {
        ServerInit {
            width: 800,
            height: 600,
            pixel_format: PixelFormat::BGRX,
            name: "beach".into(),
        }
    }

    fn run(
        server_pw: Option<&'static str>,
        client_pw: Option<&'static str>,
    ) -> (Result<ServerInit>, Result<(Version, ClientInit)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            server_handshake(&mut s, server_pw, &init())
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let client = client_handshake(&mut c, client_pw);
        drop(c);
        (client, server.join().unwrap())
    }

    #[test]
    fn no_auth() {
        let (c, s) = run(None, None);
        assert_eq!(c.unwrap(), init());
        assert_eq!(s.unwrap(), (Version::V3_8, ClientInit { shared: true }));
    }

    #[test]
    fn vnc_auth_ok() {
        let (c, s) = run(Some("secret"), Some("secret"));
        assert_eq!(c.unwrap(), init());
        assert!(s.is_ok());
    }

    #[test]
    fn vnc_auth_wrong_password() {
        let (c, s) = run(Some("secret"), Some("nope"));
        assert!(matches!(c, Err(Error::AuthFailed(r)) if r == "wrong password"));
        assert!(matches!(s, Err(Error::AuthFailed(_))));
    }

    #[test]
    fn missing_password() {
        let (c, _) = run(Some("secret"), None);
        assert!(matches!(c, Err(Error::PasswordRequired)));
    }

    /// Accepts the ticket `ok:<hex nonce>` for device `dev-1`.
    struct FakeVerifier;

    impl TicketVerifier for FakeVerifier {
        fn device(&self) -> &str {
            "dev-1"
        }

        fn verify(&self, nonce: &[u8; TICKET_NONCE_LEN], ticket: &[u8]) -> std::result::Result<Grant, String> {
            if ticket == fake_ticket(nonce).as_slice() {
                Ok(Grant {
                    user: "alice".into(),
                    view_only: true,
                })
            } else {
                Err("invalid ticket".into())
            }
        }
    }

    fn fake_ticket(nonce: &[u8; TICKET_NONCE_LEN]) -> Vec<u8> {
        let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        format!("ok:{hex}").into_bytes()
    }

    type MakeTicket = fn(&str, &[u8; TICKET_NONCE_LEN]) -> std::result::Result<Vec<u8>, String>;
    type Outcome = (Result<(ServerInit, String)>, Result<(Version, ClientInit, Option<Grant>)>);

    fn run_tickets(
        password: Option<&'static str>,
        client_pw: Option<&'static str>,
        make: Option<MakeTicket>,
    ) -> Outcome {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let security = ServerSecurity {
                password,
                tickets: Some(&FakeVerifier),
            };
            server_handshake_secure(&mut s, &security, &init())
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let client = match make {
            Some(make) => {
                let mut source = |device: &str, nonce: &[u8; TICKET_NONCE_LEN]| make(device, nonce);
                client_handshake_secure(&mut c, client_pw, Some(&mut source))
            }
            None => client_handshake_secure(&mut c, client_pw, None),
        };
        drop(c);
        (client, server.join().unwrap())
    }

    #[test]
    fn ticket_accepted() {
        let (c, s) = run_tickets(
            Some("secret"),
            None,
            Some(|device, nonce| {
                assert_eq!(device, "dev-1");
                Ok(fake_ticket(nonce))
            }),
        );
        assert_eq!(c.unwrap().0, init());
        let (_, _, grant) = s.unwrap();
        assert_eq!(
            grant,
            Some(Grant {
                user: "alice".into(),
                view_only: true
            })
        );
    }

    #[test]
    fn ticket_rejected() {
        let (c, s) = run_tickets(Some("secret"), None, Some(|_, _| Ok(b"forged".to_vec())));
        assert!(matches!(c, Err(Error::AuthFailed(r)) if r == "invalid ticket"));
        assert!(matches!(s, Err(Error::AuthFailed(_))));
    }

    #[test]
    fn ticket_source_failure_is_reported() {
        let (c, _) = run_tickets(Some("secret"), None, Some(|_, _| Err("offline".into())));
        assert!(matches!(c, Err(Error::AuthFailed(r)) if r == "offline"));
    }

    #[test]
    fn password_still_works_next_to_tickets() {
        let (c, s) = run_tickets(Some("secret"), Some("secret"), None);
        assert!(c.is_ok());
        assert_eq!(s.unwrap().2, None);
    }

    #[test]
    fn standard_type_is_offered_first() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let security = ServerSecurity {
                password: Some("secret"),
                tickets: Some(&FakeVerifier),
            };
            let _ = server_handshake_secure(&mut s, &security, &init());
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let mut banner = [0; 12];
        c.read_exact(&mut banner).unwrap();
        c.write_all(b"RFB 003.008\n").unwrap();
        let mut types = [0; 3];
        c.read_exact(&mut types).unwrap();
        assert_eq!(types, [2, SECURITY_VNC, SECURITY_BEACH_TICKET]);
    }

    #[test]
    fn parses_odd_banners() {
        assert_eq!(Version::parse(b"RFB 003.889\n").unwrap(), Version::V3_8);
        assert_eq!(Version::parse(b"RFB 003.006\n").unwrap(), Version::V3_3);
        assert!(Version::parse(b"HTTP/1.1 200").is_err());
    }
}
