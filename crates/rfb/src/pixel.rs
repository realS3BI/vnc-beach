//! Pixel formats and conversion between RGBA buffers and wire pixels.

use std::io::{Read, Write};

use crate::io::{ReadExt, WriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_color: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// 32bpp, depth 24, little endian, `0x00RRGGBB`: bytes on the wire are B, G, R, X.
    pub const BGRX: PixelFormat = PixelFormat {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: false,
        true_color: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };

    pub fn read(r: &mut impl Read) -> std::io::Result<Self> {
        let pf = PixelFormat {
            bits_per_pixel: r.u8()?,
            depth: r.u8()?,
            big_endian: r.u8()? != 0,
            true_color: r.u8()? != 0,
            red_max: r.u16()?,
            green_max: r.u16()?,
            blue_max: r.u16()?,
            red_shift: r.u8()?,
            green_shift: r.u8()?,
            blue_shift: r.u8()?,
        };
        r.skip(3)?;
        Ok(pf)
    }

    pub fn write(&self, w: &mut impl Write) -> std::io::Result<()> {
        w.put_u8(self.bits_per_pixel)?;
        w.put_u8(self.depth)?;
        w.put_u8(self.big_endian as u8)?;
        w.put_u8(self.true_color as u8)?;
        w.put_u16(self.red_max)?;
        w.put_u16(self.green_max)?;
        w.put_u16(self.blue_max)?;
        w.put_u8(self.red_shift)?;
        w.put_u8(self.green_shift)?;
        w.put_u8(self.blue_shift)?;
        w.write_all(&[0; 3])
    }

    pub fn bytes_per_pixel(&self) -> usize {
        (self.bits_per_pixel as usize).div_ceil(8)
    }

    pub fn is_supported(&self) -> bool {
        self.true_color && matches!(self.bits_per_pixel, 8 | 16 | 32)
    }

    /// Size of a ZRLE "compressed pixel" (RFC 6143 §7.7.6).
    pub fn cpixel_bytes(&self) -> usize {
        if self.bits_per_pixel == 32 && self.depth <= 24 && self.fits_in_three_bytes() {
            3
        } else {
            self.bytes_per_pixel()
        }
    }

    fn colour_mask(&self) -> u32 {
        (self.red_max as u32) << self.red_shift
            | (self.green_max as u32) << self.green_shift
            | (self.blue_max as u32) << self.blue_shift
    }

    fn fits_in_three_bytes(&self) -> bool {
        let mask = self.colour_mask();
        mask & 0xff00_0000 == 0 || mask & 0x0000_00ff == 0
    }

    /// For a 3-byte cpixel: whether the dropped byte is the most significant one.
    fn cpixel_drops_high_byte(&self) -> bool {
        self.colour_mask() & 0xff00_0000 == 0
    }

    pub fn pack(&self, r: u8, g: u8, b: u8) -> u32 {
        let scale = |v: u8, max: u16| (v as u32 * max as u32 + 127) / 255;
        scale(r, self.red_max) << self.red_shift
            | scale(g, self.green_max) << self.green_shift
            | scale(b, self.blue_max) << self.blue_shift
    }

    pub fn unpack(&self, p: u32) -> [u8; 3] {
        let comp = |shift: u8, max: u16| {
            if max == 0 {
                return 0;
            }
            let v = (p >> shift) & max as u32;
            ((v * 255 + max as u32 / 2) / max as u32) as u8
        };
        [
            comp(self.red_shift, self.red_max),
            comp(self.green_shift, self.green_max),
            comp(self.blue_shift, self.blue_max),
        ]
    }

    /// Appends one pixel in wire format.
    pub fn put_pixel(&self, out: &mut Vec<u8>, p: u32) {
        match (self.bytes_per_pixel(), self.big_endian) {
            (1, _) => out.push(p as u8),
            (2, false) => out.extend_from_slice(&(p as u16).to_le_bytes()),
            (2, true) => out.extend_from_slice(&(p as u16).to_be_bytes()),
            (_, false) => out.extend_from_slice(&p.to_le_bytes()),
            (_, true) => out.extend_from_slice(&p.to_be_bytes()),
        }
    }

    pub fn get_pixel(&self, b: &[u8]) -> u32 {
        match (self.bytes_per_pixel(), self.big_endian) {
            (1, _) => b[0] as u32,
            (2, false) => u16::from_le_bytes([b[0], b[1]]) as u32,
            (2, true) => u16::from_be_bytes([b[0], b[1]]) as u32,
            (_, false) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (_, true) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        }
    }

    /// Appends one ZRLE cpixel.
    pub fn put_cpixel(&self, out: &mut Vec<u8>, p: u32) {
        if self.cpixel_bytes() != 3 {
            return self.put_pixel(out, p);
        }
        let shift = if self.cpixel_drops_high_byte() { 0 } else { 8 };
        let b = (p >> shift).to_le_bytes();
        if self.big_endian {
            out.extend_from_slice(&[b[2], b[1], b[0]]);
        } else {
            out.extend_from_slice(&b[..3]);
        }
    }

    pub fn get_cpixel(&self, b: &[u8]) -> u32 {
        if self.cpixel_bytes() != 3 {
            return self.get_pixel(b);
        }
        let v = if self.big_endian {
            u32::from_be_bytes([0, b[0], b[1], b[2]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], 0])
        };
        if self.cpixel_drops_high_byte() {
            v
        } else {
            v << 8
        }
    }

    /// Converts an RGBA region (`stride` in bytes) into wire pixels.
    pub fn encode_rgba(&self, rgba: &[u8], stride: usize, w: usize, h: usize, out: &mut Vec<u8>) {
        out.reserve(w * h * self.bytes_per_pixel());
        if *self == Self::BGRX {
            for row in 0..h {
                let line = &rgba[row * stride..row * stride + w * 4];
                for px in line.chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0], 0]);
                }
            }
            return;
        }
        for row in 0..h {
            let line = &rgba[row * stride..row * stride + w * 4];
            for px in line.chunks_exact(4) {
                self.put_pixel(out, self.pack(px[0], px[1], px[2]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RGB565: PixelFormat = PixelFormat {
        bits_per_pixel: 16,
        depth: 16,
        big_endian: true,
        true_color: true,
        red_max: 31,
        green_max: 63,
        blue_max: 31,
        red_shift: 11,
        green_shift: 5,
        blue_shift: 0,
    };

    #[test]
    fn roundtrips_through_wire() {
        let mut buf = Vec::new();
        PixelFormat::BGRX.write(&mut buf).unwrap();
        assert_eq!(buf.len(), 16);
        assert_eq!(PixelFormat::read(&mut &buf[..]).unwrap(), PixelFormat::BGRX);
    }

    #[test]
    fn bgrx_bytes() {
        let mut out = Vec::new();
        PixelFormat::BGRX.encode_rgba(&[1, 2, 3, 255], 4, 1, 1, &mut out);
        assert_eq!(out, [3, 2, 1, 0]);
        let p = PixelFormat::BGRX.get_pixel(&out);
        assert_eq!(PixelFormat::BGRX.unpack(p), [1, 2, 3]);
    }

    #[test]
    fn cpixel_is_three_bytes_for_bgrx() {
        let pf = PixelFormat::BGRX;
        assert_eq!(pf.cpixel_bytes(), 3);
        let mut out = Vec::new();
        let p = pf.pack(10, 20, 30);
        pf.put_cpixel(&mut out, p);
        assert_eq!(out, [30, 20, 10]);
        assert_eq!(pf.get_cpixel(&out), p);
    }

    #[test]
    fn rgb565_roundtrip() {
        let p = RGB565.pack(255, 0, 255);
        let mut out = Vec::new();
        RGB565.put_pixel(&mut out, p);
        assert_eq!(out.len(), 2);
        assert_eq!(RGB565.unpack(RGB565.get_pixel(&out)), [255, 0, 255]);
    }
}
