//! Windows: keeps a thread on the desktop that currently receives input.
//!
//! The lock screen, the sign-in screen and UAC prompts live on the
//! "Winlogon" desktop, everything else on "Default". GDI capture and
//! `SendInput` only reach the desktop the calling thread is attached to, so
//! the capture and input threads call [`follow_input`] before they work. Only
//! a process running as SYSTEM in the console session (the VNC Beach Windows
//! service) may open the Winlogon desktop; for a normal user process the
//! switch fails quietly and nothing changes.

use std::cell::RefCell;
use std::ffi::c_void;

use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop, HDESK, UOI_NAME,
};

const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

/// The desktop this thread was moved to, and its name.
struct Current {
    handle: HDESK,
    name: String,
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
    /// The last failure, so a lasting one is logged once.
    static FAILURE: RefCell<String> = const { RefCell::new(String::new()) };
}

fn failed(what: String) {
    FAILURE.with(|f| {
        if *f.borrow() != what {
            log::info!("{what}");
            *f.borrow_mut() = what;
        }
    });
}

/// Moves the calling thread to the input desktop if that changed since the
/// last call. Returns true after a switch.
///
/// The thread must not own windows or hooks, or Windows refuses the switch.
pub fn follow_input() -> bool {
    // SAFETY: plain Win32 calls; the handle is closed unless it ends up as
    // this thread's desktop, in which case `CURRENT` keeps it until the next
    // switch.
    unsafe {
        let desk = OpenInputDesktop(0, 0, MAXIMUM_ALLOWED);
        if desk.is_null() {
            failed(format!("cannot open the input desktop: {}", std::io::Error::last_os_error()));
            return false;
        }
        let name = name(desk);
        let same = CURRENT.with(|c| {
            c.borrow()
                .as_ref()
                .is_some_and(|cur| Some(&cur.name) == name.as_ref())
        });
        let Some(name) = name.filter(|_| !same) else {
            CloseDesktop(desk);
            return false;
        };
        if SetThreadDesktop(desk) == 0 {
            failed(format!(
                "cannot switch to desktop {name:?}: {}",
                std::io::Error::last_os_error()
            ));
            CloseDesktop(desk);
            return false;
        }
        log::info!("now on desktop {name:?}");
        let old = CURRENT.with(|c| c.replace(Some(Current { handle: desk, name })));
        if let Some(old) = old {
            CloseDesktop(old.handle);
        }
        true
    }
}

unsafe fn name(desk: HDESK) -> Option<String> {
    let mut buf = [0u16; 64];
    let mut needed = 0;
    let ok = GetUserObjectInformationW(
        desk,
        UOI_NAME,
        buf.as_mut_ptr().cast::<c_void>(),
        (buf.len() * 2) as u32,
        &mut needed,
    );
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}
