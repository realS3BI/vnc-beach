//! Client session: handshake, framebuffer decoding, sending input.
//!
//! This mirrors `beach-viewer/src/client.rs` (keep the two in step) and adds
//! dirty-rectangle tracking, so a GPU texture only needs partial uploads.

use std::io::{BufReader, BufWriter, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rfb::encoding;
use rfb::handshake::client_handshake_with_banner;
use rfb::io::ReadExt;
use rfb::messages::{ClientMessage, Rect, ServerMessage};
use rfb::zrle::ZrleDecoder;
use rfb::{Error, PixelFormat};

/// Pixel rectangle `x, y, width, height`.
pub type DirtyRect = [usize; 4];

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// Changed pixels since the last [`Framebuffer::take_dirty`], as
    /// `x0, y0, x1, y1` (exclusive).
    dirty: Option<[usize; 4]>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Self {
        Framebuffer {
            width,
            height,
            rgba: vec![0; width * height * 4],
            dirty: Some([0, 0, width, height]),
        }
    }

    fn mark(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if w == 0 || h == 0 {
            return;
        }
        let r = [x, y, x + w, y + h];
        self.dirty = Some(match self.dirty {
            None => r,
            Some(d) => [d[0].min(r[0]), d[1].min(r[1]), d[2].max(r[2]), d[3].max(r[3])],
        });
    }

    /// Returns and clears the bounding box of everything that changed.
    pub fn take_dirty(&mut self) -> Option<DirtyRect> {
        self.dirty
            .take()
            .map(|[x0, y0, x1, y1]| [x0, y0, x1 - x0, y1 - y0])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// New pixels are waiting in the framebuffer.
    Frame,
    /// The remote desktop changed size; the framebuffer was reallocated.
    Resized,
    Clipboard(String),
    Closed(String),
}

/// What kind of server we are talking to; drives modifier mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    /// macOS Screen Sharing: announces RFB 003.889, reads Alt_L as Cmd and
    /// Meta_L as Option, ignores cut text.
    AppleScreenSharing,
    /// Our own server on a Mac (name ends with "(macOS)").
    BeachMac,
    Other,
}

impl RemoteKind {
    pub fn detect(banner: &str, name: &str) -> Self {
        if banner.contains("003.889") {
            RemoteKind::AppleScreenSharing
        } else if name.ends_with("(macOS)") {
            RemoteKind::BeachMac
        } else {
            RemoteKind::Other
        }
    }

    pub fn is_mac(self) -> bool {
        self != RemoteKind::Other
    }
}

pub struct Connection {
    pub name: String,
    pub remote: RemoteKind,
    pub fb: Arc<Mutex<Framebuffer>>,
    writer: Arc<Mutex<BufWriter<TcpStream>>>,
    stream: TcpStream,
}

