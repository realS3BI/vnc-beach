//! Normal-phase protocol messages (RFC 6143 §7.5, §7.6).

use std::io::{Read, Write};

use crate::io::{ReadExt, WriteExt};
use crate::{proto, PixelFormat, Result};

/// Upper bound for cut text so a peer can't make us allocate gigabytes.
const MAX_CUT_TEXT: usize = 16 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    SetPixelFormat(PixelFormat),
    SetEncodings(Vec<i32>),
    FramebufferUpdateRequest {
        incremental: bool,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    KeyEvent {
        down: bool,
        key: u32,
    },
    PointerEvent {
        buttons: u8,
        x: u16,
        y: u16,
    },
    ClientCutText(String),
    FileTransfer(crate::files::Request),
    /// Request for a new framebuffer size and layout (sent by viewers that
    /// saw an [`EXTENDED_DESKTOP_SIZE`](crate::encoding::EXTENDED_DESKTOP_SIZE)
    /// rectangle). Servers that can't resize answer with status
    /// [`DESKTOP_SIZE_PROHIBITED`].
    SetDesktopSize {
        width: u16,
        height: u16,
        screens: Vec<Screen>,
    },
}

impl ClientMessage {
    pub fn read(r: &mut impl Read) -> Result<Self> {
        Ok(match r.u8()? {
            0 => {
                r.skip(3)?;
                ClientMessage::SetPixelFormat(PixelFormat::read(r)?)
            }
            2 => {
                r.skip(1)?;
                let n = r.u16()?;
                let encodings = (0..n).map(|_| r.i32()).collect::<std::io::Result<_>>()?;
                ClientMessage::SetEncodings(encodings)
            }
            3 => ClientMessage::FramebufferUpdateRequest {
                incremental: r.u8()? != 0,
                x: r.u16()?,
                y: r.u16()?,
                width: r.u16()?,
                height: r.u16()?,
            },
            4 => {
                let down = r.u8()? != 0;
                r.skip(2)?;
                ClientMessage::KeyEvent { down, key: r.u32()? }
            }
            5 => ClientMessage::PointerEvent {
                buttons: r.u8()?,
                x: r.u16()?,
                y: r.u16()?,
            },
            6 => {
                r.skip(3)?;
                ClientMessage::ClientCutText(read_cut_text(r)?)
            }
            crate::files::MESSAGE_TYPE => ClientMessage::FileTransfer(crate::files::Request::read(r)?),
            251 => {
                r.skip(1)?;
                let width = r.u16()?;
                let height = r.u16()?;
                let n = r.u8()?;
                r.skip(1)?;
                let screens = (0..n).map(|_| Screen::read(r)).collect::<Result<_>>()?;
                ClientMessage::SetDesktopSize {
                    width,
                    height,
                    screens,
                }
            }
            t => return proto(format!("unknown client message type {t}")),
        })
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        let mut b = Vec::with_capacity(20);
        match self {
            ClientMessage::SetPixelFormat(pf) => {
                b.extend_from_slice(&[0, 0, 0, 0]);
                pf.write(&mut b)?;
            }
            ClientMessage::SetEncodings(encs) => {
                b.extend_from_slice(&[2, 0]);
                b.put_u16(encs.len() as u16)?;
                for &e in encs {
                    b.put_i32(e)?;
                }
            }
            &ClientMessage::FramebufferUpdateRequest {
                incremental,
                x,
                y,
                width,
                height,
            } => {
                b.extend_from_slice(&[3, incremental as u8]);
                for v in [x, y, width, height] {
                    b.put_u16(v)?;
                }
            }
            &ClientMessage::KeyEvent { down, key } => {
                b.extend_from_slice(&[4, down as u8, 0, 0]);
                b.put_u32(key)?;
            }
            &ClientMessage::PointerEvent { buttons, x, y } => {
                b.extend_from_slice(&[5, buttons]);
                b.put_u16(x)?;
                b.put_u16(y)?;
            }
            ClientMessage::FileTransfer(request) => return request.write(w),
            ClientMessage::ClientCutText(text) => {
                b.extend_from_slice(&[6, 0, 0, 0]);
                write_cut_text(&mut b, text)?;
            }
            ClientMessage::SetDesktopSize {
                width,
                height,
                screens,
            } => {
                b.extend_from_slice(&[251, 0]);
                b.put_u16(*width)?;
                b.put_u16(*height)?;
                b.extend_from_slice(&[screens.len() as u8, 0]);
                for s in screens {
                    s.write(&mut b)?;
                }
            }
        }
        w.write_all(&b)?;
        Ok(())
    }
}

