//! Explicit, persistent opt-in to unattended macOS server startup.
//! Passwords travel over stdin and a private PTY, never argv or config.
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::service::Paths;

const ROOT: &str = "/Library/Application Support/VNC Beach";
const MARKER: &str = "/Library/Application Support/VNC Beach/server-mode.json";
const HELPER: &str = "/Library/Application Support/VNC Beach/boot-services.sh";
const PLIST: &str = "/Library/LaunchDaemons/at.schlossers.vnc-beach.network.plist";
const LABEL: &str = "at.schlossers.vnc-beach.network";
const LOCK_LABEL: &str = "at.schlossers.vnc-beach.lock-on-login";
const REVISION: u32 = 2;
const SCRIPT: &str = include_str!("mac-boot-services.sh");

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Installation {
    revision: u32,
    user: String,
    uid: u32,
    home: PathBuf,
    cli: PathBuf,
    variant: String,
    automatic_login: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub enabled: bool,
    pub automatic_login: bool,
    pub message: String,
    pub repair: bool,
}

fn installation() -> Option<Installation> {
    let meta = fs::metadata(MARKER).ok()?;
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return None;
    }
    serde_json::from_slice(&fs::read(MARKER).ok()?).ok()
}

fn filevault_on() -> anyhow::Result<bool> {
    let output = timed(Command::new("/usr/bin/fdesetup").arg("isactive"), Duration::from_secs(5))?;
    match String::from_utf8_lossy(&output.stdout).trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => bail!("FileVault status could not be read"),
    }
}

pub fn status() -> Status {
    let Some(installed) = installation() else {
        return Status { message: "Servermodus ist nicht eingerichtet.".into(), ..Status::default() };
    };
    let problem = match filevault_on() {
        Ok(true) => Some("FileVault ist wieder aktiv. Nach einem Neustart ist eine Passworteingabe nötig."),
        Err(_) => Some("FileVault-Status unbekannt. Servermodus prüfen."),
        _ if installed.revision != REVISION || fs::read_to_string(HELPER).ok().as_deref() != Some(SCRIPT)
            || fs::read_to_string(PLIST).ok().as_deref() != Some(network_plist(&installed).as_str()) =>
            Some("Die Startkonfiguration muss aktualisiert werden."),
        _ => None,
    };
    Status {
        enabled: true,
        automatic_login: installed.automatic_login,
        repair: problem.is_some(),
        message: problem.unwrap_or("Servermodus eingerichtet. Ein Neustarttest bestätigt den vollständigen Start.").into(),
    }
}

pub fn needs_refresh() -> bool {
    installation().is_some_and(|i| i.revision != REVISION
        || fs::read_to_string(HELPER).ok().as_deref() != Some(SCRIPT)
        || fs::read_to_string(PLIST).ok().as_deref() != Some(network_plist(&i).as_str()))
}

/// Updates only the already-authorized startup files. No passwords and no
/// changes to FileVault or automatic-login policy during ordinary updates.
pub fn refresh() -> anyhow::Result<()> {
    if let Some(mut installed) = installation().filter(|_| needs_refresh()) {
        installed.revision = REVISION;
        install_network(&installed)?;
        write_atomic(Path::new(MARKER), &serde_json::to_string_pretty(&installed)?, 0o644)?;
    }
    Ok(())
}

/// Called on background startup, including after a self-update. Existing
/// installations without an explicit opt-in never change disk encryption.
pub fn ensure() {
    let state = status();
    if state.enabled && state.repair {
        log::warn!("unattended startup needs repair: {}", state.message);
        // Show the reason, but require a deliberate repair action. In
        // particular, a later intentional FileVault change is not undone.
        let _ = Command::new("/usr/bin/osascript")
            .args(["-e", &format!("display notification {} with title \"VNC Beach\"",
                apple_string(&format!("{} Öffne die Server-Einstellungen und wähle Servermodus reparieren.", state.message)))])
            .status();
    }
}