impl Connection {
    /// Connects, authenticates and asks the server for `pf` pixels. `on_event`
    /// is called from the reader thread.
    pub fn open(
        addr: &str,
        password: Option<&str>,
        pf: PixelFormat,
        on_event: impl Fn(Event) + Send + 'static,
    ) -> rfb::Result<Connection> {
        let mut stream = connect(addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        let (init, banner) = client_handshake_with_banner(&mut stream, password)?;
        let remote = RemoteKind::detect(&banner, &init.name);
        stream.set_read_timeout(None)?;

        let (w, h) = (init.width as usize, init.height as usize);
        let fb = Arc::new(Mutex::new(Framebuffer::new(w, h)));
        let mut writer = BufWriter::new(stream.try_clone()?);
        ClientMessage::SetPixelFormat(pf).write(&mut writer)?;
        ClientMessage::SetEncodings(vec![
            encoding::ZRLE,
            encoding::COPY_RECT,
            encoding::RAW,
            encoding::DESKTOP_SIZE,
        ])
        .write(&mut writer)?;
        full_request(w, h, false).write(&mut writer)?;
        writer.flush()?;

        let writer = Arc::new(Mutex::new(writer));
        let reader = Reader {
            input: BufReader::with_capacity(1 << 20, stream.try_clone()?),
            output: writer.clone(),
            fb: fb.clone(),
            pf,
            zrle: ZrleDecoder::new(),
            on_event: Box::new(on_event),
        };
        std::thread::Builder::new()
            .name("rfb-reader".into())
            .spawn(move || reader.run())?;

        Ok(Connection {
            name: init.name,
            remote,
            fb,
            writer,
            stream,
        })
    }

    pub fn send(&self, msgs: &[ClientMessage]) {
        let _ = send(&self.writer, msgs);
    }

    /// Asks for the whole screen again, e.g. after the app was suspended.
    pub fn refresh(&self) {
        let (w, h) = self.size();
        self.send(&[full_request(w, h, false)]);
    }

    pub fn size(&self) -> (usize, usize) {
        let fb = self.fb.lock().unwrap();
        (fb.width, fb.height)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// Accepts `host`, `host:display` (display < 100 → port 5900+display),
/// `host:port`, `host::port` and `[ipv6]:port`.
pub fn parse_address(input: &str) -> String {
    let input = input.trim();
    if let Some((host, port)) = input.rsplit_once("::") {
        if !host.is_empty() && port.parse::<u16>().is_ok() {
            return format!("{host}:{port}");
        }
    }
    let (host, n) = match input.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(':') && (h.starts_with('[') || !h.contains(':')) => {
            match p.parse::<u16>() {
                Ok(n) => (h, Some(n)),
                Err(_) => (input, None),
            }
        }
        _ => (input, None),
    };
    let port = match n {
        Some(n) if n < 100 => 5900 + n,
        Some(n) => n,
        None => 5900,
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn connect(addr: &str) -> rfb::Result<TcpStream> {
    let mut last = None;
    for a in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, Duration::from_secs(10)) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(Error::Io)
        .unwrap_or_else(|| Error::Protocol(format!("could not resolve {addr}"))))
}

/// Reader and UI share one writer so their messages never interleave.
fn send(writer: &Mutex<BufWriter<TcpStream>>, msgs: &[ClientMessage]) -> rfb::Result<()> {
    let mut w = writer.lock().unwrap();
    msgs.iter().try_for_each(|m| m.write(&mut *w))?;
    w.flush()?;
    Ok(())
}

fn full_request(w: usize, h: usize, incremental: bool) -> ClientMessage {
    ClientMessage::FramebufferUpdateRequest {
        incremental,
        x: 0,
        y: 0,
        width: w as u16,
        height: h as u16,
    }
}

struct Reader {
    input: BufReader<TcpStream>,
    output: Arc<Mutex<BufWriter<TcpStream>>>,
    fb: Arc<Mutex<Framebuffer>>,
    pf: PixelFormat,
    zrle: ZrleDecoder,
    on_event: Box<dyn Fn(Event) + Send>,
}

impl Reader {
    fn run(mut self) {
        let reason = match self.serve() {
            Ok(()) => "connection closed".to_string(),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                "server closed the connection".into()
            }
            Err(e) => e.to_string(),
        };
        (self.on_event)(Event::Closed(reason));
    }

    fn serve(&mut self) -> rfb::Result<()> {
        loop {
            match ServerMessage::read(&mut self.input)? {
                ServerMessage::FramebufferUpdate { rects } => {
                    let mut resized = false;
                    for _ in 0..rects {
                        let rect = Rect::read(&mut self.input)?;
                        resized |= self.apply(rect)?;
                    }
                    let (w, h) = {
                        let fb = self.fb.lock().unwrap();
                        (fb.width, fb.height)
                    };
                    if resized {
                        (self.on_event)(Event::Resized);
                    }
                    (self.on_event)(Event::Frame);
                    send(&self.output, &[full_request(w, h, true)])?;
                }
                ServerMessage::ServerCutText(text) => (self.on_event)(Event::Clipboard(text)),
                ServerMessage::Bell | ServerMessage::SetColourMapEntries => {}
            }
        }
    }

    /// Applies one rectangle; returns whether it changed the desktop size.
    fn apply(&mut self, r: Rect) -> rfb::Result<bool> {
        let (x, y, w, h) = (r.x as usize, r.y as usize, r.width as usize, r.height as usize);
        if r.encoding == encoding::DESKTOP_SIZE {
            *self.fb.lock().unwrap() = Framebuffer::new(w, h);
            return Ok(true);
        }
        {
            let fb = self.fb.lock().unwrap();
            if x + w > fb.width || y + h > fb.height {
                return Err(Error::Protocol(format!("rectangle {r:?} outside framebuffer")));
            }
        }
        match r.encoding {
            encoding::RAW => {
                let bpp = self.pf.bytes_per_pixel();
                let data = self.input.read_vec(w * h * bpp)?;
                let mut fb = self.fb.lock().unwrap();
                let stride = fb.width * 4;
                for (row, src) in data.chunks_exact(w * bpp).enumerate() {
                    let dst = &mut fb.rgba[(y + row) * stride + x * 4..][..w * 4];
                    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(bpp)) {
                        let [r, g, b] = self.pf.unpack(self.pf.get_pixel(s));
                        d.copy_from_slice(&[r, g, b, 255]);
                    }
                }
                fb.mark(x, y, w, h);
            }
            encoding::COPY_RECT => {
                let (sx, sy) = (self.input.u16()? as usize, self.input.u16()? as usize);
                let mut fb = self.fb.lock().unwrap();
                if sx + w > fb.width || sy + h > fb.height {
                    return Err(Error::Protocol("CopyRect source outside framebuffer".into()));
                }
                let stride = fb.width * 4;
                let src: Vec<u8> = (0..h)
                    .flat_map(|row| fb.rgba[(sy + row) * stride + sx * 4..][..w * 4].to_vec())
                    .collect();
                blit(&mut fb, x, y, w, &src);
            }
            encoding::ZRLE => {
                let rgba = self.zrle.decode(&mut self.input, &self.pf, w, h)?;
                blit(&mut self.fb.lock().unwrap(), x, y, w, &rgba);
            }
            e => return Err(Error::Protocol(format!("server used unrequested encoding {e}"))),
        }
        Ok(false)
    }
}