/// One physical display inside the framebuffer (ExtendedDesktopSize).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub id: u32,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub flags: u32,
}

impl Screen {
    pub fn read(r: &mut impl Read) -> Result<Self> {
        Ok(Screen {
            id: r.u32()?,
            x: r.u16()?,
            y: r.u16()?,
            width: r.u16()?,
            height: r.u16()?,
            flags: r.u32()?,
        })
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        w.put_u32(self.id)?;
        for v in [self.x, self.y, self.width, self.height] {
            w.put_u16(v)?;
        }
        w.put_u32(self.flags)?;
        Ok(())
    }
}

/// `x` of an ExtendedDesktopSize rectangle: why the layout is being sent.
pub const DESKTOP_SIZE_REASON_SERVER: u16 = 0;
pub const DESKTOP_SIZE_REASON_CLIENT: u16 = 1;
pub const DESKTOP_SIZE_REASON_OTHER_CLIENT: u16 = 2;
/// `y` of an ExtendedDesktopSize rectangle: the outcome of a request.
pub const DESKTOP_SIZE_OK: u16 = 0;
pub const DESKTOP_SIZE_PROHIBITED: u16 = 1;

/// Writes an ExtendedDesktopSize rectangle (header and payload). The caller
/// has already written the FramebufferUpdate header counting it.
pub fn write_extended_desktop_size(
    w: &mut impl Write,
    reason: u16,
    status: u16,
    width: u16,
    height: u16,
    screens: &[Screen],
) -> Result<()> {
    let mut b = Vec::with_capacity(16 + screens.len() * 16);
    Rect {
        x: reason,
        y: status,
        width,
        height,
        encoding: crate::encoding::EXTENDED_DESKTOP_SIZE,
    }
    .write(&mut b)?;
    b.extend_from_slice(&[screens.len() as u8, 0, 0, 0]);
    for s in screens {
        s.write(&mut b)?;
    }
    w.write_all(&b)?;
    Ok(())
}

/// Reads the payload that follows an ExtendedDesktopSize rectangle header.
pub fn read_screens(r: &mut impl Read) -> Result<Vec<Screen>> {
    let n = r.u8()?;
    r.skip(3)?;
    (0..n).map(|_| Screen::read(r)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub encoding: i32,
}

impl Rect {
    pub fn read(r: &mut impl Read) -> Result<Self> {
        Ok(Rect {
            x: r.u16()?,
            y: r.u16()?,
            width: r.u16()?,
            height: r.u16()?,
            encoding: r.i32()?,
        })
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        for v in [self.x, self.y, self.width, self.height] {
            w.put_u16(v)?;
        }
        w.put_i32(self.encoding)?;
        Ok(())
    }
}

/// Header of a FramebufferUpdate; the caller writes `count` rects after it.
pub fn write_framebuffer_update_header(w: &mut impl Write, count: u16) -> Result<()> {
    w.write_all(&[0, 0])?;
    w.put_u16(count)?;
    Ok(())
}

pub fn write_server_cut_text(w: &mut impl Write, text: &str) -> Result<()> {
    let mut b = vec![3, 0, 0, 0];
    write_cut_text(&mut b, text)?;
    w.write_all(&b)?;
    Ok(())
}

pub fn write_bell(w: &mut impl Write) -> Result<()> {
    w.write_all(&[2])?;
    Ok(())
}

/// Server-to-client message header. Rectangle payloads of a FramebufferUpdate
/// are decoded by the caller because they depend on the encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    FramebufferUpdate { rects: u16 },
    SetColourMapEntries,
    Bell,
    ServerCutText(String),
    FileTransfer(crate::files::Response),
}

