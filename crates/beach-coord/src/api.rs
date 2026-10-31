//! The JSON API the app and the devices use (see `beach_account::api`).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use beach_account::api::{
    ApiError, Device, Favorite, Heartbeat, HeartbeatResponse, Info, Me, RegisterDevice, Registration, TicketRequest, TicketResponse,
    TokenRequest, TokenResponse, User,
};
use beach_account::ticket::{self, Claims};
use rfb::handshake::TICKET_NONCE_LEN;

use crate::db::{self, Db};
use crate::Shared;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/info", get(info))
        .route("/api/token", post(token))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/devices", get(devices).post(register))
        .route("/api/devices/{id}/ticket", post(issue_ticket))
        .route("/api/devices/{id}/update", post(request_update))
        .route("/api/favorites", get(favorites))
        .route("/api/favorites/{id}", put(put_favorite))
        .route("/api/device/heartbeat", post(heartbeat))
        .route("/api/device", delete(unregister))
}

pub struct Error(StatusCode, String);

impl Error {
    fn unauthorized() -> Error {
        Error(StatusCode::UNAUTHORIZED, "not signed in".into())
    }

    fn bad(msg: impl Into<String>) -> Error {
        Error(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        log::error!("database: {e}");
        Error(StatusCode::INTERNAL_SERVER_ERROR, "database error".into())
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
    }
}

type Result<T> = std::result::Result<T, Error>;

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn session(db: &Db, headers: &HeaderMap) -> Result<User> {
    let token = bearer(headers).ok_or_else(Error::unauthorized)?;
    db.session_user(token)?.ok_or_else(Error::unauthorized)
}

fn device_id(db: &Db, headers: &HeaderMap) -> Result<String> {
    let token = bearer(headers).ok_or_else(Error::unauthorized)?;
    db.device_by_token(token)?
        .ok_or_else(|| Error(StatusCode::UNAUTHORIZED, "unknown device".into()))
}

async fn info(State(app): State<Shared>) -> Json<Info> {
    Json(Info {
        name: app.name.clone(),
        providers: app.provider_ids(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

async fn token(State(app): State<Shared>, Json(req): Json<TokenRequest>) -> Result<Json<TokenResponse>> {
    let db = app.db();
    let user_id = db
        .take_app_code(&req.code, &req.verifier)?
        .ok_or_else(|| Error::bad("invalid or expired sign-in code"))?;
    let user = db.user(&user_id)?.ok_or_else(Error::unauthorized)?;
    let token = db.create_session(&user.id, "app")?;
    Ok(Json(TokenResponse { token, user }))
}

async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Result<StatusCode> {
    let db = app.db();
    if let Some(token) = bearer(&headers) {
        db.delete_session(token)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn me(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Me>> {
    let db = app.db();
    let user = session(&db, &headers)?;
    let orgs = db.orgs(&user.id)?;
    Ok(Json(Me { user, orgs }))
}

async fn devices(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<Device>>> {
    let db = app.db();
    let user = session(&db, &headers)?;
    let mut devices = db.visible_devices(&user.id)?;
    app.mark_outdated(&mut devices);
    Ok(Json(devices))
}

async fn favorites(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<Favorite>>> {
    let db = app.db();
    let user = session(&db, &headers)?;
    Ok(Json(db.favorites(&user.id)?))
}

async fn put_favorite(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut f): Json<Favorite>,
) -> Result<StatusCode> {
    let db = app.db();
    let user = session(&db, &headers)?;
    if f.id != id {
        return Err(Error::bad("favorite id does not match the path"));
    }
    beach_account::favorites::validate(&f).map_err(Error::bad)?;
    if f.deleted {
        // A tombstone needs nothing but its id and time.
        f = Favorite {
            name: String::new(),
            host: String::new(),
            alternatives: Vec::new(),
            view_only: false,
            ..f
        };
    }
    if !db.put_favorite(&user.id, &f)? {
        return Err(Error(StatusCode::INSUFFICIENT_STORAGE, "too many favorites".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn request_update(State(app): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> Result<StatusCode> {
    let db = app.db();
    let user = session(&db, &headers)?;
    match db.may_manage_device(&id, &user.id)? {
        db::Access::Manage => {}
        db::Access::View => {
            return Err(Error(
                StatusCode::FORBIDDEN,
                "Nur Admins und wer das Gerät hinzugefügt hat, können es aktualisieren.".into(),
            ))
        }
        db::Access::None => return Err(Error(StatusCode::NOT_FOUND, "no such device".into())),
    }
    db.request_update(&id)?;
    log::info!("{} requested an update of device {id}", user.login);
    Ok(StatusCode::NO_CONTENT)
}

async fn register(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<RegisterDevice>,
) -> Result<Json<Registration>> {
    let db = app.db();
    let user = session(&db, &headers)?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(Error::bad("the device name must have 1 to 64 characters"));
    }
    if !["macos", "windows", "linux"].contains(&req.platform.as_str()) {
        return Err(Error::bad("unknown platform"));
    }
    if let Some(org) = &req.org {
        if db.org(org, &user.id)?.is_none() {
            return Err(Error(
                StatusCode::FORBIDDEN,
                "not a member of this organization".into(),
            ));
        }
    }
    let (id, token) = db.register_device(name, &req.platform, req.org.as_deref(), &user.id)?;
    log::info!("{} registered device {id} ({name})", user.login);
    Ok(Json(Registration {
        id,
        token,
        coordinator_key: beach_account::encode_key(&app.key.verifying_key()),
    }))
}

async fn issue_ticket(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<TicketRequest>,
) -> Result<Json<TicketResponse>> {
    let db = app.db();
    let user = session(&db, &headers)?;
    if !db.visible_devices(&user.id)?.iter().any(|d| d.id == id) {
        return Err(Error(StatusCode::NOT_FOUND, "no such device".into()));
    }
    let nonce = beach_account::unb64(&req.nonce)
        .filter(|n| n.len() == TICKET_NONCE_LEN)
        .ok_or_else(|| Error::bad("bad nonce"))?;
    let claims = Claims::new(&id, &user.id, &user.name, &nonce, db::now());
    Ok(Json(TicketResponse {
        ticket: ticket::sign(&app.key, &claims),
    }))
}

async fn heartbeat(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(mut hb): Json<Heartbeat>,
) -> Result<Response> {
    let db = app.db();
    let id = device_id(&db, &headers)?;
    hb.addresses.retain(|a| !a.is_empty() && a.len() <= 255);
    hb.addresses.truncate(16);
    if let Some((cut, _)) = hb.version.char_indices().nth(32) {
        hb.version.truncate(cut);
    }
    if db.heartbeat(&id, &hb)? {
        log::info!("telling device {id} ({}) to update", hb.version);
        return Ok(Json(HeartbeatResponse { update: true }).into_response());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn unregister(State(app): State<Shared>, headers: HeaderMap) -> Result<StatusCode> {
    let db = app.db();
    let id = device_id(&db, &headers)?;
    db.delete_device(&id)?;
    log::info!("device {id} removed itself");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use beach_account::api::Role;
    use http_body_util::BodyExt as _;
    use serde::de::DeserializeOwned;
    use tower::ServiceExt as _;

    use super::*;
    use crate::db::Identity;

    async fn call<T: DeserializeOwned>(
        app: &Shared,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, Option<T>) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let req = match body {
            Some(body) => req
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = crate::router(app.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).ok())
    }

    fn sign_in(app: &Shared, name: &str) -> (User, String) {
        let db = app.db();
        let user = db
            .sign_in(&Identity {
                provider: "dev".into(),
                subject: name.into(),
                name: name.into(),
                login: name.into(),
            })
            .unwrap();
        let verifier = format!("verifier-{name}");
        let code = db
            .create_app_code(&user.id, &beach_account::challenge(&verifier))
            .unwrap();
        drop(db);
        let _ = verifier;
        (user, code)
    }

    #[tokio::test]
    async fn app_login_register_heartbeat_ticket() {
        let app = crate::test_app();
        let (alice, code) = sign_in(&app, "alice");

        let (status, _) = call::<TokenResponse>(
            &app,
            "POST",
            "/api/token",
            None,
            Some(serde_json::json!({"code": code, "verifier": "wrong"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (_, code) = sign_in(&app, "alice");
        let (status, session) = call::<TokenResponse>(
            &app,
            "POST",
            "/api/token",
            None,
            Some(serde_json::json!({"code": code, "verifier": "verifier-alice"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let session = session.unwrap();
        assert_eq!(session.user, alice);
        let token = session.token.as_str();

        let (_, reg) = call::<Registration>(
            &app,
            "POST",
            "/api/devices",
            Some(token),
            Some(serde_json::json!({"name": "Büro-PC", "platform": "windows", "org": null})),
        )
        .await;
        let reg = reg.unwrap();
        assert_eq!(
            beach_account::decode_key(&reg.coordinator_key),
            Some(app.key.verifying_key())
        );

        let (status, _) = call::<()>(
            &app,
            "POST",
            "/api/device/heartbeat",
            Some(&reg.token),
            Some(serde_json::json!({"addresses": ["100.64.0.2"], "port": 5901, "version": "0.3.0"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, devices) = call::<Vec<Device>>(&app, "GET", "/api/devices", Some(token), None).await;
        let devices = devices.unwrap();
        assert_eq!(devices.len(), 1);
        assert!(devices[0].online);
        assert_eq!(devices[0].addresses, ["100.64.0.2"]);

        let nonce = [5u8; TICKET_NONCE_LEN];
        let (_, ticket) = call::<TicketResponse>(
            &app,
            "POST",
            &format!("/api/devices/{}/ticket", reg.id),
            Some(token),
            Some(serde_json::json!({"nonce": beach_account::b64(&nonce)})),
        )
        .await;
        let verifier = ticket::Verifier {
            device: reg.id.clone(),
            key: app.key.verifying_key(),
        };
        let grant =
            rfb::handshake::TicketVerifier::verify(&verifier, &nonce, ticket.unwrap().ticket.as_bytes())
                .unwrap();
        assert_eq!(grant.user, "alice");

        // Bob can't get a ticket for Alice's personal device.
        let (_, code) = sign_in(&app, "bob");
        let (_, bob) = call::<TokenResponse>(
            &app,
            "POST",
            "/api/token",
            None,
            Some(serde_json::json!({"code": code, "verifier": "verifier-bob"})),
        )
        .await;
        let (status, _) = call::<TicketResponse>(
            &app,
            "POST",
            &format!("/api/devices/{}/ticket", reg.id),
            Some(&bob.unwrap().token),
            Some(serde_json::json!({"nonce": beach_account::b64(&nonce)})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Removing itself invalidates the device token.
        let (status, _) = call::<()>(&app, "DELETE", "/api/device", Some(&reg.token), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call::<()>(
            &app,
            "POST",
            "/api/device/heartbeat",
            Some(&reg.token),
            Some(serde_json::json!({"addresses": [], "port": 5901, "version": ""})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    async fn session_token(app: &Shared, name: &str) -> String {
        let (_, code) = sign_in(app, name);
        let (_, session) = call::<TokenResponse>(
            app,
            "POST",
            "/api/token",
            None,
            Some(serde_json::json!({"code": code, "verifier": format!("verifier-{name}")})),
        )
        .await;
        session.unwrap().token
    }

    #[tokio::test]
    async fn remote_update_reaches_the_device_through_the_heartbeat() {
        let app = crate::test_app();
        let alice = session_token(&app, "alice").await;
        let bob = session_token(&app, "bob").await;
        let (_, reg) = call::<Registration>(
            &app,
            "POST",
            "/api/devices",
            Some(&alice),
            Some(serde_json::json!({"name": "PC", "platform": "windows", "org": null})),
        )
        .await;
        let reg = reg.unwrap();
        let beat = serde_json::json!({"addresses": [], "port": 5901, "version": "0.4.0", "remote_update": true});
        let (status, _) = call::<()>(&app, "POST", "/api/device/heartbeat", Some(&reg.token), Some(beat.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, devices) = call::<Vec<Device>>(&app, "GET", "/api/devices", Some(&alice), None).await;
        let device = &devices.unwrap()[0];
        assert!(device.outdated, "0.4.0 is older than the test app's 0.4.1");
        assert!(device.can_update);

        let path = format!("/api/devices/{}/update", reg.id);
        let (status, _) = call::<()>(&app, "POST", &path, Some(&bob), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "not Bob's device");
        let (status, _) = call::<()>(&app, "POST", &path, Some(&alice), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, answer) =
            call::<HeartbeatResponse>(&app, "POST", "/api/device/heartbeat", Some(&reg.token), Some(beat.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert!(answer.unwrap().update);
        let (status, _) = call::<()>(&app, "POST", "/api/device/heartbeat", Some(&reg.token), Some(beat)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "only once");
    }

    #[tokio::test]
    async fn favorites_follow_the_newest_change() {
        let app = crate::test_app();
        let alice = session_token(&app, "alice").await;
        let bob = session_token(&app, "bob").await;
        let fav = |name: &str, updated: i64| {
            serde_json::json!({"id": "f1", "name": name, "host": "mac.local", "updated": updated})
        };
        let (status, _) = call::<()>(&app, "PUT", "/api/favorites/f1", Some(&alice), Some(fav("Mac", 10))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        // An older change from another device doesn't win.
        call::<()>(&app, "PUT", "/api/favorites/f1", Some(&alice), Some(fav("Alt", 5))).await;
        let (_, list) = call::<Vec<Favorite>>(&app, "GET", "/api/favorites", Some(&alice), None).await;
        assert_eq!(list.unwrap()[0].name, "Mac");
        // Deleting leaves a tombstone without the details.
        let gone = serde_json::json!({"id": "f1", "name": "Mac", "host": "mac.local", "updated": 20, "deleted": true});
        call::<()>(&app, "PUT", "/api/favorites/f1", Some(&alice), Some(gone)).await;
        let (_, list) = call::<Vec<Favorite>>(&app, "GET", "/api/favorites", Some(&alice), None).await;
        let list = list.unwrap();
        assert!(list[0].deleted && list[0].host.is_empty());
        // Everyone has their own.
        let (_, list) = call::<Vec<Favorite>>(&app, "GET", "/api/favorites", Some(&bob), None).await;
        assert!(list.unwrap().is_empty());
        let (status, _) = call::<()>(&app, "PUT", "/api/favorites/f2", Some(&alice), Some(fav("Mac", 30))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "id must match the path");
        let (status, _) = call::<()>(&app, "GET", "/api/favorites", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn org_devices_need_membership() {
        let app = crate::test_app();
        let (alice, code) = sign_in(&app, "alice");
        let org = app.db().create_org("Team", &alice.id).unwrap();
        let (_, session) = call::<TokenResponse>(
            &app,
            "POST",
            "/api/token",
            None,
            Some(serde_json::json!({"code": code, "verifier": "verifier-alice"})),
        )
        .await;
        let token = session.unwrap().token;
        let (_, me) = call::<Me>(&app, "GET", "/api/me", Some(&token), None).await;
        assert_eq!(me.unwrap().orgs[0].role, Role::Owner);

        let (status, _) = call::<Registration>(
            &app,
            "POST",
            "/api/devices",
            Some(&token),
            Some(serde_json::json!({"name": "Empfang", "platform": "macos", "org": org})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call::<Registration>(
            &app,
            "POST",
            "/api/devices",
            Some(&token),
            Some(serde_json::json!({"name": "Fremd", "platform": "macos", "org": "other"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call::<Me>(&app, "GET", "/api/me", Some("nope"), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
