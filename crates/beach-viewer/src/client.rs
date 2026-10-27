//! Network side of the viewer: handshake, framebuffer decoding, sending input.

use std::io::{BufReader, BufWriter, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rfb::encoding;
use rfb::handshake::client_handshake_secure;
use rfb::io::ReadExt;
use rfb::messages::{self, ClientMessage, Rect, ServerMessage};
use rfb::zrle::ZrleDecoder;
use rfb::{Error, PixelFormat};

pub use rfb::messages::Screen;

/// A rectangle `x, y, width, height` in framebuffer pixels.
pub type Region = (usize, usize, usize, usize);

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// Bumped on every change, so the UI knows when to re-upload.
    pub generation: u64,
    /// The server's displays inside the framebuffer (ExtendedDesktopSize);
    /// empty if the server didn't say.
    pub screens: Vec<Screen>,
}

impl Framebuffer {
    fn new(width: usize, height: usize, generation: u64, screens: Vec<Screen>) -> Self {
        Framebuffer {
            width,
            height,
            rgba: vec![0; width * height * 4],
            generation,
            screens,
        }
    }
}

pub enum Notice {
    Clipboard(String),
    Closed(String),
}

/// What kind of server we are talking to; decides how modifier keys are sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteKind {
    /// macOS Screen Sharing (banner `RFB 003.889`). It reads Alt_L as Cmd
    /// and Meta_L as Option.
    AppleScreenSharing,
    /// VNC Beach's own server on a Mac: Super_L is Cmd, Alt_L is Option.
    BeachMac,
    /// Windows, Linux, anything else.
    Other,
}

impl RemoteKind {
    pub fn is_mac(self) -> bool {
        self != RemoteKind::Other
    }
}

pub struct Connection {
    pub name: String,
    pub remote: RemoteKind,
    pub fb: Arc<Mutex<Framebuffer>>,
    pub notices: Receiver<Notice>,
    /// The part of the framebuffer we ask updates for; `None` is all of it.
    region: Arc<Mutex<Option<Region>>>,
    writer: Arc<Mutex<BufWriter<TcpStream>>>,
    stream: TcpStream,
    pub files: Option<FileChannel>,
}

impl Connection {
    /// Continues on a socket already checked by the connection workflow.
    /// With `account`, the server is asked for an account ticket first.
    pub fn from_stream(
        mut stream: TcpStream,
        password: Option<&str>,
        account: Option<&crate::account::Target>,
        pf: PixelFormat,
        repaint: impl Fn() + Send + 'static,
    ) -> rfb::Result<Connection> {
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(15)))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        let mut tickets = |device: &str, nonce: &[u8; rfb::handshake::TICKET_NONCE_LEN]| match account {
            Some(target) => crate::account::ticket(target, device, nonce),
            None => Err("not signed in".to_string()),
        };
        let source: Option<rfb::handshake::TicketSource> = match account {
            Some(_) => Some(&mut tickets),
            None => None,
        };
        let (init, banner) = client_handshake_secure(&mut stream, password, source)?;
        let remote = detect(&banner, &init.name);
        log::debug!("server {banner:?} {:?}: {remote:?}", init.name);
        stream.set_read_timeout(None)?;

        let (w, h) = (init.width as usize, init.height as usize);
        let fb = Arc::new(Mutex::new(Framebuffer::new(w, h, 0, Vec::new())));
        let mut writer = BufWriter::new(stream.try_clone()?);
        ClientMessage::SetPixelFormat(pf).write(&mut writer)?;
        ClientMessage::SetEncodings(vec![
            encoding::ZRLE,
            encoding::COPY_RECT,
            encoding::RAW,
            encoding::EXTENDED_DESKTOP_SIZE,
            encoding::DESKTOP_SIZE,
            encoding::BEACH_FILE_TRANSFER,
        ])
        .write(&mut writer)?;
        request((0, 0, w, h), false).write(&mut writer)?;
        writer.flush()?;

        let writer = Arc::new(Mutex::new(writer));
        let region = Arc::new(Mutex::new(None));
        let (tx, notices) = channel();
        let (file_tx, replies) = sync_channel(1);
        let reader = Reader {
            input: BufReader::with_capacity(1 << 20, stream.try_clone()?),
            output: writer.clone(),
            fb: fb.clone(),
            region: region.clone(),
            pf,
            zrle: ZrleDecoder::new(),
            notices: tx,
            files: file_tx,
            repaint: Box::new(repaint),
        };
        std::thread::Builder::new()
            .name("rfb-reader".into())
            .spawn(move || reader.run())?;

        Ok(Connection {
            name: init.name,
            remote,
            fb,
            notices,
            region,
            writer: writer.clone(),
            stream,
            files: Some(FileChannel {
                writer: writer.clone(),
                replies,
            }),
        })
    }

    pub fn send(&self, msgs: &[ClientMessage]) {
        if let Err(e) = send(&self.writer, msgs) {
            log::debug!("send failed: {e}");
        }
    }

    /// Restricts updates to `region` (e.g. one display) and fetches it in
    /// full. `None` goes back to the whole framebuffer.
    pub fn set_region(&self, region: Option<Region>) {
        let (w, h) = {
            let fb = self.fb.lock().unwrap();
            (fb.width, fb.height)
        };
        let clipped = region.map(|r| clip(r, w, h)).filter(|r| r.2 > 0 && r.3 > 0);
        *self.region.lock().unwrap() = clipped;
        self.send(&[request(clipped.unwrap_or((0, 0, w, h)), false)]);
    }
}

