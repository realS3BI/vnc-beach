//! Reachable before anyone signs in: a Windows service, or a LaunchAgent for
//! the macOS login window. Both are set up on their own when VNC Beach
//! starts ([`ensure`]) and brought up to date after an update.
//!
//! **Windows:** the service (`vnc-beach.exe --service`, LocalSystem,
//! session 0) can't see the screen itself. It keeps an agent
//! (`--service-agent`, see [`crate::background::run_agent`]) running as
//! SYSTEM in the console session and starts a new one when that session
//! changes. The agent's capture and input threads follow the input desktop,
//! so they reach the lock and sign-in screen too (`beach_server::desktop`).
//! The agent answers the control channel on `service.port`; the tray process
//! forwards to it instead of running its own server. Setting the service up
//! copies the program to `%ProgramFiles%\VNC Beach` (a SYSTEM service must
//! not run from a folder the user can write to), switches Tailscale to
//! unattended mode and needs administrator rights, so it runs
//! `--install-service` elevated.
//!
//! **macOS:** a global LaunchAgent limited to the `LoginWindow` session runs
//! the same agent as root at the login window; once someone signs in, the
//! tray process's server takes over. Tailscale's Mac apps only run after
//! sign-in, so [`tailscale_daemon_script`] switches to the `tailscaled`
//! variant in Terminal, where the user sees and answers each step.

use std::path::{Path, PathBuf};

use crate::config;

/// Whether the platform has the service at all.
pub fn supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// Whether the service runs the server in place of the tray process. On
/// macOS the login window agent stops when someone signs in, so the tray
/// keeps its own server.
pub fn runs_server() -> bool {
    cfg!(windows) && installed()
}

/// Why setting up or removing the service didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupError {
    /// The user declined the administrator prompt.
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Cancelled => f.write_str(
                "Ohne Administratorrechte lässt sich der Zugang vor der Anmeldung nicht einrichten.",
            ),
            SetupError::Failed(e) => f.write_str(e),
        }
    }
}

/// Sets the service up, or brings it up to date, when it is wanted and
/// missing or outdated. Runs in the tray process at start and may show the
/// system's administrator prompt; a declined prompt isn't repeated for this
/// version.
pub fn ensure() {
    #[cfg(target_os = "macos")]
    crate::mac_server::ensure();
    if !supported() {
        return;
    }
    let mut config = config::load();
    if !config.server.service {
        return;
    }
    let Some(reason) = platform::needs_setup() else {
        return;
    };
    let version = env!("CARGO_PKG_VERSION");
    if config.server.service_declined == version {
        log::info!("service setup needed ({reason}), but declined for this version");
        return;
    }
    log::info!("setting up the service: {reason}");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return log::warn!("service setup: {e}"),
    };
    match platform::request_install(&exe) {
        Ok(()) => log::info!("service set up"),
        Err(SetupError::Cancelled) => {
            config.server.service_declined = version.into();
            if let Err(e) = config::save(&config) {
                log::warn!("cannot save config: {e}");
            }
        }
        Err(e) => log::warn!("service setup failed: {e}"),
    }
}

pub fn installed() -> bool {
    platform::installed()
}

/// Why the service needs setting up (missing, outdated), if it does.
pub fn needs_setup() -> Option<&'static str> {
    platform::needs_setup()
}

/// Sets the service up for the current user, from the program in `exe`.
/// Asks for administrator rights.
pub fn request_install(exe: &Path) -> Result<(), SetupError> {
    platform::request_install(exe)
}

/// Removes the service. Asks for administrator rights.
pub fn request_uninstall() -> Result<(), SetupError> {
    platform::request_uninstall()
}

#[cfg(windows)]
pub use platform::{install, request_display_driver, run_service, uninstall};
#[cfg(target_os = "macos")]
pub use platform::{install, on_console, uninstall};

/// Log file of the service, the agent or the setup: `%ProgramData%\VNC
/// Beach` on Windows, `/Library/Logs/VNC Beach` on macOS (these run as
/// root there).
pub fn log_path(name: &str) -> Option<PathBuf> {
    let dir = if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Logs/VNC Beach")
    } else {
        PathBuf::from(std::env::var_os("ProgramData")?).join("VNC Beach")
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

/// Arguments the service passes on to the agent.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The user's VNC Beach config directory.
    pub config_dir: PathBuf,
    /// The user's home, the root for file transfers.
    pub home: PathBuf,
}

