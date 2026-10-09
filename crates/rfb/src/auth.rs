//! Classic VNC authentication (security type 2): DES challenge/response.
//!
//! Weak by modern standards (8-char password, DES). Use it on a trusted network
//! or inside a VPN/SSH tunnel.

use des::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};

pub const CHALLENGE_LEN: usize = 16;

/// Encrypts `challenge` with `password` the way VNC does: the first 8 password
/// bytes, zero padded, with the bit order of every byte reversed, as DES key.
pub fn vnc_response(password: &str, challenge: &[u8; CHALLENGE_LEN]) -> [u8; CHALLENGE_LEN] {
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.as_bytes()) {
        *k = b.reverse_bits();
    }
    let cipher = des::Des::new_from_slice(&key).expect("8-byte key");
    let mut out = *challenge;
    for block in out.chunks_exact_mut(8) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

pub fn random_challenge() -> [u8; CHALLENGE_LEN] {
    rand::random()
}

/// Fixed key `vncpasswd` uses to store passwords (`~/.vnc/passwd`):
/// `{23, 82, 107, 6, 35, 78, 88, 7}`, bit-reversed for a standard DES implementation.
const OBFUSCATION_KEY: [u8; 8] = [0xe8, 0x4a, 0xd6, 0x60, 0xc4, 0x72, 0x1a, 0xe0];

/// Encrypts a password with the fixed `vncpasswd` key, as VNC viewers do when
/// they save passwords. This hides the password from a casual glance only;
/// anyone with the file can reverse it.
pub fn obfuscate_password(password: &str) -> [u8; 8] {
    let mut block = [0u8; 8];
    for (b, p) in block.iter_mut().zip(password.as_bytes()) {
        *b = *p;
    }
    des::Des::new(&OBFUSCATION_KEY.into()).encrypt_block(GenericArray::from_mut_slice(&mut block));
    block
}

/// Reverses [`obfuscate_password`]. Padding zeros are removed; a password that
/// is not valid UTF-8 (the file was not written by a VNC program) comes back lossily.
pub fn deobfuscate_password(stored: &[u8; 8]) -> String {
    use des::cipher::BlockDecrypt;
    let mut block = *stored;
    des::Des::new(&OBFUSCATION_KEY.into()).decrypt_block(GenericArray::from_mut_slice(&mut block));
    let len = block.iter().position(|&b| b == 0).unwrap_or(8);
    String::from_utf8_lossy(&block[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_des() {
        // Reference: `openssl enc -des-ecb -nopad -K 0e86ceceeef64e26`, the
        // bit-reversed key for "password".
        let resp = vnc_response("password", b"0123456789abcdef");
        let expected = [
            0x56, 0x45, 0xab, 0xeb, 0x5f, 0x1e, 0x64, 0x75, 0xe8, 0xfe, 0xb1, 0x1b, 0xeb, 0x66, 0xea, 0x19,
        ];
        assert_eq!(resp, expected);
    }

    #[test]
    fn obfuscation_matches_vncpasswd() {
        // `vncpasswd` output for "password" (also `openssl enc -des-ecb -nopad
        // -K e84ad660c4721ae0`, the bit-reversed fixed key).
        assert_eq!(
            obfuscate_password("password"),
            [0xdb, 0xd8, 0x3c, 0xfd, 0x72, 0x7a, 0x14, 0x58]
        );
        assert_eq!(
            obfuscate_password("abc"),
            [0x9c, 0x0a, 0x17, 0x2d, 0x34, 0x82, 0xe1, 0x22]
        );
    }

    #[test]
    fn obfuscation_round_trips() {
        for pw in ["", "a", "test1234", "geheim", "Käse"] {
            assert_eq!(deobfuscate_password(&obfuscate_password(pw)), pw);
        }
        // Only the first 8 bytes survive, like in VNC authentication.
        assert_eq!(deobfuscate_password(&obfuscate_password("123456789")), "12345678");
    }

    #[test]
    fn only_first_eight_chars_count() {
        let c = random_challenge();
        assert_eq!(vnc_response("12345678", &c), vnc_response("12345678xyz", &c));
        assert_ne!(vnc_response("12345678", &c), vnc_response("1234567", &c));
    }
}
