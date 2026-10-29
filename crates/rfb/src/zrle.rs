//! ZRLE encoding (RFC 6143 §7.7.6): 64×64 tiles, palette/RLE subencodings, one
//! zlib stream per connection and direction.

use std::io::Read;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::io::ReadExt;
use crate::{proto, PixelFormat, Result};

const TILE: usize = 64;

pub struct ZrleEncoder {
    zlib: Compress,
    raw: Vec<u8>,
    pixels: Vec<u32>,
}

impl Default for ZrleEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZrleEncoder {
    pub fn new() -> Self {
        Self {
            zlib: Compress::new(Compression::fast(), true),
            raw: Vec::new(),
            pixels: Vec::new(),
        }
    }

    /// Encodes an RGBA region and returns the rectangle payload, length prefix included.
    pub fn encode(&mut self, pf: &PixelFormat, rgba: &[u8], stride: usize, w: usize, h: usize) -> Vec<u8> {
        self.pixels.clear();
        for row in 0..h {
            for px in rgba[row * stride..row * stride + w * 4].chunks_exact(4) {
                self.pixels.push(pf.pack(px[0], px[1], px[2]));
            }
        }
        self.raw.clear();
        let mut tile = Vec::with_capacity(TILE * TILE);
        for ty in (0..h).step_by(TILE) {
            for tx in (0..w).step_by(TILE) {
                let (tw, th) = (TILE.min(w - tx), TILE.min(h - ty));
                tile.clear();
                for row in ty..ty + th {
                    tile.extend_from_slice(&self.pixels[row * w + tx..row * w + tx + tw]);
                }
                encode_tile(pf, &tile, tw, &mut self.raw);
            }
        }

        let mut out = vec![0; 4];
        out.reserve(self.raw.len() / 2 + 64);
        let mut input = &self.raw[..];
        // miniz (flate2's backend) answers a call that finds output still
        // pending by only draining it, ignoring the flush. So the flush is
        // complete only after a call that started with nothing pending, i.e.
        // one that follows a call that didn't fill the buffer.
        let mut drained = true;
        loop {
            if out.len() == out.capacity() {
                out.reserve(out.capacity().max(4096));
            }
            let before = self.zlib.total_in();
            self.zlib
                .compress_vec(input, &mut out, FlushCompress::Sync)
                .expect("zlib compression cannot fail on valid state");
            input = &input[(self.zlib.total_in() - before) as usize..];
            let full = out.len() == out.capacity();
            if input.is_empty() && !full && drained {
                break;
            }
            drained = !full;
        }
        let len = (out.len() - 4) as u32;
        out[..4].copy_from_slice(&len.to_be_bytes());
        out
    }
}

fn encode_tile(pf: &PixelFormat, tile: &[u32], width: usize, out: &mut Vec<u8>) {
    let mut palette: Vec<u32> = Vec::with_capacity(16);
    for &p in tile {
        if !palette.contains(&p) {
            if palette.len() == 16 {
                palette.clear();
                break;
            }
            palette.push(p);
        }
    }
    if palette.len() == 1 {
        out.push(1);
        pf.put_cpixel(out, palette[0]);
        return;
    }

    let cpx = pf.cpixel_bytes();
    let runs = count_runs(tile);
    let start = out.len();
    if !palette.is_empty() {
        let n = palette.len();
        let bits = match n {
            2 => 1,
            3..=4 => 2,
            _ => 4,
        };
        let packed = tile.len().div_ceil(width) * (width * bits).div_ceil(8);
        // Palette RLE costs one byte per run plus length bytes for longer runs.
        let rle_estimate = runs + runs / 2;
        let index = |p: u32| palette.iter().position(|&c| c == p).unwrap() as u8;
        if rle_estimate < packed {
            out.push(128 + n as u8);
            palette.iter().for_each(|&c| pf.put_cpixel(out, c));
            for (p, len) in Runs::new(tile) {
                if len == 1 {
                    out.push(index(p));
                } else {
                    out.push(index(p) | 128);
                    put_run_length(out, len);
                }
            }
        } else {
            out.push(n as u8);
            palette.iter().for_each(|&c| pf.put_cpixel(out, c));
            for row in tile.chunks(width) {
                let (mut byte, mut used) = (0u8, 0);
                for &p in row {
                    byte = byte << bits | index(p);
                    used += bits;
                    if used == 8 {
                        out.push(byte);
                        (byte, used) = (0, 0);
                    }
                }
                if used > 0 {
                    out.push(byte << (8 - used));
                }
            }
        }
        return;
    }

    // Many colours: plain RLE if it beats raw, otherwise raw.
    if runs * (cpx + 1) < tile.len() * cpx {
        out.push(128);
        for (p, len) in Runs::new(tile) {
            pf.put_cpixel(out, p);
            put_run_length(out, len);
        }
        if out.len() - start <= 1 + tile.len() * cpx {
            return;
        }
        out.truncate(start);
    }
    out.push(0);
    tile.iter().for_each(|&p| pf.put_cpixel(out, p));
}

