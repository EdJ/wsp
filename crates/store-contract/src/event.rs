//! Events (docs/state-core.md sections 2.1 and 2.2): what happened, by whom, to
//! what. Every transition of section 3 writes one with its principal; the log of
//! events is the store's history and what `subscribe` streams (4.3).

use serde::{Deserialize, Serialize};

use crate::id::*;

/// One event, as the store committed it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub at: At,
    pub principal: Principal,
    pub kind: EventKind,
    pub subject: Subject,
    /// What the kind carries beyond its subject; `null` when nothing.
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// An event before the store gave it an id and a moment: what
/// [`Domain::decide`](crate::Domain::decide) returns. The backend stamps each
/// with the next id, its clock and the command's principal, in one atomic write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub kind: EventKind,
    pub subject: Subject,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// What an event is about (2.1's keys). Written `row:<id>`, `seat:<id>`, ... in
/// the text forms; serde writes the derived form (`{"row":"wsp-483"}`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    Project(ProjectId),
    Row(RowId),
    List(ListId),
    Group(GroupId),
    Seat(SeatId),
    Claim(RowId),
    Order(OrderId),
    Ask(AskId),
    Run(RunKey),
    Machine(MachineName),
    Lease(LeaseName),
    User(UserName),
    Credential(TokenId),
}

/// The kinds of event the machines write (section 3), by their wire names.
/// A kind this build does not know, from a newer writer, reads as `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventKind {
    // Rows (3.3).
    #[serde(rename = "row.added")]
    RowAdded,
    #[serde(rename = "row.edited")]
    RowEdited,
    #[serde(rename = "row.claimed")]
    RowClaimed,
    #[serde(rename = "row.reviewed")]
    RowReviewed,
    #[serde(rename = "row.blocked")]
    RowBlocked,
    #[serde(rename = "row.unblocked")]
    RowUnblocked,
    #[serde(rename = "row.parked")]
    RowParked,
    #[serde(rename = "row.sent_back")]
    RowSentBack,
    #[serde(rename = "row.landed")]
    RowLanded,
    // Lists and groups (3.4).
    #[serde(rename = "list.started")]
    ListStarted,
    #[serde(rename = "list.finished")]
    ListFinished,
    #[serde(rename = "group.running")]
    GroupRunning,
    #[serde(rename = "group.barrier")]
    GroupBarrier,
    #[serde(rename = "group.passed")]
    GroupPassed,
    #[serde(rename = "group.held")]
    GroupHeld,
    #[serde(rename = "group.released")]
    GroupReleased,
    // Seats (3.2).
    #[serde(rename = "seat.requested")]
    SeatRequested,
    #[serde(rename = "seat.spawning")]
    SeatSpawning,
    #[serde(rename = "seat.started")]
    SeatStarted,
    #[serde(rename = "seat.working")]
    SeatWorking,
    #[serde(rename = "seat.idle")]
    SeatIdle,
    #[serde(rename = "seat.ending")]
    SeatEnding,
    #[serde(rename = "seat.gone")]
    SeatGone,
    #[serde(rename = "seat.lost")]
    SeatLost,
    #[serde(rename = "seat.returned")]
    SeatReturned,
    // Orders (3.1).
    #[serde(rename = "order.queued")]
    OrderQueued,
    #[serde(rename = "order.typed")]
    OrderTyped,
    #[serde(rename = "order.delivered")]
    OrderDelivered,
    #[serde(rename = "order.refused")]
    OrderRefused,
    #[serde(rename = "order.acknowledged")]
    OrderAcknowledged,
    #[serde(rename = "order.done")]
    OrderDone,
    #[serde(rename = "order.cancelled")]
    OrderCancelled,
    #[serde(rename = "order.failed")]
    OrderFailed,
    // Asks, notes and flags.
    #[serde(rename = "ask.raised")]
    AskRaised,
    #[serde(rename = "ask.answered")]
    AskAnswered,
    #[serde(rename = "ask.acked")]
    AskAcked,
    // Claims and leases (2.2).
    #[serde(rename = "claim.taken")]
    ClaimTaken,
    #[serde(rename = "claim.released")]
    ClaimReleased,
    #[serde(rename = "lease.taken")]
    LeaseTaken,
    #[serde(rename = "lease.released")]
    LeaseReleased,
    // Runs.
    #[serde(rename = "run.started")]
    RunStarted,
    #[serde(rename = "run.finished")]
    RunFinished,
    // Machines.
    #[serde(rename = "machine.set")]
    MachineSet,
    #[serde(rename = "machine.beat")]
    MachineBeat,
    // The hub's identity (multiplayer v1).
    #[serde(rename = "hub.user.added")]
    HubUserAdded,
    #[serde(rename = "hub.user.revoked")]
    HubUserRevoked,
    #[serde(rename = "hub.token.issued")]
    HubTokenIssued,
    #[serde(rename = "hub.token.revoked")]
    HubTokenRevoked,
    // Text (2.5).
    #[serde(rename = "text.set")]
    TextSet,
    // The mirror (step 1 alone, wsp-070): what the mirror domain decides
    // `mirror` into. `put` carries the whole [`Entity`](crate::Entity) under its
    // subject; `gone` carries `null`.
    #[serde(rename = "mirror.put")]
    MirrorPut,
    #[serde(rename = "mirror.gone")]
    MirrorGone,
    /// A kind this build does not know, from a newer writer.
    #[serde(rename = "unknown")]
    #[serde(other)]
    Unknown,
}
