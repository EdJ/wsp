//! `wsp-host-api`: the protocol between wsp and the agent manager on each host
//! (`docs/agent-manager.md` section 3, step M0 of 6.2).
//!
//! A contract crate under `docs/components.md` R2: types, a trait and
//! constants, and no IO. `serde` and `serde_json` are its only dependencies,
//! and it names nothing of wsp (no `wsp-store`, no `wsp-base`), so the manager
//! binary can depend on it without pulling in the store.
//!
//! **On the wire** (3.4) a connection carries one [`Frame`] per line of JSON.
//! A frame is the message's fields flattened beside its `type` (`"seat.spawn"`,
//! `"order.acked"`, …), plus two optional envelope fields:
//! - `id`, the manager's journal id, on every event it journals (2.5), so a
//!   client that reconnects can [`Message::Subscribe`] `from` the last it heard;
//! - `req`, a request id the sender picks; the answer to that request carries
//!   the same `req`.
//!
//! **Fencing** (3.3) is by [`Key`] — `(row, attempt)`, the idempotency key of
//! `seat.spawn` — and by [`Epoch`], the manager's count of starts of one seat.
//! A command naming an older epoch than the seat's current one is refused.
//!
//! **Who decides.** Nothing here carries a decision of wsp's to the manager as
//! a policy; the manager is told what to do and answers what happened. Every
//! refusal it makes carries a reason ([`Refusal`], [`OrderFail`]).
//!
//! Every change to this crate is a change to the contract (R5): a row of its
//! own. A field added later is `#[serde(default)]` so that an older peer's
//! frames still decode.

use serde::{Deserialize, Serialize};

mod conn;
#[cfg(any(test, feature = "test-support"))]
pub use conn::{pair, Memory};
pub use conn::{ConnError, Connection};

/// The protocol's version, carried in both sides' [`Hello`]. A change that an
/// older peer cannot decode bumps it; an additive field does not.
pub const PROTOCOL: u32 = 1;

/// Each side beats every `BEAT_SECS` on a timer of its own (3.4), never on the
/// daemon's tick.
pub const BEAT_SECS: u64 = 10;

/// A side that hears no frame for this many beats reconnects (3.4).
pub const MISSED_BEATS: u32 = 3;

// ---------------------------------------------------------------- identities

/// A wsp row, e.g. `wsp-002`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RowId(pub String);

/// A seat id, minted by wsp (`cpd-115`), never by the manager.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeatId(pub String);

/// A host, as the manager names itself in its [`Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostId(pub String);

/// An order id. It is compound-sup's at-most-once delivery key: the same id
/// again is the same order and gets the same answer (3.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OrderId(pub String);

/// An id in one manager's journal: a per-manager sequence (2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub u64);

/// The manager's number for one start of a seat (3.3). `seat.stop` and
/// `order.deliver` name it and are refused against a later one, so a stop sent
/// before a respawn cannot end the respawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(pub u64);

/// A request id, picked by whoever sends a request and echoed on its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReqId(pub u64);

/// A moment, as milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct At(pub u64);

/// The manager token (3.3): made at install, mode 0600 in `<exec_dir>`, one per
/// host. A client's [`Hello`] must carry it. Its `Debug` does not print it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(pub String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

/// The idempotency key of `seat.spawn` (3.3): a retry after a lost reply gets
/// the same `seat.started`, never a second agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    pub row: RowId,
    pub attempt: u32,
}

/// The idempotency key of `job.run`: the row's [`Key`] and a name for this
/// one *run* of a job, since one attempt runs several jobs and each must be
/// retried without running twice.
///
/// **`job` is unique per run, not per name.** A retry of the same run after a
/// lost reply sends the same key and gets the same answer; a new run sends a
/// new `job`, even of the same check in the same tree. The caller makes it
/// unique by appending the run's own mark to the name (`check@1763400000000`,
/// `verify:clippy@3`). The name alone is not enough: a reopened row starts its
/// attempts again, and wsp-040's reopened row sent `(row, 0)` a second time
/// and was refused as a duplicate.
///
/// **A job with no row** (a barrier's check, a warm target) carries the list
/// and the group where the row would stand: `row` is `<list>#<group>`
/// (`m3-agent-manager#1`), the group's ordinal as the barrier ran it, and
/// `attempt` is the barrier's round on that group (0 for the first).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobKey {
    pub row: RowId,
    pub attempt: u32,
    pub job: String,
}

