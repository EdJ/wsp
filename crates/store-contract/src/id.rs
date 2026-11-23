//! Ids and who (docs/state-core.md section 2.1, 2.2). Newtypes only: what a
//! value means is in its type, and nothing here parses one. One name is
//! formed here, [`LeaseName::deliver`], so its spelling has one home.
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

/// A hub user's name (`ed`). Any string is a name here: nothing parses or
/// checks one, and the text form is the string itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserName(pub String);

/// A token record's id on the hub. Not the token: the token is never in a
/// command, event or entity, only its hash is ([`Credential::hash`](crate::Credential::hash)).
/// Any string is an id here, and the text form is the string itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenId(pub String);

/// A shared workspace's name (`acme`). Any string is a name here: nothing
/// parses or checks one, and the text form is the string itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SharedName(pub String);

/// The workspace a record or an event belongs to: the scope it syncs in.
/// A spoke has one private workspace, which needs no name because it never
/// leaves the spoke; a shared one is bound to the hub and syncs to its members.
///
/// The text form, which serde writes, is `private` or `shared:<name>`. A record
/// or event with no workspace reads as [`Private`](WorkspaceId::Private), so
/// nothing stored before workspaces existed changes meaning; a private one is
/// written without the field, so it stays as it was stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkspaceId {
    /// The spoke's own workspace (`private`).
    #[default]
    Private,
    /// A workspace on the hub (`shared:<name>`).
    Shared(SharedName),
}

impl WorkspaceId {
    /// Whether this is the private workspace (what a record's `skip_serializing_if` asks).
    pub fn is_private(&self) -> bool {
        matches!(self, WorkspaceId::Private)
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceId::Private => f.write_str("private"),
            WorkspaceId::Shared(n) => write!(f, "shared:{}", n.0),
        }
    }
}

impl std::str::FromStr for WorkspaceId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        if s == "private" {
            Ok(WorkspaceId::Private)
        } else if let Some(n) = s.strip_prefix("shared:") {
            Ok(WorkspaceId::Shared(SharedName(n.to_string())))
        } else {
            Err(format!("not a workspace: `{s}` (`private` or `shared:<name>`)"))
        }
    }
}

impl Serialize for WorkspaceId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let t = String::deserialize(d)?;
        t.parse().map_err(serde::de::Error::custom)
    }
}

/// A lease's name: `tick`, `barrier:<list>/<g>`, `sweep:<kind>`,
/// `deliver:<seat>`, `claim:<row>`, `verify:<row>` (2.1). A claim is the lease
/// `claim:<row>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseName(pub String);

impl LeaseName {
    /// `deliver:<seat>`: the lease whose holder delivers the seat's orders
    /// (`order.deliver`, 3.1). A name, as 2.1 writes it; nothing parses it back.
    pub fn deliver(seat: &SeatId) -> LeaseName {
        LeaseName(format!("deliver:{}", seat.0))
    }
}

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
