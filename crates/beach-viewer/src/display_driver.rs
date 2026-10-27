//! Windows: sets up the Amyuni virtual display driver (`usbmmidd_v2`) that
//! the server adds a display with while no monitor is attached (see
//! `beach_server::headless` and `docs/headless-windows.md`).
//!
//! `--install-display-driver` runs elevated: it downloads the unmodified
//! package from the vendor, checks it against a pinned SHA-256 and the
//! Microsoft (WHCP) signatures of the driver catalog and DLL, installs it with
//! the vendor's `deviceinstaller64` (unsigned, covered by the SHA-256; Windows
//! checks the driver signature again) and sets 2560×1440 as the
//! driver's default resolution. The package is kept, with its license, in
//! `%ProgramData%\VNC Beach\usbmmidd_v2`.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};

const URL: &str = "https://www.amyuni.com/downloads/usbmmidd_v2.zip";
/// The package as published (driver 2.0.0.1, 2021-08-31). A different file
/// is refused rather than installed.
const SHA256: &str = "629b51e9944762bae73948171c65d09a79595cf4c771a82ebc003fbba5b24f51";
const MONITORS_KEY: &str =
    r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\usbmmIdd\Parameters\Monitors";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether the driver is installed.
pub fn installed() -> bool {
    beach_server::headless::driver_installed()
}

/// `--install-display-driver`: runs elevated.
pub fn install() -> anyhow::Result<()> {
    let arch = std::env::var("PROCESSOR_ARCHITEW6432")
        .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
        .unwrap_or_default();
    if !arch.eq_ignore_ascii_case("AMD64") {
        bail!("the virtual display driver is only available for 64-bit x86 Windows (this is {arch:?})");
    }
    if installed() {
        log::info!("virtual display driver already installed");
    } else {
        let dir = package_dir()?;
        fetch(&dir)?;
        let package = dir.join("usbmmidd_v2");
        for file in ["usbmmIdd.cat", r"x64\usbmmIdd.dll"] {
            check_signature(&package.join(file))?;
        }
        // Its exit code isn't documented; whether the driver is there afterwards is.
        let out = Command::new(package.join("deviceinstaller64.exe"))
            .args(["install", "usbmmidd.inf", "usbmmidd"])
            .current_dir(&package)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("starting deviceinstaller64.exe")?;
        let out = format!(
            "{} {}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        log::info!("deviceinstaller64: {}", out.trim());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !installed() {
            if std::time::Instant::now() > deadline {
                bail!(
                    "the virtual display driver did not come up after installing it: {}",
                    out.trim()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        log::info!("virtual display driver installed");
    }
    // The server sets and checks the resolution itself; this is the mode a
    // new display starts in.
    run(Command::new("reg").args(["add", MONITORS_KEY, "/ve", "/d", "2560,1440", "/f"]))
        .context("setting the default resolution")?;
    Ok(())
}

/// `%ProgramData%\VNC Beach`; only administrators may write there, so the
/// package can't be swapped between check and install.
fn package_dir() -> anyhow::Result<PathBuf> {
    let dir =
        PathBuf::from(std::env::var_os("ProgramData").context("ProgramData not set")?).join("VNC Beach");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Downloads and unpacks the package into `dir\usbmmidd_v2`.
fn fetch(dir: &Path) -> anyhow::Result<()> {
    let zip = dir.join("usbmmidd_v2.zip");
    log::info!("downloading {URL}");
    crate::update::download(URL, &zip).map_err(|e| anyhow::anyhow!("download failed: {e}"))?;
    let hash = hex(&Sha256::digest(std::fs::read(&zip)?));
    if hash != SHA256 {
        let _ = std::fs::remove_file(&zip);
        bail!("{URL} has SHA-256 {hash}, expected {SHA256}; the vendor changed the package, refusing to install it");
    }
    let package = dir.join("usbmmidd_v2");
    if package.exists() {
        std::fs::remove_dir_all(&package)?;
    }
    run(Command::new(system32("tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(dir))?;
    let _ = std::fs::remove_file(&zip);
    Ok(())
}

/// Signer of the driver catalog and DLL (Microsoft's attestation signing).
const DRIVER_SIGNER: &str = "CN=Microsoft Windows Hardware Compatibility Publisher";

/// Fails unless Windows reports a valid Authenticode signature by
/// [`DRIVER_SIGNER`] for `file`.
fn check_signature(file: &Path) -> anyhow::Result<()> {
    let script = format!(
        "$s = Get-AuthenticodeSignature -LiteralPath '{}'; \"$($s.Status)|$($s.SignerCertificate.Subject)\"",
        file.display().to_string().replace('\'', "''")
    );
    let out = run(
        Command::new(system32(r"WindowsPowerShell\v1.0\powershell.exe")).args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ]),
    )?;
    let (status, signer) = out.trim().split_once('|').unwrap_or((out.trim(), ""));
    log::info!("{}: signature {status}, {signer}", file.display());
    if status != "Valid" || !signer.starts_with(DRIVER_SIGNER) {
        bail!(
            "{} is not signed by Microsoft for Windows ({status}, {signer:?})",
            file.display()
        );
    }
    Ok(())
}

fn system32(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

/// Stdout of a successful run, without a console window.
fn run(cmd: &mut Command) -> anyhow::Result<String> {
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd
        .output()
        .with_context(|| format!("starting {:?}", cmd.get_program()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        bail!(
            "{:?} failed ({}): {} {}",
            cmd.get_program(),
            out.status,
            stdout.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(stdout)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