/// A tree, named by its row and the host that holds it (1.3), never by a path
/// on wsp's machine. This is what lets trees move between hosts (Q6) without a
/// new protocol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tree {
    pub row: RowId,
    pub host: HostId,
}

/// A directory on a host that is no row's tree: a check tree (`checks/…`), a
/// barrier's tree (`barrier/…/tree`), a warm target. Like [`Tree`] it is never
/// a path on wsp's machine: `path` is relative to the host's tree root, the
/// directory under which the manager keeps every tree it runs jobs in, with
/// `/` between its parts.
///
/// The manager refuses (`no_tree`) a `path` that falls outside that root: an
/// absolute one, or one whose `..` parts climb above it ([`HostDir::escapes`]
/// is the lexical half of that check), or one that is not there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostDir {
    pub host: HostId,
    pub path: String,
}

impl HostDir {
    /// Whether `path`, read without touching the disk, falls outside the
    /// tree root: it is empty, absolute, or climbs above the root with `..`.
    /// A link inside the root that points out of it is the manager's to
    /// catch when it resolves the path; this cannot see one.
    pub fn escapes(&self) -> bool {
        if self.path.is_empty() || self.path.starts_with('/') || self.path.starts_with('\\') {
            return true;
        }
        let mut depth: i64 = 0;
        for part in self.path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    depth -= 1;
                    if depth < 0 {
                        return true;
                    }
                }
                _ => depth += 1,
            }
        }
        false
    }
}

/// A file on a host: a transcript or a job's log. wsp reads it through the
/// manager, never by opening the path itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileRef {
    pub host: HostId,
    pub path: String,
}

// ---------------------------------------------------------------- the frame

/// One line on a connection: a [`Message`] and its envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// The journal id of a manager event. Absent on commands, requests,
    /// answers and beats, which are not journalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<EventId>,
    /// The request this frame is, or answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req: Option<ReqId>,
    #[serde(flatten)]
    pub msg: Message,
}

