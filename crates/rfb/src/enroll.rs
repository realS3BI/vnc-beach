//! VNC Beach enrollment v1 (BEN1): a viewer adds the computer it is
//! connected to to a VNC Beach account. Negotiated with BEACH_ENROLL before
//! message type 242 is used. These experimental numbers are not IANA
//! assignments. Framing as in file transfer: type 242, `BEN1`, a big-endian
//! u32 payload length, then the payload: a u8 operation and UTF-8 JSON.
//!
//! The client offers encoding 0x42454E01 in SetEncodings. Only a server that
//! can be added to an account, and only for a viewer that may control it,
//! sends Hello; the client MUST wait for it before sending type 242. A new
//! SetEncodings without the encoding revokes support.
//!
//! Server messages: 0 Hello(json): the computer and its current account
//! registration; 1 Result(json): the outcome of an Enroll.
//! Client messages: 1 Enroll(json): the registration the viewer obtained
//! from the coordination server for this computer. The JSON bodies are
//! defined in `beach_account::api` (`EnrollHello`, `Enrollment`,
//! `EnrollResult`); this module only frames them.

use crate::io::{ReadExt, WriteExt};
use crate::{proto, Result};
use std::io::{Cursor, Read, Write};

pub const MESSAGE_TYPE: u8 = 242;
const MAGIC: &[u8; 4] = b"BEN1";
const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    Hello(String),
    Result(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Enroll(String),
}

fn read_frame(r: &mut impl Read) -> Result<(u8, String)> {
    let mut magic = [0; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return proto("invalid enrollment signature");
    }
    let len = r.u32()? as usize;
    if len == 0 || len > MAX_FRAME {
        return proto("invalid enrollment frame length");
    }
    let mut r = Cursor::new(r.read_vec(len)?);
    let op = r.u8()?;
    let mut json = Vec::new();
    r.read_to_end(&mut json)?;
    match String::from_utf8(json) {
        Ok(json) => Ok((op, json)),
        Err(_) => proto("enrollment data is not UTF-8"),
    }
}

fn write_frame(w: &mut impl Write, op: u8, json: &str) -> Result<()> {
    if json.len() + 1 > MAX_FRAME {
        return proto("enrollment data too long");
    }
    w.write_all(&[MESSAGE_TYPE])?;
    w.write_all(MAGIC)?;
    w.put_u32(json.len() as u32 + 1)?;
    w.write_all(&[op])?;
    w.write_all(json.as_bytes())?;
    Ok(())
}

impl ServerMessage {
    /// Reads the frame after the message type byte.
    pub fn read(r: &mut impl Read) -> Result<Self> {
        match read_frame(r)? {
            (0, json) => Ok(Self::Hello(json)),
            (1, json) => Ok(Self::Result(json)),
            _ => proto("unknown enrollment message"),
        }
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        match self {
            Self::Hello(json) => write_frame(w, 0, json),
            Self::Result(json) => write_frame(w, 1, json),
        }
    }
}

impl ClientMessage {
    /// Reads the frame after the message type byte.
    pub fn read(r: &mut impl Read) -> Result<Self> {
        match read_frame(r)? {
            (1, json) => Ok(Self::Enroll(json)),
            _ => proto("unknown enrollment message"),
        }
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        match self {
            Self::Enroll(json) => write_frame(w, 1, json),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        for msg in [ServerMessage::Hello(r#"{"name":"Mac"}"#.into()), ServerMessage::Result("{}".into())] {
            let mut b = Vec::new();
            msg.write(&mut b).unwrap();
            assert_eq!(b[0], MESSAGE_TYPE);
            assert_eq!(ServerMessage::read(&mut &b[1..]).unwrap(), msg);
        }
        let msg = ClientMessage::Enroll(r#"{"id":"d1"}"#.into());
        let mut b = Vec::new();
        msg.write(&mut b).unwrap();
        assert_eq!(ClientMessage::read(&mut &b[1..]).unwrap(), msg);
    }

    #[test]
    fn rejects_bad_frames() {
        assert!(ServerMessage::read(&mut &b"XXXX\0\0\0\x01\0"[..]).is_err());
        assert!(ServerMessage::read(&mut &b"BEN1\0\0\0\0"[..]).is_err());
        assert!(ServerMessage::read(&mut &b"BEN1\0\x10\0\0"[..]).is_err());
        assert!(ClientMessage::read(&mut &b"BEN1\0\0\0\x01\0"[..]).is_err());
        assert!(ClientMessage::read(&mut &b"BEN1\0\0\0\x02\x01\xff"[..]).is_err());
    }
}
