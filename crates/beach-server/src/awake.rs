//! Keeps the display awake so the screen saver and display sleep don't start
//! and the system doesn't lock itself. Opt-in (`keep_awake` in the server
//! settings): without it an unattended computer locks as configured and
//! viewers sign in at the lock screen.

/// Holds the display awake until dropped.
pub struct KeepAwake {
    #[cfg(target_os = "macos")]
    id: u32,
    #[cfg(windows)]
    stop: Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)>,
}

impl KeepAwake {
    pub fn new() -> std::io::Result<KeepAwake> {
        imp::acquire()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        imp::release(self);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::KeepAwake;
    use std::ffi::{c_char, c_void};

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            s: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub fn acquire() -> std::io::Result<KeepAwake> {
        // SAFETY: both strings are NUL-terminated literals; the CFStrings
        // are released after IOKit copied them.
        unsafe {
            let kind = CFStringCreateWithCString(
                std::ptr::null(),
                c"PreventUserIdleDisplaySleep".as_ptr(),
                UTF8,
            );
            let name = CFStringCreateWithCString(
                std::ptr::null(),
                c"VNC Beach: keep the screen unlocked".as_ptr(),
                UTF8,
            );
            if kind.is_null() || name.is_null() {
                return Err(std::io::Error::other("CFStringCreateWithCString failed"));
            }
            let mut id = 0;
            let rc = IOPMAssertionCreateWithName(kind, LEVEL_ON, name, &mut id);
            CFRelease(kind);
            CFRelease(name);
            if rc != 0 {
                return Err(std::io::Error::other(format!(
                    "IOPMAssertionCreateWithName failed: {rc:#x}"
                )));
            }
            Ok(KeepAwake { id })
        }
    }

    pub fn release(k: &mut KeepAwake) {
        // SAFETY: the id came from IOPMAssertionCreateWithName and is
        // released once.
        unsafe {
            IOPMAssertionRelease(k.id);
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::KeepAwake;
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };

    /// The execution state belongs to a thread, so one thread holds it.
    pub fn acquire() -> std::io::Result<KeepAwake> {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("keep-awake".into())
            .spawn(move || {
                // SAFETY: plain flag call.
                let ok = unsafe {
                    SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED)
                } != 0;
                let _ = ready_tx.send(ok);
                if ok {
                    let _ = rx.recv();
                    // SAFETY: as above.
                    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                }
            })?;
        if ready_rx.recv().unwrap_or(false) {
            Ok(KeepAwake { stop: Some((tx, thread)) })
        } else {
            let _ = thread.join();
            Err(std::io::Error::other("SetThreadExecutionState failed"))
        }
    }

    pub fn release(k: &mut KeepAwake) {
        if let Some((tx, thread)) = k.stop.take() {
            drop(tx);
            let _ = thread.join();
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::KeepAwake;

    pub fn acquire() -> std::io::Result<KeepAwake> {
        Err(std::io::Error::other("not supported on this platform"))
    }

    pub fn release(_: &mut KeepAwake) {}
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::KeepAwake;

    /// Creating and releasing the assertion only shows up in
    /// `pmset -g assertions` while held; it changes no settings.
    #[test]
    fn acquires_and_releases() {
        let awake = KeepAwake::new().unwrap();
        drop(awake);
    }
}