/// Every message of section 3.2. The `type` on the wire is the name in the
/// plan's table. W → M is wsp to the manager, M → W the manager to wsp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    /// Both directions, first on every connection.
    #[serde(rename = "hello")]
    Hello(Hello),

    /// W → M: start an agent. Idempotent on `key`.
    #[serde(rename = "seat.spawn")]
    SeatSpawn(SeatSpawn),
    /// M → W: the seat is running.
    #[serde(rename = "seat.started")]
    SeatStarted(SeatStarted),
    /// M → W: the seat was not started, and why.
    #[serde(rename = "seat.refused")]
    SeatRefused(SeatRefused),
    /// W → M: end a seat's start `epoch`.
    #[serde(rename = "seat.stop")]
    SeatStop(SeatStop),
    /// M → W: a seat ended, whoever ended it.
    #[serde(rename = "seat.stopped")]
    SeatStopped(SeatStopped),

    /// W → M: hand an order to a seat's supervisor.
    #[serde(rename = "order.deliver")]
    OrderDeliver(OrderDeliver),
    /// W → M: withdraw an order not yet delivered.
    #[serde(rename = "order.cancel")]
    OrderCancel(OrderCancel),
    /// M → W: the order was typed and submitted.
    #[serde(rename = "order.delivered")]
    OrderDelivered(OrderDelivered),
    /// M → W: the order was not delivered, and why.
    #[serde(rename = "order.refused")]
    OrderRefused(OrderRefused),
    /// M → W: the agent's own hook saw the turn the order began.
    #[serde(rename = "order.acked")]
    OrderAcked(OrderAcked),

    /// M → W: a turn began or ended.
    #[serde(rename = "turn")]
    Turn(Turn),
    /// M → W: the seat's last word.
    #[serde(rename = "seat.said")]
    SeatSaid(SeatSaid),
    /// M → W: the seat's spend so far.
    #[serde(rename = "seat.spent")]
    SeatSpent(SeatSpent),
    /// M → W: a digest of a turn, with its transcript.
    #[serde(rename = "seat.did")]
    SeatDid(SeatDid),
    /// M → W: a seat is waiting on a command that waits on the seat.
    #[serde(rename = "seat.stuck")]
    SeatStuck(SeatStuck),

    /// W → M request: every seat the manager runs. Answered by `seats.list`.
    #[serde(rename = "seats")]
    Seats(Seats),
    /// M → W: the answer to `seats`.
    #[serde(rename = "seats.list")]
    SeatsList(SeatsList),
    /// W → M request: a seat's screen, because a person asked (never a tick).
    /// Answered by `seat.screen`.
    #[serde(rename = "seat.read")]
    SeatRead(SeatRead),
    /// M → W: the answer to `seat.read`.
    #[serde(rename = "seat.screen")]
    SeatScreen(SeatScreen),

    /// W → M: run a build or check in a tree, under the manager's admission.
    #[serde(rename = "job.run")]
    JobRun(JobRun),
    /// W → M: end a job.
    #[serde(rename = "job.stop")]
    JobStop(JobStop),
    /// M → W: a job was admitted and runs.
    #[serde(rename = "job.started")]
    JobStarted(JobStarted),
    /// M → W: a job ended.
    #[serde(rename = "job.ended")]
    JobEnded(JobEnded),
    /// M → W: output a running job wrote, sent only to the connection that
    /// asked for the job and never journalled.
    #[serde(rename = "job.output")]
    JobOutput(JobOutput),
    /// W → M request: the manager's job table. Answered by `jobs.list`.
    #[serde(rename = "jobs")]
    Jobs(Jobs),
    /// M → W: the answer to `jobs`.
    #[serde(rename = "jobs.list")]
    JobsList(JobsList),

    /// W → M, reserved for trees placed with the agents (1.3, T2/T3). No
    /// manager answers it before Q6 is decided; it refuses `unsupported`.
    #[serde(rename = "tree.checkout")]
    TreeCheckout(TreeVerb),
    /// W → M, reserved (see `tree.checkout`).
    #[serde(rename = "tree.land")]
    TreeLand(TreeVerb),
    /// W → M, reserved (see `tree.checkout`).
    #[serde(rename = "tree.copy")]
    TreeCopy(TreeVerb),

    /// Both directions, every [`BEAT_SECS`].
    #[serde(rename = "beat")]
    Beat(Beat),
    /// W → M: send every journalled event after `from`, then follow.
    #[serde(rename = "subscribe")]
    Subscribe(Subscribe),

    /// M → W: a command or request with no refusal of its own (`seat.stop`,
    /// `job.run`, `jobs`, `tree.*`, a malformed or unauthorised frame) was refused.
    /// Its `req` names what was refused.
    #[serde(rename = "refused")]
    Refused(Refused),
}

// ---------------------------------------------------------------- hello

/// `hello`. The client's and the manager's carry different facts, told apart
/// by `side`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "side", rename_all = "snake_case")]
pub enum Hello {
    Wsp(WspHello),
    Manager(ManagerHello),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WspHello {
    pub role: Role,
    /// [`PROTOCOL`] as the client was built.
    pub protocol: u32,
    /// wsp's own version, for the log.
    pub version: String,
    /// The last journal id this client heard from this manager.
    #[serde(default)]
    pub last_event: Option<EventId>,
    /// The manager token. A connection without the right one is closed.
    pub token: Token,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Cli,
    Daemon,
    Server,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManagerHello {
    pub host: HostId,
    pub protocol: u32,
    pub version: String,
    /// The compound-sup wire versions this manager speaks.
    pub compound_sup: WireRange,
    /// The compound-render wire versions this manager speaks.
    pub compound_render: WireRange,
    /// The newest id in the journal.
    pub journal_head: Option<EventId>,
    /// The seats it runs now.
    pub seats: Vec<SeatEpoch>,
    pub capacity: Capacity,
}

/// An inclusive range of wire versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRange {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatEpoch {
    pub seat: SeatId,
    pub epoch: Epoch,
}

/// What the manager will admit at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    pub seats: u32,
    /// Slots for [`JobClass::Build`].
    pub build: u32,
    /// Slots for [`JobClass::Check`].
    pub check: u32,
}

