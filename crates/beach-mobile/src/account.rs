//! Account tickets for the app (security type
//! [`rfb::handshake::SECURITY_BEACH_TICKET`]). The coordination server is
//! asked through an HTTP callback, so the app's URLSession does TLS and no
//! TLS stack is linked in. Sign-in and the device list stay in Swift.

use beach_account::api::{ApiError, TicketRequest, TicketResponse};
use serde::Deserialize;

/// The account device a connection is meant for, as JSON from the app.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Target {
    /// The coordination server, e.g. `https://beach.example.com`.
    pub url: String,
    /// Session token of the signed-in account.
    pub token: String,
    /// The device ID from the device list.
    pub device: String,
}

/// Answer of an HTTP POST: status code and body.
pub type Answer = Result<(u16, String), String>;

/// Asks the coordination server for a ticket for `device` and the server's
/// `nonce`. `post(url, token, json)` sends the request.
pub fn ticket(
    target: &Target,
    post: &dyn Fn(&str, &str, &str) -> Answer,
    device: &str,
    nonce: &[u8],
) -> Result<Vec<u8>, String> {
    if device != target.device {
        return Err("An dieser Adresse antwortet ein anderes Gerät des Kontos.".into());
    }
    let url = format!("{}/api/devices/{}/ticket", target.url.trim_end_matches('/'), target.device);
    let body = serde_json::to_string(&TicketRequest {
        nonce: beach_account::b64(nonce),
    })
    .expect("request serializes");
    let (status, answer) =
        post(&url, &target.token, &body).map_err(|e| format!("Koordinationsserver nicht erreichbar: {e}"))?;
    match status {
        200..=299 => serde_json::from_str::<TicketResponse>(&answer)
            .map(|res| res.ticket.into_bytes())
            .map_err(|e| format!("Unerwartete Antwort des Koordinationsservers: {e}")),
        401 => Err("Die Anmeldung ist abgelaufen. Bitte neu anmelden.".into()),
        _ => {
            let reason = serde_json::from_str::<ApiError>(&answer)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("HTTP {status}"));
            Err(format!("Koordinationsserver: {reason}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn target() -> Target {
        Target {
            url: "https://beach.example.com/".into(),
            token: "secret".into(),
            device: "dev1".into(),
        }
    }

    #[test]
    fn asks_for_the_announced_device() {
        let seen = RefCell::new(None);
        let post = |url: &str, token: &str, body: &str| -> Answer {
            *seen.borrow_mut() = Some((url.to_string(), token.to_string(), body.to_string()));
            Ok((200, r#"{"ticket":"abc.def"}"#.into()))
        };
        assert_eq!(ticket(&target(), &post, "dev1", &[0; 32]), Ok(b"abc.def".to_vec()));
        let (url, token, body) = seen.into_inner().unwrap();
        assert_eq!(url, "https://beach.example.com/api/devices/dev1/ticket");
        assert_eq!(token, "secret");
        let req: TicketRequest = serde_json::from_str(&body).unwrap();
        assert_eq!(beach_account::unb64(&req.nonce), Some(vec![0; 32]));
    }

    #[test]
    fn refuses_another_device() {
        let post = |_: &str, _: &str, _: &str| -> Answer { panic!("must not ask") };
        assert!(ticket(&target(), &post, "dev2", &[0; 32]).is_err());
    }

    #[test]
    fn reports_server_errors() {
        let expired = |_: &str, _: &str, _: &str| -> Answer { Ok((401, String::new())) };
        assert!(ticket(&target(), &expired, "dev1", &[0; 32]).unwrap_err().contains("abgelaufen"));
        let denied = |_: &str, _: &str, _: &str| -> Answer { Ok((403, r#"{"error":"Kein Zugriff."}"#.into())) };
        assert_eq!(ticket(&target(), &denied, "dev1", &[0; 32]).unwrap_err(), "Koordinationsserver: Kein Zugriff.");
        let offline = |_: &str, _: &str, _: &str| -> Answer { Err("offline".into()) };
        assert!(ticket(&target(), &offline, "dev1", &[0; 32]).unwrap_err().contains("offline"));
    }
}
