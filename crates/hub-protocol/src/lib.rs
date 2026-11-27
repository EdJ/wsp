//! `wsp-hub-protocol`: the hub's wire surface, the contract a spoke and the hub
//! share. Types, their encoding and constants; no logic and no IO.
//!
//! It is exactly what a hub serves and a spoke sends, cut out of `wsp-api`
//! (which is also the local state core's API) with the bytes on the wire
//! unchanged: `tests/wire.rs` encodes every frame here and through `wsp-api`
//! and holds the two to the same bytes, each decoding the other's.
//!
//! **On the wire** a connection carries one [`Frame`] per line of JSON
//! ([`conn`]'s framing rule). A frame is tagged by `type`.
//!
//! - The client's first frame is [`Hello`] (naming the client's
//!   [`Machine`], where it knows it), answered [`Welcome`] (saying, from a
//!   hub that does, whether the user is a hub admin) or [`Refused`]
//!   (and then closed); or [`Login`], answered with
//!   [`LoginPrompt`] and then [`LoginDone`] or [`Refused`], and closed.
//! - After a welcome the client sends [`Request`]s, each answered by one
//!   [`Reply`] with its `id`. After the reply to `subscribe`, the hub also sends
//!   the subscription's [`Frame::Feed`] frames (the contract's [`Feed`]).
//!
//! **The verbs** ([`Call`]):
//!
//! - the reads a hub answers ([`Verb`], [`READ_VERBS`]): `state`, `events`,
//!   `subscribe` and `whoami`;
//! - the identity commands ([`IdentityCommand`], [`COMMAND_VERBS`]): users,
//!   tokens, shared workspaces and members;
//! - the sync verbs ([`SyncCall`], [`SYNC_VERBS`]): objects by hash, refs moved
//!   by compare-and-swap, row ids, leases and messages.
//!
//! A verb in none of these decodes as [`Call::Unknown`], which keeps the
//! request's `id` so it can be answered. The state core's other read verbs
//! and commands are not here: a hub does not serve them.
//!
//! A field added later is `#[serde(default)]` so an older peer's frames still
//! decode; a [`Code`] added later decodes as [`Code::Unknown`]; a change an
//! older peer cannot decode bumps [`PROTOCOL`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod conn;
mod identity;
pub mod principal;
pub mod sync;

pub use conn::{ConnError, Connection, MAX_LINE};
pub use identity::{IdentityCommand, COMMAND_VERBS};
pub use sync::{
    Blob, HubMessage, IdsAlloc, LeaseHold, LeaseTake, Move, MsgSend, RefMove, RefRetire, SyncCall, FENCE_TOKEN, MAX_IDS,
    MAX_PAST, SYNC_VERBS,
};

/// The contract's types the frames carry whole.
pub use wsp_store_contract::{
    At, Event, EventId, Feed, IdempotencyKey, Principal, Refusal, RowId, SharedName, State, Subject, Subscribe,
    Token, TokenId, UserName, BEAT_SECS, MISSED_BEATS,
};
/// The object format's types the sync verbs carry.
pub use wsp_objects::{ObjectId, Refs, WorkspaceId};

/// The protocol's version: the envelope's `v` and both sides' `protocol` in
/// [`Hello`], [`Welcome`] and [`Login`].
pub const PROTOCOL: u32 = 1;

/// The most events one `events` reply carries; a request asking for more, or
/// for no bound, gets this many.
pub const MAX_EVENTS: u32 = 1000;

/// The most bytes of events one `events` reply carries, counted as each
/// event's compact JSON: half of [`MAX_LINE`]. A page stops before the event
/// that would pass it but always carries its first; the reply's `through`
/// says where it stopped.
pub const MAX_EVENTS_BYTES: usize = MAX_LINE / 2;

/// The read verbs a hub answers, as [`Verb`] names them on the wire.
pub const READ_VERBS: &[&str] = &["state", "events", "subscribe", "whoami"];

// ---------------------------------------------------------------- identities

/// A request id, picked by the client and echoed on the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReqId(pub u64);

/// A token: what a [`Hello`] carries and a [`LoginDone`] hands out. Its
/// `Debug` does not print it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiToken(pub String);

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiToken(..)")
    }
}

// ---------------------------------------------------------------- the frame

/// One line on a connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// Client → hub, first on a connection.
    Hello(Hello),
    /// Hub → client: the hello was accepted.
    Welcome(Welcome),
    /// Hub → client: the hello, the login, or a frame out of order was
    /// refused. After a hello or a login the connection closes; a revoked
    /// subscriber is sent one before its connection is cut.
    Refused(Refused),
    /// Client → hub: one verb.
    Request(Request),
    /// Hub → client: the answer to the request with its `id`.
    Reply(Reply),
    /// Hub → client: one frame of the connection's subscription.
    Feed(Feed),
    /// Client → hub, first on a connection in place of `hello`: a login that
    /// ends in a token. The connection carries the login and nothing else.
    Login(Login),
    /// Hub → client, during a login: what the person must do elsewhere.
    LoginPrompt(LoginPrompt),
    /// Hub → client: the login is done; the connection closes after this.
    LoginDone(LoginDone),
}