// ---------------------------------------------------------------- seats

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeatSpawn {
    pub key: Key,
    pub seat: SeatId,
    pub agent: AgentSpec,
    pub workdir: Workdir,
    pub gate: Gate,
    #[serde(default)]
    pub first_order: Option<FirstOrder>,
}

/// What to run. `env` holds variable *names*: the manager fills their values
/// from its own environment and secrets, so no secret crosses the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    /// The agent kind (`claude`, `opencode`, …).
    pub kind: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// Where the agent starts: a tree by `(row, host)`, or a plain directory on the
/// manager's host for a seat with no tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workdir {
    Tree(Tree),
    Cwd(String),
}

/// Whether the seat's tool calls pass `compound-remote gate` (section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    Required,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstOrder {
    pub order: OrderId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatStarted {
    pub key: Key,
    pub seat: SeatId,
    pub epoch: Epoch,
    /// The root of the seat's process tree.
    pub pid: u32,
    /// The session's sockets on the manager's host, for a renderer to attach.
    #[serde(default)]
    pub sockets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRefused {
    pub key: Key,
    pub seat: SeatId,
    pub why: Refusal,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatStop {
    pub seat: SeatId,
    pub epoch: Epoch,
    /// Seconds between the polite stop and the kill.
    pub grace_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatStopped {
    pub seat: SeatId,
    pub epoch: Epoch,
    /// The agent's exit status, when it exited rather than was signalled.
    #[serde(default)]
    pub exit: Option<i32>,
    /// The pids the manager ended.
    #[serde(default)]
    pub ended: Vec<u32>,
    /// Whether a renderer is still attached to the closed session.
    #[serde(default)]
    pub renderer_left: bool,
}

// ---------------------------------------------------------------- orders

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderDeliver {
    pub order: OrderId,
    pub seat: SeatId,
    pub epoch: Epoch,
    pub text: String,
    /// Interrupt a turn in progress; without it a busy seat queues the order
    /// (compound-sup's `Deliver.now`).
    #[serde(default)]
    pub now: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderCancel {
    pub order: OrderId,
    pub seat: SeatId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderDelivered {
    pub order: OrderId,
    pub seat: SeatId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRefused {
    pub order: OrderId,
    pub seat: SeatId,
    pub why: OrderFail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderAcked {
    pub order: OrderId,
    pub seat: SeatId,
    /// When the turn the order began started.
    pub turn_began: At,
}

/// Why an order was not delivered: compound-sup's `FailWhy`, spelled as it
/// spells them, and the manager's own reasons after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderFail {
    /// The seat stayed mid-turn past the deadline.
    Busy,
    /// No composer was ever seen to type into.
    NoComposer,
    /// Typed and submitted, retried, and the agent's hook never said a turn began.
    Refused,
    /// The agent's session ended first.
    SessionGone,
    /// No renderer is attached, so nothing reads the screen.
    NoRenderer,
    /// A renderer is attached but cannot read this agent's composer.
    NoReader,
    /// The order named an epoch older than the seat's.
    StaleEpoch,
    /// The manager runs no such seat.
    NoSeat,
    /// wsp cancelled it first.
    Cancelled,
    /// A reason this build does not know, from a newer peer.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------- what seats report

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub seat: SeatId,
    pub epoch: Epoch,
    pub edge: TurnEdge,
    pub at: At,
    /// The order whose ack began this turn, if one did.
    #[serde(default)]
    pub order: Option<OrderId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEdge {
    Began,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatSaid {
    pub seat: SeatId,
    pub epoch: Epoch,
    pub at: At,
    pub text: String,
}

/// Spend so far for this start of the seat. Cost is in millionths of a US
/// dollar, so that it adds up exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatSpent {
    pub seat: SeatId,
    pub epoch: Epoch,
    pub tokens: Tokens,
    pub cost_micros: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatDid {
    pub seat: SeatId,
    pub epoch: Epoch,
    pub at: At,
    pub digest: String,
    pub transcript: FileRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatStuck {
    pub seat: SeatId,
    pub epoch: Epoch,
    /// The command the seat waits on.
    pub command: String,
    /// How long it has waited, in seconds.
    pub waited_secs: u64,
}

// ---------------------------------------------------------------- requests

/// `seats` carries nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Seats {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatsList {
    pub seats: Vec<SeatRow>,
    /// The host's facts from the manager's last beat, so a `seats` caller
    /// (`wip`, 4.3) reads the host and its seats from one reply. Absent
    /// before the first beat, and from a manager older than the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRow {
    pub seat: SeatId,
    pub epoch: Epoch,
    pub state: SeatState,
    pub rss_bytes: u64,
    /// Shells the agent has open.
    pub shells: u32,
    #[serde(default)]
    pub last_turn: Option<At>,
    #[serde(default)]
    pub last_word: Option<String>,
}

/// A seat as its manager sees it. "Lost" is not here: it is what wsp concludes
/// when the manager cannot be heard, not something a manager can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatState {
    Starting,
    Idle,
    Working,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRead {
    pub seat: SeatId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatScreen {
    pub seat: SeatId,
    pub epoch: Epoch,
    pub text: String,
}

// ---------------------------------------------------------------- jobs

/// `job.run`. Idempotent on `key` (see [`JobKey`]: unique per run).
///
/// **The environment** is the rule of `seat.spawn`'s (3.2, wsp-026 d1). The
/// values wsp mints for a job cross on the argv, as an `env -u K -- K=V …`
/// prefix in front of the command. A secret never crosses: not in that prefix
/// and not anywhere else in the frame, and one found there is a defect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRun {
    pub key: JobKey,
    pub class: JobClass,
    pub argv: Vec<String>,
    /// Where it runs: on the wire `"tree": {row, host}` or `"dir": {host,
    /// path}`, beside the other fields.
    #[serde(flatten)]
    pub at: JobAt,
    /// The job is ended this many seconds after it started, whatever it is
    /// doing, and its `job.ended` says `timed_out`.
    pub timeout_secs: u64,
    /// The job is ended once it has written no output and spent no CPU for
    /// this many seconds (check.rs's wsp-609 bound), and its `job.ended` says
    /// `hung`. Absent: no such bound, and from a client older than the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hung_after_secs: Option<u64>,
    /// Whether the job outlives the connection that asked for it. A job that
    /// is not detached is stopped when that connection closes, so a `throttle
    /// run` killed with its agent leaves no build behind. Its `job.output`
    /// goes to that connection either way, while it is open.
    #[serde(default)]
    pub detach: bool,
}

/// Where a job runs: a row's tree, or a directory under the host's tree root
/// that belongs to no row. A frame from before [`HostDir`] carried only
/// `tree`, and still decodes as [`JobAt::Tree`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobAt {
    Tree(Tree),
    Dir(HostDir),
}

/// The admission class: the throttle's slots become the manager's (1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobClass {
    Build,
    Check,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStop {
    pub key: JobKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStarted {
    pub key: JobKey,
    /// The job's own process group, which is what `job.stop` ends.
    pub pgid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEnded {
    pub key: JobKey,
    /// The exit status, when it exited rather than was signalled or timed out.
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    /// It was ended by `hung_after_secs`: no output and no CPU for that long.
    #[serde(default)]
    pub hung: bool,
    /// The whole log. `job.output` is a view of it while the job runs.
    pub log: FileRef,
    /// CPU time the job's process group spent, in milliseconds.
    pub cpu_ms: u64,
}

/// `job.output`: what a running job wrote, so `throttle run` and `verify`
/// can echo it live. The [`JobEnded::log`] stays the whole of it; this is
/// sent to the connection that asked for the job and to no other, and is not
/// journalled, so a client that was not listening reads the log instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutput {
    pub key: JobKey,
    /// The byte offset in the job's log at which `text` begins. The next
    /// `job.output` starts where this one ends; a gap means some was not
    /// sent, and the log holds it.
    pub from: u64,
    /// The bytes written, decoded as UTF-8 with anything that is not
    /// replaced, so a chunk is always text.
    pub text: String,
}

/// `jobs` carries nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Jobs {}

/// The answer to `jobs`: the job table, running and recently ended. It is
/// what a caller reads where it used to probe the throttle's slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsList {
    pub jobs: Vec<JobRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRow {
    pub key: JobKey,
    pub class: JobClass,
    /// The job's process group.
    pub pgid: u32,
    pub started: At,
    pub state: JobState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Ended,
}

// ---------------------------------------------------------------- trees (reserved)

/// The body of the reserved `tree.*` verbs. Their fields beyond the key and
/// the tree are left to the row that decides Q6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeVerb {
    pub key: Key,
    pub tree: Tree,
}

// ---------------------------------------------------------------- beat and subscribe

/// `beat`. wsp's carries only `at`; the manager's adds its host's facts and
/// each seat's resident memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beat {
    pub at: At,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostFacts>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seats: Vec<SeatRss>,
}

/// The facts wsp used to read with `df`, `sysctl`, `pmset`, `vm_stat` and
/// `memory_pressure` per tick.
///
/// The fields after `thermal` are the throttle's (d1 Q11: wsp keeps the
/// decision and reads its facts from the beat). Each is optional both ways:
/// absent from an older manager's beat, ignored by an older wsp, and absent
/// when this host could not read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    /// The one-minute load average, in hundredths.
    pub load_centi: u32,
    pub cpus: u32,
    pub memory: MemoryPressure,
    pub swap_used_bytes: u64,
    pub disks: Vec<DiskFree>,
    pub thermal: Thermal,
    /// The five-minute load average, in hundredths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load5_centi: Option<u32>,
    /// The fifteen-minute load average, in hundredths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load15_centi: Option<u32>,
    /// `pmset -g therm`'s `CPU_Speed_Limit`, in percent: 100 when no limit
    /// is recorded. `thermal` is this, banded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_limit_pct: Option<u32>,
    /// Seconds since the person last touched a keyboard or pointer (HID idle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hid_idle_secs: Option<u64>,
    /// Memory free now, in bytes (`vm_stat`'s free pages times the page size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_memory_bytes: Option<u64>,
    /// Pages written out to swap since boot (`vm_stat`'s `Pageouts`), a
    /// counter: the throttle reads its rise between beats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pageouts: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Warn,
    Critical,
}

/// macOS's thermal state, by its own four names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskFree {
    /// The volume's mount point.
    pub volume: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRss {
    pub seat: SeatId,
    pub rss_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    /// The last id the client heard; `None` asks from the journal's start.
    #[serde(default)]
    pub from: Option<EventId>,
}

// ---------------------------------------------------------------- refusals

/// A refusal of a command or request that has no refusal message of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub why: Refusal,
    #[serde(default)]
    pub detail: String,
}

/// Why the manager refused. It refuses what is malformed, duplicate, stale or
/// beyond it; it never refuses on a judgement that is wsp's to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The frame did not decode, or broke the protocol's order (no `hello`).
    Malformed,
    /// The hello's token was wrong; the connection closes after this.
    Unauthorised,
    /// The peer speaks a [`PROTOCOL`] this manager does not.
    Protocol,
    /// The same key is already running under different arguments.
    Duplicate,
    /// The command named an epoch older than the seat's.
    StaleEpoch,
    /// No such seat.
    NoSeat,
    /// No such job.
    NoJob,
    /// No such tree on this host, or a [`HostDir`] outside its tree root.
    NoTree,
    /// Capacity is full for that class.
    Full,
    /// A verb this manager does not run (the reserved `tree.*`).
    Unsupported,
    /// The agent could not be started; `detail` says what failed.
    Failed,
    /// A reason this build does not know, from a newer peer.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests;
