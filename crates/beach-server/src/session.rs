//! One connected viewer.

use std::io::{BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use rfb::encoding;
use rfb::handshake::{server_handshake, ServerInit};
use rfb::messages::{self, ClientMessage, Rect};
use rfb::zrle::ZrleEncoder;
use rfb::PixelFormat;

use crate::capture::{Frame, Screen};
use crate::clipboard::Clipboard;
use crate::input::{Event, Input};

pub struct Context {
    pub screen: Arc<Screen>,
    pub clipboard: Arc<Clipboard>,
    pub input: Option<Input>,
    pub password: Option<String>,
    pub fps: u32,
    pub name: String,
}

const TILE: usize = 64;

pub fn run(stream: TcpStream, ctx: &Context) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let _watch = ctx.screen.watch();
    let frame = ctx.screen.latest();
    let init = ServerInit {
        width: frame.width as u16,
        height: frame.height as u16,
        pixel_format: PixelFormat::BGRX,
        name: ctx.name.clone(),
    };
    let mut handshake_stream = stream.try_clone()?;
    server_handshake(&mut handshake_stream, ctx.password.as_deref(), &init).context("handshake")?;

    let (tx, rx) = channel();
    let reader = BufReader::new(stream.try_clone()?);
    std::thread::spawn(move || {
        let mut reader = reader;
        loop {
            let msg = ClientMessage::read(&mut reader);
            let stop = msg.is_err();
            if tx.send(msg).is_err() || stop {
                break;
            }
        }
    });

    let mut session = Session {
        out: BufWriter::with_capacity(1 << 20, stream),
        pf: PixelFormat::BGRX,
        zrle: None,
        desktop_size: false,
        pending: None,
        sent: None,
        fb_size: (frame.width, frame.height),
        clipboard_seq: ctx.clipboard.seq(),
    };
    let result = session.serve(ctx, rx);
    if let Some(input) = &ctx.input {
        input.send(Event::ReleaseAll);
    }
    result
}

struct Session {
    out: BufWriter<TcpStream>,
    pf: PixelFormat,
    zrle: Option<ZrleEncoder>,
    desktop_size: bool,
    /// An outstanding FramebufferUpdateRequest: `true` if incremental.
    pending: Option<bool>,
    /// Last frame sent, for incremental diffs.
    sent: Option<Arc<Frame>>,
    fb_size: (usize, usize),
    clipboard_seq: u64,
}