// ---------------------------------------------------------------- login

/// The one login method a hub knows: GitHub's device flow, run by the hub.
pub const LOGIN_GITHUB: &str = "github";

/// `login { method, client, protocol }`. It carries no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    /// [`LOGIN_GITHUB`].
    pub method: String,
    /// What is calling, for the hub's log.
    pub client: String,
    /// [`PROTOCOL`] as the client was built.
    pub protocol: u32,
}

/// `login_prompt { url, code, expires_secs }`: open `url`, enter `code`,
/// within `expires_secs`. The hub waits on the same connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginPrompt {
    pub url: String,
    pub code: String,
    pub expires_secs: u64,
}

/// `login_done { user, token }`: the user the login named and a token for it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginDone {
    pub user: String,
    pub token: ApiToken,
}

impl std::fmt::Debug for LoginDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginDone").field("user", &self.user).field("token", &"..").finish()
    }
}

// ---------------------------------------------------------------- hello

/// `hello { token, by, client, protocol, machine? }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// The user's token; the hub decides who the connection is from it alone.
    pub token: ApiToken,
    /// Who the client says it is, in the text form: a hint the hub ignores.
    #[serde(with = "principal")]
    pub by: Principal,
    /// What is calling, for the hub's log.
    pub client: String,
    /// [`PROTOCOL`] as the client was built.
    pub protocol: u32,
    /// The machine the client runs on. A hub that keys leases by machine
    /// holds a lease to its user on this machine, and refuses the user's
    /// other machines. Absent from an older client (an additive field): its
    /// leases are its user's, on whichever machine, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<Machine>,
}

/// The machine a spoke runs on, as its [`Hello`] names it (not the state
/// core's executor `Machine`): `machine { id, name }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Machine {
    /// Stable across the client's runs and logins on that machine, and no
    /// other machine's: what a lease is held on. Opaque to the hub; at most
    /// [`MAX_MACHINE`] bytes, no control character.
    pub id: String,
    /// What a person calls the machine (its host name): what a refusal names.
    /// Empty reads as the id. At most [`MAX_MACHINE`] bytes, no control
    /// character.
    #[serde(default)]
    pub name: String,
}

impl Machine {
    /// What a person reads: the name, or the id where there is none.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// The longest [`Machine`] id or name, in bytes.
pub const MAX_MACHINE: usize = 256;

/// `welcome { server, protocol, head, by, machine?, admin? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The instance's name.
    pub server: String,
    /// [`PROTOCOL`] as the hub was built.
    pub protocol: u32,
    /// The id of the last event on the hub's stream.
    pub head: EventId,
    /// Who the connection is, in the text form: every request's `by` must be
    /// it. Absent from a server older than it (an additive field).
    #[serde(default, with = "principal::optional", skip_serializing_if = "Option::is_none")]
    pub by: Option<Principal>,
    /// The machine the hub holds this connection's leases on: the hello's,
    /// as the hub took it. Absent when the hello named none, and from a hub
    /// that does not key leases by machine (an additive field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<Machine>,
    /// Whether the user the connection is from is an active admin of the hub,
    /// as the hub read it at the hello (a change while the connection is open
    /// shows at the next one). `false` for any principal that is not such a
    /// user. Absent from a hub older than it (an additive field), which reads
    /// as not said, never as `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin: Option<bool>,
}

/// `refused { code, text }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub code: Code,
    #[serde(default)]
    pub text: String,
}

// ---------------------------------------------------------------- requests

/// The envelope: `{ v, id, key?, by, verb, args }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// [`PROTOCOL`].
    pub v: u32,
    pub id: ReqId,
    /// The idempotency key, on commands only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<IdempotencyKey>,
    /// Who asks, in the text form; must be the connection's principal.
    #[serde(with = "principal")]
    pub by: Principal,
    /// The verb and its `args`.
    #[serde(flatten)]
    pub call: Call,
}

/// What a request asks: `verb` and `args`, beside the envelope's fields.
///
/// Decoded in this order: a read verb ([`Verb`]), an identity command
/// ([`IdentityCommand`]), a sync verb ([`SyncCall`]), then [`Call::Unknown`],
/// which keeps the request's `id` readable: a verb in [`READ_VERBS`],
/// [`COMMAND_VERBS`] or [`SYNC_VERBS`] whose `args` did not fit, or a verb in
/// none of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Call {
    Verb(Verb),
    Command(IdentityCommand),
    Sync(SyncCall),
    Unknown {
        verb: String,
        #[serde(default)]
        args: Value,
    },
}