fn count_runs(tile: &[u32]) -> usize {
    1 + tile.windows(2).filter(|w| w[0] != w[1]).count()
}

fn put_run_length(out: &mut Vec<u8>, len: usize) {
    let mut rest = len - 1;
    while rest >= 255 {
        out.push(255);
        rest -= 255;
    }
    out.push(rest as u8);
}

struct Runs<'a> {
    tile: &'a [u32],
}

impl<'a> Runs<'a> {
    fn new(tile: &'a [u32]) -> Self {
        Self { tile }
    }
}

impl Iterator for Runs<'_> {
    type Item = (u32, usize);
    fn next(&mut self) -> Option<Self::Item> {
        let &first = self.tile.first()?;
        let len = self.tile.iter().take_while(|&&p| p == first).count();
        self.tile = &self.tile[len..];
        Some((first, len))
    }
}

pub struct ZrleDecoder {
    zlib: Decompress,
    data: Vec<u8>,
}

impl Default for ZrleDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZrleDecoder {
    pub fn new() -> Self {
        Self {
            zlib: Decompress::new(true),
            data: Vec::new(),
        }
    }

    /// Reads one ZRLE rectangle payload and returns its pixels as RGBA.
    pub fn decode(&mut self, r: &mut impl Read, pf: &PixelFormat, w: usize, h: usize) -> Result<Vec<u8>> {
        let len = r.u32()? as usize;
        // The worst case for a tile is plain RLE with runs of one pixel
        // (cpixel + 1 byte each) plus a palette; zlib adds little on top.
        // Anything beyond that is a server trying to exhaust memory.
        let limit = decoded_limit(w, h);
        if len > limit {
            return proto(format!("ZRLE payload of {len} bytes for a {w}×{h} rectangle"));
        }
        let compressed = r.read_vec(len)?;
        self.inflate(&compressed, limit)?;

        let mut rgba = vec![0u8; w * h * 4];
        let mut src = Cursor {
            data: &self.data,
            pos: 0,
        };
        let mut tile = vec![0u32; TILE * TILE];
        for ty in (0..h).step_by(TILE) {
            for tx in (0..w).step_by(TILE) {
                let (tw, th) = (TILE.min(w - tx), TILE.min(h - ty));
                decode_tile(&mut src, pf, &mut tile[..tw * th], tw)?;
                for row in 0..th {
                    for col in 0..tw {
                        let [r, g, b] = pf.unpack(tile[row * tw + col]);
                        let o = ((ty + row) * w + tx + col) * 4;
                        rgba[o..o + 4].copy_from_slice(&[r, g, b, 255]);
                    }
                }
            }
        }
        Ok(rgba)
    }

