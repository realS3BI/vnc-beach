//! Injects viewer keyboard and mouse events on the local desktop.
//!
//! One thread owns the platform input backend; sessions send events to it.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use rfb::keysym;

use crate::capture::Geometry;

pub enum Event {
    Pointer {
        buttons: u8,
        x: u16,
        y: u16,
        fb_width: usize,
        fb_height: usize,
    },
    Key {
        down: bool,
        keysym: u32,
    },
    /// A viewer disconnected: release everything it still holds.
    ReleaseAll,
}

pub struct Input {
    tx: Sender<Event>,
}

impl Input {
    pub fn start(geometry: Geometry) -> anyhow::Result<Input> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        std::thread::Builder::new().name("input".into()).spawn(move || {
            let settings = Settings {
                open_prompt_to_get_permissions: true,
                ..Settings::default()
            };
            let enigo = match Enigo::new(&settings) {
                Ok(e) => {
                    let _ = ready_tx.send(Ok(()));
                    e
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(anyhow::anyhow!(
                        "input injection unavailable ({e}). On macOS, allow this app/terminal under \
                         System Settings → Privacy & Security → Accessibility"
                    )));
                    return;
                }
            };
            let mut state = State {
                enigo,
                geometry,
                buttons: 0,
                keys: HashMap::new(),
            };
            for event in rx {
                state.handle(event);
            }
        })?;
        ready_rx.recv()??;
        Ok(Input { tx })
    }

    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

struct State {
    enigo: Enigo,
    geometry: Geometry,
    buttons: u8,
    /// Keysyms currently down, with the key we pressed for them (`None` if the
    /// press was typed as text and needs no release).
    keys: HashMap<u32, Option<Key>>,
}

impl State {
    fn handle(&mut self, event: Event) {
        let result = match event {
            Event::Pointer {
                buttons,
                x,
                y,
                fb_width,
                fb_height,
            } => self.pointer(buttons, x, y, fb_width, fb_height),
            Event::Key { down, keysym } => self.key(down, keysym),
            Event::ReleaseAll => {
                self.release_all();
                Ok(())
            }
        };
        if let Err(e) = result {
            log::debug!("input event failed: {e}");
        }
    }

    fn pointer(&mut self, buttons: u8, x: u16, y: u16, fb_w: usize, fb_h: usize) -> enigo::InputResult<()> {
        // Framebuffer pixels → desktop coordinates (handles Retina/HiDPI scaling).
        let g = self.geometry;
        let dx = g.x + (x as f64 * g.width as f64 / fb_w.max(1) as f64) as i32;
        let dy = g.y + (y as f64 * g.height as f64 / fb_h.max(1) as f64) as i32;
        self.enigo.move_mouse(dx, dy, Coordinate::Abs)?;

        let changed = buttons ^ self.buttons;
        for (bit, button) in [(0, Button::Left), (1, Button::Middle), (2, Button::Right)] {
            if changed & 1 << bit != 0 {
                let dir = if buttons & 1 << bit != 0 {
                    Direction::Press
                } else {
                    Direction::Release
                };
                self.enigo.button(button, dir)?;
            }
        }
        // Wheel "buttons" 4–7 fire on press.
        let pressed = changed & buttons;
        for (bit, len, axis) in [
            (3, -1, Axis::Vertical),
            (4, 1, Axis::Vertical),
            (5, -1, Axis::Horizontal),
            (6, 1, Axis::Horizontal),
        ] {
            if pressed & 1 << bit != 0 {
                self.enigo.scroll(len, axis)?;
            }
        }
        self.buttons = buttons;
        Ok(())
    }

    fn key(&mut self, down: bool, sym: u32) -> enigo::InputResult<()> {
        if !down {
            // Ignore releases for keys we never pressed (e.g. after ReleaseAll).
            return match self.keys.remove(&sym) {
                Some(Some(key)) => self.enigo.key(key, Direction::Release),
                _ => Ok(()),
            };
        }
        if let Some(key) = special_key(sym) {
            self.keys.insert(sym, Some(key));
            return self.enigo.key(key, Direction::Press);
        }
        let Some(c) = keysym::to_char(sym) else {
            log::debug!("unmapped keysym {sym:#x}");
            return Ok(());
        };
        if self.shortcut_modifier_held() {
            // Ctrl/Cmd+key: press the physical key so apps see a shortcut.
            let key = Key::Unicode(c.to_ascii_lowercase());
            self.keys.insert(sym, Some(key));
            self.enigo.key(key, Direction::Press)
        } else {
            // Plain typing: inject the character itself, independent of the
            // local keyboard layout.
            self.keys.insert(sym, None);
            self.enigo.text(&c.to_string())
        }
    }

    fn shortcut_modifier_held(&self) -> bool {
        use keysym::*;
        self.keys
            .keys()
            .any(|&k| matches!(k, CONTROL_L | CONTROL_R | META_L | META_R | SUPER_L | SUPER_R))
    }

    fn release_all(&mut self) {
        for key in std::mem::take(&mut self.keys).into_values().flatten() {
            let _ = self.enigo.key(key, Direction::Release);
        }
        for (bit, button) in [(0, Button::Left), (1, Button::Middle), (2, Button::Right)] {
            if self.buttons & 1 << bit != 0 {
                let _ = self.enigo.button(button, Direction::Release);
            }
        }
        self.buttons = 0;
    }
}

/// Non-character keys.
fn special_key(sym: u32) -> Option<Key> {
    use keysym::*;
    Some(match sym {
        BACKSPACE => Key::Backspace,
        TAB => Key::Tab,
        RETURN | 0xff8d => Key::Return,
        ESCAPE => Key::Escape,
        DELETE => Key::Delete,
        HOME => Key::Home,
        END => Key::End,
        PAGE_UP => Key::PageUp,
        PAGE_DOWN => Key::PageDown,
        LEFT => Key::LeftArrow,
        UP => Key::UpArrow,
        RIGHT => Key::RightArrow,
        DOWN => Key::DownArrow,
        #[cfg(not(target_os = "macos"))]
        INSERT => Key::Insert,
        SHIFT_L | SHIFT_R => Key::Shift,
        CONTROL_L | CONTROL_R => Key::Control,
        ALT_L | ALT_R | ISO_LEVEL3_SHIFT => Key::Alt,
        META_L | META_R | SUPER_L | SUPER_R => Key::Meta,
        CAPS_LOCK => Key::CapsLock,
        s if (F1..F1 + 12).contains(&s) => [
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
            Key::F11,
            Key::F12,
        ][(s - F1) as usize],
        _ => return None,
    })
}
