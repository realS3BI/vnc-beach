//! Big-endian read/write helpers.

use std::io::{Read, Write};

pub trait ReadExt: Read {
    fn u8(&mut self) -> std::io::Result<u8> {
        let mut b = [0; 1];
        self.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn u16(&mut self) -> std::io::Result<u16> {
        let mut b = [0; 2];
        self.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }
    fn u32(&mut self) -> std::io::Result<u32> {
        let mut b = [0; 4];
        self.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }
    fn i32(&mut self) -> std::io::Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn read_vec(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut v = vec![0; n];
        self.read_exact(&mut v)?;
        Ok(v)
    }
    fn skip(&mut self, n: usize) -> std::io::Result<()> {
        std::io::copy(&mut self.take(n as u64), &mut std::io::sink()).and_then(|copied| {
            if copied == n as u64 {
                Ok(())
            } else {
                Err(std::io::ErrorKind::UnexpectedEof.into())
            }
        })
    }
}

impl<R: Read + ?Sized> ReadExt for R {}

pub trait WriteExt: Write {
    fn put_u8(&mut self, v: u8) -> std::io::Result<()> {
        self.write_all(&[v])
    }
    fn put_u16(&mut self, v: u16) -> std::io::Result<()> {
        self.write_all(&v.to_be_bytes())
    }
    fn put_u32(&mut self, v: u32) -> std::io::Result<()> {
        self.write_all(&v.to_be_bytes())
    }
    fn put_i32(&mut self, v: i32) -> std::io::Result<()> {
        self.write_all(&v.to_be_bytes())
    }
}

impl<W: Write + ?Sized> WriteExt for W {}
