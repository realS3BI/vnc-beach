//! One connected viewer.

use std::io::{BufReader, BufWriter, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use rfb::encoding;
use rfb::handshake::{server_handshake, ServerInit};
use rfb::messages::{self, ClientMessage, Rect};
use rfb::zrle::ZrleEncoder;
use rfb::PixelFormat;

use crate::capture::{Frame, Layout, Screen};
use crate::clipboard::Clipboard;
use crate::input::{Event, Input};

pub struct Context {
    pub screen: Arc<Screen>,
    pub clipboard: Arc<Clipboard>,
    pub input: Option<Input>,
    pub password: Option<String>,
    pub file_transfer: bool,
    /// Root for file transfers; the server user's home directory if `None`.
    pub home: Option<std::path::PathBuf>,
    pub fps: u32,
    pub name: String,
    /// Set when the server shuts down; sessions end at their next tick.
    pub stop: Arc<AtomicBool>,
}

const TILE: usize = 64;

pub fn run(stream: TcpStream, ctx: &Context) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
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

    let (tx, rx) = sync_channel(8);
    let shutdown = stream.try_clone()?;
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
        zrle: ZrleEncoder::new(),
        use_zrle: false,
        desktop_size: false,
        extended_desktop_size: false,
        layout_announced: false,
        pending: None,
        sent: None,
        layout: frame.layout.clone(),
        clipboard_seq: ctx.clipboard.seq(),
        files: None,
    };
    let result = session.serve(ctx, rx);
    let _ = shutdown.shutdown(Shutdown::Both);
    if let Some(input) = &ctx.input {
        input.send(Event::ReleaseAll);
    }
    result
}

struct Session {
    out: BufWriter<TcpStream>,
    pf: PixelFormat,
    /// One zlib stream for the whole connection (RFC 6143 §7.7.6), kept
    /// when the client sends SetEncodings again.
    zrle: ZrleEncoder,
    use_zrle: bool,
    /// The client understands DesktopSize / ExtendedDesktopSize rectangles.
    desktop_size: bool,
    extended_desktop_size: bool,
    /// The client has seen the current layout as ExtendedDesktopSize.
    layout_announced: bool,
    /// An outstanding FramebufferUpdateRequest.
    pending: Option<Pending>,
    /// Last frame sent, for incremental diffs.
    sent: Option<Arc<Frame>>,
    /// Framebuffer size and display arrangement the client knows about.
    layout: Arc<Layout>,
    clipboard_seq: u64,
    files: Option<crate::files::Session>,
}

/// Outstanding update requests, merged: a full request always wins over an
/// incremental one, and the regions are joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    incremental: bool,
    region: Region,
}

/// A rectangle `x, y, width, height` in framebuffer pixels.
type Region = (usize, usize, usize, usize);

fn union(a: Region, b: Region) -> Region {
    let x0 = a.0.min(b.0);
    let y0 = a.1.min(b.1);
    let x1 = (a.0 + a.2).max(b.0 + b.2);
    let y1 = (a.1 + a.3).max(b.1 + b.3);
    (x0, y0, x1 - x0, y1 - y0)
}

/// Clips `r` to a `width` × `height` framebuffer; an empty result means
/// nothing of it is inside.
fn clip(r: Region, width: usize, height: usize) -> Region {
    let x0 = r.0.min(width);
    let y0 = r.1.min(height);
    let x1 = (r.0 + r.2).min(width);
    let y1 = (r.1 + r.3).min(height);
    (x0, y0, x1 - x0, y1 - y0)
}

/// The ExtendedDesktopSize screens for a layout.
fn screens(layout: &Layout) -> Vec<messages::Screen> {
    layout
        .screens
        .iter()
        .map(|s| messages::Screen {
            id: s.id,
            x: s.x as u16,
            y: s.y as u16,
            width: s.width as u16,
            height: s.height as u16,
            flags: 0,
        })
        .collect()
}

