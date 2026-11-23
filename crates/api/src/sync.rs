//! The sync verbs (wsp-459; the multiplayer design's "Hash-based sync, and why
//! it scales"): a spoke and the hub sync as git fetches and pushes. Compare
//! heads, send only the objects the other side lacks, then move a ref only from
//! the head the writer saw.
//!
//! - `sync.heads { workspace }` answers [`Answer::Heads`]: the workspace's
//!   [`Refs`], each subject's head.
//! - `sync.have { ids }` answers [`Answer::Missing`]: those of `ids` the hub
//!   does not hold, in the order asked.
//! - `sync.put { objects }` answers [`Answer::Stored`]: each object's bytes
//!   (base64 on the wire) are held to its id ([`wsp_objects::verify`]) before
//!   any is kept, and an id already held is not written again, so a second put
//!   of the same object stores nothing. One that fails the check refuses the
//!   whole put `invalid`, naming it.
//! - `sync.get { ids }` answers [`Answer::Objects`]: those of `ids` the hub
//!   holds, with their bytes; one it lacks is left out.
//! - `refs.move { workspace, subject, from, to }` answers [`Answer::Move`]:
//!   [`Move::Moved`] when the subject's head was `from` (none for a subject
//!   with no head yet) and is now `to`, or [`Move::Stale`] with the head it
//!   has when that is not `from`. A stale move is an answer, not an `err`: the
//!   request was understood and the writer has a reconcile to do (wsp-176),
//!   with the head to do it from.
//!
//! None of them carries an idempotency key: a put is idempotent by its hash
//! and a move by its `from`, so a retry after a lost reply is harmless.
//!
//! Which workspaces a principal may read or write is the hub's (wsp-460), not
//! the wire's.

use serde::{Deserialize, Serialize};
use wsp_store_contract::Subject;

pub use wsp_objects::{ObjectId, Refs, WorkspaceId};

#[cfg(doc)]
use crate::Answer;

/// The sync verbs, as [`SyncCall`] names them on the wire.
pub const SYNC_VERBS: &[&str] = &["sync.heads", "sync.have", "sync.put", "sync.get", "refs.move"];

/// A sync verb, tagged by `verb`, with its `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args")]
pub enum SyncCall {
    /// Each subject's head in `workspace`. Answered [`Answer::Heads`].
    #[serde(rename = "sync.heads")]
    Heads { workspace: WorkspaceId },
    /// Which of `ids` the hub lacks. Answered [`Answer::Missing`].
    #[serde(rename = "sync.have")]
    Have { ids: Vec<ObjectId> },
    /// Keep these objects. Answered [`Answer::Stored`].
    #[serde(rename = "sync.put")]
    Put { objects: Vec<Blob> },
    /// Hand these objects over. Answered [`Answer::Objects`].
    #[serde(rename = "sync.get")]
    Get { ids: Vec<ObjectId> },
    /// Move a subject's head from the one the writer saw. Answered
    /// [`Answer::Move`].
    #[serde(rename = "refs.move")]
    Move(RefMove),
}

impl SyncCall {
    /// The verb's name on the wire, one of [`SYNC_VERBS`].
    pub fn verb(&self) -> &'static str {
        match self {
            SyncCall::Heads { .. } => "sync.heads",
            SyncCall::Have { .. } => "sync.have",
            SyncCall::Put { .. } => "sync.put",
            SyncCall::Get { .. } => "sync.get",
            SyncCall::Move(_) => "refs.move",
        }
    }
}

/// One object on the wire: its id and its canonical bytes, base64 (RFC 4648,
/// the standard alphabet, padded) so that a schema-free store hands on exactly
/// the bytes it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub id: ObjectId,
    #[serde(with = "base64")]
    pub bytes: Vec<u8>,
}

/// `refs.move`'s `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefMove {
    pub workspace: WorkspaceId,
    pub subject: Subject,
    /// The head the writer saw: none for a subject it saw no head for.
    pub from: Option<ObjectId>,
    pub to: ObjectId,
}

/// What `refs.move` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Move {
    /// The head was `from`, and is now `head` (the move's `to`).
    Moved { head: ObjectId },
    /// The head is not `from` but `head` (none: the subject has no head), and
    /// was left as it is.
    Stale { head: Option<ObjectId> },
}

/// Bytes as base64 text, for `#[serde(with = "base64")]`; [`base64::encode`]
/// and [`base64::decode`] for anyone else. Written here rather than taken from
/// a crate, since this crate takes no dependency for it.
pub mod base64 {
    use serde::{Deserialize, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// `bytes` as padded standard base64.
    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let at = |i: usize| chunk.get(i).copied().unwrap_or(0) as u32;
            let n = (at(0) << 16) | (at(1) << 8) | at(2);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Padded standard base64 read back; anything else is refused with why.
    pub fn decode(text: &str) -> Result<Vec<u8>, String> {
        let s = text.as_bytes();
        if !s.len().is_multiple_of(4) {
            return Err(format!("base64 of {} characters, not a multiple of 4", s.len()));
        }
        let value = |c: u8| match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        };
        let quads = s.len() / 4;
        let mut out = Vec::with_capacity(quads * 3);
        for (q, chunk) in s.chunks(4).enumerate() {
            let pad = chunk.iter().rev().take_while(|c| **c == b'=').count();
            if pad > 2 || (pad > 0 && q + 1 != quads) {
                return Err("base64 padding before the end".into());
            }
            let mut n = 0u32;
            for (i, c) in chunk.iter().enumerate() {
                let v = if i >= 4 - pad { 0 } else { value(*c).ok_or_else(|| format!("{:?} is not base64", *c as char))? };
                n = (n << 6) | v as u32;
            }
            out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8][..3 - pad]);
        }
        Ok(out)
    }

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        decode(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::base64::{decode, encode};

    #[test]
    fn base64_is_rfc_4648s() {
        // RFC 4648 section 10's vectors.
        for (raw, text) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(raw.as_bytes()), text);
            assert_eq!(decode(text).unwrap(), raw.as_bytes());
        }
        let every: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&every)).unwrap(), every);
        for bad in ["Zg=", "Z===", "Zg==Zm9v", "Zm9!", "Zm=v"] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }
}