fn blit(fb: &mut Framebuffer, x: usize, y: usize, w: usize, rgba: &[u8]) {
    let stride = fb.width * 4;
    let h = rgba.len() / (w * 4).max(1);
    for (row, src) in rgba.chunks_exact(w * 4).enumerate() {
        fb.rgba[(y + row) * stride + x * 4..][..w * 4].copy_from_slice(src);
    }
    fb.mark(x, y, w, h);
}

#[cfg(test)]
mod tests {
    use super::{parse_address, Framebuffer, RemoteKind};

    #[test]
    fn detects_macs() {
        assert_eq!(
            RemoteKind::detect("RFB 003.889", "Sebastians Mac mini"),
            RemoteKind::AppleScreenSharing
        );
        assert_eq!(
            RemoteKind::detect("RFB 003.008", "VNC Beach @ mini (macOS)"),
            RemoteKind::BeachMac
        );
        assert_eq!(
            RemoteKind::detect("RFB 003.008", "VNC Beach @ pc (Windows)"),
            RemoteKind::Other
        );
        assert_eq!(RemoteKind::detect("RFB 003.008", "TigerVNC"), RemoteKind::Other);
        assert!(RemoteKind::BeachMac.is_mac());
        assert!(!RemoteKind::Other.is_mac());
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address("mac.local"), "mac.local:5900");
        assert_eq!(parse_address("sebastian-pc"), "sebastian-pc:5900");
        assert_eq!(parse_address("10.0.0.2:1"), "10.0.0.2:5901");
        assert_eq!(parse_address("10.0.0.2:5905"), "10.0.0.2:5905");
        assert_eq!(parse_address("10.0.0.2::5"), "10.0.0.2:5");
        assert_eq!(parse_address("[::1]:5901"), "[::1]:5901");
        assert_eq!(parse_address("::1"), "[::1]:5900");
    }

    #[test]
    fn dirty_rects_accumulate() {
        let mut fb = Framebuffer::new(100, 50);
        assert_eq!(fb.take_dirty(), Some([0, 0, 100, 50]));
        assert_eq!(fb.take_dirty(), None);
        fb.mark(10, 10, 5, 5);
        fb.mark(20, 0, 2, 30);
        fb.mark(0, 0, 0, 10);
        assert_eq!(fb.take_dirty(), Some([10, 0, 12, 30]));
        assert_eq!(fb.take_dirty(), None);
        super::blit(&mut fb, 90, 40, 10, &[7; 10 * 10 * 4]);
        assert_eq!(fb.take_dirty(), Some([90, 40, 10, 10]));
        assert_eq!(fb.rgba[(49 * 100 + 99) * 4], 7);
    }
}
