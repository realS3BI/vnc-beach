//! Plays an H.264 file as a viewer's webcam (VNC Beach camera v1), to test
//! a server's virtual camera without a Windows viewer.
//!
//!   ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30 -t 20 -c:v libx264 \
//!     -profile:v baseline -x264-params aud=1:keyint=60 -bsf:v h264_mp4toannexb test.h264
//!   cargo run -p rfb --example camera_feed -- HOST:PORT PASSWORD test.h264
//!
//! The file needs access unit delimiters (`aud=1`); it loops until Ctrl+C.

use std::io::{BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rfb::camera::{ClientMessage as Camera, ServerMessage as CameraServer, Video};
use rfb::encoding;
use rfb::handshake::client_handshake;
use rfb::messages::{ClientMessage, ServerMessage};

/// Splits an Annex B stream at its access unit delimiters (NAL type 9).
fn access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i..i + 3] == [0, 0, 1] && data[i + 3] & 0x1f == 9 {
            let start = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push(start);
            i += 4;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    starts.windows(2).map(|w| data[w[0]..w[1]].to_vec()).filter(|u| !u.is_empty()).collect()
}

fn is_key(unit: &[u8]) -> bool {
    unit.windows(4).any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, addr, password, file] = &args[..] else {
        eprintln!("usage: camera_feed HOST:PORT PASSWORD FILE.h264");
        std::process::exit(2);
    };
    let units = access_units(&std::fs::read(file)?);
    println!("{} access units, {} key frames", units.len(), units.iter().filter(|u| is_key(u)).count());

    let mut stream = TcpStream::connect(addr)?;
    let init = client_handshake(&mut stream, Some(password))?;
    println!("connected to {:?}", init.name);
    let mut out = BufWriter::new(stream.try_clone()?);
    ClientMessage::SetEncodings(vec![encoding::RAW, encoding::BEACH_CAMERA]).write(&mut out)?;
    out.flush()?;

    let wanted = Arc::new(AtomicBool::new(false));
    let key = Arc::new(AtomicBool::new(false));
    let (hello_tx, hello_rx) = std::sync::mpsc::channel();
    std::thread::spawn({
        let (wanted, key) = (wanted.clone(), key.clone());
        let mut input = BufReader::new(stream.try_clone()?);
        move || loop {
            match ServerMessage::read(&mut input) {
                Ok(ServerMessage::Camera(msg)) => {
                    println!("server: {msg:?}");
                    match msg {
                        CameraServer::Hello { .. } => {
                            let _ = hello_tx.send(());
                        }
                        CameraServer::Start => wanted.store(true, Ordering::Release),
                        CameraServer::Stop => wanted.store(false, Ordering::Release),
                        CameraServer::KeyFrame => key.store(true, Ordering::Release),
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("connection: {e}");
                    std::process::exit(1);
                }
            }
        }
    });
    hello_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "the server offers no camera (extension not installed?)")?;
    ClientMessage::Camera(Camera::Sharing(true)).write(&mut out)?;
    out.flush()?;

    let frame = Duration::from_secs_f64(1.0 / 30.0);
    let started = Instant::now();
    let mut next = Instant::now();
    let mut i = 0;
    let mut sending = false;
    loop {
        if !wanted.load(Ordering::Acquire) {
            sending = false;
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        // Start, and recover, at a key frame.
        if !sending || key.swap(false, Ordering::AcqRel) {
            while !is_key(&units[i % units.len()]) {
                i += 1;
            }
            sending = true;
        }
        let unit = &units[i % units.len()];
        ClientMessage::Camera(Camera::Video(Video {
            key_frame: is_key(unit),
            timestamp_us: started.elapsed().as_micros() as u64,
            data: unit.clone(),
        }))
        .write(&mut out)?;
        out.flush()?;
        i += 1;
        next += frame;
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}
