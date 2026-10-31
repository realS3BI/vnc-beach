//! The app side of the browser sign-in: a listener on 127.0.0.1 that the
//! coordination server sends the browser back to with
//! `GET /callback?code=…`. Shared by the desktop viewer and the iOS app.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

pub struct Loopback {
    listener: TcpListener,
}

impl Loopback {
    pub fn bind() -> std::io::Result<Loopback> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        Ok(Loopback { listener })
    }

    pub fn port(&self) -> u16 {
        self.listener.local_addr().map(|a| a.port()).unwrap_or_default()
    }

    /// Answers requests for up to `wait` and returns the first callback
    /// code; other requests get a 404.
    pub fn accept_code(&self, wait: Duration) -> Result<Option<String>, String> {
        let until = Instant::now() + wait;
        loop {
            let mut stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= until {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            match callback_code(&String::from_utf8_lossy(&buf[..n])) {
                Some(code) => {
                    let _ = stream.write_all(DONE_PAGE.as_bytes());
                    return Ok(Some(code));
                }
                None => {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
        }
    }
}

const DONE_PAGE: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
<!doctype html><html lang=\"de\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
<title>VNC Beach</title>\
<body style=\"font:16px system-ui;background:#FFF8EE;color:#17364A;display:grid;place-items:center;height:90vh\">\
<div><h1>Angemeldet</h1><p>Du kannst dieses Fenster schließen und zu VNC Beach zurückkehren.</p></div></body></html>";

/// The `code` of `GET /callback?code=… HTTP/1.1`.
fn callback_code(request: &str) -> Option<String> {
    let line = request.lines().next()?;
    let target = line.strip_prefix("GET ")?.split(' ').next()?;
    let query = target.strip_prefix("/callback?")?;
    query.split('&').find_map(|pair| {
        let value = pair.strip_prefix("code=")?;
        percent_decode(value).filter(|code| !code.is_empty())
    })
}

fn percent_decode(text: &str) -> Option<String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hex = [bytes.next()?, bytes.next()?];
                out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn reads_the_callback_code() {
        assert_eq!(
            callback_code("GET /callback?code=ab%2Bc-_ HTTP/1.1\r\nHost: x\r\n\r\n").as_deref(),
            Some("ab+c-_")
        );
        assert_eq!(callback_code("GET /favicon.ico HTTP/1.1\r\n"), None);
        assert_eq!(callback_code("POST /callback?code=x HTTP/1.1\r\n"), None);
        assert_eq!(callback_code("GET /callback?code= HTTP/1.1\r\n"), None);
    }

    #[test]
    fn serves_the_browser() {
        let loopback = Loopback::bind().unwrap();
        assert_eq!(loopback.accept_code(Duration::from_millis(10)), Ok(None));
        let port = loopback.port();
        let browser = std::thread::spawn(move || {
            let get = |path: &str| {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).unwrap();
                let mut answer = String::new();
                s.read_to_string(&mut answer).unwrap();
                answer
            };
            (get("/favicon.ico"), get("/callback?code=abc"))
        });
        assert_eq!(loopback.accept_code(Duration::from_secs(5)), Ok(Some("abc".into())));
        let (other, done) = browser.join().unwrap();
        assert!(other.starts_with("HTTP/1.1 404"));
        assert!(done.contains("Angemeldet"));
    }
}