    /// Decompresses `input`; more than `limit` bytes of output is a
    /// protocol error (a compression bomb).
    fn inflate(&mut self, mut input: &[u8], limit: usize) -> Result<()> {
        self.data.clear();
        self.data.reserve((input.len() * 4 + 1024).min(limit + 1));
        loop {
            if self.data.len() > limit {
                return proto("ZRLE data decompresses to more than the rectangle can hold");
            }
            if self.data.len() == self.data.capacity() {
                self.data
                    .reserve(self.data.capacity().min(limit + 1 - self.data.len()).max(1));
            }
            let before = self.zlib.total_in();
            let status = self
                .zlib
                .decompress_vec(input, &mut self.data, FlushDecompress::Sync)
                .map_err(|e| crate::Error::Protocol(format!("zlib: {e}")))?;
            input = &input[(self.zlib.total_in() - before) as usize..];
            if status == Status::StreamEnd || (input.is_empty() && self.data.len() < self.data.capacity()) {
                return Ok(());
            }
        }
    }
}

/// Upper bound for the decompressed ZRLE data of a `w` × `h` rectangle.
fn decoded_limit(w: usize, h: usize) -> usize {
    let tiles = w.div_ceil(TILE) * h.div_ceil(TILE);
    w * h * 5 + tiles * (1 + 128 * 4) + 1024
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let Some(s) = self.data.get(self.pos..self.pos + n) else {
            return proto("ZRLE data truncated");
        };
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn cpixel(&mut self, pf: &PixelFormat) -> Result<u32> {
        let n = pf.cpixel_bytes();
        Ok(pf.get_cpixel(self.take(n)?))
    }
    fn run_length(&mut self) -> Result<usize> {
        let mut len = 1;
        loop {
            let b = self.u8()?;
            len += b as usize;
            if b != 255 {
                return Ok(len);
            }
        }
    }
}

