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
//! **Verdicts and landings** (wsp-468). A shared row's verdict (`row.reviewed`)
//! and landing (`row.landed`) are pushed as `Event` objects ([`fenced_event`])
//! on the subject `lease:row:<row>`, each carrying the row's lease token in its
//! payload's `token`. A `sync.put` of one, or a `refs.move` to one, is refused
//! `fenced` ([`Code::Fenced`](crate::Code::Fenced)) when that token is not the
//! row's current one (the lease expired, or was taken by another user since),
//! or the caller is not its holder; nothing is written, so the row stays as its
//! current holder left it. An object with no `token` is refused the same.
//!
//! An expired lease is released by the hub, which announces `lease.expired
//! { workspace, row, holder, token }` on its stream.
//!
//! **Messages** (wsp-470; the design's "Message routing and the user
//! primary"). A `wsp tell` to an address the spoke cannot resolve on its own
//! machine goes to the hub as a [`MsgSend`]:
//!
//! - `msg.send { to, from, seat?, text, re? }` answers [`Answer::Queued`]: the
//!   hub resolves `to` to a user, `seat:<row>` to the holder of the row's
//!   lease, `governor:<list>` to the holder of the list's drive lease
//!   (`drive:<list>`), `primary:<user>` to that user, and queues the message
//!   for that user's spokes, announcing it on the stream as `order.queued`
//!   on the subject `user <user>` ([`msg_subject`]), in a shared workspace the
//!   user is a member of, with `{ user, id }` and never the text. A message
//!   whose `re` names one the hub delivered to the caller goes back to that
//!   one's sender whatever `to` resolves to, so a reply needs no lease. An
//!   address the hub cannot resolve is refused `not_found`; one in no
//!   workspace the caller is a member of, `forbidden`.
//! - `msg.take {}` answers [`Answer::Messages`]: every [`HubMessage`] queued
//!   for the caller's user and not yet acknowledged, oldest first. The first
//!   spoke of the user to take and acknowledge one has it: the hub keys a
//!   lease on its holder's user (wsp-466), so a user is what it routes to.
//! - `msg.ack { ids }` answers [`Answer::Acked`]: those messages leave the
//!   caller's queue, and no later `msg.take` hands them out again. An id not
//!   queued for the caller is no error and acks nothing.
//!
//! None of them carries an idempotency key: a put is idempotent by its hash
//! and a move by its `from`, so a retry after a lost reply is harmless. An
//! `ids.alloc` retried after a lost reply allocates again; the ids lost are
//! gaps, never reused, which is the counter's whole promise.
//!
//! Which workspaces a principal may read or write is the hub's (wsp-460), not
//! the wire's.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wsp_objects::Object;
use wsp_store_contract::{At, EventKind, LeaseName, Principal, RowId, Subject, Token};

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
    "msg.send",
    "msg.take",
    "msg.ack",
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
    /// Route a tell to another user's spoke. Answered [`Answer::Queued`].
    #[serde(rename = "msg.send")]
    MsgSend(MsgSend),
    /// The messages queued for the caller. Answered [`Answer::Messages`].
    #[serde(rename = "msg.take")]
    MsgTake(#[serde(default)] crate::NoArgs),
    /// Messages handled: never handed out again. Answered [`Answer::Acked`].
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

/// `msg.send`'s `args`: a tell to an address the sending spoke does not hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsgSend {
    /// The address as the sender wrote it: `seat:<row>`, `governor:<list>`,
    /// `primary:<user>`.
    pub to: String,
    /// The sender as an address its own spoke resolves (`seat:<row>`,
    /// `governor:<list>`, `primary`), or `person` from a shell: what a reply
    /// names.
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
    /// The hub's id for it (`hm-<n>`), unique on the hub.
    pub id: String,
    /// The address it was sent to, which the receiving spoke resolves to a seat.
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

/// The subject a queued message is announced on: the user it is queued for.
/// A user is no record a subscriber holds, so the announcement goes out as it
/// is to every subscriber of its workspace, and names only the user and the
/// message's id.
pub fn msg_subject(user: &str) -> Subject {
    Subject::User(wsp_store_contract::UserName(user.to_string()))
}

/// The payload field a verdict's or landing's lease token is in.
pub const FENCE_TOKEN: &str = "token";

/// The subject a row's verdicts and landings are chained on: the row's lease.
pub fn fence_subject(row: &RowId) -> Subject {
    Subject::Lease(LeaseName(format!("row:{}", row.0)))
}

/// A verdict (`row.reviewed`) or a landing (`row.landed`) of `row` as an
/// object under the lease `token`: `payload`'s fields (the verdict, the commit
/// on the remote) with the token added. `parent` is the head of
/// [`fence_subject`] the writer saw.
pub fn fenced_event(
    workspace: WorkspaceId,
    row: &RowId,
    kind: EventKind,
    payload: Value,
    token: Token,
    parent: Option<ObjectId>,
    by: Principal,
    at: At,
) -> Object {
    let mut payload = match payload {
        Value::Object(o) => Value::Object(o),
        Value::Null => json!({}),
        other => json!({ "value": other }),
    };
    payload[FENCE_TOKEN] = json!(token.0);
    Object::Event { workspace, subject: fence_subject(row), kind, payload, parent, by, at }
}

/// The row an object is a verdict or a landing of, and the token it carries
/// (none when it names none): `Some` for a `row.reviewed` or `row.landed` event
/// on a row or on its lease, which the hub holds to the row's current token.
pub fn fenced_by(object: &Object) -> Option<(RowId, Option<Token>)> {
    let Object::Event { subject, kind, payload, .. } = object else { return None };
    if !matches!(kind, EventKind::RowReviewed | EventKind::RowLanded) {
        return None;
    }
    let row = match subject {
        Subject::Row(row) => row.clone(),
        Subject::Lease(name) => RowId(name.0.strip_prefix("row:")?.to_string()),
        _ => return None,
    };
    Some((row, payload.get(FENCE_TOKEN).and_then(Value::as_u64).map(Token)))
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