/// Runs off the UI thread. This native password dialog is followed by sudo;
/// the application never saves the supplied administrator password.
pub fn request_setup(automatic_login: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe.ancestors().nth(3);
    if crate::update::translocated() || !app.is_some_and(|p| p.extension().is_some_and(|e| e == "app")) {
        return Err("Bitte zuerst VNC Beach in den Programme-Ordner verschieben.".into());
    }
    let paths = Paths::current().ok_or("Konfigurationsordner unbekannt")?;
    let prompt = "Servermodus einrichten: FileVault wird deaktiviert. SSH, Tailscale und VNC Beach starten vor der Anmeldung. Gib das Passwort deines lokalen Mac-Benutzers ein.";
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", &format!("text returned of (display dialog {} with title \"VNC Beach\" default answer \"\" with hidden answer buttons {{\"Abbrechen\", \"Einrichten\"}} default button \"Einrichten\" cancel button \"Abbrechen\")", apple_string(prompt))])
        .output().map_err(|e| e.to_string())?;
    let secret = Zeroizing::new(output.stdout);
    if !output.status.success() {
        return Err("Einrichtung abgebrochen. Der Servermodus wurde nicht eingerichtet.".into());
    }
    let password = Zeroizing::new(String::from_utf8(secret.to_vec()).map_err(|_| "Passworteingabe ungültig")?.trim_end_matches('\n').to_string());
    if password.is_empty() { return Err("Ein Passwort ist erforderlich.".into()); }
    // Authenticate separately. If the password is wrong, sudo must not
    // consume the JSON intended for the elevated installer as another try.
    // The installer below runs with `sudo -n` on the credentials cached
    // here, so `-v` must not get `-k` (which skips caching); a separate
    // `sudo -k` drops an older cache first, so the password is checked.
    let _ = Command::new("/usr/bin/sudo").arg("-k").status();
    let mut auth = Command::new("/usr/bin/sudo").args(["-S", "-p", "", "-v"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    writeln!(auth.stdin.take().ok_or("Passworteingabe nicht verfügbar")?, "{}", password.as_str()).map_err(|e| e.to_string())?;
    if !auth.wait_with_output().map_err(|e| e.to_string())?.status.success() {
        return Err("Das Mac-Passwort wurde nicht akzeptiert. Bitte erneut versuchen.".into());
    }
    let mut command = Command::new("/usr/bin/sudo");
    command.arg("-n").arg(&exe).arg("--install-server-mode");
    command.args(paths.args());
    if automatic_login { command.arg("--automatic-login"); }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    {
        let mut input = child.stdin.take().ok_or("Passworteingabe nicht verfügbar")?;
        let encoded = Zeroizing::new(serde_json::to_vec(password.as_str()).map_err(|e| e.to_string())?);
        input.write_all(&encoded).map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).replace(password.as_str(), "[redacted]");
        return Err(format!("Servermodus nicht vollständig eingerichtet: {}", error.trim()));
    }
    Ok(())
}

