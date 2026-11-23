//! The hub's tokens, shared by the hub and the CLI: [`mint`] makes one,
//! [`hash`] is what a token record keeps of it (never the token), and [`same`]
//! compares two without the time saying how much of a guess was right.
//!
//! These are plain functions, not part of any [`Domain`](wsp_store_contract::Domain):
//! `mint` reads the operating system's randomness, which `decide` and `evolve`
//! never do.

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

/// The hex SHA-256 of `token`: what a `hub.token.issue` command and a
/// [`Credential`](wsp_store_contract::Credential) carry.
pub fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// Whether two tokens (or hashes) are equal, comparing every byte whatever the
/// first difference, so the time taken does not say how much of a guess was right.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
