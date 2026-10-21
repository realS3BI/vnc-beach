//! SSH authentication and host identity checks, shared by target sign-in and
//! Wake-on-LAN relays. No login password is serialized or passed to a process.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context as _};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use ssh2::{CheckResult, HashType, KnownHostFileKind, Session};
use zeroize::Zeroizing;

use super::{endpoint, socket_address, Answer, Authentication, Prompt, Ssh, Worker};

pub(super) fn login(
    worker: &Worker,
    config: &Ssh,
    allow_disconnect: bool,
) -> anyhow::Result<Option<Session>> {
    config.validate()?;
    let (host, port) = endpoint(&config.address, Some(22))?;
    let stream = worker.tcp(&socket_address(&host, port))?;
    let mut session = Session::new()?;
    session.set_timeout(8_000);
    session.set_tcp_stream(stream);
    session
        .handshake()
        .context("SSH-Verbindungsaufbau fehlgeschlagen")?;
    verify(worker, &session, &host, port)?;
    worker.cancel.check()?;
    let user = config.user.trim();
    let result = match &config.auth {
        Authentication::Password => {
            let methods = session.auth_methods(user)?.to_owned();
            worker.cancel.check()?;
            if methods.split(',').any(|m| m == "password") {
                let password = secret(
                    worker,
                    format!("SSH-Anmeldung an {}", config.address),
                    format!("Passwort für {user}"),
                    false,
                )?;
                worker.cancel.check()?;
                session.userauth_password(user, &password)
            } else if methods.split(',').any(|m| m == "keyboard-interactive") {
                let mut prompts = Interactive {
                    worker,
                    address: &config.address,
                    error: None,
                };
                // Human interaction can take longer than an ordinary network
                // read. Cancellation still closes the underlying socket.
                session.set_timeout(300_000);
                let result = session.userauth_keyboard_interactive(user, &mut prompts);
                session.set_timeout(8_000);
                if let Some(error) = prompts.error {
                    return Err(error);
                }
                result
            } else {
                bail!(
                    "{} erlaubt keine Passwort-Anmeldung. Bitte einen SSH-Schlüssel einrichten.",
                    config.address
                );
            }
        }
        Authentication::Key { path } => {
            let path = PathBuf::from(path.trim());
            // An unencrypted key needs no prompt. A failed authentication is
            // not retried with a password; only key decryption gets a prompt.
            let encrypted = fs::read_to_string(&path).context("SSH-Schlüssel konnte nicht gelesen werden")?;
            let encrypted = Zeroizing::new(encrypted);
            let needs_passphrase = encrypted.contains("ENCRYPTED") || openssh_encrypted(&encrypted);
            let passphrase = if needs_passphrase {
                Some(secret(
                    worker,
                    "SSH-Schlüssel entsperren".into(),
                    "Passphrase des privaten Schlüssels".into(),
                    false,
                )?)
            } else {
                None
            };
            worker.cancel.check()?;
            session.userauth_pubkey_file(user, None, &path, passphrase.as_deref().map(String::as_str))
        }
    };
    worker.cancel.check()?;
    match result {
        Ok(()) if session.authenticated() => Ok(Some(session)),
        Err(error)
            if allow_disconnect && matches!(error.code(), ssh2::ErrorCode::Session(-43 | -13 | -7)) =>
        {
            Ok(None)
        }
        Err(error) => Err(error)
            .context("SSH-Anmeldung fehlgeschlagen. Benutzername, Passwort und SSH-Berechtigung prüfen"),
        _ => bail!("Die SSH-Anmeldung wurde nicht bestätigt."),
    }
}

fn secret(worker: &Worker, title: String, label: String, echo: bool) -> anyhow::Result<Zeroizing<String>> {
    match worker.ask(Prompt::Secret { title, label, echo })? {
        Answer::Secret(secret) => Ok(secret),
        Answer::Trust => bail!("Keine Antwort auf die SSH-Passwortabfrage erhalten."),
    }
}

struct Interactive<'a> {
    worker: &'a Worker,
    address: &'a str,
    error: Option<anyhow::Error>,
}