/// The read verbs a hub answers, tagged by `verb`, with their `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args", rename_all = "snake_case")]
pub enum Verb {
    /// The hub's whole state and the id it is consistent at.
    State(#[serde(default)] NoArgs),
    /// Events with ids greater than `from`, at most `limit` (never more than
    /// [`MAX_EVENTS`] nor [`MAX_EVENTS_BYTES`]). `limit: 0` is a subscriber's
    /// keep-alive.
    Events {
        from: EventId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// `subscribe { from? }`: from an id, every event after it; absent, a
    /// snapshot first. Answered [`Answer::Subscribed`], then the feed.
    Subscribe(#[serde(default)] Subscribe),
    /// Who this connection is. Answered [`Answer::Whoami`].
    Whoami(#[serde(default)] NoArgs),
}

/// The `args` of a verb that takes none: written `{}`, and read from `{}`,
/// `null` or no `args` at all. Fields a newer peer adds are ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct NoArgs {}

impl<'de> Deserialize<'de> for NoArgs {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Nothing;
        impl<'de> serde::de::Visitor<'de> for Nothing {
            type Value = NoArgs;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("no arguments: none, null or {}")
            }
            fn visit_none<E>(self) -> Result<NoArgs, E> {
                Ok(NoArgs {})
            }
            fn visit_unit<E>(self) -> Result<NoArgs, E> {
                Ok(NoArgs {})
            }
            fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<NoArgs, D::Error> {
                d.deserialize_any(Nothing)
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(self, mut m: M) -> Result<NoArgs, M::Error> {
                while m.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {}
                Ok(NoArgs {})
            }
        }
        // `deserialize_option`, so that `args` left out reads as none rather than failing.
        d.deserialize_option(Nothing)
    }
}

// ---------------------------------------------------------------- replies

/// The reply: `{ id, ok | err, events }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    /// The request's id.
    pub id: ReqId,
    /// `ok` with the answer, or `err` with why not.
    #[serde(flatten)]
    pub outcome: Outcome,
    /// The ids of the events the request wrote, in commit order; empty for a
    /// read and for a refusal.
    #[serde(default)]
    pub events: Vec<EventId>,
}

/// `ok` or `err`, beside a reply's other fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Answer),
    Err(Failure),
}

/// What `ok` carries: one answer per verb, tagged by its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// `state`: the snapshot and the id it is consistent at.
    State { state: State, at: EventId },
    /// `events`: the events, the head when they were read, and where the page
    /// stopped (absent from an older server: 0).
    Events {
        events: Vec<Event>,
        head: EventId,
        #[serde(default)]
        through: EventId,
    },
    /// `subscribe`: the feed follows on this connection.
    Subscribed,
    /// An identity command: it was applied, and its events are the reply's `events`.
    Applied,
    /// `whoami`: who the connection is, in the text form, and the instance.
    Whoami {
        #[serde(with = "principal")]
        by: Principal,
        server: String,
    },
    /// `sync.heads`: the workspace's refs, each subject's head.
    Heads(Refs),
    /// `sync.have`: the ids asked about that the hub does not hold (or does not serve).
    Missing(Vec<ObjectId>),
    /// `sync.put`: how many of the objects were written.
    Stored { stored: u32 },
    /// `sync.get`: the objects asked for that the hub holds and serves.
    Objects(Vec<Blob>),
    /// `refs.move`: moved, or stale with the head there is.
    Move(Move),
    /// `ids.alloc`: the ids handed out, in increasing order.
    Ids(Vec<String>),
    /// `lease.take` and `lease.renew`: the row's fencing token and when the
    /// lease expires, on the hub's clock.
    Lease { token: Token, expires: At },
    /// `lease.release`: the lease is not the caller's any more.
    Released,
    /// `msg.send`: the message's id on the hub and the user it was routed to.
    Queued { id: String, user: String },
    /// `msg.take`: the messages queued for the caller, oldest first.
    Messages(Vec<HubMessage>),
    /// `msg.ack`: how many of the ids left the caller's queue.
    Acked { acked: u32 },
    /// `refs.retire`: `subject` is retired, continuing as `successor` (none:
    /// gone). A repeat, or a retire of a subject retired before, answers the
    /// successor it was retired with. Only `refs.retire` answers it, which an
    /// older client never sends.
    Retired {
        subject: Subject,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        successor: Option<Subject>,
    },
}

/// What `err` carries: `{ code, text }`, and the contract's refusal whole
/// when there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: Code,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

/// An error's code. The first eight are the contract's [`Refusal`] codes,
/// spelled as its `code` tag; the rest are the protocol's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    // The contract's refusals.
    Stale,
    TooOld,
    Conflict,
    Held,
    NotFound,
    Illegal,
    Forbidden,
    Invalid,
    // The protocol's own.
    /// A line or a request that did not decode, a known verb whose `args` did
    /// not fit, or a frame out of order.
    Malformed,
    /// The hello's token names nobody, or the token was revoked.
    Unauthorised,
    /// The peer speaks a [`PROTOCOL`] (or a `v`) this side does not.
    Protocol,
    /// A verb this hub does not run.
    Unsupported,
    /// The store failed; a retry may pass.
    Backend,
    /// A verdict or a landing under a lease token that is not the row's
    /// current one. Carries no contract refusal; nothing was written.
    Fenced,
    /// The reply would be a line longer than [`MAX_LINE`].
    TooLarge,
    /// A code this build does not know, from a newer peer.
    #[serde(other)]
    Unknown,
}