fn decode_tile(src: &mut Cursor, pf: &PixelFormat, tile: &mut [u32], width: usize) -> Result<()> {
    let sub = src.u8()?;
    match sub {
        0 => {
            for p in tile.iter_mut() {
                *p = src.cpixel(pf)?;
            }
        }
        1 => tile.fill(src.cpixel(pf)?),
        2..=16 => {
            let palette = (0..sub).map(|_| src.cpixel(pf)).collect::<Result<Vec<_>>>()?;
            let bits = match sub {
                2 => 1,
                3..=4 => 2,
                _ => 4,
            };
            let mask = (1u8 << bits) - 1;
            for row in tile.chunks_mut(width) {
                let bytes = src.take((width * bits).div_ceil(8))?;
                for (i, p) in row.iter_mut().enumerate() {
                    let bit = i * bits;
                    let idx = bytes[bit / 8] >> (8 - bits - bit % 8) & mask;
                    *p = *palette
                        .get(idx as usize)
                        .ok_or(crate::Error::Protocol("bad palette index".into()))?;
                }
            }
        }
        128 => {
            let mut i = 0;
            while i < tile.len() {
                let p = src.cpixel(pf)?;
                let len = src.run_length()?;
                let end = (i + len).min(tile.len());
                tile[i..end].fill(p);
                i = end;
            }
        }
        130..=255 => {
            let palette = (0..sub - 128)
                .map(|_| src.cpixel(pf))
                .collect::<Result<Vec<_>>>()?;
            let mut i = 0;
            while i < tile.len() {
                let idx = src.u8()?;
                let len = if idx & 128 != 0 { src.run_length()? } else { 1 };
                let p = *palette
                    .get((idx & 127) as usize)
                    .ok_or(crate::Error::Protocol("bad palette index".into()))?;
                let end = (i + len).min(tile.len());
                tile[i..end].fill(p);
                i = end;
            }
        }
        _ => return proto(format!("invalid ZRLE subencoding {sub}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(rgba: &[u8], w: usize, h: usize, pf: PixelFormat) {
        let mut enc = ZrleEncoder::new();
        let mut dec = ZrleDecoder::new();
        // Twice, to exercise the shared zlib stream.
        for _ in 0..2 {
            let payload = enc.encode(&pf, rgba, w * 4, w, h);
            let out = dec.decode(&mut &payload[..], &pf, w, h).unwrap();
            assert_eq!(out, rgba);
        }
    }

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(x, y);
                v.extend_from_slice(&[r, g, b, 255]);
            }
        }
        v
    }

    #[test]
    fn solid() {
        roundtrip(&image(100, 70, |_, _| [9, 8, 7]), 100, 70, PixelFormat::BGRX);
    }

    #[test]
    fn palettes() {
        for n in [2usize, 3, 5, 16] {
            let img = image(67, 65, |x, y| [((x * 7 + y) % n * 10) as u8, 0, 0]);
            roundtrip(&img, 67, 65, PixelFormat::BGRX);
            let img = image(67, 65, |x, _| [((x / 20) % n * 10) as u8, 1, 2]);
            roundtrip(&img, 67, 65, PixelFormat::BGRX);
        }
    }

    #[test]
    fn many_colours() {
        let noisy = image(130, 66, |x, y| {
            [(x * 31 + y * 17) as u8, (x ^ y) as u8, (x * y) as u8]
        });
        roundtrip(&noisy, 130, 66, PixelFormat::BGRX);
        let stripes = image(130, 66, |x, _| [(x / 3) as u8, 0, (x / 3) as u8]);
        roundtrip(&stripes, 130, 66, PixelFormat::BGRX);
    }

    #[test]
    fn refuses_oversized_payloads_and_compression_bombs() {
        let pf = PixelFormat::BGRX;
        // A length far beyond what a 4×4 rectangle can need isn't even read.
        let mut dec = ZrleDecoder::new();
        let huge = [0xff, 0xff, 0xff, 0xff];
        assert!(dec.decode(&mut &huge[..], &pf, 4, 4).is_err());
        // A valid stream that inflates to much more than the rectangle holds.
        let bomb = {
            let mut z = Compress::new(Compression::best(), true);
            let zeros = vec![0u8; 1 << 20];
            let mut input = &zeros[..];
            let mut out = Vec::with_capacity(4096);
            loop {
                if out.len() == out.capacity() {
                    out.reserve(4096);
                }
                let before = z.total_in();
                z.compress_vec(input, &mut out, FlushCompress::Sync).unwrap();
                input = &input[(z.total_in() - before) as usize..];
                if input.is_empty() && out.len() < out.capacity() {
                    break;
                }
            }
            out
        };
        assert!(
            bomb.len() < decoded_limit(4, 4),
            "the test needs a small compressed bomb"
        );
        let mut payload = (bomb.len() as u32).to_be_bytes().to_vec();
        payload.extend_from_slice(&bomb);
        let mut dec = ZrleDecoder::new();
        let err = dec.decode(&mut &payload[..], &pf, 4, 4).unwrap_err();
        assert!(err.to_string().contains("decompresses"), "{err}");
    }

    #[test]
    fn incompressible_rects_share_the_stream() {
        // Noise compresses worse than the initial output reservation, so the
        // output buffer often fills just as the input runs out.
        let mut seed = 1u32;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        };
        let (w, h) = (300, 200);
        let img: Vec<u8> = (0..w * h * 4).map(|_| noise()).collect();
        let pf = PixelFormat::BGRX;
        let mut enc = ZrleEncoder::new();
        let mut dec = ZrleDecoder::new();
        for (rw, rh) in (1..=w).step_by(7).zip((1..=h).step_by(5)) {
            let payload = enc.encode(&pf, &img, w * 4, rw, rh);
            let out = dec.decode(&mut &payload[..], &pf, rw, rh).unwrap();
            for row in 0..rh {
                for col in 0..rw {
                    assert_eq!(out[(row * rw + col) * 4..][..3], img[(row * w + col) * 4..][..3]);
                }
            }
        }
    }
}