impl Paths {
    /// The current user's directories.
    pub fn current() -> Option<Paths> {
        Some(Paths {
            config_dir: crate::servers::config_dir()?,
            home: dirs::home_dir()?,
        })
    }

    pub fn args(&self) -> Vec<String> {
        vec![
            "--config-dir".into(),
            self.config_dir.display().to_string(),
            "--home".into(),
            self.home.display().to_string(),
        ]
    }
}

/// Command line text for `args`, quoted for `CommandLineToArgvW`.
#[cfg_attr(not(windows), allow(dead_code))]
fn command_line<S: AsRef<str>>(args: &[S]) -> String {
    args.iter()
        .map(|a| {
            let a = a.as_ref();
            if a.is_empty() || a.contains([' ', '\t', '"']) {
                // Backslashes only need doubling before a quote; ours end at
                // the closing one.
                let trailing = a.len() - a.trim_end_matches('\\').len();
                format!("\"{}{}\"", a.replace('"', "\\\""), "\\".repeat(trailing))
            } else {
                a.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `'text'` for sh, with embedded single quotes escaped.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// An AppleScript string literal.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Whether `installed` (as printed by `--version`, e.g. `beach-viewer
/// 0.2.3`) is older than this program.
#[cfg_attr(not(windows), allow(dead_code))]
fn outdated(installed: &str) -> bool {
    use crate::update::Version;
    match installed.split_whitespace().last().and_then(Version::parse) {
        Some(v) => v < Version::current(),
        None => true,
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::SetupError;
    use std::path::Path;

    pub fn installed() -> bool {
        false
    }

    pub fn needs_setup() -> Option<&'static str> {
        None
    }

    pub fn request_install(_exe: &Path) -> Result<(), SetupError> {
        Err(SetupError::Failed("Nur unter Windows und macOS.".into()))
    }

    pub fn request_uninstall() -> Result<(), SetupError> {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{applescript_string, sh_quote, Paths, SetupError};

    const LABEL: &str = "at.schlossers.vnc-beach.loginwindow";

    fn plist_path() -> PathBuf {
        PathBuf::from("/Library/LaunchAgents").join(format!("{LABEL}.plist"))
    }

    pub fn installed() -> bool {
        plist_path().exists()
    }

    /// Whether this process's GUI session is the one on the screen
    /// (`kCGSSessionOnConsoleKey`). True when that can't be read.
    pub fn on_console() -> bool {
        use std::ffi::{c_char, c_void};
        type CFTypeRef = *const c_void;
        const UTF8: u32 = 0x0800_0100;
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
        }
        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            fn CFStringCreateWithCString(alloc: CFTypeRef, text: *const c_char, encoding: u32) -> CFTypeRef;
            fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
            fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
            fn CFRelease(object: CFTypeRef);
        }
        // SAFETY: Create/Copy results are released once; the value is owned
        // by the dictionary and read before the dictionary is released.
        unsafe {
            let dict = CGSessionCopyCurrentDictionary();
            if dict.is_null() {
                return true;
            }
            let key = CFStringCreateWithCString(std::ptr::null(), c"kCGSSessionOnConsoleKey".as_ptr(), UTF8);
            let value = if key.is_null() { std::ptr::null() } else { CFDictionaryGetValue(dict, key) };
            let on = value.is_null() || CFBooleanGetValue(value) != 0;
            if !key.is_null() {
                CFRelease(key);
            }
            CFRelease(dict);
            on
        }
    }

    /// The executable of an installed app bundle. `None` for builds run from
    /// elsewhere and for translocated apps, whose path is gone after a
    /// restart.
    fn bundled_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        if crate::update::translocated() {
            return None;
        }
        exe.ancestors()
            .nth(3)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))?;
        Some(exe)
    }

    fn xml(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    /// The agent runs as root in the login window session only; launchd
    /// ends it when someone signs in, and the tray process's server takes
    /// over. An update replaces the app in place, so the path stays valid.
    fn plist(exe: &Path, paths: &Paths) -> String {
        let mut args = vec![exe.display().to_string(), "--service-agent".into()];
        args.extend(paths.args());
        let args: String = args
            .iter()
            .map(|a| format!("        <string>{}</string>\n", xml(a)))
            .collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>LimitLoadToSessionType</key>
    <array><string>LoginWindow</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
</dict>
</plist>
"#
        )
    }

    pub fn needs_setup() -> Option<&'static str> {
        let exe = bundled_exe()?;
        let paths = Paths::current()?;
        match std::fs::read_to_string(plist_path()) {
            Ok(text) if text == plist(&exe, &paths) => {
                crate::mac_server::needs_refresh().then_some("unattended startup files outdated")
            }
            Ok(_) => Some("login window agent outdated"),
            Err(_) => Some("login window agent missing"),
        }
    }

    /// Runs `command` through the administrator prompt of macOS.
    fn run_as_admin(command: &str) -> Result<(), SetupError> {
        let script = format!(
            "do shell script {} with administrator privileges",
            applescript_string(command)
        );
        let out = Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| SetupError::Failed(e.to_string()))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("(-128)") {
            Err(SetupError::Cancelled)
        } else {
            Err(SetupError::Failed(format!(
                "Einrichten fehlgeschlagen: {}",
                err.trim()
            )))
        }
    }

    pub fn request_install(exe: &Path) -> Result<(), SetupError> {
        let paths = Paths::current().ok_or_else(|| SetupError::Failed("Konfigurationsordner unbekannt".into()))?;
        let mut command = vec![sh_quote(&exe.display().to_string()), "--install-service".into()];
        command.extend(paths.args().iter().map(|a| sh_quote(a)));
        run_as_admin(&command.join(" "))
    }

    pub fn request_uninstall() -> Result<(), SetupError> {
        run_as_admin(&format!("rm -f {}", sh_quote(&plist_path().display().to_string())))
    }

    /// `--install-service`: runs as root through [`request_install`].
    pub fn install(paths: &Paths) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let exe = std::env::current_exe()?;
        let path = plist_path();
        let tmp = path.with_extension("plist.tmp");
        std::fs::write(&tmp, plist(&exe, paths))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
        std::fs::rename(&tmp, &path)?;
        log::info!("wrote {}", path.display());
        crate::mac_server::refresh()?;
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        match std::fs::remove_file(plist_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn login_window_plist() {
            let paths = Paths {
                config_dir: "/Users/a & b/Library/Application Support/vnc-beach".into(),
                home: "/Users/a & b".into(),
            };
            let text = plist(Path::new("/Applications/VNC Beach.app/Contents/MacOS/vnc-beach"), &paths);
            assert!(text.contains("<string>LoginWindow</string>"), "{text}");
            assert!(text.contains("<string>--service-agent</string>"));
            assert!(text.contains("<string>/Users/a &amp; b</string>"));
            assert!(!text.contains("Aqua"));
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use anyhow::Context as _;
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
        ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId,
        TOKEN_DUPLICATE, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
    use windows_sys::Win32::System::Threading::{
        CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, TerminateProcess,
        WaitForSingleObject, CREATE_NO_WINDOW, INFINITE, PROCESS_INFORMATION, STARTUPINFOW,
    };
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    use super::{command_line, outdated, Paths, SetupError};

    const NAME: &str = "vnc-beach";
    const DISPLAY_NAME: &str = "VNC Beach";
    const DESCRIPTION: &str =
        "Macht diesen Computer per VNC erreichbar, auch im Sperrbildschirm und vor der Anmeldung.";
    const FIREWALL_RULE: &str = "VNC Beach (Dienst)";
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

    /// Whether the service is set up (regardless of whether it runs). Works
    /// without administrator rights.
    pub fn installed() -> bool {
        let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else {
            return false;
        };
        manager.open_service(NAME, ServiceAccess::QUERY_STATUS).is_ok()
    }

    /// `%ProgramFiles%\VNC Beach\vnc-beach.exe`, where the service runs from.
    fn installed_exe() -> anyhow::Result<PathBuf> {
        let dir = std::env::var_os("ProgramFiles").context("ProgramFiles not set")?;
        Ok(PathBuf::from(dir).join("VNC Beach").join("vnc-beach.exe"))
    }

    // ---- Setup (elevated) -------------------------------------------------

    /// What makes [`super::ensure`] set the service up: missing, older than
    /// this program, or Tailscale would stop at sign-out.
    pub fn needs_setup() -> Option<&'static str> {
        if !installed() {
            return Some("not installed");
        }
        let version = installed_exe().ok().and_then(|exe| output(&exe, &["--version"]));
        if version.as_deref().is_none_or(outdated) {
            return Some("older version installed");
        }
        if let Some(tailscale) = tailscale_exe() {
            // Prefs are readable without administrator rights.
            if output(&tailscale, &["debug", "prefs"]).is_some_and(|p| !p.contains("\"ForceDaemon\": true")) {
                return Some("Tailscale not in unattended mode");
            }
        }
        None
    }

    /// Stdout of a successful run, without a console window.
    fn output(exe: &Path, args: &[&str]) -> Option<String> {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new(exe)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The Tailscale CLI in its default install location, if present.
    fn tailscale_exe() -> Option<PathBuf> {
        let exe = PathBuf::from(std::env::var_os("ProgramFiles")?).join("Tailscale\\tailscale.exe");
        exe.exists().then_some(exe)
    }

    /// Runs `exe` elevated with `args` and waits for it. The user may
    /// decline the UAC prompt.
    fn run_elevated(exe: &Path, args: &[String], what: &str) -> Result<(), SetupError> {
        let file = wide(exe.as_os_str());
        let params = wide(OsStr::new(&command_line(args)));
        let verb = wide(OsStr::new("runas"));
        // SAFETY: the strings outlive the call; the process handle is closed.
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_HIDE;
            if ShellExecuteExW(&mut info) == 0 {
                return Err(if GetLastError() == ERROR_CANCELLED {
                    SetupError::Cancelled
                } else {
                    SetupError::Failed(format!(
                        "Start mit Administratorrechten fehlgeschlagen: {}",
                        std::io::Error::last_os_error()
                    ))
                });
            }
            if info.hProcess.is_null() {
                return Err(SetupError::Failed("Start mit Administratorrechten fehlgeschlagen.".into()));
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1;
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            if code != 0 {
                let log = super::log_path("setup.log").map(|p| p.display().to_string()).unwrap_or_default();
                return Err(SetupError::Failed(format!(
                    "Einrichten {what} fehlgeschlagen, Details in {log}"
                )));
            }
        }
        Ok(())
    }

    /// Sets the service up for the current user's configuration, or updates
    /// it to the program version in `exe`. Asks for administrator rights.
    pub fn request_install(exe: &Path) -> Result<(), SetupError> {
        let paths = Paths::current().ok_or_else(|| SetupError::Failed("Konfigurationsordner unbekannt".into()))?;
        let mut args = vec!["--install-service".to_string()];
        args.extend(paths.args());
        run_elevated(exe, &args, "des Windows-Dienstes")
    }

    /// Removes the service. Asks for administrator rights.
    pub fn request_uninstall() -> Result<(), SetupError> {
        let exe = std::env::current_exe().map_err(|e| SetupError::Failed(e.to_string()))?;
        run_elevated(&exe, &["--uninstall-service".into()], "des Windows-Dienstes")
    }

    /// Installs the virtual display driver (see `crate::display_driver`).
    /// Asks for administrator rights.
    pub fn request_display_driver() -> Result<(), SetupError> {
        let exe = std::env::current_exe().map_err(|e| SetupError::Failed(e.to_string()))?;
        run_elevated(&exe, &["--install-display-driver".into()], "des virtuellen Bildschirms")
    }

    /// `--install-service`: runs elevated.
    pub fn install(paths: &Paths) -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;
        // A running copy keeps its exe locked.
        if let Ok(service) = manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP) {
            stop(&service)?;
        }
        let exe = installed_exe()?;
        let current = std::env::current_exe()?;
        if !same_file(&current, &exe) {
            std::fs::create_dir_all(exe.parent().unwrap())?;
            copy_retrying(&current, &exe)?;
            log::info!("copied {} to {}", current.display(), exe.display());
        }
        let mut launch_arguments = vec![OsString::from("--service")];
        launch_arguments.extend(paths.args().into_iter().map(OsString::from));
        let info = ServiceInfo {
            name: NAME.into(),
            display_name: DISPLAY_NAME.into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe.clone(),
            launch_arguments,
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS;
        let service = match manager.open_service(NAME, access) {
            Ok(service) => {
                service.change_config(&info)?;
                service
            }
            Err(_) => manager.create_service(&info, access)?,
        };
        service.set_description(DESCRIPTION)?;
        service.update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![
                ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_secs(5),
                };
                3
            ]),
        })?;
        service.set_failure_actions_on_non_crash_failures(true)?;
        firewall(Some(&exe));
        service.start::<&OsStr>(&[])?;
        log::info!("service installed and started");
        // Tailscale normally disconnects when its GUI user signs out and only
        // connects after sign-in; unattended mode keeps it up from boot.
        // This process runs elevated as that user, which Tailscale accepts.
        if let Some(tailscale) = tailscale_exe() {
            match output(&tailscale, &["set", "--unattended"]) {
                Some(_) => log::info!("Tailscale set to unattended mode"),
                None => log::warn!("tailscale set --unattended failed"),
            }
        }
        Ok(())
    }

    /// `--uninstall-service`: runs elevated.
    pub fn uninstall() -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        if let Ok(service) = manager.open_service(
            NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        ) {
            stop(&service)?;
            service.delete()?;
        }
        firewall(None);
        let exe = installed_exe()?;
        if exe.exists() {
            if let Err(e) = remove_retrying(&exe) {
                log::warn!("cannot remove {}: {e}", exe.display());
            }
            let _ = std::fs::remove_dir(exe.parent().unwrap());
        }
        log::info!("service removed");
        Ok(())
    }

    fn stop(service: &windows_service::service::Service) -> anyhow::Result<()> {
        if service.query_status()?.current_state != ServiceState::Stopped {
            let _ = service.stop();
        }
        let until = Instant::now() + Duration::from_secs(20);
        while service.query_status()?.current_state != ServiceState::Stopped {
            anyhow::ensure!(Instant::now() < until, "the service does not stop");
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// The agent may still be exiting after the service stopped.
    fn copy_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
        let mut tries = 0;
        loop {
            match std::fs::copy(from, to) {
                Ok(_) => return Ok(()),
                Err(e) if tries < 20 => {
                    log::debug!("copy failed, retrying: {e}");
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn remove_retrying(path: &Path) -> std::io::Result<()> {
        let mut tries = 0;
        loop {
            match std::fs::remove_file(path) {
                Err(_) if tries < 20 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
                r => return r,
            }
        }
    }

    /// Replaces the inbound rule for the service's exe (private networks,
    /// which includes Tailscale), or only removes it.
    fn firewall(exe: Option<&Path>) {
        let netsh = |args: &[String]| {
            use std::os::windows::process::CommandExt;
            match std::process::Command::new("netsh")
                .args(args)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                Ok(out) if out.status.success() => true,
                Ok(out) => {
                    log::debug!("netsh {args:?}: {}", String::from_utf8_lossy(&out.stdout).trim());
                    false
                }
                Err(e) => {
                    log::warn!("netsh: {e}");
                    false
                }
            }
        };
        let base = ["advfirewall".to_string(), "firewall".into()];
        let delete = [&base[..], &["delete".into(), "rule".into(), format!("name={FIREWALL_RULE}")]].concat();
        netsh(&delete);
        if let Some(exe) = exe {
            let add = [
                &base[..],
                &[
                    "add".into(),
                    "rule".into(),
                    format!("name={FIREWALL_RULE}"),
                    "dir=in".into(),
                    "action=allow".into(),
                    format!("program={}", exe.display()),
                    "profile=private,domain".into(),
                    "enable=yes".into(),
                ],
            ]
            .concat();
            if !netsh(&add) {
                log::warn!("cannot add the firewall rule");
            }
        }
    }

    // ---- The service ------------------------------------------------------

    static PATHS: OnceLock<Paths> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    /// `--service`: hands the process to the service control manager.
    pub fn run_service(paths: Paths) -> anyhow::Result<()> {
        let _ = PATHS.set(paths);
        service_dispatcher::start(NAME, ffi_service_main).context("not started as a service")?;
        Ok(())
    }

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = serve() {
            log::error!("service failed: {e:#}");
        }
    }

    fn serve() -> anyhow::Result<()> {
        let paths = PATHS.get().context("paths missing")?.clone();
        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = service_control_handler::register(NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = stop_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        let status = |state, controls| ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: controls,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        };
        handle.set_service_status(status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ))?;
        log::info!("service running, config in {}", paths.config_dir.display());

        let exe = std::env::current_exe()?;
        let mut agent: Option<Agent> = None;
        let mut last_start = None::<Instant>;
        loop {
            match stop_rx.recv_timeout(Duration::from_secs(1)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                _ => break,
            }
            // SAFETY: no arguments.
            let session = unsafe { WTSGetActiveConsoleSessionId() };
            if let Some(a) = &agent {
                if let Some(code) = a.exit_code() {
                    log::info!("agent exited with {code}");
                    agent = None;
                } else if a.session != session {
                    log::info!("console session {} -> {session}, restarting the agent", a.session);
                    agent = None;
                }
            }
            // 0xFFFFFFFF while sessions are being switched.
            let backoff = last_start.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
            if agent.is_none() && session != u32::MAX && !backoff {
                last_start = Some(Instant::now());
                match Agent::spawn(&exe, session, &paths) {
                    Ok(a) => {
                        log::info!("agent started in session {session}");
                        agent = Some(a);
                    }
                    Err(e) => log::warn!("cannot start the agent in session {session}: {e}"),
                }
            }
        }
        drop(agent);
        log::info!("service stopping");
        handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
        Ok(())
    }

    /// The agent process; dropping it ends it.
    struct Agent {
        process: HANDLE,
        session: u32,
    }

    impl Agent {
        /// Starts `--service-agent` as SYSTEM in `session` on the interactive
        /// window station.
        fn spawn(exe: &Path, session: u32, paths: &Paths) -> std::io::Result<Agent> {
            let mut args = vec![exe.display().to_string(), "--service-agent".into()];
            args.extend(paths.args());
            let mut cmd = wide(OsStr::new(&command_line(&args)));
            let mut desktop = wide(OsStr::new("winsta0\\default"));
            // SAFETY: handles are closed on every path; the buffers outlive
            // the calls.
            unsafe {
                let mut own = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut own) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut token = std::ptr::null_mut();
                let ok = DuplicateTokenEx(
                    own,
                    MAXIMUM_ALLOWED,
                    std::ptr::null(),
                    SecurityImpersonation,
                    TokenPrimary,
                    &mut token,
                );
                CloseHandle(own);
                if ok == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if SetTokenInformation(
                    token,
                    TokenSessionId,
                    (&session as *const u32).cast(),
                    std::mem::size_of::<u32>() as u32,
                ) == 0
                {
                    let e = std::io::Error::last_os_error();
                    CloseHandle(token);
                    return Err(e);
                }
                let mut si: STARTUPINFOW = std::mem::zeroed();
                si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
                si.lpDesktop = desktop.as_mut_ptr();
                let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
                let ok = CreateProcessAsUserW(
                    token,
                    std::ptr::null(),
                    cmd.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    CREATE_NO_WINDOW,
                    std::ptr::null(),
                    std::ptr::null(),
                    &si,
                    &mut pi,
                );
                let e = std::io::Error::last_os_error();
                CloseHandle(token);
                if ok == 0 {
                    return Err(e);
                }
                CloseHandle(pi.hThread);
                Ok(Agent {
                    process: pi.hProcess,
                    session,
                })
            }
        }

        fn exit_code(&self) -> Option<u32> {
            // SAFETY: valid process handle until drop.
            unsafe {
                if WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 {
                    return None;
                }
                let mut code = 0;
                GetExitCodeProcess(self.process, &mut code);
                Some(code)
            }
        }
    }

    impl Drop for Agent {
        fn drop(&mut self) {
            // SAFETY: valid process handle, closed here.
            unsafe {
                if self.exit_code().is_none() {
                    TerminateProcess(self.process, 0);
                    WaitForSingleObject(self.process, 5000);
                }
                CloseHandle(self.process);
            }
        }
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_command_line_arguments() {
        assert_eq!(
            command_line(&["C:\\Program Files\\VNC Beach\\vnc-beach.exe", "--home", "C:\\Users\\A B\\"]),
            r#""C:\Program Files\VNC Beach\vnc-beach.exe" --home "C:\Users\A B\\""#
        );
        assert_eq!(command_line(&["a", "", "say \"hi\""]), r#"a "" "say \"hi\"""#);
    }

    #[test]
    fn compares_installed_version() {
        assert!(outdated("beach-viewer 0.0.1"));
        assert!(!outdated(&format!("beach-viewer {}", env!("CARGO_PKG_VERSION"))));
        assert!(!outdated("beach-viewer 99.0.0"));
        assert!(outdated("garbage"));
    }

    #[test]
    fn quotes_for_shell_and_applescript() {
        assert_eq!(sh_quote("/Applications/VNC Beach.app"), "'/Applications/VNC Beach.app'");
        assert_eq!(sh_quote("it's"), r#"'it'\''s'"#);
        assert_eq!(applescript_string(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }
}