/// Explicit elevated CLI entry point. Only this path may disable FileVault.
/// Ordinary service installation and updates do not call it implicitly.
pub fn install(paths: &Paths, automatic_login: bool) -> anyhow::Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "Administrator rights are required");
    let mut input = Zeroizing::new(String::new());
    std::io::stdin().read_to_string(&mut input)?;
    let password = Zeroizing::new(serde_json::from_str::<String>(&input).context("Expected a password on stdin")?);
    ensure!(!password.is_empty(), "A local account password is required");
    let uid = fs::metadata(&paths.home)?.uid();
    ensure!(uid != 0, "The target must be a regular local user");
    let user = checked(Command::new("/usr/bin/id").args(["-nu", &uid.to_string()]))?;
    ensure!(user != "root" && !user.contains(['\n', '\r']), "Invalid target user");
    password_command(Command::new("/usr/bin/dscl").args([".", "-authonly", &user]), &password)?;
    let (cli, variant) = tailscale()?;
    let installed = Installation { revision: REVISION, user, uid, home: paths.home.clone(), cli, variant, automatic_login };
    fs::create_dir_all(ROOT)?;
    fs::set_permissions(ROOT, fs::Permissions::from_mode(0o755))?;
    // Preserve originals once; no password is written to this backup.
    let backup = Path::new(ROOT).join("before-server-mode");
    if !backup.exists() {
        fs::create_dir(&backup)?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o700))?;
        for source in ["/Library/Preferences/com.apple.loginwindow.plist", PLIST] {
            if Path::new(source).exists() { fs::copy(source, backup.join(Path::new(source).file_name().unwrap()))?; }
        }
    }
    if filevault_on()? {
        let plist = Zeroizing::new(format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>Username</key><string>{}</string><key>Password</key><string>{}</string></dict></plist>", xml(&installed.user), xml(&password)));
        let mut child = Command::new("/usr/bin/fdesetup").args(["disable", "-inputplist"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        child.stdin.take().context("FileVault stdin unavailable")?.write_all(plist.as_bytes())?;
        let output = child.wait_with_output()?;
        ensure!(output.status.success(), "FileVault could not be disabled: {}", String::from_utf8_lossy(&output.stderr).replace(password.as_str(), "[redacted]"));
    }
    ensure!(!filevault_on()?, "FileVault is still active or decryption is in progress. Wait, then run setup again.");
    checked(Command::new("/bin/launchctl").args(["enable", "system/com.openssh.sshd"]))?;
    if !Command::new("/bin/launchctl").args(["print", "system/com.openssh.sshd"]).stdout(Stdio::null()).stderr(Stdio::null()).status()?.success() {
        checked(Command::new("/bin/launchctl").args(["bootstrap", "system", "/System/Library/LaunchDaemons/ssh.plist"]))?;
    }
    checked(Command::new("/usr/bin/pmset").args(["-a", "autorestart", "1", "sleep", "0"]))?;
    if automatic_login {
        let result = password_command(Command::new("/bin/launchctl").args([
            "asuser", &uid.to_string(), "/usr/sbin/sysadminctl",
            "-adminUser", &installed.user, "-adminPassword", "-", "-autologin", "set", "-userName", &installed.user, "-password", "-"
        ]), &password);
        if let Err(error) = result {
            if !error.to_string().contains("SACSetAutoLoginPassword error:22") { return Err(error); }
            // The SessionAgent API can reject an otherwise valid setup
            // over SSH. Use loginwindow's existing root-only credential
            // format after dscl has authenticated the target account.
            // This is the OS auto-login credential, never an app secret.
            let encoded = autologin_credential(&password);
            let path = Path::new("/etc/kcpassword");
            let temporary = path.with_extension("vnc-beach-new");
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&encoded)?;
            fs::rename(temporary, path)?;
            checked(Command::new("/usr/bin/defaults").args([
                "write", "/Library/Preferences/com.apple.loginwindow", "autoLoginUser", "-string", &installed.user
            ]))?;
        }
        let actual_user = checked(Command::new("/usr/bin/defaults").args([
            "read", "/Library/Preferences/com.apple.loginwindow", "autoLoginUser"
        ]))?;
        ensure!(actual_user == installed.user, "Automatic login was not enabled for the requested user");
        password_command(Command::new("/bin/launchctl").args(["asuser", &uid.to_string(),
            "/usr/sbin/sysadminctl", "-screenLock", "immediate", "-password", "-"
        ]), &password)?;
        let lock_status = timed(Command::new("/bin/launchctl").args(["asuser", &uid.to_string(),
            "/usr/sbin/sysadminctl", "-screenLock", "status"]), Duration::from_secs(5))?;
        let lock_status = format!("{}{}", String::from_utf8_lossy(&lock_status.stdout), String::from_utf8_lossy(&lock_status.stderr)).to_ascii_lowercase();
        ensure!(lock_status.contains("immediate") || lock_status.contains("delay is 0 seconds"), "Immediate screen locking was not confirmed");
        let agent = paths.home.join("Library/LaunchAgents").join(format!("{LOCK_LABEL}.plist"));
        write_atomic(&agent, &lock_plist(), 0o644)?;
        checked(Command::new("/usr/sbin/chown").arg(format!("{}:staff", installed.user)).arg(&agent))?;
    } else if installation().is_some_and(|i| i.automatic_login) {
        password_command(Command::new("/usr/sbin/sysadminctl").args([
            "-adminUser", &installed.user, "-adminPassword", "-", "-autologin", "off"
        ]), &password)?;
        let agent = paths.home.join("Library/LaunchAgents").join(format!("{LOCK_LABEL}.plist"));
        if agent.exists() { fs::remove_file(agent)?; }
    }
    let mut config = crate::config::load();
    config.server.enabled = true;
    config.server.service = true;
    config.server.autostart = true;
    crate::config::save(&config)?;
    if let Some(path) = crate::config::path() {
        checked(Command::new("/usr/sbin/chown").arg(format!("{}:staff", installed.user)).arg(path))?;
    }
    let autostart = paths.home.join("Library/LaunchAgents/at.schlossers.vnc-beach.plist");
    let exe = std::env::current_exe()?;
    let launch = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Label</key><string>at.schlossers.vnc-beach</string>
<key>ProgramArguments</key><array><string>{}</string><string>--background</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>
"#, xml(&exe.to_string_lossy()));
    write_atomic(&autostart, &launch, 0o644)?;
    checked(Command::new("/usr/sbin/chown").arg(format!("{}:staff", installed.user)).arg(&autostart))?;
    // The existing implementation installs VNC Beach's LoginWindow agent.
    crate::service::install(paths)?;
    install_network(&installed)?;
    write_atomic(Path::new(MARKER), &serde_json::to_string_pretty(&installed)?, 0o644)?;
    println!("VNC Beach server mode configured. A restart test is still required.");
    Ok(())
}

fn install_network(installed: &Installation) -> anyhow::Result<()> {
    // launchd opens a user job's output after dropping privileges. Create
    // its log explicitly; a root-only log directory otherwise yields
    // EX_CONFIG even though bootstrap itself reports success.
    let log_dir = Path::new("/Library/Logs/VNC Beach");
    fs::create_dir_all(log_dir)?;
    let log = log_dir.join("network.log");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new().create(true).append(true).custom_flags(libc::O_NOFOLLOW).open(&log)?;
        file.set_permissions(fs::Permissions::from_mode(0o640))?;
    }
    checked(Command::new("/usr/sbin/chown").arg(format!("{}:staff", installed.user)).arg(&log))?;
    write_atomic(Path::new(HELPER), SCRIPT, 0o755)?;
    write_atomic(Path::new(PLIST), &network_plist(installed), 0o644)?;
    let _ = Command::new("/bin/launchctl").args(["bootout", &format!("system/{LABEL}")]).output();
    checked(Command::new("/bin/launchctl").args(["enable", &format!("system/{LABEL}")]))?;
    checked(Command::new("/bin/launchctl").args(["bootstrap", "system", PLIST]))?;
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let result = checked(Command::new("/bin/launchctl").args(["print", &format!("system/{LABEL}")]))?;
        if result.contains("last exit code = 0") { break; }
        // A freshly bootstrapped job initially reports "never exited";
        // wait for its first completed run instead of treating that as an
        // error or reusing the result of the previous job instance.
        if Instant::now() >= until {
            bail!("Tailscale startup job did not complete successfully. See /Library/Logs/VNC Beach/network.log");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

fn tailscale() -> anyhow::Result<(PathBuf, String)> {
    for plist in ["/Library/LaunchDaemons/com.tailscale.tailscaled.plist", "/Library/LaunchDaemons/homebrew.mxcl.tailscale.plist"] {
        if Path::new(plist).exists() {
            for cli in ["/opt/homebrew/bin/tailscale", "/usr/local/bin/tailscale"] {
                if Path::new(cli).exists() { return Ok((cli.into(), "daemon".into())); }
            }
        }
    }
    let app = "/Applications/Tailscale.app/Contents";
    let id = checked(Command::new("/usr/libexec/PlistBuddy").args(["-c", "Print :CFBundleIdentifier", &format!("{app}/Info.plist")]))?;
    ensure!(id == "io.tailscale.ipn.macsys", "Install and sign in to the standalone Tailscale app first. The App Store variant is not supported before login.");
    Ok((format!("{app}/MacOS/Tailscale").into(), "standalone".into()))
}

fn network_plist(i: &Installation) -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>/bin/sh</string><string>{HELPER}</string><string>{}</string><string>{}</string></array>
<key>UserName</key><string>{}</string><key>GroupName</key><string>staff</string>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string></dict>
<key>RunAtLoad</key><true/><key>StartInterval</key><integer>60</integer>
<key>ProcessType</key><string>Background</string>
<key>StandardOutPath</key><string>/Library/Logs/VNC Beach/network.log</string>
<key>StandardErrorPath</key><string>/Library/Logs/VNC Beach/network.log</string>
</dict></plist>
"#, xml(&i.cli.to_string_lossy()), xml(&i.variant), xml(&i.user), xml(&i.home.to_string_lossy()))
}

fn lock_plist() -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>{LOCK_LABEL}</string>
<key>ProgramArguments</key><array><string>/bin/sh</string><string>-c</string><string>sleep 15; /usr/bin/pmset displaysleepnow</string></array>
<key>RunAtLoad</key><true/></dict></plist>
"#)
}