impl Session {
    fn serve(
        &mut self,
        ctx: &Context,
        rx: std::sync::mpsc::Receiver<rfb::Result<ClientMessage>>,
    ) -> anyhow::Result<()> {
        let tick = Duration::from_secs_f64(1.0 / ctx.fps as f64);
        loop {
            if ctx.stop.load(Ordering::Acquire) {
                return Ok(());
            }
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
                let text = ctx.clipboard.text();
                log::debug!("sending clipboard: {} chars", text.chars().count());
                messages::write_server_cut_text(&mut self.out, &text)?;
                self.out.flush()?;
            }
            if let Some(pending) = self.pending {
                if self.send_update(ctx, pending)? {
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
                self.use_zrle = encs.contains(&encoding::ZRLE);
                self.desktop_size = encs.contains(&encoding::DESKTOP_SIZE);
                self.extended_desktop_size = encs.contains(&encoding::EXTENDED_DESKTOP_SIZE);
                self.layout_announced = false;
                if ctx.file_transfer && encs.contains(&encoding::BEACH_FILE_TRANSFER) {
                    if self.files.is_none() {
                        match crate::files::Session::home(ctx.home.as_deref()) {
                            Ok(files) => {
                                files.hello().write(&mut self.out)?;
                                self.out.flush()?;
                                self.files = Some(files);
                            }
                            Err(e) => log::warn!("file transfer unavailable: {e}"),
                        }
                    }
                } else {
                    self.files = None;
                }
            }
            ClientMessage::FramebufferUpdateRequest {
                incremental,
                x,
                y,
                width,
                height,
            } => {
                let region = (x as usize, y as usize, width as usize, height as usize);
                self.pending = Some(match self.pending {
                    Some(p) => Pending {
                        incremental: incremental && p.incremental,
                        region: union(p.region, region),
                    },
                    None => Pending { incremental, region },
                });
            }
            ClientMessage::SetDesktopSize { width, height, .. } => {
                // We share real displays and can't resize them.
                log::debug!("viewer asked for {width}x{height}; refusing");
                messages::write_framebuffer_update_header(&mut self.out, 1)?;
                messages::write_extended_desktop_size(
                    &mut self.out,
                    messages::DESKTOP_SIZE_REASON_CLIENT,
                    messages::DESKTOP_SIZE_PROHIBITED,
                    self.layout.width as u16,
                    self.layout.height as u16,
                    &screens(&self.layout),
                )?;
                self.out.flush()?;
            }
            ClientMessage::KeyEvent { down, key } => {
                if let Some(input) = &ctx.input {
                    input.send(Event::Key { down, keysym: key });
                }
            }
            ClientMessage::PointerEvent { buttons, x, y } => {
                if let Some(input) = &ctx.input {
                    input.send(Event::Pointer {
                        buttons,
                        x,
                        y,
                        layout: self.layout.clone(),
                    });
                }
            }
            ClientMessage::FileTransfer(request) => {
                // Receiving an extension request without negotiation is a protocol error.
                let Some(files) = &mut self.files else {
                    anyhow::bail!("file transfer was not negotiated");
                };
                files.handle(request).write(&mut self.out)?;
                self.out.flush()?;
            }
            ClientMessage::ClientCutText(text) => {
                log::debug!("viewer clipboard: {} chars", text.chars().count());
                if ctx.input.is_some() {
                    ctx.clipboard.set(text);
                }
            }
        }
        Ok(())
    }

