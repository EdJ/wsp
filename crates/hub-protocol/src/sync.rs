//! The sync verbs: a spoke and the hub sync as git fetches and pushes, and
//! the hub hands out row ids, leases shared rows and routes messages.
//!
//! - `sync.heads { workspace }` → [`Answer::Heads`]: the workspace's [`Refs`].
//! - `sync.have { ids }` → [`Answer::Missing`]: those of `ids` the hub does
//!   not hold, in the order asked.
//! - `sync.put { objects }` → [`Answer::Stored`]: how many were written.
//! - `sync.get { ids }` → [`Answer::Objects`]: those of `ids` the hub holds.
//! - `refs.move { workspace, subject, from, to }` → [`Answer::Move`]:
//!   [`Move::Moved`], or [`Move::Stale`] with the head there is (an answer,
//!   not an `err`).
//! - `ids.alloc { workspace, project, n, past? }` → [`Answer::Ids`].
//! - `lease.take { workspace, row }` and `lease.renew { row, token }` →
//!   [`Answer::Lease`]; `lease.release { row, token }` → [`Answer::Released`].
//! - `msg.send { to, from, seat?, text, re? }` → [`Answer::Queued`];
//!   `msg.take {}` → [`Answer::Messages`]; `msg.ack { ids }` →
//!   [`Answer::Acked`].
//!
//! None carries an idempotency key.

use serde::{Deserialize, Serialize};
use wsp_objects::{ObjectId, WorkspaceId};
use wsp_store_contract::{At, RowId, Subject, Token};

#[cfg(doc)]
use crate::Answer;
#[cfg(doc)]
use wsp_objects::Refs;

/// The sync verbs, as [`SyncCall`] names them on the wire.
pub const SYNC_VERBS: &[&str] = &[
    "sync.heads",
    "sync.have",
    "sync.put",
    "sync.get",
    "refs.move",
    "ids.alloc",
    "lease.take",
    "lease.renew",
    "lease.release",
    "msg.send",
    "msg.take",
    "msg.ack",
];

/// The most ids one `ids.alloc` hands out; more is refused `invalid`.
pub const MAX_IDS: u32 = 1000;

/// The highest `past` an `ids.alloc` may name; higher is refused `invalid`.
pub const MAX_PAST: u64 = 1_000_000_000;

/// The payload field a verdict's or a landing's lease token is in.
pub const FENCE_TOKEN: &str = "token";

/// A sync verb, tagged by `verb`, with its `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args")]
pub enum SyncCall {
    #[serde(rename = "sync.heads")]
    Heads { workspace: WorkspaceId },
    #[serde(rename = "sync.have")]
    Have { ids: Vec<ObjectId> },
    #[serde(rename = "sync.put")]
    Put { objects: Vec<Blob> },
    #[serde(rename = "sync.get")]
    Get { ids: Vec<ObjectId> },
    #[serde(rename = "refs.move")]
    Move(RefMove),
    #[serde(rename = "ids.alloc")]
    Alloc(IdsAlloc),
    #[serde(rename = "lease.take")]
    LeaseTake(LeaseTake),
    #[serde(rename = "lease.renew")]
    LeaseRenew(LeaseHold),
    #[serde(rename = "lease.release")]
    LeaseRelease(LeaseHold),
    #[serde(rename = "msg.send")]
    MsgSend(MsgSend),
    #[serde(rename = "msg.take")]
    MsgTake(#[serde(default)] crate::NoArgs),
    #[serde(rename = "msg.ack")]
    MsgAck { ids: Vec<String> },
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
            SyncCall::Alloc(_) => "ids.alloc",
            SyncCall::LeaseTake(_) => "lease.take",
            SyncCall::LeaseRenew(_) => "lease.renew",
            SyncCall::LeaseRelease(_) => "lease.release",
            SyncCall::MsgSend(_) => "msg.send",
            SyncCall::MsgTake(_) => "msg.take",
            SyncCall::MsgAck { .. } => "msg.ack",
        }
    }
}

/// One object on the wire: its id and its canonical bytes, base64 (RFC 4648,
/// the standard alphabet, padded).
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

/// `ids.alloc`'s `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdsAlloc {
    /// The shared workspace the ids are for.
    pub workspace: WorkspaceId,
    /// The id prefix the rows number under: the counter is this one's.
    pub project: String,
    /// How many, at most [`MAX_IDS`].
    pub n: u32,
    /// The caller's highest number under `project`, at most [`MAX_PAST`]:
    /// every id handed out is above it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub past: Option<u64>,
}

/// `lease.take`'s `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseTake {
    /// The shared workspace the row is in.
    pub workspace: WorkspaceId,
    pub row: RowId,
}

/// `lease.renew`'s and `lease.release`'s `args`: the row and the token its
/// `lease.take` answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseHold {
    pub row: RowId,
    pub token: Token,
}

/// `msg.send`'s `args`: a tell to an address the sending spoke does not hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsgSend {
    /// The address as the sender wrote it: `seat:<row>`, `governor:<list>`,
    /// `primary:<user>`.
    pub to: String,
    /// The sender as an address its own spoke resolves: what a reply names.
    pub from: String,
    /// The sending seat's pane, where a seat sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    pub text: String,
    /// The message this one answers, by the hub's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
}

/// One message the hub routes: what `msg.take` hands a spoke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubMessage {
    /// The hub's id for it, unique on the hub.
    pub id: String,
    /// The address it was sent to.
    pub to: String,
    /// The user it was routed to.
    pub user: String,
    /// The user who sent it.
    pub from_user: String,
    /// The sender as an address on the sender's spoke ([`MsgSend::from`]).
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    /// When the hub queued it.
    pub at: At,
}

/// What `refs.move` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Move {
    /// The head was `from`, and is now `head` (the move's `to`).
    Moved { head: ObjectId },
    /// The head is not `from` but `head` (none: the subject has no head).
    Stale { head: Option<ObjectId> },
}

/// Bytes as base64 text, for `#[serde(with = "base64")]`: padded standard
/// base64 (RFC 4648), written and read here so the crate takes no dependency
/// for it.
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
