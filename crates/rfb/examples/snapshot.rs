//! Connects to a VNC server, fetches one full frame plus a few incremental
//! updates and writes the screen as PPM. Handy for testing servers headless.
//!
//!     cargo run -p rfb --example snapshot -- 127.0.0.1:5900 password out.ppm [UPDATES]
//!
//! Incremental updates only arrive when the screen changes.

use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::time::Instant;

use rfb::encoding;
use rfb::handshake::client_handshake;
use rfb::messages::{ClientMessage, Rect, ServerMessage};
use rfb::zrle::ZrleDecoder;
use rfb::PixelFormat;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (addr, password, out, updates) = match &args[1..] {
        [a, p, o] => (a, p, o, 1),
        [a, p, o, n] => (a, p, o, n.parse()?),
        _ => return Err("usage: snapshot HOST:PORT PASSWORD OUT.ppm [UPDATES]".into()),
    };
    let started = Instant::now();
    let mut stream = TcpStream::connect(addr)?;
    let pw = (!password.is_empty()).then_some(password.as_str());
    let init = client_handshake(&mut stream, pw)?;
    println!(
        "connected to {:?} {}x{} in {:?}",
        init.name,
        init.width,
        init.height,
        started.elapsed()
    );

    let (w, h) = (init.width as usize, init.height as usize);
    let pf = PixelFormat::BGRX;
    let mut writer = stream.try_clone()?;
    ClientMessage::SetPixelFormat(pf).write(&mut writer)?;
    ClientMessage::SetEncodings(vec![encoding::ZRLE, encoding::RAW]).write(&mut writer)?;
    let request = |incremental| ClientMessage::FramebufferUpdateRequest {
        incremental,
        x: 0,
        y: 0,
        width: w as u16,
        height: h as u16,
    };
    request(false).write(&mut writer)?;

    let mut reader = BufReader::new(stream);
    let mut fb = vec![0u8; w * h * 4];
    let mut zrle = ZrleDecoder::new();
    for update in 0..updates {
        let t = Instant::now();
        let ServerMessage::FramebufferUpdate { rects } = ServerMessage::read(&mut reader)? else {
            continue;
        };
        for _ in 0..rects {
            let r = Rect::read(&mut reader)?;
            let (x, y, rw, rh) = (r.x as usize, r.y as usize, r.width as usize, r.height as usize);
            let rgba = match r.encoding {
                encoding::ZRLE => zrle.decode(&mut reader, &pf, rw, rh)?,
                encoding::RAW => {
                    let mut raw = vec![0; rw * rh * 4];
                    std::io::Read::read_exact(&mut reader, &mut raw)?;
                    raw.chunks_exact(4)
                        .flat_map(|p| [p[2], p[1], p[0], 255])
                        .collect()
                }
                e => return Err(format!("unexpected encoding {e}").into()),
            };
            for row in 0..rh {
                fb[((y + row) * w + x) * 4..][..rw * 4].copy_from_slice(&rgba[row * rw * 4..][..rw * 4]);
            }
        }
        println!("update {update}: {rects} rects in {:?}", t.elapsed());
        request(true).write(&mut writer)?;
    }

    let mut f = std::io::BufWriter::new(std::fs::File::create(out)?);
    write!(f, "P6\n{w} {h}\n255\n")?;
    for px in fb.chunks_exact(4) {
        f.write_all(&px[..3])?;
    }
    println!("wrote {out}");
    Ok(())
}