impl Session {
    fn serve(
        &mut self,
        ctx: &Context,
        rx: std::sync::mpsc::Receiver<rfb::Result<ClientMessage>>,
    ) -> anyhow::Result<()> {
        let tick = Duration::from_secs_f64(1.0 / ctx.fps as f64);
        loop {
            match rx.recv_timeout(tick) {
                Ok(msg) => self.handle(ctx, msg?)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            // Drain whatever else arrived so input stays snappy.
            while let Ok(msg) = rx.try_recv() {
                self.handle(ctx, msg?)?;
            }

            if ctx.clipboard.seq() != self.clipboard_seq {
                self.clipboard_seq = ctx.clipboard.seq();
                messages::write_server_cut_text(&mut self.out, &ctx.clipboard.text())?;
                self.out.flush()?;
            }
            if let Some(incremental) = self.pending {
                if self.send_update(ctx, incremental)? {
                    self.pending = None;
                }
            }
        }
    }

    fn handle(&mut self, ctx: &Context, msg: ClientMessage) -> anyhow::Result<()> {
        match msg {
            ClientMessage::SetPixelFormat(pf) => {
                if !pf.is_supported() {
                    anyhow::bail!("unsupported pixel format {pf:?}");
                }
                self.pf = pf;
                self.sent = None;
            }
            ClientMessage::SetEncodings(encs) => {
                log::debug!("client encodings: {encs:?}");
                self.zrle = encs.contains(&encoding::ZRLE).then(ZrleEncoder::new);
                self.desktop_size = encs.contains(&encoding::DESKTOP_SIZE);
            }
            ClientMessage::FramebufferUpdateRequest { incremental, .. } => {
                // A full request always wins over an incremental one.
                self.pending = Some(incremental && self.pending.unwrap_or(true));
                if !incremental {
                    self.sent = None;
                }
            }
            ClientMessage::KeyEvent { down, key } => {
                if let Some(input) = &ctx.input {
                    input.send(Event::Key { down, keysym: key });
                }
            }
            ClientMessage::PointerEvent { buttons, x, y } => {
                if let Some(input) = &ctx.input {
                    let (fb_width, fb_height) = self.fb_size;
                    input.send(Event::Pointer {
                        buttons,
                        x,
                        y,
                        fb_width,
                        fb_height,
                    });
                }
            }
            ClientMessage::ClientCutText(text) => {
                if ctx.input.is_some() {
                    ctx.clipboard.set(text);
                }
            }
        }
        Ok(())
    }

    /// Sends changed regions. Returns false if there was nothing to send yet.
    fn send_update(&mut self, ctx: &Context, incremental: bool) -> anyhow::Result<bool> {
        let frame = ctx.screen.latest();
        if let Some(sent) = &self.sent {
            if incremental && Arc::ptr_eq(sent, &frame) {
                return Ok(false);
            }
        }

        let mut resized = false;
        if (frame.width, frame.height) != self.fb_size {
            if !self.desktop_size {
                anyhow::bail!("screen resolution changed and the viewer can't handle resizing");
            }
            self.fb_size = (frame.width, frame.height);
            self.sent = None;
            resized = true;
        }

        let rects = match (&self.sent, incremental) {
            (Some(prev), true) => dirty_rects(prev, &frame),
            _ => vec![(0, 0, frame.width, frame.height)],
        };
        self.sent = Some(frame.clone());
        if rects.is_empty() && !resized {
            return Ok(false);
        }

        let count = rects.len() + resized as usize;
        messages::write_framebuffer_update_header(&mut self.out, count as u16)?;
        if resized {
            let (w, h) = self.fb_size;
            Rect {
                x: 0,
                y: 0,
                width: w as u16,
                height: h as u16,
                encoding: encoding::DESKTOP_SIZE,
            }
            .write(&mut self.out)?;
        }
        let stride = frame.width * 4;
        let mut buf = Vec::new();
        for (x, y, w, h) in rects {
            let region = &frame.rgba[y * stride + x * 4..];
            let encoding = if self.zrle.is_some() {
                encoding::ZRLE
            } else {
                encoding::RAW
            };
            Rect {
                x: x as u16,
                y: y as u16,
                width: w as u16,
                height: h as u16,
                encoding,
            }
            .write(&mut self.out)?;
            match &mut self.zrle {
                Some(zrle) => self.out.write_all(&zrle.encode(&self.pf, region, stride, w, h))?,
                None => {
                    buf.clear();
                    self.pf.encode_rgba(region, stride, w, h, &mut buf);
                    self.out.write_all(&buf)?;
                }
            }
        }
        self.out.flush()?;
        Ok(true)
    }
}

/// Changed 64×64 tiles, merged into horizontal runs per tile row.
fn dirty_rects(prev: &Frame, cur: &Frame) -> Vec<(usize, usize, usize, usize)> {
    let (w, h) = (cur.width, cur.height);
    let stride = w * 4;
    let mut rects = Vec::new();
    for ty in (0..h).step_by(TILE) {
        let th = TILE.min(h - ty);
        let mut run: Option<(usize, usize)> = None;
        for tx in (0..w).step_by(TILE) {
            let tw = TILE.min(w - tx);
            let changed = (ty..ty + th).any(|row| {
                let s = row * stride + tx * 4;
                prev.rgba[s..s + tw * 4] != cur.rgba[s..s + tw * 4]
            });
            run = match (run, changed) {
                (None, true) => Some((tx, tx + tw)),
                (Some((start, _)), true) => Some((start, tx + tw)),
                (Some((start, end)), false) => {
                    rects.push((start, ty, end - start, th));
                    None
                }
                (None, false) => None,
            };
        }
        if let Some((start, end)) = run {
            rects.push((start, ty, end - start, th));
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize) -> Frame {
        Frame {
            width: w,
            height: h,
            rgba: vec![0; w * h * 4],
        }
    }

    #[test]
    fn finds_changed_tiles() {
        let prev = frame(200, 130);
        let mut cur = frame(200, 130);
        assert!(dirty_rects(&prev, &cur).is_empty());
        // Pixel (70, 10) is in tile column 1, row 0; (199, 129) in the last tile.
        cur.rgba[(10 * 200 + 70) * 4] = 1;
        cur.rgba[(129 * 200 + 199) * 4] = 1;
        assert_eq!(dirty_rects(&prev, &cur), vec![(64, 0, 64, 64), (192, 128, 8, 2)]);
    }
}
