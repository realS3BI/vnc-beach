//! macOS privacy permissions the server depends on. Elsewhere everything is
//! allowed.

/// Screen Recording. Without it captures contain only wallpaper and menu bar.
/// macOS answers for the running process; a grant often only shows up after
/// a restart.
pub fn screen_recording() -> bool {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGPreflightScreenCaptureAccess() -> bool;
        }
        // SAFETY: plain CoreGraphics call without arguments.
        unsafe { CGPreflightScreenCaptureAccess() }
    }
    #[cfg(not(target_os = "macos"))]
    true
}

/// Accessibility, needed to inject keyboard and mouse input. Changes apply
/// immediately.
pub fn accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "ApplicationServices", kind = "framework")]
        extern "C" {
            fn AXIsProcessTrusted() -> bool;
        }
        // SAFETY: plain ApplicationServices call without arguments.
        unsafe { AXIsProcessTrusted() }
    }
    #[cfg(not(target_os = "macos"))]
    true
}
