//! Server clipboard: polls for local changes and applies text from viewers.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Clipboard {
    text: Mutex<String>,
    seq: AtomicU64,
    stopped: AtomicBool,
}

impl Clipboard {
    pub fn start() -> Arc<Clipboard> {
        let cb = Arc::new(Clipboard {
            text: Mutex::new(String::new()),
            seq: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        });
        let c = cb.clone();
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || c.poll())
            .expect("spawn clipboard thread");
        cb
    }

    fn poll(&self) {
        let Ok(mut board) = arboard::Clipboard::new() else {
            log::warn!("clipboard unavailable, clipboard sync disabled");
            return;
        };
        // Don't push whatever happens to be in the clipboard at startup.
        if let Ok(t) = board.get_text() {
            *self.text.lock().unwrap() = t;
        }
        while !self.stopped.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(500));
            let Ok(current) = board.get_text() else { continue };
            let mut text = self.text.lock().unwrap();
            if *text != current {
                log::debug!("local clipboard changed: {} chars", current.chars().count());
                *text = current;
                self.seq.fetch_add(1, Ordering::Release);
            }
        }
    }

    /// Ends the polling thread.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    pub fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    /// Text from a viewer. Recorded as current so it isn't echoed back.
    pub fn set(&self, new: String) {
        let mut text = self.text.lock().unwrap();
        if *text == new {
            return;
        }
        match arboard::Clipboard::new().and_then(|mut b| b.set_text(new.clone())) {
            Ok(()) => {
                log::debug!("clipboard set from viewer: {} chars", new.chars().count());
                *text = new;
            }
            Err(e) => log::warn!("setting clipboard failed: {e}"),
        }
    }
}
