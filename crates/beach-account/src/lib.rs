//! VNC Beach accounts, shared by the coordination server (`beach-coord`),
//! the server and the viewers.
//!
//! The coordination server keeps users, organizations and the devices they
//! registered. It never carries screen data: a viewer asks it for a short
//! [`ticket`] bound to the nonce the server sent in the RFB handshake
//! (security type [`rfb::handshake::SECURITY_BEACH_TICKET`]), and the server
//! checks the signature against the coordination server's public key, which
//! it stored when it was registered.

pub mod api;
pub mod ticket;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

pub use ed25519_dalek::{SigningKey, VerifyingKey};

/// Unpadded URL-safe base64, the encoding of every binary value in the API.
pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text.trim()).ok()
}

/// The PKCE-style challenge for the app login: `b64(sha256(verifier))`.
pub fn challenge(verifier: &str) -> String {
    b64(&Sha256::digest(verifier.as_bytes()))
}

pub fn encode_key(key: &VerifyingKey) -> String {
    b64(key.as_bytes())
}

pub fn decode_key(text: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = unb64(text)?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let key = SigningKey::from_bytes(&[7; 32]).verifying_key();
        assert_eq!(decode_key(&encode_key(&key)), Some(key));
        assert_eq!(decode_key("not a key"), None);
    }

    #[test]
    fn challenge_matches_rfc7636_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
