//! `wsp-api`: the state core's API on the wire (docs/state-core.md sections 4
//! and 6): the envelope of 4.1, the verbs of 4.2, subscribe's frames (4.3),
//! `hello` and its token, where the server listens, a [`Principal`]'s text
//! form, and the [`Connection`] trait.
//!
//! A contract crate under `docs/components.md` R2: types, a trait and
//! constants, and no IO. It names nothing of the workspace but
//! `wsp-store-contract`, whose types (rows, seats, events, commands, refusals,
//! `Feed`) it carries. Every change to this crate is a change to the contract
//! (R5): a row of its own. A field added later is `#[serde(default)]` so that
//! an older peer's frames still decode; a code added later decodes as
//! [`Code::Unknown`]; a change an older peer cannot decode bumps [`PROTOCOL`].
//!
//! **On the wire** a connection carries one [`Frame`] per line of JSON. A frame
//! is tagged by `type` (`hello`, `welcome`, `refused`, `request`, `reply`,
//! `feed`), with its fields beside the tag. A request is 4.1's envelope:
//!
//! ```text
//! {"type":"request","v":1,"id":7,"key":"…","by":"seat:cpd-3698","verb":"row.claim","args":{…}}
//! {"type":"reply","id":7,"ok":"applied","events":[412]}
//! {"type":"reply","id":8,"err":{"code":"unsupported","text":"…"},"events":[]}
//! ```
//!
//! **The order on a connection.** The client's first frame is [`Hello`],
//! answered [`Welcome`] or [`Refused`] (and then closed), as the manager's is
//! (agent-manager 3.3). After it the client sends [`Request`]s, each answered by
//! one [`Reply`] with the same `id`. After the reply to `subscribe`, the server
//! also sends the subscription's frames ([`Frame::Feed`], the contract's
//! [`Feed`]): a snapshot first when `from` was absent, then every event in
//! commit order, and a beat every [`BEAT_SECS`]. A connection holds at most one
//! subscription, so a feed frame names no request.
//!
//! **Where and with what.** One token per instance, in `$WSP_STATE/`[`TOKEN_FILE`]
//! (mode [`TOKEN_MODE`]). The server writes where it listens to
//! `$WSP_STATE/`[`LISTEN_FILE`] ([`Listen`]) and clients read it. That file
//! stands for 4.1's `api.sock`: per D19 the API is TCP on loopback and the
//! tailnet from the start, with no unix socket.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod conn;
pub mod principal;

#[cfg(any(test, feature = "test-support"))]
pub use conn::{pair, Memory};
pub use conn::{ConnError, Connection, MAX_LINE};

pub use wsp_store_contract::{
    Applied, Ask, AskKind, AskState, Command, Event, EventId, Feed, IdempotencyKey, List, ListId,
    Machine, Order, OrderState, Principal, Refusal, Row, RowFilter, RowId, Seat, SeatFilter,
    SeatId, State, Subscribe, BEAT_SECS, MISSED_BEATS,
};

/// The protocol's version: the envelope's `v` and both sides' `protocol` in
/// [`Hello`] and [`Welcome`]. A change that an older peer cannot decode bumps
/// it; an additive field does not.
pub const PROTOCOL: u32 = 1;

/// The token's file under `$WSP_STATE`: one token per instance, which every
/// client's [`Hello`] carries.
pub const TOKEN_FILE: &str = "api.token";

/// The token file's mode.
pub const TOKEN_MODE: u32 = 0o600;

/// Where the server listens, under `$WSP_STATE`, as a [`Listen`]: written by the
/// server when it binds, read by clients. It stands for 4.1's `api.sock` (D19).
pub const LISTEN_FILE: &str = "api.json";

/// The most events one `events` reply carries; a request asking for more, or
/// for no bound, gets this many.
pub const MAX_EVENTS: u32 = 1000;

/// The read verbs of 4.2 and `subscribe`, as [`Verb`] names them on the wire.
pub const READ_VERBS: &[&str] = &[
    "state",
    "row",
    "rows",
    "list",
    "lists",
    "seat",
    "seats",
    "orders",
    "asks",
    "machines",
    "events",
    "subscribe",
    "wip",
];

