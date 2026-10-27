//! "Sign in with GitHub / Google": the OAuth authorization code flow. The
//! client secret stays on this server; only the provider's account ID,
//! name and login are kept.

use anyhow::{bail, Context as _};
use serde::Deserialize;

use crate::db::Identity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    GitHub,
    Google,
}

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::GitHub => "github",
            Kind::Google => "google",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::GitHub => "GitHub",
            Kind::Google => "Google",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub kind: Kind,
    pub client_id: String,
    pub client_secret: String,
}

impl Provider {
    /// Where the browser goes to sign in.
    pub fn authorize_url(&self, redirect_uri: &str, state: &str) -> String {
        let (base, scope) = match self.kind {
            Kind::GitHub => ("https://github.com/login/oauth/authorize", "read:user"),
            Kind::Google => (
                "https://accounts.google.com/o/oauth2/v2/auth",
                "openid email profile",
            ),
        };
        let mut query = vec![
            ("client_id", self.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("state", state),
            ("scope", scope),
        ];
        if self.kind == Kind::Google {
            query.push(("response_type", "code"));
            query.push(("prompt", "select_account"));
        }
        format!(
            "{base}?{}",
            serde_urlencoded::to_string(query).expect("query encodes")
        )
    }

    /// Trades the callback's code for the signed-in identity. Blocking.
    pub fn identify(&self, code: &str, redirect_uri: &str) -> anyhow::Result<Identity> {
        match self.kind {
            Kind::GitHub => self.github(code, redirect_uri),
            Kind::Google => self.google(code, redirect_uri),
        }
    }

    fn access_token(&self, url: &str, code: &str, redirect_uri: &str) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Token {
            access_token: Option<String>,
            error_description: Option<String>,
            error: Option<String>,
        }
        let token: Token = ureq::post(url)
            .header("Accept", "application/json")
            .send_form([
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .context("token request")?
            .body_mut()
            .read_json()
            .context("token response")?;
        match token.access_token {
            Some(token) => Ok(token),
            None => bail!(
                "{} refused the sign-in: {}",
                self.kind.label(),
                token.error_description.or(token.error).unwrap_or_default()
            ),
        }
    }

    fn github(&self, code: &str, redirect_uri: &str) -> anyhow::Result<Identity> {
        #[derive(Deserialize)]
        struct GitHubUser {
            id: u64,
            login: String,
            name: Option<String>,
        }
        let token = self.access_token("https://github.com/login/oauth/access_token", code, redirect_uri)?;
        let user: GitHubUser = ureq::get("https://api.github.com/user")
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "VNC-Beach-Coordination")
            .call()
            .context("GitHub user request")?
            .body_mut()
            .read_json()
            .context("GitHub user response")?;
        Ok(Identity {
            provider: "github".into(),
            subject: user.id.to_string(),
            name: user
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| user.login.clone()),
            login: user.login,
        })
    }

    fn google(&self, code: &str, redirect_uri: &str) -> anyhow::Result<Identity> {
        #[derive(Deserialize)]
        struct GoogleUser {
            sub: String,
            name: Option<String>,
            email: Option<String>,
        }
        let token = self.access_token("https://oauth2.googleapis.com/token", code, redirect_uri)?;
        let user: GoogleUser = ureq::get("https://openidconnect.googleapis.com/v1/userinfo")
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .context("Google user request")?
            .body_mut()
            .read_json()
            .context("Google user response")?;
        let login = user.email.unwrap_or_default();
        Ok(Identity {
            provider: "google".into(),
            subject: user.sub,
            name: user
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| login.clone()),
            login,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_urls() {
        let github = Provider {
            kind: Kind::GitHub,
            client_id: "abc".into(),
            client_secret: String::new(),
        };
        assert_eq!(
            github.authorize_url("https://beach.example.com/auth/github/callback", "s t"),
            "https://github.com/login/oauth/authorize?client_id=abc&redirect_uri=https%3A%2F%2Fbeach.example.com%2Fauth%2Fgithub%2Fcallback&state=s+t&scope=read%3Auser"
        );
        let google = Provider {
            kind: Kind::Google,
            ..github
        };
        let url = google.authorize_url("https://x/cb", "s");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("scope=openid+email+profile"));
        assert!(url.contains("response_type=code"));
    }
}