    /// Sends changed regions. Returns false if there was nothing to send yet.
    fn send_update(&mut self, ctx: &Context, pending: Pending) -> anyhow::Result<bool> {
        let frame = ctx.screen.latest();
        if let Some(sent) = &self.sent {
            if pending.incremental && Arc::ptr_eq(sent, &frame) {
                return Ok(false);
            }
        }

        if !Arc::ptr_eq(&frame.layout, &self.layout) && *frame.layout != *self.layout {
            // A display came or went, or changed resolution. Tell the client
            // in an update of its own; it asks for the pixels afresh.
            let resized = (frame.width, frame.height) != (self.layout.width, self.layout.height);
            if resized && !self.desktop_size && !self.extended_desktop_size {
                anyhow::bail!("screen resolution changed and the viewer can't handle resizing");
            }
            self.layout = frame.layout.clone();
            self.sent = None;
            if self.extended_desktop_size {
                self.announce_layout()?;
                return Ok(true);
            }
            if resized {
                messages::write_framebuffer_update_header(&mut self.out, 1)?;
                Rect {
                    x: 0,
                    y: 0,
                    width: frame.width as u16,
                    height: frame.height as u16,
                    encoding: encoding::DESKTOP_SIZE,
                }
                .write(&mut self.out)?;
                self.out.flush()?;
                return Ok(true);
            }
        }
        if self.extended_desktop_size && !self.layout_announced {
            self.announce_layout()?;
        }

        let region = clip(pending.region, frame.width, frame.height);
        if region.2 == 0 || region.3 == 0 {
            self.sent = Some(frame);
            return Ok(true);
        }
        let rects = match (&self.sent, pending.incremental) {
            (Some(prev), true) => dirty_rects(prev, &frame, region),
            _ => vec![region],
        };
        self.sent = Some(frame.clone());
        if rects.is_empty() {
            return Ok(false);
        }

        messages::write_framebuffer_update_header(&mut self.out, rects.len() as u16)?;
        let stride = frame.width * 4;
        let mut buf = Vec::new();
        for (x, y, w, h) in rects {
            let region = &frame.rgba[y * stride + x * 4..];
            let encoding = if self.use_zrle {
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
            if self.use_zrle {
                self.out.write_all(&self.zrle.encode(&self.pf, region, stride, w, h))?;
            } else {
                buf.clear();
                self.pf.encode_rgba(region, stride, w, h, &mut buf);
                self.out.write_all(&buf)?;
            }
        }
        self.out.flush()?;
        Ok(true)
    }

    /// Sends the current layout as an ExtendedDesktopSize update of its own.
    fn announce_layout(&mut self) -> anyhow::Result<()> {
        messages::write_framebuffer_update_header(&mut self.out, 1)?;
        messages::write_extended_desktop_size(
            &mut self.out,
            messages::DESKTOP_SIZE_REASON_SERVER,
            messages::DESKTOP_SIZE_OK,
            self.layout.width as u16,
            self.layout.height as u16,
            &screens(&self.layout),
        )?;
        self.out.flush()?;
        self.layout_announced = true;
        Ok(())
    }
}

/// Changed 64×64 tiles inside `region`, merged into horizontal runs per
/// tile row. Tiles are aligned to the framebuffer, not the region.
fn dirty_rects(prev: &Frame, cur: &Frame, region: Region) -> Vec<Region> {
    let stride = cur.width * 4;
    let (rx, ry, rw, rh) = region;
    let (x_end, y_end) = (rx + rw, ry + rh);
    let mut rects = Vec::new();
    for tile_y in (ry - ry % TILE..y_end).step_by(TILE) {
        let y0 = tile_y.max(ry);
        let th = (tile_y + TILE).min(y_end) - y0;
        let mut run: Option<(usize, usize)> = None;
        for tile_x in (rx - rx % TILE..x_end).step_by(TILE) {
            let x0 = tile_x.max(rx);
            let tw = (tile_x + TILE).min(x_end) - x0;
            let changed = (y0..y0 + th).any(|row| {
                let s = row * stride + x0 * 4;
                prev.rgba[s..s + tw * 4] != cur.rgba[s..s + tw * 4]
            });
            run = match (run, changed) {
                (None, true) => Some((x0, x0 + tw)),
                (Some((start, _)), true) => Some((start, x0 + tw)),
                (Some((start, end)), false) => {
                    rects.push((start, y0, end - start, th));
                    None
                }
                (None, false) => None,
            };
        }
        if let Some((start, end)) = run {
            rects.push((start, y0, end - start, th));
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
            layout: Arc::new(Layout::default()),
        }
    }

    #[test]
    fn finds_changed_tiles() {
        let prev = frame(200, 130);
        let mut cur = frame(200, 130);
        let all = (0, 0, 200, 130);
        assert!(dirty_rects(&prev, &cur, all).is_empty());
        // Pixel (70, 10) is in tile column 1, row 0; (199, 129) in the last tile.
        cur.rgba[(10 * 200 + 70) * 4] = 1;
        cur.rgba[(129 * 200 + 199) * 4] = 1;
        assert_eq!(
            dirty_rects(&prev, &cur, all),
            vec![(64, 0, 64, 64), (192, 128, 8, 2)]
        );
        // Only the part inside the requested region is reported, clipped to it.
        assert_eq!(dirty_rects(&prev, &cur, (0, 0, 100, 130)), vec![(64, 0, 36, 64)]);
        assert_eq!(
            dirty_rects(&prev, &cur, (100, 100, 100, 30)),
            vec![(192, 128, 8, 2)]
        );
        assert!(dirty_rects(&prev, &cur, (0, 64, 64, 64)).is_empty());
    }

    #[test]
    fn merges_requests() {
        assert_eq!(union((0, 0, 10, 10), (5, 5, 10, 10)), (0, 0, 15, 15));
        assert_eq!(union((20, 20, 5, 5), (0, 0, 10, 10)), (0, 0, 25, 25));
        assert_eq!(clip((90, 10, 50, 50), 100, 40), (90, 10, 10, 30));
        assert_eq!(clip((200, 0, 10, 10), 100, 100).2, 0);
    }
}