/// The command verbs, as the contract's [`Command`] names them on the wire: the
/// 19 of 4.2, step 1's `mirror`, and step 2's orders machine (`order.deliver`
/// and the executor's answers, wsp-117). A server that does not run one yet answers
/// it [`Code::Unsupported`]; a verb in neither table is unknown to this build.
pub const COMMAND_VERBS: &[&str] = &[
    "row.add",
    "row.edit",
    "row.claim",
    "row.review",
    "row.block",
    "row.park",
    "row.land",
    "list.go",
    "list.hold",
    "list.release",
    "seat.spawn",
    "seat.end",
    "seat.despawn",
    "order.send",
    "order.cancel",
    "ask.raise",
    "ask.answer",
    "ask.ack",
    "machine.set",
    "mirror",
    "order.deliver",
    "order.delivered",
    "order.acked",
    "order.refused",
    "order.done",
];

// ---------------------------------------------------------------- identities

/// A request id, picked by the client and echoed on the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReqId(pub u64);

/// The instance's API token (the contents of [`TOKEN_FILE`]). A client's
/// [`Hello`] must carry it. Its `Debug` does not print it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiToken(pub String);

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiToken(..)")
    }
}

/// [`LISTEN_FILE`]'s contents: every address the server listens on
/// (`host:port`), loopback first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listen {
    pub addrs: Vec<String>,
}

// ---------------------------------------------------------------- the frame

/// One line on a connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// Client → server, first on every connection.
    Hello(Hello),
    /// Server → client: the hello was accepted.
    Welcome(Welcome),
    /// Server → client: the hello (or a frame before it) was refused; the
    /// connection closes after this.
    Refused(Refused),
    /// Client → server: one verb.
    Request(Request),
    /// Server → client: the answer to the request with its `id`.
    Reply(Reply),
    /// Server → client: one frame of the connection's subscription.
    Feed(Feed),
}

// ---------------------------------------------------------------- hello

/// `hello { token, by, client, protocol }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// The instance's token. A connection without the right one is refused
    /// [`Code::Unauthorised`] and closed.
    pub token: ApiToken,
    /// Who the connection acts for, in the text form; every request's `by` is
    /// checked against it (4.1).
    #[serde(with = "principal")]
    pub by: Principal,
    /// What is calling, for the server's log (`wsp 0.1.0`, `web-chrome`).
    pub client: String,
    /// [`PROTOCOL`] as the client was built.
    pub protocol: u32,
}

/// `welcome { server, protocol, head }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The instance the server serves (the name in `wsp:<instance>`).
    pub server: String,
    /// [`PROTOCOL`] as the server was built.
    pub protocol: u32,
    /// The id of the last committed event.
    pub head: EventId,
}

/// `refused { code, text }`: the answer to a hello the server will not take
/// ([`Code::Unauthorised`], [`Code::Protocol`]) or to a first frame that is no
/// hello ([`Code::Malformed`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub code: Code,
    #[serde(default)]
    pub text: String,
}

// ---------------------------------------------------------------- requests

/// 4.1's envelope: `{ v, id, key?, by, verb, args }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// [`PROTOCOL`].
    pub v: u32,
    pub id: ReqId,
    /// The idempotency key, on commands only: a retry after a lost reply sends
    /// the same key and gets the first result (2.2). A command without one is
    /// refused [`Code::Invalid`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<IdempotencyKey>,
    /// Who asks, in the text form (`seat:cpd-3698`); checked against the
    /// connection's hello.
    #[serde(with = "principal")]
    pub by: Principal,
    /// The verb and its `args`.
    #[serde(flatten)]
    pub call: Call,
}

/// What a request asks: `verb` and `args`, beside the envelope's fields.
///
/// Decoded in this order: a read verb ([`Verb`]), then a command ([`Command`]),
/// then [`Call::Unknown`], which keeps the request's `id` readable so the
/// server can answer it: [`Code::Malformed`] when the verb is in
/// [`READ_VERBS`] or [`COMMAND_VERBS`] (its `args` did not fit), and
/// [`Code::Unsupported`] when it is in neither.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Call {
    Verb(Verb),
    Command(Command),
    Unknown {
        verb: String,
        #[serde(default)]
        args: Value,
    },
}

