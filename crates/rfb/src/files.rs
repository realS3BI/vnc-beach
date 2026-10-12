//! VNC Beach file transfer v1. Negotiated with BEACH_FILE_TRANSFER before
//! message type 240 is used. These experimental numbers are not IANA assignments.
//! Every message has `BFT1`, a big-endian u32 payload length, then the payload.
//! Paths are UTF-8, relative to the server's home directory, separated by `/`.
//!
//! The client offers encoding 0x42465401 in SetEncodings. Only an accepting
//! server sends Hello; the client MUST wait for it before sending type 240.
//! A server in view-only mode does not accept the extension. Each connection
//! has one transfer and one outstanding request. Every request gets one reply.
//! A new SetEncodings without this encoding revokes support and cancels work.
//!
//! Payloads start with a u8 operation. Strings are u16 byte length + UTF-8;
//! data is u32 byte length + bytes; sizes are u64. All integers are big-endian.
//! Requests: 1 List(path), 2 ReadStart(path), 3 ReadChunk, 4 WriteStart(path,
//! size), 5 WriteChunk(data), 6 WriteFinish, 7 Cancel.
//! Replies: 0 Hello(root display name, initial relative directory), 1 Directory
//! (path, u16 count, entries of name/u8 directory flag/u64 size), 2 Started(size),
//! 3 Data(data), 4 Ok, 5 Failed(u8 code). Failure codes 0..6 are Denied, NotFound,
//! Exists, Invalid, Io, Busy, TooLarge. Failure and Cancel discard active work.
//! WriteStart/WriteChunk/WriteFinish/Cancel return Ok; ReadStart returns Started.
//! ReadChunk returns up to 64 KiB, exactly the announced total across all reads.
//! Cancel closes a finished download. WriteFinish atomically publishes a file
//! without replacing an existing destination. Disconnect discards partial files.
//!
//! See RFC 6143 section 7.5.2 for extension negotiation. The identifiers are
//! experimental, not registered as a general-purpose VNC file-transfer format.

use crate::io::{ReadExt, WriteExt};
use crate::{proto, Result};
use std::io::{Cursor, Read, Write};

pub const MESSAGE_TYPE: u8 = 240;
pub const CHUNK_SIZE: usize = 64 * 1024;
pub const MAX_ENTRIES: usize = 4096;
const MAX_FRAME: usize = 1024 * 1024;
const MAX_STRING: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Denied,
    NotFound,
    Exists,
    Invalid,
    Io,
    Busy,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    List(String),
    ReadStart(String),
    ReadChunk,
    WriteStart { path: String, size: u64 },
    WriteChunk(Vec<u8>),
    WriteFinish,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Hello { root: String, initial: String },
    Directory { path: String, entries: Vec<Entry> },
    Started { size: u64 },
    Data(Vec<u8>),
    Ok,
    Failed(Failure),
}

