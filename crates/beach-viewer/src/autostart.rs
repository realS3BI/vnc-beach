//! Start the background process at login: HKCU Run key on Windows, a
//! LaunchAgent on macOS.

use std::io;

#[cfg(windows)]
const NAME: &str = "VNC Beach";

pub fn supported() -> bool {
    cfg!(windows) || cfg!(target_os = "macos")
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
pub fn enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(NAME))
        .is_ok()
}

#[cfg(windows)]
pub fn set(on: bool) -> io::Result<()> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE)?;
    if on {
        let exe = std::env::current_exe()?;
        key.set_value(NAME, &format!("\"{}\" --background", exe.display()))
    } else {
        match key.delete_value(NAME) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
}

#[cfg(target_os = "macos")]
const LABEL: &str = "at.schlossers.vnc-beach";

#[cfg(target_os = "macos")]
fn plist_path() -> io::Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME not set"))?;
    Ok(std::path::PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
pub fn enabled() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

#[cfg(target_os = "macos")]
pub fn set(on: bool) -> io::Result<()> {
    let path = plist_path()?;
    let launchctl = |args: &[&str]| {
        std::process::Command::new("launchctl")
            .args(args)
            .output()
            .map(|_| ())
    };
    if on {
        let exe = std::env::current_exe()?;
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array><string>{}</string><string>--background</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><false/>
</dict>
</plist>
"#,
            exe.display()
        );
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = launchctl(&["unload", &path.to_string_lossy()]);
        std::fs::write(&path, plist)?;
        launchctl(&["load", &path.to_string_lossy()])
    } else {
        if path.exists() {
            let _ = launchctl(&["unload", &path.to_string_lossy()]);
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn enabled() -> bool {
    false
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn set(_on: bool) -> io::Result<()> {
    Err(io::Error::other(
        "autostart is only supported on Windows and macOS",
    ))
}
