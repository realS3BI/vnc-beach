//! Protocol version, security and initialisation phases (RFC 6143 §7.1–7.3).

use std::io::{Read, Write};

use crate::auth::{random_challenge, vnc_response, CHALLENGE_LEN};
use crate::io::{ReadExt, WriteExt};
use crate::{proto, Error, PixelFormat, Result};

pub const SECURITY_NONE: u8 = 1;
pub const SECURITY_VNC: u8 = 2;

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
    s.write_all(Version::V3_8.banner())?;
    s.flush()?;
    let mut banner = [0; 12];
    s.read_exact(&mut banner)?;
    let version = Version::parse(&banner)?;

    let offered = if password.is_some() {
        SECURITY_VNC
    } else {
        SECURITY_NONE
    };
    if version == Version::V3_3 {
        s.put_u32(offered as u32)?;
    } else {
        s.write_all(&[1, offered])?;
        s.flush()?;
        let chosen = s.u8()?;
        if chosen != offered {
            fail(s, version, "unsupported security type")?;
            return Err(Error::AuthFailed(format!("client chose security type {chosen}")));
        }
    }

    if let Some(password) = password {
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
    } else if version == Version::V3_8 {
        s.put_u32(0)?;
    }
    s.flush()?;

    let shared = s.u8()? != 0;
    init.write(s)?;
    s.flush()?;
    Ok((version, ClientInit { shared }))
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
        let chosen = if types.contains(&SECURITY_NONE) {
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
        t => return Err(Error::UnsupportedSecurity(vec![t])),
    }

    let has_result = security == SECURITY_VNC || version == Version::V3_8;
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

    #[test]
    fn parses_odd_banners() {
        assert_eq!(Version::parse(b"RFB 003.889\n").unwrap(), Version::V3_8);
        assert_eq!(Version::parse(b"RFB 003.006\n").unwrap(), Version::V3_3);
        assert!(Version::parse(b"HTTP/1.1 200").is_err());
    }
}
