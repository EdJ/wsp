//! Commands (docs/state-core.md sections 2.2 and 4.2): one variant per command
//! verb of 4.2, tagged by its verb, with its arguments beside it, as 4.1's
//! envelope carries them (`"verb": "row.claim", "args": {...}`), and `mirror`,
//! step 1's projection (below).
//!
//! **The mirror** (wsp-070, section 7's step 1). Step 1's store is a projection
//! of today's truth (the files, and the managers' events the daemon folds), not
//! the owner of any state, and 4.2's verbs each carry section 3's guards, so
//! none says "this entity is now X". [`Command::Mirror`] says it: it puts whole
//! entities and removes subjects. It is not fenced. Only the server's mirror
//! domain (wsp-073) decides it, into `mirror.put` and `mirror.gone`; a real
//! domain refuses it as `illegal`. Steps 2-4 remove it as each machine takes its
//! entities over.
//!
//! A command issued under a lease carries a [`Fence`], and `apply` refuses it
//! with [`Refusal::Stale`](crate::Refusal::Stale) when the lease's token has
//! moved on (2.2), before `decide` runs. Which command is legal in which state
//! is the domain's ([`Domain::decide`](crate::Domain::decide)), not this crate's.
//!
//! **A command's fence** (wsp-065). The contract has no accessor (an accessor
//! is logic, R2); it names the fence by a rule and a table instead. A fenced
//! verb carries its fence as the argument named `fence`, and no other argument
//! has that name, so on the wire a command is fenced when `args.fence` is
//! present and not `null`. [`FENCED_VERBS`] lists those verbs, and
//! `tests/round_trip.rs` holds the table to the variants, so a fifth fenced
//! verb does not pass until it is in the table. A backend may match the verbs
//! or read `args.fence`; it checks the table in its own tests either way.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::entity::{AskKind, Entity};
use crate::event::Subject;
use crate::id::*;

/// A lease and the token the writer holds it at (2.2, 8.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fence {
    pub lease: LeaseName,
    pub token: Token,
}

/// The verbs whose `args` carry a `fence` (`seat.end`'s may be `null`: the
/// person's has none), which `apply` checks before `decide` (2.2).
pub const FENCED_VERBS: &[&str] = &["row.claim", "row.review", "row.land", "seat.end"];

/// What `apply` returns: the new events' ids, in commit order. A repeated key
/// returns the first `Applied` and writes nothing; a refusal is no result and
/// records no key ([`Commands::apply`](crate::Commands::apply)).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub events: Vec<EventId>,
}

/// Block or unblock (3.3: `block` → `blocked`, `unblock` → the state before).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Block {
    On { reason: String },
    Off,
}

/// What a verifier found (3.3: `holds` → `done`, `blocks` → `sent_back`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Landed at this commit.
    Holds { commit: String },
    /// Sent back, and why.
    Blocks { reason: String },
}

/// Where a held group is released to (3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTo {
    Barrier,
    Running,
}

/// One command: a verb of 4.2 and its arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args")]
pub enum Command {
    /// Add a row; the domain names it.
    #[serde(rename = "row.add")]
    RowAdd {
        project: ProjectId,
        title: String,
        #[serde(default)]
        fields: BTreeMap<String, String>,
    },
    /// Set (`Some`) or remove (`None`) a row's fields.
    #[serde(rename = "row.edit")]
    RowEdit {
        row: RowId,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, Option<String>>,
    },
    /// `todo` or `sent_back` → `doing`, under the row's claim.
    #[serde(rename = "row.claim")]
    RowClaim {
        row: RowId,
        seat: SeatId,
        fence: Fence,
    },
    /// `doing` → `review`, at a commit, under a current claim.
    #[serde(rename = "row.review")]
    RowReview {
        row: RowId,
        commit: String,
        fence: Fence,
    },
    #[serde(rename = "row.block")]
    RowBlock { row: RowId, block: Block },
    #[serde(rename = "row.park")]
    RowPark { row: RowId },
    /// A verifier's verdict, under `verify:<row>`; landing itself is an effect
    /// whose result comes back as this command.
    #[serde(rename = "row.land")]
    RowLand {
        row: RowId,
        verdict: Verdict,
        fence: Fence,
    },
    /// `planned` → `running`.
    #[serde(rename = "list.go")]
    ListGo { list: ListId },
    /// Hold a group, or the list's running group when none is named.
    #[serde(rename = "list.hold")]
    ListHold {
        list: ListId,
        #[serde(default)]
        group: Option<u32>,
        reason: String,
    },
    /// A held group → `barrier` or `running`.
    #[serde(rename = "list.release")]
    ListRelease {
        list: ListId,
        group: u32,
        to: ReleaseTo,
    },
    /// Ask for a seat; `(row, attempt)` is the spawn's idempotency key (3.2).
    #[serde(rename = "seat.spawn")]
    SeatSpawn {
        #[serde(default)]
        row: Option<RowId>,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        machine: Option<MachineName>,
        kind: String,
        #[serde(default)]
        model: Option<String>,
    },
    /// A live seat → `ending`; by the claim's holder (fenced) or the person.
    #[serde(rename = "seat.end")]
    SeatEnd {
        seat: SeatId,
        #[serde(default)]
        fence: Option<Fence>,
    },
    /// Remove a seat's record; `lost` says it was `lost` and is given up (3.2).
    #[serde(rename = "seat.despawn")]
    SeatDespawn {
        seat: SeatId,
        #[serde(default)]
        lost: bool,
    },
    /// Queue an order for a seat.
    #[serde(rename = "order.send")]
    OrderSend { to: SeatId, text: String },
    #[serde(rename = "order.cancel")]
    OrderCancel { order: OrderId },
    /// Raise an ask, a note or a flag.
    #[serde(rename = "ask.raise")]
    AskRaise {
        kind: AskKind,
        #[serde(default)]
        to: Option<Principal>,
        #[serde(default)]
        about: Option<Subject>,
        text: String,
    },
    #[serde(rename = "ask.answer")]
    AskAnswer { ask: AskId, text: String },
    #[serde(rename = "ask.ack")]
    AskAck { ask: AskId },
    /// Record a machine, or change how it is reached or what it reported.
    #[serde(rename = "machine.set")]
    MachineSet {
        name: MachineName,
        #[serde(default)]
        transport: Option<crate::entity::Transport>,
        #[serde(default)]
        facts: BTreeMap<String, String>,
    },
    /// Step 1's projection (the module docs): each of `put` is now this, and
    /// each of `gone` is no more. Not fenced; only the mirror domain decides it.
    #[serde(rename = "mirror")]
    Mirror {
        #[serde(default)]
        put: Vec<Entity>,
        #[serde(default)]
        gone: Vec<Subject>,
    },
}