impl ServerMessage {
    pub fn read(r: &mut impl Read) -> Result<Self> {
        Ok(match r.u8()? {
            0 => {
                r.skip(1)?;
                ServerMessage::FramebufferUpdate { rects: r.u16()? }
            }
            1 => {
                r.skip(1)?;
                let _first = r.u16()?;
                let n = r.u16()? as usize;
                r.skip(n * 6)?;
                ServerMessage::SetColourMapEntries
            }
            2 => ServerMessage::Bell,
            3 => {
                r.skip(3)?;
                ServerMessage::ServerCutText(read_cut_text(r)?)
            }
            crate::files::MESSAGE_TYPE => ServerMessage::FileTransfer(crate::files::Response::read(r)?),
            t => return proto(format!("unknown server message type {t}")),
        })
    }
}

/// Cut text is Latin-1 on the wire. A negative length signals the extended
/// clipboard format, which we don't negotiate, so it's skipped.
fn read_cut_text(r: &mut impl Read) -> Result<String> {
    let len = r.i32()?;
    if len < 0 {
        r.skip(len.unsigned_abs() as usize)?;
        return Ok(String::new());
    }
    let len = len as usize;
    if len > MAX_CUT_TEXT {
        r.skip(len)?;
        return Ok(String::new());
    }
    Ok(r.read_vec(len)?.into_iter().map(char::from).collect())
}

fn write_cut_text(b: &mut Vec<u8>, text: &str) -> Result<()> {
    let latin1: Vec<u8> = text
        .replace("\r\n", "\n")
        .chars()
        .map(|c| u8::try_from(c as u32).unwrap_or(b'?'))
        .collect();
    b.put_u32(latin1.len() as u32)?;
    b.extend_from_slice(&latin1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_roundtrip() {
        let msgs = [
            ClientMessage::SetPixelFormat(PixelFormat::BGRX),
            ClientMessage::SetEncodings(vec![16, 0, -223]),
            ClientMessage::FramebufferUpdateRequest {
                incremental: true,
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
            ClientMessage::KeyEvent {
                down: true,
                key: 0xff0d,
            },
            ClientMessage::PointerEvent {
                buttons: 1,
                x: 640,
                y: 480,
            },
            ClientMessage::ClientCutText("Grüße".into()),
            ClientMessage::SetDesktopSize {
                width: 3000,
                height: 1200,
                screens: vec![
                    Screen {
                        id: 7,
                        x: 0,
                        y: 0,
                        width: 1800,
                        height: 1200,
                        flags: 0,
                    },
                    Screen {
                        id: 8,
                        x: 1800,
                        y: 100,
                        width: 1200,
                        height: 900,
                        flags: 0,
                    },
                ],
            },
        ];
        for m in msgs {
            let mut buf = Vec::new();
            m.write(&mut buf).unwrap();
            let mut r = &buf[..];
            assert_eq!(ClientMessage::read(&mut r).unwrap(), m);
            assert!(r.is_empty());
        }
    }

    #[test]
    fn extended_desktop_size_roundtrip() {
        let screens = [
            Screen {
                id: 1,
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
                flags: 0,
            },
            Screen {
                id: 2,
                x: 2560,
                y: 200,
                width: 1920,
                height: 1080,
                flags: 0,
            },
        ];
        let mut buf = Vec::new();
        write_framebuffer_update_header(&mut buf, 1).unwrap();
        write_extended_desktop_size(
            &mut buf,
            DESKTOP_SIZE_REASON_SERVER,
            DESKTOP_SIZE_OK,
            4480,
            1440,
            &screens,
        )
        .unwrap();
        let mut r = &buf[..];
        assert_eq!(
            ServerMessage::read(&mut r).unwrap(),
            ServerMessage::FramebufferUpdate { rects: 1 }
        );
        let rect = Rect::read(&mut r).unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 0,
                y: 0,
                width: 4480,
                height: 1440,
                encoding: crate::encoding::EXTENDED_DESKTOP_SIZE,
            }
        );
        assert_eq!(read_screens(&mut r).unwrap(), screens);
        assert!(r.is_empty());
    }

    #[test]
    fn cut_text_replaces_non_latin1() {
        let mut buf = Vec::new();
        write_server_cut_text(&mut buf, "a€b").unwrap();
        let msg = ServerMessage::read(&mut &buf[..]).unwrap();
        assert_eq!(msg, ServerMessage::ServerCutText("a?b".into()));
    }
}
