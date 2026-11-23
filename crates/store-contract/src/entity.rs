//! The entities of docs/state-core.md section 2.1, with the states of section
//! 3's tables as enums, and [`State`], the whole domain as `snapshot()` returns
//! it. Records only: which transition is legal is the domain's (section 3,
//! [`Domain`](crate::Domain)), never this crate's.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::id::*;

/// A row's state (3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowState {
    Todo,
    Doing,
    Review,
    SentBack,
    Blocked,
    Parked,
    Done,
}

/// A list's or a group's state (3.4). One enum for both, as the table has one
/// column: a list is `planned`, `running` or `finished`; a group is `pending`,
/// `running`, `barrier`, `held` or `passed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListState {
    Planned,
    Running,
    Pending,
    Barrier,
    Held,
    Passed,
    Finished,
}

/// A seat's state (3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatState {
    Requested,
    Spawning,
    Started,
    Working,
    Idle,
    Ending,
    Gone,
    Lost,
}

/// An order's state (3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Queued,
    Typed,
    Delivered,
    Acknowledged,
    Done,
    Cancelled,
    Failed,
}

/// Which of the three an [`Ask`] is (2.1's `Ask` / `Note` / `Flag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    Ask,
    Note,
    Flag,
}

/// An ask's state (2.1: raised, acked, answered).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskState {
    Raised,
    Acked,
    Answered,
}

/// What a [`Run`] runs (2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Check,
    Verification,
    Barrier,
}

/// A run's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Finished,
    /// Its runner went away before a verdict.
    Abandoned,
}

/// A finished run's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunVerdict {
    Pass,
    Fail,
}

/// A hub user's or token record's state: live, or revoked and kept as a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    Active,
    Revoked,
}

/// How wsp reaches a machine's executor.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// In this process, or on this host's socket.
    Local,
    /// TCP to `addr` (`host:port`).
    Tcp { addr: String },
}

/// A row (2.1, 3.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub id: RowId,
    pub project: ProjectId,
    /// The workspace it belongs to; none reads as private.
    #[serde(default, skip_serializing_if = "WorkspaceId::is_private")]
    pub workspace: WorkspaceId,
    pub title: String,
    pub state: RowState,
    /// The state `unblock` returns to; set while `blocked`.
    #[serde(default)]
    pub before_block: Option<RowState>,
    /// The seat that serves it, while it has one.
    #[serde(default)]
    pub seat: Option<SeatId>,
    /// The group it is a member of.
    #[serde(default)]
    pub group: Option<GroupId>,
    /// The commit `review` recorded.
    #[serde(default)]
    pub commit: Option<String>,
    /// Its other fields, by name (tier, kind, tags, ...).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

/// A group of a list (2.1, 3.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub state: ListState,
    pub members: Vec<RowId>,
    /// The barrier attempt it is on, from 0.
    #[serde(default)]
    pub attempt: u32,
    /// Its other fields, by name, as today's files say them (its stop, ...);
    /// untyped until step 4 types the ones that stay (wsp-070).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

/// A list (2.1, 3.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct List {
    pub id: ListId,
    /// The workspace it belongs to; none reads as private.
    #[serde(default, skip_serializing_if = "WorkspaceId::is_private")]
    pub workspace: WorkspaceId,
    pub state: ListState,
    pub groups: Vec<Group>,
    /// Its other fields, by name, as today's files say them (its caption, ...);
    /// untyped until step 4 types the ones that stay (wsp-070).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

/// A seat (2.1, 3.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    pub id: SeatId,
    pub machine: MachineName,
    /// The agent kind (`claude`, ...).
    pub kind: String,
    #[serde(default)]
    pub model: Option<String>,
    pub state: SeatState,
    /// The state a returning beat restores; set while `lost`.
    #[serde(default)]
    pub before_lost: Option<SeatState>,
    /// The row it serves, and the attempt: the spawn's idempotency key (3.2).
    #[serde(default)]
    pub row: Option<RowId>,
    #[serde(default)]
    pub attempt: u32,
    /// The agent's pid, as the executor recorded it.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Its other fields, by name, as today's files say them (its role, the row a
    /// governor governs, its said or working word, ...); untyped until step 4
    /// types the ones that stay (wsp-070).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

/// A claim: the lease `claim:<row>`, held by a seat (2.1, 2.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub row: RowId,
    pub seat: SeatId,
    pub token: Token,
    pub expires: At,
}

