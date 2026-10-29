//! The browser side: sign-in, the overview of one's devices and
//! organizations, and managing members and invitation links. Plain HTML
//! forms, no JavaScript beyond confirmation prompts (in `app.js`, so the
//! content security policy can forbid inline scripts).

use std::fmt::Write as _;

use axum::extract::{Form, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{AppendHeaders, Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use beach_account::api::{Device, Org, Role, User};
use serde::Deserialize;

use crate::db::{self, Db, Identity, Login};
use crate::{App, Shared};

const COOKIE: &str = "beach_session";
/// Ties a pending sign-in to the browser that started it, so nobody can
/// sign somebody else in by handing them a callback link.
const LOGIN_COOKIE: &str = "beach_login";
const REPO: &str = "https://github.com/realS3BI/vnc-beach";

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/", get(home))
        .route("/login", get(login_page))
        .route("/login/{provider}", get(login_start).post(dev_login))
        .route("/auth/{provider}/callback", get(callback))
        .route("/logout", post(logout))
        .route("/orgs", post(create_org))
        .route("/orgs/{id}", get(org_page))
        .route("/orgs/{id}/invites", post(create_invite))
        .route("/orgs/{id}/invites/{invite}/delete", post(delete_invite))
        .route("/orgs/{id}/members/{user}/role", post(set_role))
        .route("/orgs/{id}/members/{user}/remove", post(remove_member))
        .route("/orgs/{id}/delete", post(delete_org))
        .route("/devices/{id}/delete", post(delete_device))
        .route("/invite/{token}", get(invite_page).post(accept_invite))
        .route("/style.css", get(style))
        .route("/app.js", get(script))
        .route("/favicon.svg", get(favicon_svg))
        .route("/favicon.ico", get(favicon_ico))
        .route("/apple-touch-icon.png", get(apple_touch_icon))
}

/// Runs around every request: turns cross-site form posts away (the session
/// cookie is `SameSite=Lax`, this is the second line) and adds the security
/// headers to every answer. HTML is never cached, since pages show
/// invitation links and sign-in codes.
pub async fn protect(State(app): State<Shared>, request: Request, next: Next) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let cross_site = request
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == "cross-site");
        if cross_site {
            return Error(
                StatusCode::FORBIDDEN,
                "Diese Aktion kann nur von dieser Website aus ausgelöst werden.".into(),
            )
            .into_response();
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let fixed = [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
             font-src 'self'; form-action 'self' http://127.0.0.1:*; frame-ancestors 'none'; base-uri 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "same-origin"),
    ];
    for (name, value) in fixed {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if app.secure() {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    let html = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if html && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

// Errors

pub struct Error(StatusCode, String);

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        log::error!("database: {e}");
        Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Datenbankfehler. Bitte später noch einmal versuchen.".into(),
        )
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let body = format!(
            r#"<section class="card"><h1>Das ging nicht</h1><p>{}</p><p><a class="button" href="/">Zur Übersicht</a></p></section>"#,
            esc(&self.1)
        );
        (self.0, Html(page("Fehler", None, &body))).into_response()
    }
}

type Result<T> = std::result::Result<T, Error>;

fn not_found() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "Nicht gefunden, oder du hast keinen Zugriff darauf.".into(),
    )
}

fn forbidden(msg: &str) -> Error {
    Error(StatusCode::FORBIDDEN, msg.into())
}

// Sessions

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    cookie(headers, COOKIE)
}

fn cookie(headers: &HeaderMap, wanted: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == wanted)
        .map(|(_, value)| value.to_string())
}

fn current_user(db: &Db, headers: &HeaderMap) -> Result<Option<User>> {
    match cookie_token(headers) {
        Some(token) => Ok(db.session_user(&token)?),
        None => Ok(None),
    }
}

/// The signed-in user, or a redirect to the sign-in page.
fn require_user(db: &Db, headers: &HeaderMap, here: &str) -> std::result::Result<User, Box<Response>> {
    match current_user(db, headers) {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(Box::new(
            Redirect::to(&format!("/login?next={}", urlencode(here))).into_response(),
        )),
        Err(e) => Err(Box::new(e.into_response())),
    }
}

fn session_cookie(app: &App, token: &str, max_age: i64) -> String {
    set_cookie(app, COOKIE, token, max_age)
}

