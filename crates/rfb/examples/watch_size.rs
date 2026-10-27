//! Stays connected to a VNC server and prints every framebuffer size and
//! screen layout change it announces (DesktopSize / ExtendedDesktopSize),
//! plus the size of the first complete frame after each change. For testing
//! display hotplug, e.g. the Windows virtual display (docs/headless-windows.md).
//!
//!     cargo run -p rfb --example watch_size -- HOST:PORT PASSWORD [SECONDS]

use std::io::BufReader;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use rfb::encoding;
use rfb::handshake::client_handshake;
use rfb::messages::{read_screens, ClientMessage, Rect, ServerMessage};
use rfb::zrle::ZrleDecoder;
use rfb::PixelFormat;

/// Connects, requests the size pseudo-encodings and prints every change
/// until the time limit (default 600 s) runs out.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (addr, password, seconds) = match &args[1..] {
        [a, p] => (a, p, 600),
        [a, p, s] => (a, p, s.parse()?),
        _ => return Err("usage: watch_size HOST:PORT PASSWORD [SECONDS]".into()),
    };
    let started = Instant::now();
    let mut stream = TcpStream::connect(addr)?;
    let pw = (!password.is_empty()).then_some(password.as_str());
    let init = client_handshake(&mut stream, pw)?;
    let (mut w, mut h) = (init.width as usize, init.height as usize);
    let stamp = |started: Instant| format!("{:7.1}s", started.elapsed().as_secs_f64());
    println!("{} connected to {:?} {w}x{h}", stamp(started), init.name);

    let pf = PixelFormat::BGRX;
    let mut writer = stream.try_clone()?;
    ClientMessage::SetPixelFormat(pf).write(&mut writer)?;
    ClientMessage::SetEncodings(vec![
        encoding::ZRLE,
        encoding::RAW,
        encoding::DESKTOP_SIZE,
        encoding::EXTENDED_DESKTOP_SIZE,
    ])
    .write(&mut writer)?;
    let request = |w: usize, h: usize, incremental| ClientMessage::FramebufferUpdateRequest {
        incremental,
        x: 0,
        y: 0,
        width: w as u16,
        height: h as u16,
    };
    request(w, h, false).write(&mut writer)?;

    stream.set_read_timeout(Some(Duration::from_secs(seconds)))?;
    let mut reader = BufReader::new(stream);
    let mut zrle = ZrleDecoder::new();
    // Pixels received since the last size change; a full frame covers w*h.
    let mut covered = 0usize;
    let mut reported = false;
    while started.elapsed() < Duration::from_secs(seconds) {
        let ServerMessage::FramebufferUpdate { rects } = ServerMessage::read(&mut reader)? else {
            continue;
        };
        let mut full = false;
        for _ in 0..rects {
            let r = Rect::read(&mut reader)?;
            let (rw, rh) = (r.width as usize, r.height as usize);
            match r.encoding {
                encoding::ZRLE => {
                    zrle.decode(&mut reader, &pf, rw, rh)?;
                    covered += rw * rh;
                }
                encoding::RAW => {
                    let mut raw = vec![0; rw * rh * pf.bytes_per_pixel()];
                    std::io::Read::read_exact(&mut reader, &mut raw)?;
                    covered += rw * rh;
                }
                encoding::DESKTOP_SIZE => {
                    (w, h) = (rw, rh);
                    (covered, reported, full) = (0, false, true);
                    println!("{} DesktopSize {w}x{h}", stamp(started));
                }
                encoding::EXTENDED_DESKTOP_SIZE => {
                    let screens = read_screens(&mut reader)?;
                    if (rw, rh) != (w, h) {
                        (w, h) = (rw, rh);
                        (covered, reported, full) = (0, false, true);
                    }
                    let list: Vec<String> = screens
                        .iter()
                        .map(|s| format!("{}x{}@{},{}", s.width, s.height, s.x, s.y))
                        .collect();
                    println!(
                        "{} ExtendedDesktopSize {rw}x{rh}, screens [{}]",
                        stamp(started),
                        list.join(", ")
                    );
                }
                e => return Err(format!("unexpected encoding {e}").into()),
            }
        }
        if !reported && covered >= w * h {
            println!("{} complete frame {w}x{h}", stamp(started));
            reported = true;
        }
        request(w, h, !full).write(&mut writer)?;
    }
    Ok(())
}
