//! A server that can be added to an account from a viewer (`rfb::enroll`),
//! for testing without the viewer's background process. It keeps the
//! registration in memory, accepts the account's tickets from then on and
//! prints the device token, so heartbeats can be sent by hand:
//!
//! ```sh
//! cargo run -p beach-server --example enroll -- 127.0.0.1:5998 PASSWORD
//! ```

use std::sync::{Arc, Mutex, OnceLock};

use beach_account::api::{EnrollHello, EnrolledDevice, Enrollment};
use beach_server::session::Enroller;
use beach_server::{Options, Server};

struct Memory {
    server: OnceLock<Arc<Server>>,
    device: Mutex<Option<Enrollment>>,
}

impl Enroller for Memory {
    fn hello(&self) -> EnrollHello {
        EnrollHello {
            name: "Enroll-Test".into(),
            platform: if cfg!(windows) { "windows" } else { "macos" }.into(),
            device: self.device.lock().unwrap().as_ref().map(|e| EnrolledDevice {
                url: e.url.clone(),
                id: e.id.clone(),
                name: e.name.clone(),
            }),
        }
    }

    fn enroll(&self, e: Enrollment) -> Result<(), String> {
        let mut device = self.device.lock().unwrap();
        if device.is_some() {
            return Err("Schon in einem Konto.".into());
        }
        let key = beach_account::decode_key(&e.coordinator_key).ok_or("Ungültiger Schlüssel.")?;
        let verifier = beach_account::ticket::Verifier { device: e.id.clone(), key };
        if let Some(server) = self.server.get() {
            server.set_tickets(Some(verifier));
        }
        println!("enrolled as {} ({}) at {}, device token {}", e.name, e.id, e.url, e.token);
        *device = Some(e);
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args().skip(1);
    let (Some(addr), Some(password)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: enroll HOST:PORT PASSWORD");
    };
    let (bind, port) = addr.rsplit_once(':').ok_or_else(|| anyhow::anyhow!("HOST:PORT expected"))?;
    let server = Arc::new(Server::start(Options {
        bind: bind.into(),
        port: port.parse()?,
        password: Some(password),
        fps: 10,
        ..Options::default()
    })?);
    let memory = Arc::new(Memory {
        server: OnceLock::new(),
        device: Mutex::new(None),
    });
    let _ = memory.server.set(server.clone());
    server.set_enroller(memory);
    println!("listening on {}", server.local_addr());
    loop {
        std::thread::park();
    }
}
