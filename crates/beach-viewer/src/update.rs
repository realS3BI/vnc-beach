//! Update check and self-update from GitHub releases.
//!
//! A release is tagged `vX.Y.Z` and carries `VNC-Beach-macOS.zip` (the app
//! bundle) and `VNC-Beach-Windows.exe`. HTTP goes through the system's
//! `curl` (macOS, and Windows 10 1803 or newer), so no TLS stack is linked in.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::control::{self, Command as ControlCommand, Endpoint};

/// Where releases are published.
pub const REPO: &str = "realS3BI/vnc-beach";

pub const MACOS_ASSET: &str = "VNC-Beach-macOS.zip";
pub const WINDOWS_ASSET: &str = "VNC-Beach-Windows.exe";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// `v1.2.3`, `1.2.3`, `1.2` and `1.2.3-beta` (suffix ignored).
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let core = text.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let major = parts.next()??;
        let minor = parts.next().unwrap_or(Some(0))?;
        let patch = parts.next().unwrap_or(Some(0))?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version(major, minor, patch))
    }

    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release page, for a manual download.
    pub page: String,
    /// Download URL of this platform's asset, if the release has one.
    pub asset: Option<String>,
    /// SHA-256 of the asset as GitHub's API reports it (lowercase hex), so
    /// the download is checked against what the API promised.
    pub digest: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>`, present for assets uploaded since 2025.
    #[serde(default)]
    digest: Option<String>,
}

/// This platform's asset name; `None` where self-update isn't supported.
pub fn asset_name() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some(MACOS_ASSET)
    } else if cfg!(windows) {
        Some(WINDOWS_ASSET)
    } else {
        None
    }
}

fn parse_release(json: &[u8]) -> Result<Release, String> {
    let api: ApiRelease =
        serde_json::from_slice(json).map_err(|e| format!("Antwort von GitHub nicht lesbar: {e}"))?;
    if api.draft || api.prerelease {
        return Err("Keine veröffentlichte Version gefunden.".into());
    }
    let version = Version::parse(&api.tag_name)
        .ok_or_else(|| format!("Unbekanntes Versionsformat {:?}.", api.tag_name))?;
    let asset = asset_name().and_then(|name| api.assets.into_iter().find(|a| a.name == name));
    let digest = asset.as_ref().and_then(|a| {
        a.digest
            .as_deref()?
            .strip_prefix("sha256:")
            .filter(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase)
    });
    let asset = asset.map(|a| a.browser_download_url);
    if let Some(url) = &asset {
        if !trusted_download_url(url) {
            return Err(format!("Unerwartete Download-Adresse {url:?}."));
        }
    }
    Ok(Release {
        version,
        page: api.html_url,
        asset,
        digest,
    })
}

/// Downloads come over HTTPS only. Tests against a local server set
/// `VNC_BEACH_UPDATE_URL` and may use plain HTTP.
fn trusted_download_url(url: &str) -> bool {
    url.starts_with("https://")
        || (std::env::var_os("VNC_BEACH_UPDATE_URL").is_some() && url.starts_with("http://"))
}

/// Fails unless `file` has the SHA-256 `digest` (lowercase hex).
fn check_digest(file: &Path, digest: &str) -> Result<(), String> {
    use sha2::Digest as _;
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let actual: String = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual == digest {
        Ok(())
    } else {
        Err("Der Download ist beschädigt oder verändert (Prüfsumme stimmt nicht).".into())
    }
}

fn latest_url() -> String {
    // Override for testing against a local server.
    std::env::var("VNC_BEACH_UPDATE_URL")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"))
}

/// Asks GitHub for the latest release. Blocking; run it off the UI thread.
pub fn latest() -> Result<Release, String> {
    let json = curl(&["-H", "Accept: application/vnd.github+json", &latest_url()]).map_err(|e| {
        if e.contains("404") {
            "Keine Update-Information gefunden (Repository nicht öffentlich oder noch keine \
             Veröffentlichung)."
                .to_string()
        } else {
            format!("Update-Prüfung fehlgeschlagen: {e}")
        }
    })?;
    parse_release(&json)
}

/// Whether `release` is newer than this build.
pub fn is_newer(release: &Release) -> bool {
    release.version.cmp(&Version::current()) == Ordering::Greater
}

/// Downloads and installs `release` over the running program, then starts
/// the new version (and the background process, if one was running). The
/// caller should quit right after `Ok`.
pub fn install(release: &Release) -> Result<(), String> {
    let url = release
        .asset
        .as_deref()
        .ok_or("Diese Version hat keinen Download für dieses System.")?;
    let work = std::env::temp_dir().join(format!("vnc-beach-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = platform::install(url, release.digest.as_deref(), &work);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&work);
    }
    result
}

/// Removes what an earlier update left behind (Windows keeps the old exe
/// next to the new one until the next start).
pub fn clean_up() {
    platform::clean_up();
}

/// Can this copy replace itself? Not when run from a build directory or
/// through App Translocation; the error says why.
pub fn can_install() -> Result<(), String> {
    platform::target().map(|_| ())
}

/// Why a translocated copy can't update itself or register for autostart.
#[cfg(target_os = "macos")]
pub const MOVE_TO_APPLICATIONS: &str = "macOS startet VNC Beach aus einer schreibgeschützten Kopie, \
    weil die App nach dem Download nicht verschoben wurde. Bitte VNC Beach beenden, im Finder in den \
    Programme-Ordner ziehen und von dort starten.";

/// Whether macOS runs this copy through App Translocation: a quarantined
/// app started where it was unpacked runs from a read-only mount under
/// `…/AppTranslocation/<uuid>/` that is gone after a restart.
#[cfg(target_os = "macos")]
pub fn translocated() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_translocated(&exe))
}

#[cfg(target_os = "macos")]
fn is_translocated(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "AppTranslocation")
}

/// Stops a running background process; returns true if there was one.
pub(crate) fn stop_background() -> bool {
    if control::call_at(Endpoint::Tray, ControlCommand::Status).is_none() {
        return false;
    }
    control::call_at(Endpoint::Tray, ControlCommand::Quit);
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until && control::call_at(Endpoint::Tray, ControlCommand::Status).is_some() {
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

/// Downloads `url` to the file `to` with the system `curl`. An HTTPS
/// download may not be redirected to plain HTTP.
pub(crate) fn download(url: &str, to: &Path) -> Result<(), String> {
    let to = to.to_string_lossy();
    if url.starts_with("https://") {
        curl(&["--proto-redir", "=https", "-o", &to, url]).map(|_| ())
    } else {
        curl(&["-o", &to, url]).map(|_| ())
    }
}

/// Downloads `url` to `to` and checks it against `digest`, if there is one.
pub(crate) fn download_checked(url: &str, digest: Option<&str>, to: &Path) -> Result<(), String> {
    download(url, to)?;
    if let Some(digest) = digest {
        check_digest(to, digest)?;
    }
    Ok(())
}

/// Runs curl with redirects, retries and failure on HTTP errors; returns
/// stdout.
fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(curl_path());
    cmd.args([
        "-fsSL",
        "--retry",
        "2",
        "--connect-timeout",
        "10",
        "-A",
        "VNC-Beach",
    ])
    .args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("curl nicht verfügbar: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("curl beendet mit {}", out.status)
        } else {
            err
        })
    }
}

pub(crate) fn curl_path() -> PathBuf {
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        let path = PathBuf::from(root).join(r"System32\curl.exe");
        if path.exists() {
            return path;
        }
    }
    PathBuf::from("curl")
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The `.app` bundle this binary lives in.
    pub fn target() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if super::is_translocated(&exe) {
            return Err(super::MOVE_TO_APPLICATIONS.into());
        }
        let app = exe
            .ancestors()
            .nth(3)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or("Nur die installierte App (VNC Beach.app) kann sich selbst aktualisieren.")?;
        Ok(app.to_path_buf())
    }

    pub fn install(url: &str, digest: Option<&str>, work: &Path) -> Result<(), String> {
        let app = target()?;
        let zip = work.join("update.zip");
        super::download_checked(url, digest, &zip)?;
        let extracted = work.join("extracted");
        run(Command::new("ditto")
            .arg("-x")
            .arg("-k")
            .arg(&zip)
            .arg(&extracted))?;
        let new_app = std::fs::read_dir(&extracted)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or("Im Download steckt keine App.")?;
        run(Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&new_app))
        .map_err(|e| format!("Signatur des Updates ungültig: {e}"))?;
        // A valid signature alone would accept anybody's app (even an ad hoc
        // signed one): the update has to come from the same team as the
        // running app.
        if let Some(team) = team_identifier(&app) {
            match team_identifier(&new_app) {
                Some(other) if other == team => {}
                other => {
                    return Err(format!(
                        "Das Update ist nicht von derselben Entwickler-ID signiert ({} statt {team}).",
                        other.as_deref().unwrap_or("unsigniert")
                    ))
                }
            }
        }

        // Swap the bundles; the running process keeps its open files.
        let old = work.join("old.app");
        move_dir(&app, &old).map_err(|e| format!("Alte Version lässt sich nicht verschieben: {e}"))?;
        if let Err(e) = move_dir(&new_app, &app) {
            let _ = move_dir(&old, &app);
            return Err(format!("Neue Version lässt sich nicht installieren: {e}"));
        }

        let background = super::stop_background();
        let exe = app.join("Contents/MacOS/vnc-beach");
        if background {
            crate::background::spawn(&exe).map_err(|e| e.to_string())?;
        }
        Command::new("open")
            .arg("-n")
            .arg(&app)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The `TeamIdentifier` of a signed bundle; `None` for unsigned and ad
    /// hoc signed ones.
    fn team_identifier(app: &Path) -> Option<String> {
        let out = Command::new("codesign").args(["-dvv"]).arg(app).output().ok()?;
        // codesign prints the details on stderr.
        parse_team_identifier(&String::from_utf8_lossy(&out.stderr))
    }

    fn parse_team_identifier(details: &str) -> Option<String> {
        details
            .lines()
            .find_map(|l| l.strip_prefix("TeamIdentifier="))
            .map(str::trim)
            .filter(|t| !t.is_empty() && *t != "not set")
            .map(str::to_string)
    }

    /// Rename, or copy and delete across volumes. Other rename errors (read
    /// only, no permission) return before anything is copied or deleted.
    fn move_dir(from: &Path, to: &Path) -> Result<(), String> {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() != std::io::ErrorKind::CrossesDevices => return Err(e.to_string()),
            Err(_) => {}
        }
        run(Command::new("ditto").arg(from).arg(to))?;
        std::fs::remove_dir_all(from).map_err(|e| e.to_string())
    }

    fn run(cmd: &mut Command) -> Result<(), String> {
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn clean_up() {
        let dir = std::env::temp_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with("vnc-beach-update-") {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn reads_team_identifier() {
            let details = "Executable=/x\nIdentifier=at.schlossers.vnc-beach\nTeamIdentifier=ABCDE12345\n";
            assert_eq!(
                super::parse_team_identifier(details).as_deref(),
                Some("ABCDE12345")
            );
            assert_eq!(super::parse_team_identifier("TeamIdentifier=not set\n"), None);
            assert_eq!(super::parse_team_identifier("Signature=adhoc\n"), None);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub fn target() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let in_build_dir = exe
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("target"));
        if in_build_dir {
            return Err("Ein Entwicklungs-Build aktualisiert sich nicht selbst.".into());
        }
        Ok(exe)
    }

    fn old_path(exe: &Path) -> PathBuf {
        exe.with_extension("exe.old")
    }

    pub fn install(url: &str, digest: Option<&str>, work: &Path) -> Result<(), String> {
        let exe = target()?;
        let download = work.join("vnc-beach.exe");
        super::download_checked(url, digest, &download)?;
        let bytes = std::fs::read(&download).map_err(|e| e.to_string())?;
        if bytes.len() < 1_000_000 || !bytes.starts_with(b"MZ") {
            return Err("Der Download ist keine gültige Programmdatei.".into());
        }

        // Windows lets a running exe be renamed but not overwritten.
        let new = exe.with_extension("exe.new");
        std::fs::write(&new, &bytes).map_err(|e| format!("Schreiben fehlgeschlagen: {e}"))?;
        let old = old_path(&exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).map_err(|e| format!("Alte Version lässt sich nicht umbenennen: {e}"))?;
        if let Err(e) = std::fs::rename(&new, &exe) {
            let _ = std::fs::rename(&old, &exe);
            return Err(format!("Neue Version lässt sich nicht installieren: {e}"));
        }
        let _ = std::fs::remove_dir_all(work);

        if super::stop_background() {
            Command::new(&exe)
                .arg("--background")
                .spawn()
                .map_err(|e| e.to_string())?;
        }
        Command::new(&exe).spawn().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn clean_up() {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::remove_file(old_path(&exe));
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::path::{Path, PathBuf};

    pub fn target() -> Result<PathBuf, String> {
        Err("Selbst-Update gibt es nur unter macOS und Windows.".into())
    }

    pub fn install(_url: &str, _digest: Option<&str>, _work: &Path) -> Result<(), String> {
        target().map(|_| ())
    }

    pub fn clean_up() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.10.0"), Some(Version(0, 10, 0)));
        assert_eq!(Version::parse("2.1"), Some(Version(2, 1, 0)));
        assert_eq!(Version::parse("1.0.0-beta.2"), Some(Version(1, 0, 0)));
        assert_eq!(Version::parse("vX"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recognizes_app_translocation() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/qr/x/T/AppTranslocation/8B1B/d/VNC Beach.app/Contents/MacOS/vnc-beach"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/VNC Beach.app/Contents/MacOS/vnc-beach"
        )));
        assert!(!is_translocated(Path::new(
            "/Users/x/AppTranslocation.app/Contents/MacOS/vnc-beach"
        )));
    }

    #[test]
    fn reads_github_release() {
        let sha = "a".repeat(64);
        let json = format!(
            r#"{{"tag_name":"v9.0.1","html_url":"https://example.org/r","draft":false,"prerelease":false,
                "assets":[{{"name":"other.txt","browser_download_url":"https://example.org/o"}},
                          {{"name":"{MACOS_ASSET}","browser_download_url":"https://example.org/mac","digest":"sha256:{sha}"}},
                          {{"name":"{WINDOWS_ASSET}","browser_download_url":"https://example.org/win","digest":"sha256:{sha}"}}]}}"#
        );
        let r = parse_release(json.as_bytes()).unwrap();
        assert_eq!(r.version, Version(9, 0, 1));
        assert_eq!(r.page, "https://example.org/r");
        let expected = if cfg!(target_os = "macos") {
            Some("https://example.org/mac".to_string())
        } else if cfg!(windows) {
            Some("https://example.org/win".to_string())
        } else {
            None
        };
        assert_eq!(r.asset, expected);
        assert_eq!(r.digest.is_some(), expected.is_some());
        assert!(is_newer(&r));
        // A download address that isn't HTTPS is refused with the release.
        if asset_name().is_some() {
            let plain = json
                .replace("https://example.org/mac", "http://example.org/mac")
                .replace("https://example.org/win", "http://example.org/win");
            assert!(parse_release(plain.as_bytes()).is_err());
            // An odd digest is ignored rather than trusted.
            let odd = json.replace(&format!("sha256:{sha}"), "md5:abc");
            assert_eq!(parse_release(odd.as_bytes()).unwrap().digest, None);
        }

        let draft = r#"{"tag_name":"v9.0.0","html_url":"x","draft":true,"assets":[]}"#;
        assert!(parse_release(draft.as_bytes()).is_err());
        let old = r#"{"tag_name":"v0.0.1","html_url":"x","assets":[]}"#;
        assert!(!is_newer(&parse_release(old.as_bytes()).unwrap()));
    }

    #[test]
    fn downloads_are_checked_against_the_digest() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("asset");
        std::fs::write(&file, b"hello").unwrap();
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(check_digest(&file, good).is_ok());
        assert!(check_digest(&file, &"0".repeat(64)).is_err());
    }
}