fn clip(r: Region, width: usize, height: usize) -> Region {
    let x0 = r.0.min(width);
    let y0 = r.1.min(height);
    (x0, y0, (r.0 + r.2).min(width) - x0, (r.1 + r.3).min(height) - y0)
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// macOS Screen Sharing announces RFB 003.889; our own server names its OS.
fn detect(banner: &str, name: &str) -> RemoteKind {
    if banner.contains("003.889") {
        RemoteKind::AppleScreenSharing
    } else if name.ends_with("(macOS)") {
        RemoteKind::BeachMac
    } else {
        RemoteKind::Other
    }
}

/// Reader and UI share one writer so their messages never interleave.
fn send(writer: &Mutex<BufWriter<TcpStream>>, msgs: &[ClientMessage]) -> rfb::Result<()> {
    let mut w = writer.lock().unwrap();
    msgs.iter().try_for_each(|m| m.write(&mut *w))?;
    w.flush()?;
    Ok(())
}

fn request((x, y, w, h): Region, incremental: bool) -> ClientMessage {
    ClientMessage::FramebufferUpdateRequest {
        incremental,
        x: x as u16,
        y: y as u16,
        width: w as u16,
        height: h as u16,
    }
}

struct Reader {
    input: BufReader<TcpStream>,
    output: Arc<Mutex<BufWriter<TcpStream>>>,
    fb: Arc<Mutex<Framebuffer>>,
    region: Arc<Mutex<Option<Region>>>,
    pf: PixelFormat,
    zrle: ZrleDecoder,
    notices: Sender<Notice>,
    files: SyncSender<rfb::files::Response>,
    repaint: Box<dyn Fn() + Send>,
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
        let _ = self.notices.send(Notice::Closed(reason));
        (self.repaint)();
    }

    fn serve(&mut self) -> rfb::Result<()> {
        loop {
            match ServerMessage::read(&mut self.input)? {
                ServerMessage::FramebufferUpdate { rects } => {
                    for _ in 0..rects {
                        let rect = Rect::read(&mut self.input)?;
                        self.apply(rect)?;
                    }
                    let (w, h) = {
                        let mut fb = self.fb.lock().unwrap();
                        fb.generation += 1;
                        (fb.width, fb.height)
                    };
                    (self.repaint)();
                    let region = self.region.lock().unwrap().unwrap_or((0, 0, w, h));
                    send(&self.output, &[request(region, true)])?;
                }
                ServerMessage::ServerCutText(text) => {
                    let _ = self.notices.send(Notice::Clipboard(text));
                    (self.repaint)();
                }
                ServerMessage::FileTransfer(response) => {
                    // There is one outstanding file request. A peer that floods
                    // unsolicited replies must not block framebuffer decoding.
                    if let Err(std::sync::mpsc::TrySendError::Full(_)) = self.files.try_send(response) {
                        return Err(Error::Protocol("unsolicited file transfer response".into()));
                    }
                }
                ServerMessage::Bell | ServerMessage::SetColourMapEntries => {}
            }
        }
    }

    fn apply(&mut self, r: Rect) -> rfb::Result<()> {
        let (x, y, w, h) = (r.x as usize, r.y as usize, r.width as usize, r.height as usize);
        if r.encoding == encoding::DESKTOP_SIZE {
            let mut fb = self.fb.lock().unwrap();
            *fb = Framebuffer::new(w, h, fb.generation, Vec::new());
            *self.region.lock().unwrap() = None;
            return Ok(());
        }
        if r.encoding == encoding::EXTENDED_DESKTOP_SIZE {
            let screens = messages::read_screens(&mut self.input)?;
            // `x` is the reason, `y` the status of a request. A refused
            // request keeps the old size; only a success changes anything.
            if r.y != messages::DESKTOP_SIZE_OK {
                return Ok(());
            }
            let mut fb = self.fb.lock().unwrap();
            if (w, h) != (fb.width, fb.height) {
                *fb = Framebuffer::new(w, h, fb.generation, screens);
                *self.region.lock().unwrap() = None;
            } else if fb.screens != screens {
                fb.screens = screens;
                *self.region.lock().unwrap() = None;
            }
            return Ok(());
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
        Ok(())
    }
}

fn blit(fb: &mut Framebuffer, x: usize, y: usize, w: usize, rgba: &[u8]) {
    let stride = fb.width * 4;
    for (row, src) in rgba.chunks_exact(w * 4).enumerate() {
        fb.rgba[(y + row) * stride + x * 4..][..w * 4].copy_from_slice(src);
    }
}

/// Owned by one file worker; no file I/O or response waits run on the UI thread.
pub struct FileChannel {
    writer: Arc<Mutex<BufWriter<TcpStream>>>,
    replies: Receiver<rfb::files::Response>,
}

impl FileChannel {
    pub fn hello(&self) -> Result<rfb::files::Response, std::sync::mpsc::RecvTimeoutError> {
        self.replies.recv_timeout(Duration::from_secs(8))
    }

    pub fn cancel_pending(&self) {
        let _ = send(
            &self.writer,
            &[ClientMessage::FileTransfer(rfb::files::Request::Cancel)],
        );
    }

    pub fn request(&self, request: rfb::files::Request) -> rfb::Result<rfb::files::Response> {
        send(&self.writer, &[ClientMessage::FileTransfer(request)])?;
        self.replies
            .recv_timeout(Duration::from_secs(30))
            .map_err(|e| Error::Protocol(format!("file transfer response missing: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use beach_access::parse_address;

    #[test]
    fn detects_macs() {
        use super::{detect, RemoteKind};
        assert_eq!(
            detect("RFB 003.889", "Sebastians Mac mini"),
            RemoteKind::AppleScreenSharing
        );
        assert_eq!(
            detect("RFB 003.008", "VNC Beach @ mini (macOS)"),
            RemoteKind::BeachMac
        );
        assert_eq!(
            detect("RFB 003.008", "VNC Beach @ pc (Windows)"),
            RemoteKind::Other
        );
        assert_eq!(detect("RFB 003.008", "TigerVNC"), RemoteKind::Other);
        assert!(RemoteKind::BeachMac.is_mac() && !RemoteKind::Other.is_mac());
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address("mac.local"), "mac.local:5900");
        assert_eq!(parse_address("10.0.0.2:1"), "10.0.0.2:5901");
        assert_eq!(parse_address("10.0.0.2:5905"), "10.0.0.2:5905");
        assert_eq!(parse_address("10.0.0.2::5"), "10.0.0.2:5");
        assert_eq!(parse_address("[::1]:5901"), "[::1]:5901");
        assert_eq!(parse_address("::1"), "[::1]:5900");
        assert_eq!(parse_address("fd00::9"), "[fd00::9]:5900");
    }
}
