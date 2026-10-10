//! The local clipboard during a session: polled for changes (which go to the
//! server) and set from the server's cut text.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

pub struct Clipboard {
    set: Sender<String>,
    changes: Receiver<String>,
}

impl Clipboard {
    /// Starts polling. The current clipboard counts as a change right away,
    /// so what was copied before connecting can be pasted remotely.
    pub fn start(ctx: egui::Context) -> Clipboard {
        let (set, set_rx) = channel::<String>();
        let (changes_tx, changes) = channel();
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || poll(ctx, set_rx, changes_tx))
            .expect("spawn clipboard thread");
        Clipboard { set, changes }
    }

    /// Text from the server; not reported back as a local change.
    pub fn set(&self, text: String) {
        let _ = self.set.send(text);
    }

    /// Local changes since the last call.
    pub fn changes(&self) -> Vec<String> {
        self.changes.try_iter().collect()
    }
}

fn poll(ctx: egui::Context, set_rx: Receiver<String>, changes: Sender<String>) {
    let Ok(mut board) = arboard::Clipboard::new() else {
        log::warn!("clipboard unavailable, clipboard sync disabled");
        return;
    };
    let mut last = String::new();
    loop {
        match set_rx.recv_timeout(Duration::from_millis(400)) {
            Ok(text) => {
                if let Err(e) = board.set_text(text.clone()) {
                    log::warn!("setting clipboard failed: {e}");
                }
                last = text;
            }
            Err(RecvTimeoutError::Timeout) => {
                let Ok(current) = board.get_text() else { continue };
                if current != last && !current.is_empty() {
                    log::debug!("local clipboard changed: {} chars", current.chars().count());
                    last = current.clone();
                    if changes.send(current).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