fn read_frame(r: &mut impl Read) -> Result<Cursor<Vec<u8>>> {
    let mut magic = [0; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"BFT1" {
        return proto("invalid file transfer signature");
    }
    let len = r.u32()? as usize;
    if len == 0 || len > MAX_FRAME {
        return proto("invalid file transfer frame length");
    }
    Ok(Cursor::new(r.read_vec(len)?))
}

fn write_frame(w: &mut impl Write, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_FRAME {
        return proto("file transfer frame too large");
    }
    w.write_all(&[MESSAGE_TYPE])?;
    w.write_all(b"BFT1")?;
    w.put_u32(payload.len() as u32)?;
    w.write_all(payload)?;
    Ok(())
}

fn string(r: &mut impl Read) -> Result<String> {
    let len = r.u16()? as usize;
    if len > MAX_STRING {
        return proto("file transfer string too long");
    }
    String::from_utf8(r.read_vec(len)?)
        .map_err(|_| crate::Error::Protocol("invalid file transfer UTF-8".into()))
}

fn put_string(w: &mut impl Write, s: &str) -> Result<()> {
    if s.len() > MAX_STRING {
        return proto("file transfer string too long");
    }
    w.put_u16(s.len() as u16)?;
    w.write_all(s.as_bytes())?;
    Ok(())
}

fn u64_value(r: &mut impl Read) -> Result<u64> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

fn data(r: &mut impl Read) -> Result<Vec<u8>> {
    let len = r.u32()? as usize;
    if len > CHUNK_SIZE {
        return proto("file transfer chunk too large");
    }
    Ok(r.read_vec(len)?)
}

fn put_data(w: &mut impl Write, bytes: &[u8]) -> Result<()> {
    if bytes.len() > CHUNK_SIZE {
        return proto("file transfer chunk too large");
    }
    w.put_u32(bytes.len() as u32)?;
    w.write_all(bytes)?;
    Ok(())
}

fn finish<T>(r: Cursor<Vec<u8>>, value: T) -> Result<T> {
    if r.position() != r.get_ref().len() as u64 {
        return proto("trailing file transfer data");
    }
    Ok(value)
}

impl Request {
    /// Reads the frame after the message type byte.
    pub fn read(r: &mut impl Read) -> Result<Self> {
        let mut r = read_frame(r)?;
        let value = match r.u8()? {
            1 => Self::List(string(&mut r)?),
            2 => Self::ReadStart(string(&mut r)?),
            3 => Self::ReadChunk,
            4 => Self::WriteStart {
                path: string(&mut r)?,
                size: u64_value(&mut r)?,
            },
            5 => Self::WriteChunk(data(&mut r)?),
            6 => Self::WriteFinish,
            7 => Self::Cancel,
            _ => return proto("unknown file transfer request"),
        };
        finish(r, value)
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        let mut b = Vec::new();
        match self {
            Self::List(path) => {
                b.push(1);
                put_string(&mut b, path)?;
            }
            Self::ReadStart(path) => {
                b.push(2);
                put_string(&mut b, path)?;
            }
            Self::ReadChunk => b.push(3),
            Self::WriteStart { path, size } => {
                b.push(4);
                put_string(&mut b, path)?;
                b.write_all(&size.to_be_bytes())?;
            }
            Self::WriteChunk(bytes) => {
                b.push(5);
                put_data(&mut b, bytes)?;
            }
            Self::WriteFinish => b.push(6),
            Self::Cancel => b.push(7),
        }
        write_frame(w, &b)
    }
}

impl Response {
    pub fn read(r: &mut impl Read) -> Result<Self> {
        let mut r = read_frame(r)?;
        let value = match r.u8()? {
            0 => Self::Hello {
                root: string(&mut r)?,
                initial: string(&mut r)?,
            },
            1 => {
                let path = string(&mut r)?;
                let len = r.u16()? as usize;
                if len > MAX_ENTRIES {
                    return proto("too many file transfer entries");
                }
                let mut entries = Vec::with_capacity(len);
                for _ in 0..len {
                    let name = string(&mut r)?;
                    let directory = match r.u8()? {
                        0 => false,
                        1 => true,
                        _ => return proto("invalid file type"),
                    };
                    entries.push(Entry {
                        name,
                        directory,
                        size: u64_value(&mut r)?,
                    });
                }
                Self::Directory { path, entries }
            }
            2 => Self::Started {
                size: u64_value(&mut r)?,
            },
            3 => Self::Data(data(&mut r)?),
            4 => Self::Ok,
            5 => Self::Failed(match r.u8()? {
                0 => Failure::Denied,
                1 => Failure::NotFound,
                2 => Failure::Exists,
                3 => Failure::Invalid,
                4 => Failure::Io,
                5 => Failure::Busy,
                6 => Failure::TooLarge,
                _ => return proto("unknown file transfer failure"),
            }),
            _ => return proto("unknown file transfer response"),
        };
        finish(r, value)
    }

    pub fn write(&self, w: &mut impl Write) -> Result<()> {
        let mut b = Vec::new();
        match self {
            Self::Hello { root, initial } => {
                b.push(0);
                put_string(&mut b, root)?;
                put_string(&mut b, initial)?;
            }
            Self::Directory { path, entries } => {
                if entries.len() > MAX_ENTRIES {
                    return proto("too many file transfer entries");
                }
                b.push(1);
                put_string(&mut b, path)?;
                b.put_u16(entries.len() as u16)?;
                for entry in entries {
                    put_string(&mut b, &entry.name)?;
                    b.push(u8::from(entry.directory));
                    b.write_all(&entry.size.to_be_bytes())?;
                    if b.len() > MAX_FRAME {
                        return proto("directory listing too large");
                    }
                }
            }
            Self::Started { size } => {
                b.push(2);
                b.write_all(&size.to_be_bytes())?;
            }
            Self::Data(bytes) => {
                b.push(3);
                put_data(&mut b, bytes)?;
            }
            Self::Ok => b.push(4),
            Self::Failed(code) => {
                b.push(5);
                b.push(match code {
                    Failure::Denied => 0,
                    Failure::NotFound => 1,
                    Failure::Exists => 2,
                    Failure::Invalid => 3,
                    Failure::Io => 4,
                    Failure::Busy => 5,
                    Failure::TooLarge => 6,
                });
            }
        }
        write_frame(w, &b)
    }
}

/// A portable path component. Excludes traversal, Windows streams/device names,
/// and ambiguous trailing dots/spaces even when the sender runs on macOS.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

pub fn valid_path(path: &str) -> bool {
    path.len() <= MAX_STRING && (path.is_empty() || path.split('/').all(valid_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trips_requests_and_responses() {
        for request in [
            Request::List("Downloads/Übung".into()),
            Request::ReadStart("a".into()),
            Request::ReadChunk,
            Request::WriteStart {
                path: "x".into(),
                size: u64::MAX,
            },
            Request::WriteChunk(vec![0, 255, 17]),
            Request::WriteFinish,
            Request::Cancel,
        ] {
            let mut bytes = Vec::new();
            request.write(&mut bytes).unwrap();
            assert_eq!(bytes[0], MESSAGE_TYPE);
            assert_eq!(Request::read(&mut &bytes[1..]).unwrap(), request);
        }
        for response in [
            Response::Hello {
                root: "C:\\Users\\test".into(),
                initial: "Downloads".into(),
            },
            Response::Directory {
                path: "".into(),
                entries: vec![Entry {
                    name: "Ä.txt".into(),
                    directory: false,
                    size: 42,
                }],
            },
            Response::Started { size: u64::MAX },
            Response::Data(vec![0; CHUNK_SIZE]),
            Response::Ok,
            Response::Failed(Failure::Exists),
        ] {
            let mut bytes = Vec::new();
            response.write(&mut bytes).unwrap();
            assert_eq!(Response::read(&mut &bytes[1..]).unwrap(), response);
        }
    }
    #[test]
    fn rejects_malformed_and_unbounded_frames() {
        assert!(Request::read(&mut &b"BFT1\xff\xff\xff\xff"[..]).is_err());
        assert!(Request::read(&mut &b"BAD!\0\0\0\x01\x07"[..]).is_err());
        assert!(Request::read(&mut &b"BFT1\0\0\0\x02\x07\0"[..]).is_err());
        assert!(Request::read(&mut &b"BFT1\0\0\0\x04\x01\0\x01\xff"[..]).is_err());
        assert!(Request::WriteChunk(vec![0; CHUNK_SIZE + 1])
            .write(&mut Vec::new())
            .is_err());
        let mut b = Vec::new();
        Request::WriteStart {
            path: "a".into(),
            size: 10,
        }
        .write(&mut b)
        .unwrap();
        for len in 1..b.len() {
            assert!(Request::read(&mut &b[1..len]).is_err());
        }
    }
    #[test]
    fn portable_paths() {
        for p in ["", "Downloads/Übung.txt", "a/b"] {
            assert!(valid_path(p));
        }
        for p in [
            "../x", "/x", "a//b", "C:/x", "a\\b", "a/..", "NUL.txt", "a:stream", "COM1", "x. ", "LPT¹",
        ] {
            assert!(!valid_path(p), "{p}");
        }
    }
}