fn write_atomic(path: &Path, content: &str, mode: u32) -> anyhow::Result<()> {
    let temporary = path.with_extension("vnc-beach-new");
    // Do not follow an old temporary symlink while elevated.
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(content.as_bytes())?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn checked(command: &mut Command) -> anyhow::Result<String> {
    let output = timed(command, Duration::from_secs(30))?;
    ensure!(output.status.success(), "Command failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn timed(command: &mut Command, timeout: Duration) -> anyhow::Result<std::process::Output> {
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let until = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() { return Ok(child.wait_with_output()?); }
        if Instant::now() >= until {
            let _ = child.kill(); let _ = child.wait(); bail!("Command timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// sysadminctl reads passwords from its controlling terminal, including
/// when invoked as root. Give it a private PTY with echo disabled.
fn password_command(command: &mut Command, password: &str) -> anyhow::Result<()> {
    use std::os::fd::{FromRawFd, AsRawFd};
    use std::os::unix::process::CommandExt;
    let (mut master, mut slave) = (0, 0);
    ensure!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) } == 0, "Cannot create authentication terminal");
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    unsafe {
        let mut settings = std::mem::zeroed();
        ensure!(libc::tcgetattr(slave.as_raw_fd(), &mut settings) == 0, "Cannot read terminal settings");
        settings.c_lflag &= !libc::ECHO;
        ensure!(libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) == 0, "Cannot disable terminal echo");
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        });
    }
    let mut child = command.stdin(slave.try_clone()?).stdout(slave.try_clone()?).stderr(slave).spawn()?;
    let until = Instant::now() + Duration::from_secs(45);
    let mut transcript = Zeroizing::new(String::new());
    let mut prompt = String::new();
    let mut count = 0;
    loop {
        let mut fd = libc::pollfd { fd: terminal.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut fd, 1, 100) } > 0 {
            let mut bytes = Zeroizing::new([0u8; 2048]);
            match terminal.read(bytes.as_mut()) {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&bytes[..n]);
                    transcript.push_str(&text); prompt.push_str(&text);
                    if prompt.to_ascii_lowercase().contains("password") && prompt.trim_end().ends_with(':') {
                        count += 1;
                        if count > 3 { let _ = child.kill(); let _ = child.wait(); bail!("Account authentication failed"); }
                        writeln!(terminal, "{password}")?; prompt.clear();
                    }
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error.into()); }
            }
        }
        if child.try_wait()?.is_some() {
            // Drain output written immediately before exit, including
            // sysadminctl's zero-exit-code error messages.
            loop {
                let mut fd = libc::pollfd { fd: terminal.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                if unsafe { libc::poll(&mut fd, 1, 0) } <= 0 { break; }
                let mut bytes = Zeroizing::new([0u8; 2048]);
                match terminal.read(bytes.as_mut()) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => transcript.push_str(&String::from_utf8_lossy(&bytes[..n])),
                }
            }
            break;
        }
        if Instant::now() >= until { let _ = child.kill(); let _ = child.wait(); bail!("Account authentication timed out"); }
    }
    let status = child.wait()?;
    // sysadminctl sometimes reports failure in stderr but exits with zero.
    let lower = transcript.to_ascii_lowercase();
    ensure!(status.success() && !lower.contains("failed") && !lower.contains("error") && !lower.contains("incorrect"),
        "Account setup failed: {}", transcript.replace(password, "[redacted]"));
    Ok(())
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The standard loginwindow credential file is reversible obfuscation,
/// not encryption. Keep it root-only, and never treat it as a secret vault.
fn autologin_credential(password: &str) -> Zeroizing<Vec<u8>> {
    const KEY: [u8; 11] = [0x7d, 0x89, 0x52, 0x23, 0xd2, 0xbc, 0xdd, 0xea, 0xa3, 0xb9, 0x1f];
    let mut bytes = Zeroizing::new(password.as_bytes().to_vec());
    let padded = (bytes.len() / 12 + 1) * 12;
    bytes.resize(padded, 0);
    for (index, byte) in bytes.iter_mut().enumerate() { *byte ^= KEY[index % KEY.len()]; }
    bytes
}

