//! Windows: the downloaded `VNC-Beach-Windows.exe` installs itself on its
//! first start. It copies itself to `%LOCALAPPDATA%\Programs\vnc-beach`,
//! adds the Start menu shortcut "VNC Beach" and starts the installed copy,
//! which deletes the download. Without this, Windows Search lists the
//! download under its file name instead of "VNC Beach".

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Context as _;

use crate::update::Version;

/// Hidden argument for the installed copy: the download it was copied from.
pub const INSTALLED_FROM: &str = "--installed-from";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Installs a copy that runs from anywhere else and starts the installed
/// one with the same arguments; returns true if the caller should exit.
/// Development builds (in a `target` directory) stay where they are.
pub fn relocate() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(target) = installed_exe() else {
        return false;
    };
    if same_path(&exe, &target) || in_build_dir(&exe) {
        return false;
    }
    match install(&exe, &target) {
        Ok(stopped_background) => {
            if let Err(e) = add_shortcut(&target) {
                log::warn!("start menu shortcut: {e:#}");
            }
            let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
            let starts_background = args.iter().any(|a| a == "--background");
            args.push(INSTALLED_FROM.into());
            args.push(exe.into());
            if stopped_background && !starts_background {
                // Restart the tray process that was stopped for the copy.
                if let Err(e) = Command::new(&target).arg("--background").spawn() {
                    log::warn!("restart background process: {e}");
                }
            }
            match Command::new(&target).args(&args).spawn() {
                Ok(_) => true,
                Err(e) => {
                    log::warn!("start installed copy: {e}");
                    false
                }
            }
        }
        Err(e) => {
            log::warn!("install to {}: {e:#}", target.display());
            false
        }
    }
}

/// Deletes the download the installed copy came from, once the process
/// that ran it has exited (right after starting this one); only if it is
/// the same program. Blocks: a second tray process exits right away.
pub fn remove_download(download: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if same_path(&exe, download) || !same_contents(&exe, download) {
        return;
    }
    let until = Instant::now() + Duration::from_secs(5);
    while let Err(e) = std::fs::remove_file(download) {
        if Instant::now() > until {
            return log::warn!("remove {}: {e}", download.display());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `%LOCALAPPDATA%\Programs\vnc-beach\vnc-beach.exe`
fn installed_exe() -> Option<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(dir).join(r"Programs\vnc-beach\vnc-beach.exe"))
}

/// Copies `exe` to `target` unless a newer or identical copy is there.
/// Returns whether a background process was stopped for it.
fn install(exe: &Path, target: &Path) -> anyhow::Result<bool> {
    if target.exists() {
        // Never replace a newer version (an old download started again).
        if installed_version(target).is_some_and(|v| v > Version::current()) || same_contents(exe, target) {
            return Ok(false);
        }
    }
    let dir = target.parent().context("no install directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let background = crate::update::stop_background();
    // A running copy (the viewer window) can be renamed, not overwritten.
    let new = target.with_extension("exe.new");
    std::fs::copy(exe, &new).context("copy")?;
    if target.exists() {
        let old = target.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old).context("move installed copy aside")?;
    }
    std::fs::rename(&new, target).context("move new copy in place")?;
    log::info!("installed to {}", target.display());
    Ok(background)
}

/// The installed copy's version, from its `--version` (`beach-viewer 0.2.5`).
fn installed_version(target: &Path) -> Option<Version> {
    let out = Command::new(target)
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Version::parse(String::from_utf8_lossy(&out.stdout).split_whitespace().last()?)
}

/// Creates or updates `VNC Beach.lnk` in the user's Start menu programs.
fn add_shortcut(target: &Path) -> anyhow::Result<()> {
    let programs = PathBuf::from(std::env::var_os("APPDATA").context("APPDATA not set")?)
        .join(r"Microsoft\Windows\Start Menu\Programs");
    std::fs::create_dir_all(&programs)?;
    let link = programs.join("VNC Beach.lnk");
    let dir = target.parent().context("no install directory")?;
    let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); \
         $s.TargetPath = {}; $s.WorkingDirectory = {}; $s.Description = 'VNC Beach'; $s.Save()",
        quote(&link),
        quote(target),
        quote(dir)
    );
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("run powershell")?;
    anyhow::ensure!(
        out.status.success(),
        "powershell: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.as_os_str().eq_ignore_ascii_case(b.as_os_str()),
    }
}

fn same_contents(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn in_build_dir(exe: &Path) -> bool {
    exe.components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
}
