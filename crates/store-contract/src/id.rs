//! Ids and who (docs/state-core.md section 2.1, 2.2). Newtypes only: what a
//! value means is in its type, and nothing here parses or formats one.
//!
//! There is no tenant field on any id or record: a tenant is an instance, with
//! its own store, socket and executors (section 8.6).

use serde::{Deserialize, Serialize};

/// An event's id. Monotonic within one store: a later commit has a greater id,
/// and `events_after(n)` returns ids greater than `n` in commit order (2.3 case 5).
/// `EventId(0)` is before the first event.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct EventId(pub u64);

/// The key that makes `apply` idempotent: a second `apply` with the same key
/// returns the first result and writes nothing (2.2).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdempotencyKey(pub String);

/// A lease's fencing token: strictly increasing per lease name, `last + 1` on
/// every successful `take` (2.2, 8.2).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Token(pub u64);

/// A revision of a text section, opaque to the caller: `set_text` is refused
/// with [`Refusal::Conflict`](crate::Refusal::Conflict) unless it names the current one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rev(pub String);

/// A moment, in milliseconds since the Unix epoch, as the backend's clock read it.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct At(pub u64);

/// A project's id (`wsp`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(pub String);

/// A row's id (`wsp-483`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RowId(pub String);

/// A list's id (`state-core`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ListId(pub String);

/// A group: its list and its number, from 1 (`state-core/2`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupId {
    pub list: ListId,
    pub n: u32,
}

/// A seat's id (`cpd-3698`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeatId(pub String);

/// An order's id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OrderId(pub String);

/// An ask's, a note's or a flag's id (2.1's "message id").
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AskId(pub String);

/// A machine's (an executor's) name (`eds-macbook-i5`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineName(pub String);

/// A lease's name: `tick`, `barrier:<list>/<g>`, `sweep:<kind>`,
/// `deliver:<seat>`, `claim:<row>`, `verify:<row>` (2.1). A claim is the lease
/// `claim:<row>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseName(pub String);

/// A run's key: one row, one attempt, one kind (2.1).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunKey {
    pub row: RowId,
    pub attempt: u32,
    pub kind: crate::RunKind,
}

/// Who did it (2.1). Every command and every event names one; the API checks
/// it against the connection's credential (4.1).
///
/// The text form 2.1 writes (`person:ed`, `seat:cpd-3698`) is the CLI's and the
/// API envelope's rendering; serde writes the derived form (`{"person":"ed"}`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Principal {
    /// A person (`person:ed`).
    Person(String),
    /// An agent in a seat (`seat:cpd-3698`).
    Seat(SeatId),
    /// An agent manager on a host.
    Manager(MachineName),
    /// An executor (`executor:eds-macbook-i5`).
    Executor(MachineName),
    /// wsp itself, as one instance (`wsp:<instance>`): the tick, a sweep, a barrier.
    Wsp(String),
}