impl ssh2::KeyboardInteractivePrompt for Interactive<'_> {
    fn prompt(&mut self, _username: &str, instructions: &str, prompts: &[ssh2::Prompt<'_>]) -> Vec<String> {
        let mut answers = Vec::new();
        for prompt in prompts {
            let label = if instructions.is_empty() {
                prompt.text.to_string()
            } else {
                format!("{instructions}\n{}", prompt.text)
            };
            match secret(
                self.worker,
                format!("SSH-Anmeldung an {}", self.address),
                label,
                prompt.echo,
            ) {
                Ok(mut value) => answers.push(std::mem::take(&mut *value)),
                Err(error) => {
                    self.error = Some(error);
                    return vec![String::new(); prompts.len()];
                }
            }
        }
        answers
    }
}

fn verify(worker: &Worker, session: &Session, host: &str, port: u16) -> anyhow::Result<()> {
    let directory = worker
        .known_hosts
        .as_ref()
        .context("Kein Konfigurationsverzeichnis für SSH-Geräteschlüssel verfügbar.")?;
    let name: String = Sha256::digest(socket_address(host, port).as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = directory.join(name);
    let mut known = session.known_hosts()?;
    match fs::metadata(&path) {
        Ok(_) => {
            known
                .read_file(&path, KnownHostFileKind::OpenSSH)
                .context("Gespeicherte SSH-Geräteschlüssel konnten nicht gelesen werden")?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("SSH-Geräteschlüssel konnten nicht geprüft werden"),
    }
    let (key, kind) = session
        .host_key()
        .context("SSH-Server liefert keinen Geräteschlüssel.")?;
    match known.check_port(host, port, key) {
        CheckResult::Match => return Ok(()),
        CheckResult::Mismatch => bail!("Der SSH-Geräteschlüssel für {} hat sich geändert. Die Anmeldung wurde gestoppt. Den Schlüssel am Gerät prüfen und den betreffenden Eintrag in {} erst danach berichtigen.", socket_address(host, port), path.display()),
        CheckResult::Failure => bail!("Der SSH-Geräteschlüssel konnte nicht geprüft werden."),
        CheckResult::NotFound => {}
    }
    let digest = session
        .host_key_hash(HashType::Sha256)
        .context("Kein SHA-256-Fingerabdruck für SSH verfügbar.")?;
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    );
    match worker.ask(Prompt::Trust {
        address: socket_address(host, port),
        fingerprint,
    })? {
        Answer::Trust => {}
        Answer::Secret(_) => bail!("SSH-Geräteschlüssel wurde nicht bestätigt."),
    }
    worker.cancel.check()?;
    fs::create_dir_all(directory)?;
    known.add(&format!("[{host}]:{port}"), key, "VNC Beach", kind.into())?;
    // Each endpoint owns a file, so trusting another host cannot overwrite it.
    let entry = known
        .hosts()?
        .into_iter()
        .find(|entry| entry.name() == Some(format!("[{host}]:{port}").as_str()))
        .context("SSH-Geräteschlüssel konnte nicht gespeichert werden.")?;
    let text = known.write_string(&entry, KnownHostFileKind::OpenSSH)?;
    use std::io::Write as _;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => file.write_all(text.as_bytes())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut current = session.known_hosts()?;
            current.read_file(&path, KnownHostFileKind::OpenSSH)?;
            if !matches!(current.check_port(host, port, key), CheckResult::Match) {
                bail!("Der SSH-Geräteschlüssel wurde zwischenzeitlich geändert. Bitte den gespeicherten Schlüssel prüfen.");
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn openssh_encrypted(text: &str) -> bool {
    if !text.contains("BEGIN OPENSSH PRIVATE KEY") {
        return false;
    }
    let encoded = Zeroizing::new(
        text.lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>(),
    );
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes()) else {
        return true;
    };
    let bytes = Zeroizing::new(bytes);
    // OpenSSH's first string after the magic is the cipher name.
    let Some(length) = bytes
        .get(15..19)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_be_bytes)
    else {
        return true;
    };
    bytes.get(19..19 + length as usize) != Some(b"none".as_slice())
}
