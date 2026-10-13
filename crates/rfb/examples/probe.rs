//! Pokes a VNC server with single messages, to learn how it reacts.
//!
//!     cargo run -p rfb --example probe -- HOST:PORT PASSWORD CMD...
//!
//! Commands: `cut:TEXT` sends ClientCutText; `key:HEX:MS` presses keysym HEX
//! for MS milliseconds; `move:X:Y` moves the pointer; `click:X:Y` clicks the
//! left button there; `listen:SECS` prints ServerCutText for a while.

use std::io::BufReader;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use rfb::handshake::client_handshake_with_banner;
use rfb::messages::{ClientMessage, ServerMessage};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [addr, password, cmds @ ..] = &args[1..] else {
        return Err("usage: probe HOST:PORT PASSWORD CMD...".into());
    };
    let mut stream = TcpStream::connect(addr)?;
    let pw = (!password.is_empty()).then_some(password.as_str());
    let (init, banner) = client_handshake_with_banner(&mut stream, pw)?;
    println!("connected: {banner} {:?}", init.name);
    let mut writer = stream.try_clone()?;
    for cmd in cmds {
        let parts: Vec<&str> = cmd.splitn(3, ':').collect();
        match parts.as_slice() {
            ["cut", text] => {
                ClientMessage::ClientCutText(text.to_string()).write(&mut writer)?;
                println!("sent cut text {text:?}");
            }
            ["key", hex, ms] => {
                let key = u32::from_str_radix(hex, 16)?;
                ClientMessage::KeyEvent { down: true, key }.write(&mut writer)?;
                println!("key {hex} down");
                std::thread::sleep(Duration::from_millis(ms.parse()?));
                ClientMessage::KeyEvent { down: false, key }.write(&mut writer)?;
                println!("key {hex} up");
            }
            ["move", x, y] => {
                let (x, y) = (x.parse()?, y.parse()?);
                ClientMessage::PointerEvent { buttons: 0, x, y }.write(&mut writer)?;
                println!("pointer at {x},{y}");
            }
            ["click", x, y] => {
                let (x, y) = (x.parse()?, y.parse()?);
                for buttons in [1, 0] {
                    ClientMessage::PointerEvent { buttons, x, y }.write(&mut writer)?;
                    std::thread::sleep(Duration::from_millis(50));
                }
                println!("clicked at {x},{y}");
            }
            ["listen", secs] => {
                let until = Instant::now() + Duration::from_secs(secs.parse()?);
                stream.set_read_timeout(Some(Duration::from_millis(500)))?;
                let mut reader = BufReader::new(stream.try_clone()?);
                while Instant::now() < until {
                    match ServerMessage::read(&mut reader) {
                        Ok(ServerMessage::ServerCutText(t)) => println!("server cut text {t:?}"),
                        Ok(ServerMessage::Bell) => println!("bell"),
                        Ok(_) => println!("other message"),
                        Err(rfb::Error::Io(e))
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            _ => return Err(format!("unknown command {cmd:?}").into()),
        }
    }
    Ok(())
}
