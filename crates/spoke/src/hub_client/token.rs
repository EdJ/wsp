//! A hub token, as `wsp hub user add` and `wsp hub token` make one: [`mint`]
//! makes it, [`hash`] is what the hub's token record keeps of it (never the
//! token). The same as the hub's own `token` (moved here from `wsp-machines`,
//! so the spoke links no machine of the state core for it).

use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::io::Read;

/// Random bytes in a token; the token is twice as many hex characters.
pub const BYTES: usize = 32;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A new token (32 random bytes as 64 hex characters) and its [`hash`].
///
/// Panics if the system has no randomness (`/dev/urandom`): a token made from
/// anything weaker would be worse than none.
pub fn mint() -> (String, String) {
    let mut raw = [0u8; BYTES];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .expect("the system's randomness (/dev/urandom) is readable");
    let token = hex(&raw);
    let h = hash(&token);
    (token, h)
}

/// The hex SHA-256 of `token`: what a `hub.token.issue` command carries.
pub fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_64_hex_characters_and_its_hash_is_its_sha256() {
        let (token, h) = mint();
        assert_eq!(token.len(), 2 * BYTES);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, hash(&token));
        assert_ne!(mint().0, token);
        // The SHA-256 of "abc", as the standard's own example gives it.
        assert_eq!(hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