fn apple_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boot_job_is_independent_of_gui_and_runs_without_root() {
        let i = Installation { revision: REVISION, user: "a & b".into(), uid: 501,
            home: "/Users/a & b".into(), cli: "/Applications/Tailscale.app/Contents/MacOS/Tailscale".into(),
            variant: "standalone".into(), automatic_login: false };
        let text = network_plist(&i);
        assert!(text.contains("<key>UserName</key><string>a &amp; b</string>"));
        assert!(text.contains("<key>RunAtLoad</key><true/>"));
        assert!(text.contains("<key>StartInterval</key><integer>60</integer>"));
        assert!(!text.contains("Aqua"));
        assert!(!text.contains("LoginWindow"));
    }
    #[test]
    fn credentials_are_escaped_for_filevault_input() {
        assert_eq!(xml("<&p>"), "&lt;&amp;p&gt;");
    }
    #[test]
    fn password_authentication_uses_a_terminal_without_echo() {
        password_command(Command::new("/bin/sh").args(["-c",
            "printf 'Password: '; IFS= read -r answer; [ \"$answer\" = 'contains spaces & <xml>' ] || exit 1"
        ]), "contains spaces & <xml>").unwrap();
    }
    #[test]
    fn failed_account_setup_is_not_reported_as_success() {
        assert!(password_command(Command::new("/bin/sh").args(["-c", "echo 'Operation failed' >&2"]), "unused").is_err());
    }
    #[test]
    fn loginwindow_credential_has_a_terminating_zero_and_complete_blocks() {
        let encoded = autologin_credential("abcdefghijkl");
        assert_eq!(encoded.len(), 24);
        let key = [0x7d, 0x89, 0x52, 0x23, 0xd2, 0xbc, 0xdd, 0xea, 0xa3, 0xb9, 0x1f];
        let decoded: Vec<u8> = encoded.iter().enumerate().map(|(i,b)| b ^ key[i % key.len()]).collect();
        assert_eq!(&decoded[..12], b"abcdefghijkl");
        assert!(decoded[12..].iter().all(|b| *b == 0));
    }
}