/// The read verbs of 4.2 and `subscribe` (4.3), tagged by `verb`, with their
/// `args`. A verb with no arguments ([`NoArgs`]) reads `args` left out,
/// `null` or `{}`; any other verb needs its `args`, `{}` when every one has a
/// default (a filter, `subscribe`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args", rename_all = "snake_case")]
pub enum Verb {
    /// The whole domain and the id it is consistent at.
    State(#[serde(default)] NoArgs),
    Row {
        row: RowId,
    },
    Rows(#[serde(default)] RowFilter),
    List {
        list: ListId,
    },
    Lists(#[serde(default)] NoArgs),
    Seat {
        seat: SeatId,
    },
    Seats(#[serde(default)] SeatFilter),
    /// Orders, of one seat when `seat` is named, in any of `states` (all when
    /// empty).
    Orders {
        #[serde(default)]
        seat: Option<SeatId>,
        #[serde(default)]
        states: Vec<OrderState>,
    },
    /// Asks, notes and flags of any of `kinds` in any of `states` (all when
    /// empty).
    Asks {
        #[serde(default)]
        kinds: Vec<AskKind>,
        #[serde(default)]
        states: Vec<AskState>,
    },
    Machines(#[serde(default)] NoArgs),
    /// Events with ids greater than `from`, in commit order, at most `limit`
    /// and never more than [`MAX_EVENTS`]. Refused `too_old` when `from` is
    /// before what retention keeps (4.3).
    Events {
        from: EventId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// `subscribe { from? }` (4.3): from an id, every event after it; absent,
    /// a snapshot first. Answered [`Answer::Subscribed`], then the feed.
    Subscribe(#[serde(default)] Subscribe),
    /// What `wip` draws of `state` (wsp-695), as the store's
    /// `Reads::wip_snapshot` says: open rows whole, the rest as heads, lists
    /// whole but finished ones no seat still sits on without their text, only
    /// raised asks, the seats and claims; small enough
    /// for one line under [`MAX_LINE`] where `state`'s whole domain is not.
    Wip(#[serde(default)] NoArgs),
}

/// The `args` of a verb that takes none: written `{}`, and read from `{}`,
/// `null` or no `args` at all. Fields a newer peer adds are ignored, as any
/// struct's are.
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
                while m
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(NoArgs {})
            }
        }
        // `deserialize_option`, so that `args` left out (serde's missing
        // field) reads as none rather than failing.
        d.deserialize_option(Nothing)
    }
}

// ---------------------------------------------------------------- replies

/// 4.1's reply: `{ id, ok | err, events }`.
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
    State {
        state: State,
        at: EventId,
    },
    /// `row`: the row, or `null` when there is none.
    Row(Option<Row>),
    Rows(Vec<Row>),
    List(Option<List>),
    Lists(Vec<List>),
    Seat(Option<Seat>),
    Seats(Vec<Seat>),
    Orders(Vec<Order>),
    Asks(Vec<Ask>),
    Machines(Vec<Machine>),
    /// `events`: the events, and the head when they were read, so a reader
    /// knows whether to ask again from the last.
    Events {
        events: Vec<Event>,
        head: EventId,
    },
    /// `subscribe`: the feed follows on this connection.
    Subscribed,
    /// A command: it was applied, and its events are the reply's `events`.
    Applied,
    /// `wip`: the part of the snapshot `wip` draws, and the id it is
    /// consistent at.
    Wip {
        state: State,
        at: EventId,
    },
}

/// What `err` carries: `{ code, text }` (4.1), and the contract's refusal
/// whole when the code is one (so `too_old`'s `oldest` is read, not parsed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: Code,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

/// An error's code. The first eight are the contract's [`Refusal`] codes,
/// spelled as its `code` tag; the rest are the API's own.
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
    // The API's own.
    /// A line or a request that did not decode, a known verb whose `args` did
    /// not fit, or a frame out of order (a request before the hello).
    Malformed,
    /// The hello's token was wrong; the connection closes after this.
    Unauthorised,
    /// The peer speaks a [`PROTOCOL`] (or a `v`) this server does not.
    Protocol,
    /// A verb this server does not run: a command at step 1, or a verb it does
    /// not know.
    Unsupported,
    /// The store failed (the contract's `Error::Backend`); a retry may pass.
    Backend,
    /// A code this build does not know, from a newer peer.
    #[serde(other)]
    Unknown,
}
