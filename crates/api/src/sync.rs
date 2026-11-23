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
//! - `ids.alloc { workspace, project, n, past? }` answers [`Answer::Ids`]
//!   (wsp-185): `n` row ids for `project`, from the hub's one counter for that
//!   project, so two spokes that publish at once get disjoint ids. An id is
//!   never handed out twice; each is above `past` (the caller's highest number
//!   in the project, so none is one it already uses) and above every id handed
//!   out before. The caller must be a member of `workspace`, the shared
//!   workspace it publishes into.
//!
//! - `lease.take { workspace, row }` answers [`Answer::Lease`] (wsp-466): the
//!   hub's lease on a shared row, `{ token, expires }`, for the hub's TTL (7
//!   days by default). The token is the row's fencing token: every take of
//!   the row after a release or an expiry is higher than any before it. The
//!   holder taking it again keeps its token and is renewed. Refused `held {
//!   holder }` while another user holds it unexpired, and `forbidden` when the
//!   caller is no member of `workspace` or the row (or its list) is allocated
//!   to another user (its `for` field, wsp-414).
//! - `lease.renew { row, token }` answers [`Answer::Lease`]: the holder's lease
//!   runs the TTL again from now. Refused `stale` once the lease has expired or
//!   the token is not the current one, `held` when another user holds it,
//!   `not_found` when no one does.
//! - `lease.release { row, token }` answers [`Answer::Released`]: the lease is
//!   let go; one already released or retaken (another token) is answered the
//!   same, as the store's release is.
//!
//! An expired lease is released by the hub, which announces `lease.expired
//! { workspace, row, holder, token }` on its stream.
//!
//! None of them carries an idempotency key: a put is idempotent by its hash
//! and a move by its `from`, so a retry after a lost reply is harmless. An
//! `ids.alloc` retried after a lost reply allocates again; the ids lost are
//! gaps, never reused, which is the counter's whole promise.
//!
//! Which workspaces a principal may read or write is the hub's (wsp-460), not
//! the wire's.

use serde::{Deserialize, Serialize};
use wsp_store_contract::{RowId, Subject, Token};

pub use wsp_objects::{ObjectId, Refs, WorkspaceId};

#[cfg(doc)]
use crate::Answer;

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
];

/// The most ids one `ids.alloc` hands out; more is refused `invalid`.
pub const MAX_IDS: u32 = 1000;

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
    /// Row ids for a project from the hub's counter. Answered [`Answer::Ids`].
    #[serde(rename = "ids.alloc")]
    Alloc(IdsAlloc),
    /// Lease a shared row. Answered [`Answer::Lease`].
    #[serde(rename = "lease.take")]
    LeaseTake(LeaseTake),
    /// Renew the caller's lease. Answered [`Answer::Lease`].
    #[serde(rename = "lease.renew")]
    LeaseRenew(LeaseHold),
    /// Let the caller's lease go. Answered [`Answer::Released`].
    #[serde(rename = "lease.release")]
    LeaseRelease(LeaseHold),
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

/// `ids.alloc`'s `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdsAlloc {
    /// The shared workspace the ids are for; the caller must be its member.
    pub workspace: WorkspaceId,
    /// The id prefix the rows number under (a project's code: `wsp` for
    /// `wsp-185`): the counter is this one's.
    pub project: String,
    /// How many, at most [`MAX_IDS`].
    pub n: u32,
    /// The caller's highest number under `project`: every id handed out is
    /// above it. Left out, nothing is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub past: Option<u64>,
}

impl IdsAlloc {
    /// Why these `args` cannot be answered, or `None` when they can: a
    /// project that is no id prefix (empty, or a character an id cannot
    /// hold), or more than [`MAX_IDS`].
    pub fn invalid(&self) -> Option<String> {
        if self.project.is_empty() || !self.project.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Some(format!("{:?} is no project id prefix: letters, digits, - and _ only", self.project));
        }
        if self.n > MAX_IDS {
            return Some(format!("{} ids asked for, and one ids.alloc hands out at most {MAX_IDS}", self.n));
        }
        None
    }
}

/// `lease.take`'s `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseTake {
    /// The shared workspace the row is in; the caller must be its member.
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

/// The id numbered `n` under `project`, as a store names it (`wsp-007`).
pub fn row_id(project: &str, n: u64) -> String {
    format!("{project}-{n:03}")
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