fn set_cookie(app: &App, name: &str, value: &str, max_age: i64) -> String {
    let secure = if app.secure() { "; Secure" } else { "" };
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

fn check_login_cookie(headers: &HeaderMap, state: &str) -> Result<()> {
    if cookie(headers, LOGIN_COOKIE).as_deref() == Some(state) {
        Ok(())
    } else {
        Err(Error(
            StatusCode::BAD_REQUEST,
            "Diese Anmeldung wurde in einem anderen Browser begonnen. Bitte noch einmal anmelden.".into(),
        ))
    }
}

/// Only local paths, so a crafted link can't send people elsewhere.
fn safe_next(next: Option<&str>) -> Option<String> {
    next.filter(|n| {
        // Browsers drop tabs and newlines from URLs, so "/\t/x" would become "//x".
        n.starts_with('/')
            && !n.starts_with("//")
            && !n.contains('\\')
            && !n.chars().any(|c| c.is_control() || c.is_whitespace())
    })
    .map(str::to_string)
}

// Sign-in

#[derive(Deserialize, Default)]
struct LoginQuery {
    app_port: Option<u16>,
    challenge: Option<String>,
    next: Option<String>,
}

impl LoginQuery {
    fn login(&self, provider: &str) -> Result<Login> {
        let app = match (self.app_port, &self.challenge) {
            (Some(port), Some(challenge)) if port >= 1024 && challenge.len() == 43 => {
                Some((port, challenge.clone()))
            }
            (None, None) => None,
            _ => {
                return Err(Error(
                    StatusCode::BAD_REQUEST,
                    "Ungültige Anmeldeanfrage der App.".into(),
                ))
            }
        };
        Ok(Login {
            provider: provider.into(),
            app,
            next: safe_next(self.next.as_deref()),
        })
    }

    fn query(&self) -> String {
        serde_urlencoded::to_string([
            ("app_port", self.app_port.map(|p| p.to_string())),
            ("challenge", self.challenge.clone()),
            ("next", self.next.clone()),
        ])
        .unwrap_or_default()
    }
}

async fn login_page(State(app): State<Shared>, Query(q): Query<LoginQuery>) -> Result<Html<String>> {
    q.login("check")?;
    let mut buttons = String::new();
    for provider in &app.providers {
        let _ = write!(
            buttons,
            r#"<a class="button primary wide" href="/login/{}?{}">Mit {} anmelden</a>"#,
            provider.kind.id(),
            esc(&q.query()),
            provider.kind.label()
        );
    }
    if app.dev_login {
        let _ = write!(
            buttons,
            r#"<a class="button wide" href="/login/dev?{}">Testanmeldung (nur zum Entwickeln)</a>"#,
            esc(&q.query())
        );
    }
    let hint = if q.app_port.is_some() {
        "<p>Nach der Anmeldung geht es automatisch zurück in die VNC-Beach-App.</p>"
    } else {
        ""
    };
    let body = format!(
        r#"<section class="card narrow">
<h1>Anmelden</h1>
<p>Mit deinem Konto siehst du in VNC Beach alle deine Geräte und die deiner Organisationen, auf jedem Computer, auf dem du angemeldet bist.</p>
{hint}
<div class="stack">{buttons}</div>
<p class="muted small">Gespeichert werden nur Name, Benutzername bzw. E-Mail-Adresse und die Konto-ID des Anbieters.</p>
</section>"#
    );
    Ok(Html(page("Anmelden", None, &body)))
}

async fn login_start(
    State(app): State<Shared>,
    Path(provider): Path<String>,
    Query(q): Query<LoginQuery>,
) -> Result<Response> {
    if provider == "dev" && app.dev_login {
        let state = app.db().create_login(&q.login("dev")?)?;
        let cookie = set_cookie(&app, LOGIN_COOKIE, &state, 600);
        let body = format!(
            r#"<section class="card narrow"><h1>Testanmeldung</h1>
<p class="muted">Nur zum Entwickeln: Jeder Name ist ein eigenes Konto, ohne Passwort.</p>
<form method="post" action="/login/dev" class="stack">
<input type="hidden" name="state" value="{}">
<label>Name<input name="name" required maxlength="64" autofocus></label>
<button class="primary">Anmelden</button></form></section>"#,
            esc(&state)
        );
        return Ok((
            [(header::SET_COOKIE, cookie)],
            Html(page("Testanmeldung", None, &body)),
        )
            .into_response());
    }
    let Some(oauth) = app.provider(&provider) else {
        return Err(not_found());
    };
    let state = app.db().create_login(&q.login(&provider)?)?;
    let redirect_uri = format!("{}/auth/{provider}/callback", app.public_url);
    let cookie = set_cookie(&app, LOGIN_COOKIE, &state, 600);
    Ok((
        [(header::SET_COOKIE, cookie)],
        Redirect::to(&oauth.authorize_url(&redirect_uri, &state)),
    )
        .into_response())
}

#[derive(Deserialize)]
struct DevLogin {
    state: String,
    name: String,
}

async fn dev_login(
    State(app): State<Shared>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Form(f): Form<DevLogin>,
) -> Result<Response> {
    if !app.dev_login || provider != "dev" {
        return Err(not_found());
    }
    check_login_cookie(&headers, &f.state)?;
    let name = f.name.trim();
    if name.is_empty() {
        return Err(Error(
            StatusCode::BAD_REQUEST,
            "Bitte einen Namen eingeben.".into(),
        ));
    }
    let identity = Identity {
        provider: "dev".into(),
        subject: name.to_lowercase(),
        name: name.into(),
        login: name.to_lowercase(),
    };
    finish(&app, &f.state, Some("dev"), identity)
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(app): State<Shared>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(cb): Query<Callback>,
) -> Result<Response> {
    let oauth = app.provider(&provider).cloned().ok_or_else(not_found)?;
    let (Some(code), Some(state)) = (cb.code, cb.state) else {
        let reason = cb.error.unwrap_or_else(|| "unbekannt".into());
        return Err(Error(
            StatusCode::BAD_REQUEST,
            format!("Die Anmeldung wurde abgebrochen ({reason})."),
        ));
    };
    check_login_cookie(&headers, &state)?;
    let redirect_uri = format!("{}/auth/{provider}/callback", app.public_url);
    let identity = tokio::task::spawn_blocking(move || oauth.identify(&code, &redirect_uri))
        .await
        .map_err(|e| Error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| {
            log::warn!("{provider} sign-in failed: {e:#}");
            Error(
                StatusCode::BAD_GATEWAY,
                format!("Die Anmeldung bei {} hat nicht geklappt.", oauth_label(&provider)),
            )
        })?;
    finish(&app, &state, Some(&provider), identity)
}

fn oauth_label(provider: &str) -> &str {
    match provider {
        "github" => "GitHub",
        "google" => "Google",
        other => other,
    }
}

/// Signs the browser in and, for the app, hands it a one-time code.
fn finish(app: &App, state: &str, provider: Option<&str>, identity: Identity) -> Result<Response> {
    let db = app.db();
    let login = db
        .take_login(state)?
        .filter(|l| Some(l.provider.as_str()) == provider)
        .ok_or_else(|| {
            Error(
                StatusCode::BAD_REQUEST,
                "Die Anmeldung ist abgelaufen. Bitte noch einmal versuchen.".into(),
            )
        })?;
    let user = db.sign_in(&identity)?;
    log::info!("{} signed in via {}", user.login, identity.provider);
    let token = db.create_session(&user.id, "web")?;
    let cookie = session_cookie(app, &token, db::WEB_SESSION_LIFETIME);
    let target = match &login.app {
        Some((port, challenge)) => {
            let code = db.create_app_code(&user.id, challenge)?;
            format!("http://127.0.0.1:{port}/callback?code={}", urlencode(&code))
        }
        None => login.next.unwrap_or_else(|| "/".into()),
    };
    let done = set_cookie(app, LOGIN_COOKIE, "", 0);
    Ok((
        AppendHeaders([(header::SET_COOKIE, cookie), (header::SET_COOKIE, done)]),
        Redirect::to(&target),
    )
        .into_response())
}

async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Result<Response> {
    if let Some(token) = cookie_token(&headers) {
        app.db().delete_session(&token)?;
    }
    Ok((
        [(header::SET_COOKIE, session_cookie(&app, "", 0))],
        Redirect::to("/login"),
    )
        .into_response())
}

// Overview

async fn home(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let db = app.db();
    let user = match require_user(&db, &headers, "/") {
        Ok(user) => user,
        Err(redirect) => return *redirect,
    };
    match home_body(&db, &user) {
        Ok(body) => Html(page("Übersicht", Some(&user), &body)).into_response(),
        Err(e) => e.into_response(),
    }
}

fn home_body(db: &Db, user: &User) -> Result<String> {
    let devices: Vec<Device> = db
        .visible_devices(&user.id)?
        .into_iter()
        .filter(|d| d.org.is_none())
        .collect();
    let orgs = db.orgs(&user.id)?;
    let mut body = String::new();
    body.push_str(r#"<section class="card"><div class="head"><h2>Meine Geräte</h2></div>"#);
    if devices.is_empty() {
        body.push_str(EMPTY_DEVICES);
    } else {
        body.push_str(&device_table(&devices, |_| true));
    }
    body.push_str("</section>");

    body.push_str(r#"<section class="card"><div class="head"><h2>Organisationen</h2></div>"#);
    if orgs.is_empty() {
        body.push_str(r#"<p class="muted">Mit einer Organisation teilst du Geräte mit deinem Team. Mitglieder lädst du per Link ein.</p>"#);
    } else {
        body.push_str(r#"<ul class="list">"#);
        for org in &orgs {
            let _ = write!(
                body,
                r#"<li><a href="/orgs/{}"><strong>{}</strong></a><span class="badge">{}</span></li>"#,
                esc(&org.id),
                esc(&org.name),
                role_label(org.role)
            );
        }
        body.push_str("</ul>");
    }
    body.push_str(
        r#"<form method="post" action="/orgs" class="inline">
<label class="grow">Neue Organisation<input name="name" required maxlength="64" placeholder="z. B. Büro Schlosser"></label>
<button class="primary">Anlegen</button></form></section>"#,
    );
    Ok(body)
}

const EMPTY_DEVICES: &str = r#"<p class="muted">Noch keine Geräte. Füge einen Computer in der VNC-Beach-App unter <em>Dieser Computer → Zum Konto hinzufügen</em> hinzu.</p>"#;

/// Devices with status and addresses; `removable` decides the button.
fn device_table(devices: &[Device], removable: impl Fn(&Device) -> bool) -> String {
    let now = db::now();
    let mut html = String::from(
        r#"<table><thead><tr><th>Gerät</th><th>Status</th><th>Adressen</th><th></th></tr></thead><tbody>"#,
    );
    for d in devices {
        let status = if d.online {
            r#"<span class="dot on"></span>online"#.to_string()
        } else {
            match d.last_seen {
                Some(t) => format!(r#"<span class="dot"></span>{}"#, ago(now - t)),
                None => r#"<span class="dot"></span>noch nie online"#.into(),
            }
        };
        let addresses = if d.addresses.is_empty() {
            "–".to_string()
        } else {
            d.addresses
                .iter()
                .map(|a| format!("<code>{}</code>", esc(a)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let remove = if removable(d) {
            format!(
                r#"<form method="post" action="/devices/{}/delete" data-confirm="{} wirklich entfernen? Danach ist das Gerät über das Konto nicht mehr erreichbar."><button class="link danger">Entfernen</button></form>"#,
                esc(&d.id),
                esc(&d.name)
            )
        } else {
            String::new()
        };
        let _ = write!(
            html,
            r#"<tr><td><strong>{}</strong><div class="muted small">{} · Port {}</div></td><td class="nowrap">{status}</td><td>{addresses}</td><td class="right">{remove}</td></tr>"#,
            esc(&d.name),
            platform_label(&d.platform),
            d.port
        );
    }
    html.push_str("</tbody></table>");
    html
}

// Organizations

#[derive(Deserialize)]
struct NameForm {
    name: String,
}

async fn create_org(State(app): State<Shared>, headers: HeaderMap, Form(f): Form<NameForm>) -> Response {
    let db = app.db();
    let user = match require_user(&db, &headers, "/") {
        Ok(user) => user,
        Err(redirect) => return *redirect,
    };
    let name = f.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Error(
            StatusCode::BAD_REQUEST,
            "Der Name braucht 1 bis 64 Zeichen.".into(),
        )
        .into_response();
    }
    match db.create_org(name, &user.id) {
        Ok(id) => Redirect::to(&format!("/orgs/{id}")).into_response(),
        Err(e) => Error::from(e).into_response(),
    }
}

/// The organization if the user is a member.
fn member_org(db: &Db, id: &str, user: &User) -> Result<Org> {
    db.org(id, &user.id)?.ok_or_else(not_found)
}

async fn org_page(State(app): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    render_org(&app, &headers, &id, None)
}

fn render_org(app: &App, headers: &HeaderMap, id: &str, new_link: Option<(&str, Role)>) -> Response {
    let db = app.db();
    let user = match require_user(&db, headers, &format!("/orgs/{id}")) {
        Ok(user) => user,
        Err(redirect) => return *redirect,
    };
    let result = (|| -> Result<String> {
        let org = member_org(&db, id, &user)?;
        org_body(app, &db, &org, &user, new_link)
    })();
    match result {
        Ok(body) => Html(page("Organisation", Some(&user), &body)).into_response(),
        Err(e) => e.into_response(),
    }
}

fn org_body(app: &App, db: &Db, org: &Org, user: &User, new_link: Option<(&str, Role)>) -> Result<String> {
    let manage = org.role.can_manage();
    let oid = esc(&org.id);
    let mut body = format!(
        r#"<p class="crumbs"><a href="/">Übersicht</a> / {}</p><h1>{}</h1><p class="muted">Deine Rolle: {}</p>"#,
        esc(&org.name),
        esc(&org.name),
        role_label(org.role)
    );

    // Devices
    let devices = db.org_devices(&org.id)?;
    body.push_str(r#"<section class="card"><div class="head"><h2>Geräte</h2></div>"#);
    if devices.is_empty() {
        body.push_str(r#"<p class="muted">Noch keine Geräte. Wer Mitglied ist, fügt einen Computer in der VNC-Beach-App unter <em>Dieser Computer → Zum Konto hinzufügen</em> hinzu und wählt dort diese Organisation.</p>"#);
    } else {
        body.push_str(&device_table(&devices, |_| manage));
    }
    body.push_str("</section>");

    // Members
    let members = db.members(&org.id)?;
    let owners = members.iter().filter(|m| m.role == Role::Owner).count();
    body.push_str(r#"<section class="card"><div class="head"><h2>Mitglieder</h2></div><table><tbody>"#);
    for m in &members {
        let me = m.user.id == user.id;
        let uid = esc(&m.user.id);
        let last_owner = m.role == Role::Owner && owners == 1;
        let may_edit = manage && !me && (m.role != Role::Owner || org.role == Role::Owner);
        let role = if may_edit {
            let mut options = String::new();
            let roles: &[Role] = if org.role == Role::Owner {
                &[Role::Member, Role::Admin, Role::Owner]
            } else {
                &[Role::Member, Role::Admin]
            };
            for r in roles {
                let selected = if *r == m.role { " selected" } else { "" };
                let _ = write!(
                    options,
                    r#"<option value="{}"{selected}>{}</option>"#,
                    r.as_str(),
                    role_label(*r)
                );
            }
            format!(
                r#"<form method="post" action="/orgs/{oid}/members/{uid}/role" class="inline"><select name="role" aria-label="Rolle von {}">{options}</select><button>Ändern</button></form>"#,
                esc(&m.user.name)
            )
        } else {
            format!(r#"<span class="badge">{}</span>"#, role_label(m.role))
        };
        let remove = if (may_edit || me) && !last_owner {
            let (label, question) = if me {
                ("Verlassen", format!("{} wirklich verlassen?", org.name))
            } else {
                (
                    "Entfernen",
                    format!("{} aus {} entfernen?", m.user.name, org.name),
                )
            };
            format!(
                r#"<form method="post" action="/orgs/{oid}/members/{uid}/remove" data-confirm="{}"><button class="link danger">{label}</button></form>"#,
                esc(&question)
            )
        } else {
            String::new()
        };
        let you = if me {
            r#" <span class="muted">(du)</span>"#
        } else {
            ""
        };
        let _ = write!(
            body,
            r#"<tr><td><strong>{}</strong>{you}<div class="muted small">{}</div></td><td>{role}</td><td class="right">{remove}</td></tr>"#,
            esc(&m.user.name),
            esc(&m.user.login)
        );
    }
    body.push_str("</tbody></table></section>");

    // Invitations
    if manage {
        body.push_str(r#"<section class="card"><div class="head"><h2>Einladen</h2></div>"#);
        if let Some((token, role)) = new_link {
            let _ = write!(
                body,
                r#"<div class="notice"><p><strong>Neuer Einladungslink als {}.</strong> Er wird nur jetzt angezeigt, gilt 7 Tage und funktioniert einmal.</p><input class="copy" readonly value="{}/invite/{}" data-select></div>"#,
                role_label(role),
                esc(&app.public_url),
                esc(token)
            );
        }
        let _ = write!(
            body,
            r#"<form method="post" action="/orgs/{oid}/invites" class="inline">
<label>Rolle<select name="role"><option value="member">Mitglied</option><option value="admin">Admin</option></select></label>
<button class="primary">Einladungslink erstellen</button></form>
<p class="muted small">Wer den Link öffnet, meldet sich mit GitHub oder Google an und ist danach Mitglied. Mitglieder sehen alle Geräte der Organisation und können sich verbinden; Admins verwalten zusätzlich Mitglieder, Einladungen und Geräte.</p>"#
        );
        let invites = db.invites(&org.id)?;
        if !invites.is_empty() {
            body.push_str(r#"<h3>Offene Einladungen</h3><ul class="list">"#);
            let now = db::now();
            for i in invites {
                let _ = write!(
                    body,
                    r#"<li>{} · läuft {} ab<form method="post" action="/orgs/{oid}/invites/{}/delete"><button class="link danger">Widerrufen</button></form></li>"#,
                    role_label(i.role),
                    until(i.expires - now),
                    esc(&i.id)
                );
            }
            body.push_str("</ul>");
        }
        body.push_str("</section>");
    }

    if org.role == Role::Owner {
        let _ = write!(
            body,
            r#"<details class="card danger-zone"><summary>Organisation löschen</summary>
<p>Entfernt die Organisation, alle Mitgliedschaften und alle {} Geräte aus dem Konto. Die Geräte selbst laufen weiter, sind aber nur noch über ihr VNC-Passwort erreichbar.</p>
<form method="post" action="/orgs/{oid}/delete" class="inline"><label class="grow">Zum Bestätigen den Namen eingeben<input name="name" required autocomplete="off"></label><button class="danger">Endgültig löschen</button></form></details>"#,
            devices.len()
        );
    }
    Ok(body)
}

#[derive(Deserialize)]
struct RoleForm {
    role: String,
}

async fn create_invite(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<RoleForm>,
) -> Response {
    let token = {
        let db = app.db();
        let user = match require_user(&db, &headers, &format!("/orgs/{id}")) {
            Ok(user) => user,
            Err(redirect) => return *redirect,
        };
        let result = (|| -> Result<(String, Role)> {
            let org = member_org(&db, &id, &user)?;
            if !org.role.can_manage() {
                return Err(forbidden("Nur Admins können einladen."));
            }
            let role = match Role::parse(&f.role) {
                Some(Role::Owner) | None => Role::Member,
                Some(role) => role,
            };
            Ok((db.create_invite(&org.id, role, &user.id)?, role))
        })();
        match result {
            Ok(token) => token,
            Err(e) => return e.into_response(),
        }
    };
    render_org(&app, &headers, &id, Some((&token.0, token.1)))
}

async fn delete_invite(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path((id, invite)): Path<(String, String)>,
) -> Response {
    org_action(&app, &headers, &id, |db, org, _| {
        if !org.role.can_manage() {
            return Err(forbidden("Nur Admins verwalten Einladungen."));
        }
        db.delete_invite(&org.id, &invite)?;
        Ok(())
    })
}

async fn set_role(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path((id, target)): Path<(String, String)>,
    Form(f): Form<RoleForm>,
) -> Response {
    org_action(&app, &headers, &id, |db, org, user| {
        let new =
            Role::parse(&f.role).ok_or_else(|| Error(StatusCode::BAD_REQUEST, "Unbekannte Rolle.".into()))?;
        let members = db.members(&org.id)?;
        let current = members
            .iter()
            .find(|m| m.user.id == target)
            .ok_or_else(not_found)?
            .role;
        check_member_change(org, user, &target, current, Some(new), &members)?;
        db.set_role(&org.id, &target, new)?;
        Ok(())
    })
}

async fn remove_member(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path((id, target)): Path<(String, String)>,
) -> Response {
    let leaving = {
        let db = app.db();
        matches!(current_user(&db, &headers), Ok(Some(u)) if u.id == target)
    };
    let response = org_action(&app, &headers, &id, |db, org, user| {
        let members = db.members(&org.id)?;
        let current = members
            .iter()
            .find(|m| m.user.id == target)
            .ok_or_else(not_found)?
            .role;
        check_member_change(org, user, &target, current, None, &members)?;
        db.remove_member(&org.id, &target)?;
        Ok(())
    });
    if leaving && response.status().is_redirection() {
        return Redirect::to("/").into_response();
    }
    response
}

/// Who may change or remove (`new: None`) a member: admins for members and
/// admins, owners for everybody, everybody for leaving. The last owner
/// stays.
fn check_member_change(
    org: &Org,
    user: &User,
    target: &str,
    current: Role,
    new: Option<Role>,
    members: &[db::Member],
) -> Result<()> {
    let me = target == user.id;
    let touches_owner = current == Role::Owner || new == Some(Role::Owner);
    let allowed = if me && new.is_none() {
        true
    } else {
        !me && org.role.can_manage() && (!touches_owner || org.role == Role::Owner)
    };
    if !allowed {
        return Err(forbidden("Dafür fehlen dir die Rechte in dieser Organisation."));
    }
    let owners = members.iter().filter(|m| m.role == Role::Owner).count();
    if current == Role::Owner && new != Some(Role::Owner) && owners == 1 {
        return Err(forbidden("Die Organisation braucht mindestens einen Owner."));
    }
    Ok(())
}

async fn delete_org(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<NameForm>,
) -> Response {
    let response = org_action(&app, &headers, &id, |db, org, _| {
        if org.role != Role::Owner {
            return Err(forbidden("Nur Owner können die Organisation löschen."));
        }
        if f.name.trim() != org.name {
            return Err(Error(
                StatusCode::BAD_REQUEST,
                "Der eingegebene Name stimmt nicht.".into(),
            ));
        }
        db.delete_org(&org.id)?;
        Ok(())
    });
    if response.status().is_redirection() {
        return Redirect::to("/").into_response();
    }
    response
}

/// Runs a change on an organization the user belongs to and returns to its page.
fn org_action(
    app: &App,
    headers: &HeaderMap,
    id: &str,
    change: impl FnOnce(&Db, &Org, &User) -> Result<()>,
) -> Response {
    let db = app.db();
    let here = format!("/orgs/{id}");
    let user = match require_user(&db, headers, &here) {
        Ok(user) => user,
        Err(redirect) => return *redirect,
    };
    let result = member_org(&db, id, &user).and_then(|org| change(&db, &org, &user));
    match result {
        Ok(()) => Redirect::to(&here).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn delete_device(State(app): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let db = app.db();
    let user = match require_user(&db, &headers, "/") {
        Ok(user) => user,
        Err(redirect) => return *redirect,
    };
    let result = (|| -> Result<String> {
        let owner = db.device_owner(&id)?.ok_or_else(not_found)?;
        let back = match &owner.org_id {
            Some(org_id) => {
                let org = member_org(&db, org_id, &user)?;
                if !org.role.can_manage() && owner.registered_by != user.id {
                    return Err(forbidden("Nur Admins entfernen Geräte der Organisation."));
                }
                format!("/orgs/{org_id}")
            }
            None if owner.user_id.as_deref() == Some(user.id.as_str()) => "/".into(),
            None => return Err(not_found()),
        };
        db.delete_device(&id)?;
        log::info!("{} removed device {id}", user.login);
        Ok(back)
    })();
    match result {
        Ok(back) => Redirect::to(&back).into_response(),
        Err(e) => e.into_response(),
    }
}

// Invitations

async fn invite_page(State(app): State<Shared>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let db = app.db();
    let here = format!("/invite/{token}");
    let user = match require_user(&db, &headers, &here) {
        Ok(user) => user,
        Err(redirect) => return *redirect,
    };
    let result = (|| -> Result<String> {
        let (org_id, name) = db.invite_org(&token)?.ok_or_else(|| {
            Error(
                StatusCode::NOT_FOUND,
                "Dieser Einladungslink ist abgelaufen, wurde widerrufen oder schon benutzt. Bitte um einen neuen.".into(),
            )
        })?;
        if db.org(&org_id, &user.id)?.is_some() {
            return Ok(format!(
                r#"<section class="card narrow"><h1>{}</h1><p>Du bist schon Mitglied.</p><p><a class="button primary" href="/orgs/{}">Zur Organisation</a></p></section>"#,
                esc(&name),
                esc(&org_id)
            ));
        }
        Ok(format!(
            r#"<section class="card narrow"><h1>Einladung</h1>
<p>Du wurdest zu <strong>{}</strong> eingeladen. Als Mitglied siehst du die Geräte der Organisation in VNC Beach und kannst dich mit ihnen verbinden.</p>
<p class="muted">Angemeldet als {} ({}).</p>
<form method="post" action="{}"><button class="primary">Einladung annehmen</button></form></section>"#,
            esc(&name),
            esc(&user.name),
            esc(&user.login),
            esc(&here)
        ))
    })();
    match result {
        Ok(body) => Html(page("Einladung", Some(&user), &body)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn accept_invite(State(app): State<Shared>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let db = app.db();
    let user = match require_user(&db, &headers, &format!("/invite/{token}")) {
        Ok(user) => user,
        Err(redirect) => return *redirect,
    };
    match db.accept_invite(&token, &user.id) {
        Ok(Some(org)) => {
            log::info!("{} joined {org}", user.login);
            Redirect::to(&format!("/orgs/{org}")).into_response()
        }
        Ok(None) => Error(
            StatusCode::NOT_FOUND,
            "Dieser Einladungslink gilt nicht mehr.".into(),
        )
        .into_response(),
        Err(e) => Error::from(e).into_response(),
    }
}

// Rendering

fn page(title: &str, user: Option<&User>, body: &str) -> String {
    let account = match user {
        Some(user) => format!(
            r#"<div class="who"><span>{}</span><form method="post" action="/logout"><button class="link">Abmelden</button></form></div>"#,
            esc(&user.name)
        ),
        None => String::new(),
    };
    format!(
        r#"<!doctype html>
<html lang="de"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{} · VNC Beach</title><link rel="stylesheet" href="/style.css">
<link rel="icon" href="/favicon.svg" type="image/svg+xml"><link rel="icon" href="/favicon.ico" sizes="any">
<link rel="apple-touch-icon" href="/apple-touch-icon.png"><script src="/app.js" defer></script></head>
<body><header><a class="brand" href="/">VNC Beach <span>Konto</span></a>{account}</header>
<main>{body}</main>
<footer>VNC Beach ist Open Source · <a href="{REPO}">Quellcode</a> · diesen Server kann jeder selbst betreiben</footer>
</body></html>"#,
        esc(title)
    )
}

async fn style() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

async fn script() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], SCRIPT)
}

/// Confirmation prompts for forms with `data-confirm`, and click-to-select
/// for inputs with `data-select`.
const SCRIPT: &str = r#"
document.addEventListener('submit', function (e) {
  var q = e.target.getAttribute('data-confirm');
  if (q && !window.confirm(q)) { e.preventDefault(); }
});
document.addEventListener('click', function (e) {
  if (e.target.hasAttribute && e.target.hasAttribute('data-select')) { e.target.select(); }
});
"#;

// The same icons as the landing page in docs/.
const CACHE_ICON: (header::HeaderName, &str) = (header::CACHE_CONTROL, "public, max-age=86400");

async fn favicon_svg() -> impl IntoResponse {
    let svg = include_str!("../../../docs/favicon.svg");
    ([(header::CONTENT_TYPE, "image/svg+xml"), CACHE_ICON], svg)
}

async fn favicon_ico() -> impl IntoResponse {
    let ico = include_bytes!("../../../docs/favicon.ico");
    ([(header::CONTENT_TYPE, "image/x-icon"), CACHE_ICON], ico)
}

async fn apple_touch_icon() -> impl IntoResponse {
    let png = include_bytes!("../../../docs/apple-touch-icon.png");
    ([(header::CONTENT_TYPE, "image/png"), CACHE_ICON], png)
}

const STYLE: &str = r#"
:root { --sea:#1E97CC; --deep:#0E5F8F; --deeper:#0A4568; --sand:#F4DCA7; --cream:#FFF8EE; --red:#D93A3D; --ink:#17364A; --line:#E8DCC6;
  --display:"Avenir Next","Segoe UI",system-ui,sans-serif; --body:system-ui,"Segoe UI",sans-serif; --mono:ui-monospace,"SF Mono",Menlo,Consolas,monospace; }
* { box-sizing:border-box; }
body { margin:0; background:var(--cream); color:var(--ink); font:16px/1.5 var(--body); }
header { display:flex; justify-content:space-between; align-items:center; padding:14px 24px; background:var(--deeper); color:white; }
.brand { color:white; text-decoration:none; font:700 20px var(--display); }
.brand span { font-weight:400; opacity:.75; }
.who { display:flex; gap:12px; align-items:center; }
.who .link { color:#BFE6F8; }
main { max-width:880px; margin:0 auto; padding:28px 20px 48px; }
h1,h2,h3 { font-family:var(--display); line-height:1.15; }
h1 { font-size:30px; margin:0 0 4px; } h2 { font-size:20px; margin:0; } h3 { font-size:16px; margin:20px 0 8px; }
a { color:var(--deep); }
.card { background:white; border:1px solid var(--line); border-radius:14px; padding:20px 22px; margin:18px 0; }
.card.narrow { max-width:460px; margin:48px auto; }
.head { display:flex; justify-content:space-between; align-items:center; margin-bottom:12px; }
.crumbs { margin:0 0 6px; font-size:14px; }
.muted { color:#5B7385; } .small { font-size:13px; }
.stack { display:flex; flex-direction:column; gap:10px; margin:18px 0; }
.inline { display:flex; gap:10px; align-items:flex-end; flex-wrap:wrap; margin:0; }
.grow { flex:1; min-width:200px; }
label { display:flex; flex-direction:column; gap:4px; font-size:14px; font-weight:600; }
input, select { font:inherit; padding:8px 10px; border:1px solid #C9BBA3; border-radius:8px; background:white; color:var(--ink); }
input:focus, select:focus, button:focus-visible, .button:focus-visible { outline:3px solid #8EDDFB; outline-offset:1px; }
button, .button { font:600 15px var(--body); padding:9px 16px; border-radius:9px; border:1px solid #C9BBA3; background:white; color:var(--ink); cursor:pointer; text-decoration:none; display:inline-block; text-align:center; }
.primary { background:var(--sea); border-color:var(--sea); color:white; }
.primary:hover { background:var(--deep); }
.wide { width:100%; }
button.danger { background:var(--red); border-color:var(--red); color:white; }
.link { background:none; border:none; padding:0; color:var(--deep); font-weight:600; }
.link.danger { color:var(--red); background:none; }
table { width:100%; border-collapse:collapse; }
th { text-align:left; font-size:13px; color:#5B7385; font-weight:600; padding:6px 8px; border-bottom:1px solid var(--line); }
td { padding:10px 8px; border-bottom:1px solid var(--line); vertical-align:top; }
tr:last-child td { border-bottom:none; }
td form { margin:0; }
.right { text-align:right; } .nowrap { white-space:nowrap; }
code { font:13px var(--mono); background:#F3EEE4; padding:1px 5px; border-radius:5px; }
.dot { display:inline-block; width:9px; height:9px; border-radius:50%; background:#B8B0A2; margin-right:6px; }
.dot.on { background:#2E9D5B; }
.badge { font-size:12px; font-weight:700; background:#E4F4FB; color:var(--deep); padding:2px 8px; border-radius:20px; margin-left:8px; }
.list { list-style:none; padding:0; margin:0 0 14px; }
.list li { display:flex; justify-content:space-between; align-items:center; padding:8px 0; border-bottom:1px solid var(--line); gap:10px; }
.list li form { margin:0; }
.notice { background:#FFF3D1; border:1px solid #F0D58A; border-radius:10px; padding:12px 14px; margin-bottom:16px; }
.notice p { margin:0 0 8px; }
.copy { width:100%; font:13px var(--mono); }
.danger-zone summary { cursor:pointer; font-weight:700; color:var(--red); }
footer { text-align:center; font-size:13px; color:#5B7385; padding:0 20px 32px; }
@media (max-width:640px) { th:nth-child(3), td:nth-child(3) { display:none; } }
"#;

pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn urlencode(text: &str) -> String {
    serde_urlencoded::to_string([("", text)])
        .map(|s| s[1..].to_string())
        .unwrap_or_default()
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Member => "Mitglied",
        Role::Admin => "Admin",
        Role::Owner => "Owner",
    }
}

fn platform_label(platform: &str) -> &str {
    match platform {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

/// "vor 5 Min." for a duration in seconds.
fn ago(secs: i64) -> String {
    if secs < 3600 {
        format!("vor {} Min.", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("vor {} Std.", secs / 3600)
    } else {
        format!("vor {} Tagen", secs / 86_400)
    }
}

/// "in 6 Tagen" for a remaining duration in seconds.
fn until(secs: i64) -> String {
    if secs < 3600 {
        format!("in {} Min.", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("in {} Std.", secs / 3600)
    } else {
        format!("in {} Tagen", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use super::*;

    async fn send(app: &Shared, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let res = crate::router(app.clone()).oneshot(req).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
        let mut req = Request::get(path);
        if let Some(c) = cookie {
            req = req.header("cookie", format!("{COOKIE}={c}"));
        }
        req.body(Body::empty()).unwrap()
    }

    fn post(path: &str, cookie: Option<&str>, form: &str) -> Request<Body> {
        let mut req = Request::post(path).header("content-type", "application/x-www-form-urlencoded");
        if let Some(c) = cookie {
            req = req.header("cookie", format!("{COOKIE}={c}"));
        }
        req.body(Body::from(form.to_string())).unwrap()
    }

    /// Goes through the dev sign-in like a browser; returns the cookie.
    async fn dev_sign_in(app: &Shared, name: &str, query: &str) -> (String, HeaderMap) {
        let (_, headers, html) = send(app, get(&format!("/login/dev?{query}"), None)).await;
        let login_cookie = headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let state = html
            .split(r#"name="state" value=""#)
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let form = format!("state={}&name={name}", urlencode(state));
        let without = post("/login/dev", None, &form);
        assert_eq!(
            send(app, without).await.0,
            StatusCode::BAD_REQUEST,
            "needs the login cookie"
        );
        let (_, headers, html) = send(app, get(&format!("/login/dev?{query}"), None)).await;
        let login_cookie2 = headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        assert_ne!(login_cookie, login_cookie2);
        let state = html
            .split(r#"name="state" value=""#)
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let form = format!("state={}&name={name}", urlencode(state));
        let req = Request::post("/login/dev")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", login_cookie2)
            .body(Body::from(form))
            .unwrap();
        let (status, headers, _) = send(app, req).await;
        assert!(status.is_redirection());
        let cookie = headers[header::SET_COOKIE].to_str().unwrap();
        let token = cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_string();
        (token, headers)
    }

    #[tokio::test]
    async fn signed_out_visitors_go_to_the_login() {
        let app = crate::test_app();
        let (status, headers, _) = send(&app, get("/orgs/x", None)).await;
        assert!(status.is_redirection());
        assert_eq!(headers[header::LOCATION], "/login?next=%2Forgs%2Fx");
        let (status, _, html) = send(&app, get("/login?next=/", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Testanmeldung"));
    }

    #[tokio::test]
    async fn app_login_redirects_to_loopback_with_a_code() {
        let app = crate::test_app();
        let challenge = beach_account::challenge("v");
        let (_, headers) = dev_sign_in(&app, "Alice", &format!("app_port=4711&challenge={challenge}")).await;
        let location = headers[header::LOCATION].to_str().unwrap();
        let code = location
            .strip_prefix("http://127.0.0.1:4711/callback?code=")
            .unwrap();
        let code = serde_urlencoded::from_str::<Vec<(String, String)>>(&format!("c={code}")).unwrap()[0]
            .1
            .clone();
        let user = app.db().take_app_code(&code, "v").unwrap();
        assert!(user.is_some());
    }

    #[tokio::test]
    async fn team_flow_invite_join_and_leave() {
        let app = crate::test_app();
        let (alice, _) = dev_sign_in(&app, "Alice", "").await;
        let (status, headers, _) = send(&app, post("/orgs", Some(&alice), "name=B%C3%BCro")).await;
        assert!(status.is_redirection());
        let org_path = headers[header::LOCATION].to_str().unwrap().to_string();

        let (_, _, html) = send(
            &app,
            post(&format!("{org_path}/invites"), Some(&alice), "role=admin"),
        )
        .await;
        assert!(html.contains("Neuer Einladungslink als Admin"));
        let link = html
            .split("http://beach.test/invite/")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();

        let (bob, _) = dev_sign_in(&app, "Bob", "").await;
        let (_, _, html) = send(&app, get(&format!("/invite/{link}"), Some(&bob))).await;
        assert!(html.contains("Büro"));
        let (status, headers, _) = send(&app, post(&format!("/invite/{link}"), Some(&bob), "")).await;
        assert!(status.is_redirection());
        assert_eq!(headers[header::LOCATION], org_path.as_str());

        let (_, _, html) = send(&app, get(&org_path, Some(&bob))).await;
        assert!(html.contains("Deine Rolle: Admin"));
        assert!(html.contains("Alice"));

        // An admin can't demote the owner, and the last owner can't leave.
        let alice_id = app.db().session_user(&alice).unwrap().unwrap().id;
        let (status, _, _) = send(
            &app,
            post(
                &format!("{org_path}/members/{alice_id}/role"),
                Some(&bob),
                "role=member",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = send(
            &app,
            post(&format!("{org_path}/members/{alice_id}/remove"), Some(&alice), ""),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let bob_id = app.db().session_user(&bob).unwrap().unwrap().id;
        let (status, headers, _) = send(
            &app,
            post(&format!("{org_path}/members/{bob_id}/remove"), Some(&bob), ""),
        )
        .await;
        assert!(status.is_redirection());
        assert_eq!(headers[header::LOCATION], "/");
        let (status, _, _) = send(&app, get(&org_path, Some(&bob))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Deleting needs the exact name.
        let (status, _, _) = send(
            &app,
            post(&format!("{org_path}/delete"), Some(&alice), "name=Buero"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _) = send(
            &app,
            post(&format!("{org_path}/delete"), Some(&alice), "name=B%C3%BCro"),
        )
        .await;
        assert!(status.is_redirection());
    }

    #[tokio::test]
    async fn pages_escape_user_input() {
        let app = crate::test_app();
        let (alice, _) = dev_sign_in(&app, "%3Cscript%3E", "").await;
        let (_, _, html) = send(&app, get("/", Some(&alice))).await;
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[tokio::test]
    async fn answers_carry_security_headers_and_refuse_cross_site_posts() {
        let app = crate::test_app();
        let (status, headers, _) = send(&app, get("/login", None)).await;
        assert_eq!(status, StatusCode::OK);
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("script-src 'self'"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert!(
            !headers.contains_key(header::STRICT_TRANSPORT_SECURITY),
            "test app is plain HTTP"
        );
        // Icons keep their own cache header; the script is served for the CSP.
        let (_, headers, _) = send(&app, get("/favicon.svg", None)).await;
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=86400");
        let (status, _, js) = send(&app, get("/app.js", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(js.contains("data-confirm"));

        let (alice, _) = dev_sign_in(&app, "Alice", "").await;
        let mut req = post("/orgs", Some(&alice), "name=Team");
        req.headers_mut()
            .insert("sec-fetch-site", "cross-site".parse().unwrap());
        let (status, _, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let mut req = post("/orgs", Some(&alice), "name=Team");
        req.headers_mut()
            .insert("sec-fetch-site", "same-origin".parse().unwrap());
        let (status, _, _) = send(&app, req).await;
        assert!(status.is_redirection());
        // No inline event handlers anywhere, so the CSP can forbid them.
        let (_, _, html) = send(&app, get("/", Some(&alice))).await;
        assert!(
            !html.contains("onsubmit=") && !html.contains("onclick="),
            "{html}"
        );
    }

    #[test]
    fn next_must_be_local() {
        assert_eq!(safe_next(Some("/orgs/1")), Some("/orgs/1".into()));
        assert_eq!(safe_next(Some("//evil.example")), None);
        assert_eq!(safe_next(Some("https://evil.example")), None);
        assert_eq!(safe_next(Some("/\\evil.example")), None);
        assert_eq!(safe_next(Some("/\t/evil.example")), None);
        assert_eq!(safe_next(Some("/\n")), None);
    }
}