/// An order to a seat (2.1, 3.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    /// The workspace it belongs to; none reads as private.
    #[serde(default, skip_serializing_if = "WorkspaceId::is_private")]
    pub workspace: WorkspaceId,
    pub from: Principal,
    pub to: SeatId,
    /// The text, or a reference to where it is kept.
    pub text: String,
    pub state: OrderState,
    #[serde(default)]
    pub attempts: u32,
    /// The spawn's first work order, which went out inside `seat.spawn` rather
    /// than through delivery (3.1, wsp-117).
    #[serde(default)]
    pub first: bool,
    /// The last refusal's words, which wip draws (wsp-117).
    #[serde(default)]
    pub why: Option<String>,
}

/// An ask, a note or a flag (2.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub id: AskId,
    /// The workspace it belongs to; none reads as private.
    #[serde(default, skip_serializing_if = "WorkspaceId::is_private")]
    pub workspace: WorkspaceId,
    pub kind: AskKind,
    pub from: Principal,
    /// Whom it is for; none is everybody who reads asks.
    #[serde(default)]
    pub to: Option<Principal>,
    /// What it is about.
    #[serde(default)]
    pub about: Option<crate::Subject>,
    pub text: String,
    pub state: AskState,
    #[serde(default)]
    pub answer: Option<String>,
    /// Its other fields, by name, as today's files say them; untyped until step
    /// 4 types the ones that stay (wsp-070).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

/// A check, a verification or a barrier run (2.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub key: RunKey,
    pub state: RunState,
    #[serde(default)]
    pub verdict: Option<RunVerdict>,
    /// Where its log is kept.
    #[serde(default)]
    pub log: Option<String>,
}

/// A machine: an executor and what it last said (2.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub name: MachineName,
    pub transport: Transport,
    /// When it last beat.
    #[serde(default)]
    pub beat: Option<At>,
    /// Its load facts, by name, as it reported them.
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
}

/// A lease (2.1, 2.2, 8.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub name: LeaseName,
    pub holder: Principal,
    pub token: Token,
    pub expires: At,
}

/// A person on the hub (multiplayer v1: users and tokens only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub name: UserName,
    pub state: IdentityState,
    #[serde(default)]
    pub admin: bool,
}

/// A token's record on the hub. `hash` is the hex SHA-256 of the token; the
/// token itself is never in a command, event or entity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub id: TokenId,
    pub user: UserName,
    pub hash: String,
    pub state: IdentityState,
    pub issued: At,
    #[serde(default)]
    pub revoked: Option<At>,
}

/// A shared workspace on the hub: a sync scope its members read and write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedWorkspace {
    pub name: SharedName,
}

/// A user's membership of a shared workspace: the user may read and write it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub workspace: SharedName,
    pub user: UserName,
}

/// One entity of 2.1, whole, as [`Command::Mirror`](crate::Command::Mirror)
/// puts it and `mirror.put` carries it. Serialized in serde's derived form, as
/// [`Subject`](crate::Subject) is (`{"seat": {...}}`). Step 1's alone (wsp-070):
/// steps 2-4 remove it as each machine takes its entities over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entity {
    Row(Row),
    List(List),
    Seat(Seat),
    Claim(Claim),
    Order(Order),
    Ask(Ask),
    Machine(Machine),
}

/// The whole domain, as `snapshot()` returns it beside the id it is consistent
/// at. It equals the fold of [`Domain::evolve`](crate::Domain::evolve) over
/// every event up to that id (2.3 case 6).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub rows: BTreeMap<RowId, Row>,
    #[serde(default)]
    pub lists: BTreeMap<ListId, List>,
    #[serde(default)]
    pub seats: BTreeMap<SeatId, Seat>,
    #[serde(default)]
    pub claims: BTreeMap<RowId, Claim>,
    #[serde(default)]
    pub orders: BTreeMap<OrderId, Order>,
    #[serde(default)]
    pub asks: BTreeMap<AskId, Ask>,
    /// Runs, in the order they began (their key is a triple, which a JSON map cannot key by).
    #[serde(default)]
    pub runs: Vec<Run>,
    #[serde(default)]
    pub machines: BTreeMap<MachineName, Machine>,
    #[serde(default)]
    pub leases: BTreeMap<LeaseName, Lease>,
    /// The hub's users (wsp-159's identity domain folds them).
    #[serde(default)]
    pub users: BTreeMap<UserName, User>,
    /// The hub's token records, by id.
    #[serde(default)]
    pub credentials: BTreeMap<TokenId, Credential>,
    /// The hub's shared workspaces, by name (wsp-478's identity domain folds them).
    #[serde(default)]
    pub workspaces: BTreeMap<SharedName, SharedWorkspace>,
    /// Each shared workspace's members: the users `hub.member.set` made so.
    #[serde(default)]
    pub members: BTreeMap<SharedName, BTreeSet<UserName>>,
}
