//! `beach-coord`: the VNC Beach coordination server. Keeps accounts
//! (sign-in with GitHub or Google), organizations and the devices people
//! registered, and issues the tickets viewers connect with. Screen data
//! never passes through it. Self-hosting: `docs/coordination-server.md`.

mod api;
mod db;
mod oauth;
mod web;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context as _};
use beach_account::SigningKey;
use clap::Parser;

use crate::db::Db;
use crate::oauth::{Kind, Provider};

#[derive(Parser, Debug)]
#[command(version, about = "VNC Beach coordination server")]
struct Args {
    /// Address and port to listen on (plain HTTP; put a TLS proxy in front).
    #[arg(long, env = "BEACH_LISTEN", default_value = "0.0.0.0:8080")]
    listen: SocketAddr,
    /// Directory for the database and the ticket signing key.
    #[arg(long, env = "BEACH_DATA", default_value = "data")]
    data: PathBuf,
    /// The public HTTPS address, e.g. https://beach.example.com. Sign-in
    /// callbacks and invitation links are built from it.
    #[arg(long, env = "BEACH_PUBLIC_URL")]
    public_url: String,
    /// Name shown on the web pages and in the app.
    #[arg(long, env = "BEACH_NAME", default_value = "VNC Beach")]
    name: String,
    #[arg(long, env = "GITHUB_CLIENT_ID")]
    github_client_id: Option<String>,
    #[arg(long, env = "GITHUB_CLIENT_SECRET", hide_env_values = true)]
    github_client_secret: Option<String>,
    #[arg(long, env = "GOOGLE_CLIENT_ID")]
    google_client_id: Option<String>,
    #[arg(long, env = "GOOGLE_CLIENT_SECRET", hide_env_values = true)]
    google_client_secret: Option<String>,
    /// Sign in by just typing a name. For local testing only: anybody can
    /// become anybody.
    #[arg(long, env = "BEACH_DEV_LOGIN", value_parser = clap::builder::BoolishValueParser::new())]
    dev_login: bool,
}

pub struct App {
    pub db: Mutex<Db>,
    pub key: SigningKey,
    pub providers: Vec<Provider>,
    pub dev_login: bool,
    /// Without trailing slash.
    pub public_url: String,
    pub name: String,
}

impl App {
    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.kind.id() == id)
    }

    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.providers.iter().map(|p| p.kind.id().to_string()).collect();
        if self.dev_login {
            ids.push("dev".into());
        }
        ids
    }

    /// Cookies get `Secure` when the public address is HTTPS.
    pub fn secure(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

pub type Shared = Arc<App>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let mut providers = Vec::new();
    for (kind, id, secret) in [
        (Kind::GitHub, &args.github_client_id, &args.github_client_secret),
        (Kind::Google, &args.google_client_id, &args.google_client_secret),
    ] {
        match (id, secret) {
            (Some(client_id), Some(client_secret)) => providers.push(Provider {
                kind,
                client_id: client_id.clone(),
                client_secret: client_secret.clone(),
            }),
            (None, None) => {}
            _ => bail!("{} needs both client ID and client secret", kind.label()),
        }
    }
    if providers.is_empty() && !args.dev_login {
        bail!("configure GitHub and/or Google sign-in (see docs/coordination-server.md)");
    }
    if args.dev_login {
        log::warn!("--dev-login is on: anybody can sign in as anybody");
    }
    std::fs::create_dir_all(&args.data).with_context(|| format!("creating {}", args.data.display()))?;
    let db = Db::open(&args.data.join("beach.db"))?;
    db.clean_up()?;
    let key = load_key(&args.data.join("ticket-key"))?;
    log::info!("ticket key {}", beach_account::encode_key(&key.verifying_key()));

    let app = Arc::new(App {
        db: Mutex::new(db),
        key,
        providers,
        dev_login: args.dev_login,
        public_url: args.public_url.trim_end_matches('/').to_string(),
        name: args.name,
    });
    tokio::spawn({
        let app = app.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                if let Err(e) = app.db.lock().unwrap().clean_up() {
                    log::warn!("clean-up: {e}");
                }
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    log::info!("listening on {} for {}", args.listen, app.public_url);
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

pub fn router(app: Shared) -> axum::Router {
    web::routes().merge(api::routes()).with_state(app)
}

/// The Ed25519 key tickets are signed with. Devices pin its public half
/// when they are registered, so it must survive restarts and backups.
fn load_key(path: &Path) -> anyhow::Result<SigningKey> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let seed: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("{} is not a 32-byte key", path.display()))?;
            Ok(SigningKey::from_bytes(&seed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let seed: [u8; 32] = rand::random();
            write_private(path, &seed).with_context(|| format!("writing {}", path.display()))?;
            log::info!("created a new ticket key in {}", path.display());
            Ok(SigningKey::from_bytes(&seed))
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

#[cfg(test)]
pub fn test_app() -> Shared {
    Arc::new(App {
        db: Mutex::new(Db::memory()),
        key: SigningKey::from_bytes(&[3; 32]),
        providers: Vec::new(),
        dev_login: true,
        public_url: "http://beach.test".into(),
        name: "VNC Beach".into(),
    })
}
