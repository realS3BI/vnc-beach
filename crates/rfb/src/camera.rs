//! VNC Beach camera v1 (BCM1): the viewer sends its webcam to the server, which
//! offers it to local apps as a camera. Negotiated with BEACH_CAMERA before
//! message type 241 is used. These experimental numbers are not IANA assignments.
//! Framing as in file transfer: type 241, `BCM1`, a big-endian u32 payload
//! length, then the payload, which starts with a u8 operation.
//!
//! The client offers encoding 0x42434D01 in SetEncodings. Only a server that
//! can show a camera sends Hello; the client MUST wait for it before sending
//! type 241. A new SetEncodings without the encoding revokes support.
//!
//! Server messages: 0 Hello(u16 width, u16 height, u8 fps): the largest picture
//! the server shows; 1 Start: an application reads the camera, send video
//! beginning with a key frame; 2 Stop: nobody reads it, stop sending;
//! 3 KeyFrame: the decoder lost sync, send a key frame next.
//! Client messages: 1 Sharing(u8 0/1): whether the user shares a camera, sent
//! after Hello and on every change; 2 Video(u8 flags, u64 timestamp in
//! microseconds, u32 length + data): one H.264 access unit in Annex B format,
//! flag bit 0 marks a key frame (IDR with SPS and PPS). The client sends video
//! only between Start and Stop while it shares, never larger than Hello.

use crate::io::{ReadExt, WriteExt};
use crate::{proto, Result};
use std::io::{Cursor, Read, Write};

pub const MESSAGE_TYPE: u8 = 241;
const MAGIC: &[u8; 4] = b"BCM1";
/// Room for a key frame at 1080p and high quality.
pub const MAX_VIDEO: usize = 4 * 1024 * 1024;
const MAX_FRAME: usize = MAX_VIDEO + 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    Hello { width: u16, height: u16, fps: u8 },
    Start,
    Stop,
    KeyFrame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    pub key_frame: bool,
    pub timestamp_us: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Sharing(bool),
    Video(Video),
}

fn read_frame(r: &mut impl Read) -> Result<Cursor<Vec<u8>>> {
    let mut magic = [0; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return proto("invalid camera signature");
    }
    let len = r.u32()? as usize;
    if len == 0 || len > MAX_FRAME {
        return proto("invalid camera frame length");
    }
    Ok(Cursor::new(r.read_vec(len)?))
}

fn write_frame(w: &mut impl Write, payload: &[u8]) -> Result<()> {
    w.write_all(&[MESSAGE_TYPE])?;
    w.write_all(MAGIC)?;
    w.put_u32(payload.len() as u32)?;
    w.write_all(payload)?;
    Ok(())
}

fn finish<T>(r: Cursor<Vec<u8>>, value: T) -> Result<T> {
    if r.position() != r.get_ref().len() as u64 {
        return proto("trailing camera data");
    }
    Ok(value)
}

impl ServerMessage {
    /// Reads the frame after the message type byte.
    pub fn read(r: &mut impl Read) -> Result<Self> {
        let mut r = read_frame(r)?;
        let value = match r.u8()? {
            0 => {
                let (width, height, fps) = (r.u16()?, r.u16()?, r.u8()?);
                if width == 0 || height == 0 || fps == 0 {
                    return proto("invalid camera size");
                }
                Self::Hello { width, height, fps }
            }
            1 => Self::Start,
            2 => Self::Stop,
            3 => Self::KeyFrame,
            _ => return proto("unknown camera message"),
        };
        finish(r, value)
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        let mut b = Vec::new();
        match self {
            Self::Hello { width, height, fps } => {
                b.push(0);
                b.put_u16(*width)?;
                b.put_u16(*height)?;
                b.push(*fps);
            }
            Self::Start => b.push(1),
            Self::Stop => b.push(2),
            Self::KeyFrame => b.push(3),
        }
        write_frame(w, &b)
    }
}

impl ClientMessage {
    /// Reads the frame after the message type byte.
    pub fn read(r: &mut impl Read) -> Result<Self> {
        let mut r = read_frame(r)?;
        let value = match r.u8()? {
            1 => Self::Sharing(match r.u8()? {
                0 => false,
                1 => true,
                _ => return proto("invalid camera sharing state"),
            }),
            2 => {
                let flags = r.u8()?;
                let mut ts = [0; 8];
                r.read_exact(&mut ts)?;
                let len = r.u32()? as usize;
                if len == 0 || len > MAX_VIDEO {
                    return proto("invalid camera video length");
                }
                Self::Video(Video {
                    key_frame: flags & 1 != 0,
                    timestamp_us: u64::from_be_bytes(ts),
                    data: r.read_vec(len)?,
                })
            }
            _ => return proto("unknown camera message"),
        };
        finish(r, value)
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        let mut b = Vec::new();
        match self {
            Self::Sharing(on) => b.extend_from_slice(&[1, u8::from(*on)]),
            Self::Video(v) => {
                if v.data.is_empty() || v.data.len() > MAX_VIDEO {
                    return proto("invalid camera video length");
                }
                b.reserve(14 + v.data.len());
                b.push(2);
                b.push(u8::from(v.key_frame));
                b.write_all(&v.timestamp_us.to_be_bytes())?;
                b.put_u32(v.data.len() as u32)?;
                b.write_all(&v.data)?;
            }
        }
        write_frame(w, &b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for msg in [
            ServerMessage::Hello {
                width: 1280,
                height: 720,
                fps: 30,
            },
            ServerMessage::Start,
            ServerMessage::Stop,
            ServerMessage::KeyFrame,
        ] {
            let mut b = Vec::new();
            msg.write(&mut b).unwrap();
            assert_eq!(b[0], MESSAGE_TYPE);
            assert_eq!(ServerMessage::read(&mut &b[1..]).unwrap(), msg);
        }
        for msg in [
            ClientMessage::Sharing(true),
            ClientMessage::Sharing(false),
            ClientMessage::Video(Video {
                key_frame: true,
                timestamp_us: u64::MAX,
                data: vec![0, 0, 0, 1, 0x67, 0x42],
            }),
        ] {
            let mut b = Vec::new();
            msg.write(&mut b).unwrap();
            assert_eq!(b[0], MESSAGE_TYPE);
            assert_eq!(ClientMessage::read(&mut &b[1..]).unwrap(), msg);
        }
    }

    #[test]
    fn rejects_malformed_frames() {
        assert!(ClientMessage::read(&mut &b"BFT1\0\0\0\x02\x01\x01"[..]).is_err());
        assert!(ClientMessage::read(&mut &b"BCM1\0\0\0\x02\x01\x02"[..]).is_err());
        assert!(ClientMessage::read(&mut &b"BCM1\0\0\0\x03\x01\x01\x00"[..]).is_err());
        assert!(ClientMessage::read(&mut &b"BCM1\xff\xff\xff\xff"[..]).is_err());
        assert!(ServerMessage::read(&mut &b"BCM1\0\0\0\x06\0\0\0\x02\xd0\x1e"[..]).is_err());
        let empty = ClientMessage::Video(Video {
            key_frame: false,
            timestamp_us: 0,
            data: Vec::new(),
        });
        assert!(empty.write(&mut Vec::new()).is_err());
        let mut b = Vec::new();
        ClientMessage::Video(Video {
            key_frame: false,
            timestamp_us: 5,
            data: vec![1, 2, 3],
        })
        .write(&mut b)
        .unwrap();
        for len in 1..b.len() {
            assert!(ClientMessage::read(&mut &b[1..len]).is_err());
        }
    }
}
