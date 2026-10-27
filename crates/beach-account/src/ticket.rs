//! Connection tickets: `b64(claims JSON) "." b64(Ed25519 signature)`, signed
//! by the coordination server over the JSON bytes.
//!
//! A ticket names one device and one handshake nonce and lives
//! [`LIFETIME`] seconds, so it can't be replayed against another session.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use rfb::handshake::{Grant, TicketVerifier, TICKET_NONCE_LEN};
use serde::{Deserialize, Serialize};

use crate::{b64, unb64};

/// Seconds a ticket is valid.
pub const LIFETIME: i64 = 60;
/// Clock difference between coordination server and device we tolerate.
pub const SKEW: i64 = 300;
const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    pub v: u8,
    /// Device ID.
    pub dev: String,
    /// User ID and display name.
    pub sub: String,
    pub name: String,
    /// The handshake nonce, base64.
    pub nonce: String,
    /// Issued and expiry, Unix seconds.
    pub iat: i64,
    pub exp: i64,
    #[serde(default)]
    pub view_only: bool,
}

impl Claims {
    pub fn new(device: &str, user_id: &str, user_name: &str, nonce: &[u8], now: i64) -> Claims {
        Claims {
            v: VERSION,
            dev: device.into(),
            sub: user_id.into(),
            name: user_name.into(),
            nonce: b64(nonce),
            iat: now,
            exp: now + LIFETIME,
            view_only: false,
        }
    }
}

pub fn sign(key: &SigningKey, claims: &Claims) -> String {
    let json = serde_json::to_vec(claims).expect("claims serialize");
    let signature = key.sign(&json);
    format!("{}.{}", b64(&json), b64(&signature.to_bytes()))
}

/// Checks signature, device, nonce and validity period.
pub fn verify(
    key: &VerifyingKey,
    ticket: &[u8],
    device: &str,
    nonce: &[u8],
    now: i64,
) -> Result<Claims, String> {
    let invalid = || "invalid ticket".to_string();
    let text = std::str::from_utf8(ticket).map_err(|_| invalid())?;
    let (payload, signature) = text.split_once('.').ok_or_else(invalid)?;
    let json = unb64(payload).ok_or_else(invalid)?;
    let signature: [u8; 64] = unb64(signature)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(invalid)?;
    key.verify_strict(&json, &Signature::from_bytes(&signature))
        .map_err(|_| "ticket signature does not match this server's coordination server".to_string())?;
    let claims: Claims = serde_json::from_slice(&json).map_err(|_| invalid())?;
    if claims.v != VERSION {
        return Err(format!("unsupported ticket version {}", claims.v));
    }
    if claims.dev != device {
        return Err("ticket is for another device".into());
    }
    if unb64(&claims.nonce).as_deref() != Some(nonce) {
        return Err("ticket is for another session".into());
    }
    if claims.exp - claims.iat > LIFETIME || claims.iat > now + SKEW || claims.exp < now - SKEW {
        return Err("ticket expired".into());
    }
    Ok(claims)
}

/// The server side: accepts tickets for `device` signed with `key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verifier {
    pub device: String,
    pub key: VerifyingKey,
}

impl TicketVerifier for Verifier {
    fn device(&self) -> &str {
        &self.device
    }

    fn verify(&self, nonce: &[u8; TICKET_NONCE_LEN], ticket: &[u8]) -> Result<Grant, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        let claims = verify(&self.key, ticket, &self.device, nonce, now)?;
        Ok(Grant {
            user: claims.name,
            view_only: claims.view_only,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[1; 32])
    }

    fn ticket() -> String {
        sign(&key(), &Claims::new("dev-1", "u1", "Alice", &[9; 32], NOW))
    }

    #[test]
    fn accepts_a_fresh_ticket() {
        let claims = verify(
            &key().verifying_key(),
            ticket().as_bytes(),
            "dev-1",
            &[9; 32],
            NOW + 5,
        )
        .unwrap();
        assert_eq!(claims.name, "Alice");
        assert!(!claims.view_only);
    }

    #[test]
    fn rejects_other_key_device_nonce_and_age() {
        let other = SigningKey::from_bytes(&[2; 32]).verifying_key();
        let good = key().verifying_key();
        let t = ticket();
        assert!(verify(&other, t.as_bytes(), "dev-1", &[9; 32], NOW).is_err());
        assert_eq!(
            verify(&good, t.as_bytes(), "dev-2", &[9; 32], NOW).unwrap_err(),
            "ticket is for another device"
        );
        assert_eq!(
            verify(&good, t.as_bytes(), "dev-1", &[8; 32], NOW).unwrap_err(),
            "ticket is for another session"
        );
        assert_eq!(
            verify(&good, t.as_bytes(), "dev-1", &[9; 32], NOW + LIFETIME + SKEW + 1).unwrap_err(),
            "ticket expired"
        );
    }

    #[test]
    fn rejects_tampered_claims() {
        let t = ticket();
        let (_, signature) = t.split_once('.').unwrap();
        let mut claims = Claims::new("dev-1", "u1", "Alice", &[9; 32], NOW);
        claims.view_only = true;
        let forged = format!("{}.{signature}", b64(&serde_json::to_vec(&claims).unwrap()));
        assert!(verify(&key().verifying_key(), forged.as_bytes(), "dev-1", &[9; 32], NOW).is_err());
        assert!(verify(&key().verifying_key(), b"garbage", "dev-1", &[9; 32], NOW).is_err());
    }

    #[test]
    fn rejects_long_lived_tickets() {
        let mut claims = Claims::new("dev-1", "u1", "Alice", &[9; 32], NOW);
        claims.exp = NOW + 86_400;
        let t = sign(&key(), &claims);
        assert!(verify(&key().verifying_key(), t.as_bytes(), "dev-1", &[9; 32], NOW).is_err());
    }
}
