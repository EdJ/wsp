//! The place-work port: what wsp asks of whatever is running its agents.
//!
//! The contract robustness-024 was opened to write. `wsp spawn` is on it —
//! `place_backend` is the first backend and the fake behind a socket is the
//! second — and the rest of the call sites are robustness-018's, still to move.
//! Read it as the answer to one question: **when wsp puts an agent on a piece
//! of work, what does it actually need from the thing underneath?**
//!
//! Ed's sentence, 2026-08-16, and the port is a transcription of it:
//!
//! > start an agent on this task, in this directory; give me a handle; tell me
//! > its state; let me send it a prompt; tell me when it stops.
//!
//! backend satisfies that by creating a workspace and splitting a pane. A bare
//! process supervisor satisfies it by forking. A web runtime satisfies it
//! however it does. **None of them has to pretend to have panes**, and that is
//! the whole of what this file is for — today `place work` means
//! `workspace.create` then `agent.start`, which is a sentence only a terminal
//! multiplexer can hear.
//!
//! # A TTY is not required
//!
//! An agent nobody is sitting in front of needs no terminal. A Claude Code
//! agent's durable output is a JSONL transcript, so *hosting* one needs a
//! supervisor rather than a PTY — a PTY is what a human attaching to it needs,
//! which is a strict superset and is the hard part. So every verb below is
//! written so that a backend which has never heard of a terminal can implement
//! all of it, and the test applied to each one was: *could a supervisor with no
//! TTY, and a web runtime with no processes, both answer this?*
//!
//! # What this port is not
//!
//! There are **two** backend ports, not one (decision on robustness-026). This is
//! the place-work port. The other is the arrange-panes port — `pane.split`,
//! `pane.close`, `pane.focus`, `pane.layout`, `pane.swap`, `pane.send_text`,
//! `tab.create`, `tab.close`, `tab.focus`, `workspace.focus` — which belongs to
//! robustness-027. Those ten verbs are what a program does when it *lives inside*
//! a multiplexer as a pane, and every one of their call sites is in `panel/` or
//! `detail/` bar one, so they are the arrange-panes port's backend adapter rather
//! than anything wsp needs from a backend.
//!
//! The split is between what wsp *means*, not between backend methods:
//! [`Place::stop`] and the arrange port's close are both `pane.close` on this
//! backend, and are still two verbs, because one ends somebody's work and the
//! other tidies a viewport. A method can serve both ports; a verb cannot.
//!
//! Reading is not here either. `pane.list`, `workspace.list`, `agent.list`,
//! `agent.get`, `pane.read` and `events.subscribe` are the observe half, whose
//! seam is [`crate::live`] — `panel::Snapshot` was that seam when this was
//! written, and now sits behind it, one view among three. [`Place::census`]
//! and [`Place::watch`] are the two places the two halves touch, and they are
//! here rather than there because a thing that starts agents is the only thing
//! that can be asked what it started.
//!
//! # The three fates of the seven verbs `place work` means today
//!
//! robustness-022 measured the place-work group at seven backend methods. They do
//! not have one fate, they have three, and telling them apart is most of the
//! design:
//!
//! | backend method | fate |
//! |---|---|
//! | `workspace.create` | ported — [`Place::open`] |
//! | `agent.start` | ported — [`Place::start`] |
//! | `agent.prompt` | ported — [`Place::tell`] |
//! | `workspace.rename` | **demoted** into the backend adapter |
//! | `pane.rename` | **demoted** into the backend adapter, and split |
//! | `workspace.report_metadata` | **deleted** |
//! | `pane.report_metadata` | **deleted** |
//!
//! **Deleted** is the easy pair and robustness-018's correction already had it
//! right, understated: the metadata is not merely display-only, it is
//! *write-only*. `Workspace.tokens` is parsed at `backend.rs:328` and read
//! nowhere in the tree, and `Pane` has no `tokens` field at all — so wsp pushes
//! six workspace tokens and four pane tokens per sync and never asks for one
//! back. They exist to draw backend's sidebar, which under the four-part
//! partition is the renderer's job (part 4) and not a capability any backend
//! owes us.
//!
//! **Demoted** is the pair robustness-018 defended, and it was right about the
//! code and wrong about the conclusion. `workspace.rename` and `pane.rename`
//! are not display projection: a claim stores `workspace_label` and four places
//! match on it — `reconcile --reap`'s aliveness test (`cmd_agent.rs:1301`),
//! reconcile's rebinding when an id has changed (`1344`),
//! `resolve::claimed_project` (`resolve.rs:410`) and the `project_for_label`
//! fallback (`resolve.rs:466`). Stop writing it and claims resolve to the wrong
//! workspace. But *why* is it a secondary key? Because backend's ids are
//! perishable and wsp had nothing durable to hold. The first duty of this port
//! is a durable handle ([`Seat`]) — so the label-as-key is not ported and not
//! deleted, it is demoted: **the backend adapter owes a durable seat, and if
//! backend cannot supply one, re-finding it by label is how the adapter keeps its
//! promise.** That is the adapter's private business, exactly where the 16-key
//! cap and the 24h TTL already live.
//!
//! `pane.rename` splits on the way down. Its other callers — `panel/install.rs`
//! labelling the panel `wsp`, `panel/verbs.rs` labelling the board and the
//! detail view, `detail/editors.rs` labelling an editor — are arrange-panes.
//! And its third caller is `wsp say`, which is below.
//!
//! # What the daemon's refresh loop was for, and what happens to it
//!
//! `REFRESH` in `daemon.rs:17` is 15 minutes, and it exists solely because
//! backend expires metadata: `sync.rs:12` says 6h "refreshed by the daemon well
//! inside backend's 24h ceiling". Delete the tokens and that justification goes
//! with them.
//!
//! One measured thing must not go with it silently. `force` does two jobs in
//! that loop: it re-pushes the tokens, and it is the **only** thing that makes
//! `sync` run when nothing woke the daemon and the store has not changed
//! (`daemon.rs:345`). `sync` reaps dead bindings as well as pushing tokens, so
//! the 15-minute tick is also the backstop for a `pane.closed` event that was
//! never delivered. Whoever removes the tokens must either re-argue that
//! backstop on its own merits or lose it on purpose. It is not inherited.
//!
//! # Where `wsp say` lives
//!
//! Not here. `say` is not a port verb, and the answer robustness-026 gives is
//! confirmed: the sentence belongs to the agent module's ephemeral state.
//!
//! The reason it looks like a backend call today is an accident of ownership —
//! backend owns the label the sidebar draws, so the sentence was put where it
//! would be seen. It writes nothing to the store, and is read back out of
//! `pane.list`. Three things that look like three different backend calls
//! (`report_metadata`, `name_bound`, `say`) are one thing: a projection of
//! wsp state onto whatever surface is drawing.
//!
//! What this port owes `say` is narrower and real: **a durable key to hang it
//! on**. Without one there is nowhere to keep "what is this agent doing right
//! now" that survives a restart, which is precisely why it ended up in a pane
//! label. [`Seat`] is that key.
//!
//! It also fixes a live defect by construction rather than by care.
//! `reconcile` ends in `name_bound` (`cmd_agent.rs:1399`), which renames every
//! bound pane to the task's label whenever it differs — and `wsp say` writes
//! its sentence into that same field. So reconcile erases every agent's
//! current sentence, today, on a daemon restart or a hand-run `wsp reconcile`.
//! The seam operation destroys the state on the far side of the seam. Once the
//! sentence stops living in the label there is nothing left to erase.
//!
//! # How an agent says which one it is
//!
//! This is the half of "give me a handle" that nobody had written down, it is
//! where a TTY-less backend breaks first, and it is settled here (decision on
//! robustness-040) — after both of the ways out that were filed turned out not
//! to exist on the only backend there is.
//!
//! Every agent-side verb — `wsp claim`, `wsp say`, `wsp release` — has to name
//! the seat it is standing in, and reads `WSP_BACKEND_PANE_ID` to do it
//! (`cmd_agent::my_pane`). The port's first answer was that wsp would put the
//! seat's own name into [`Order::env`] under [`SEAT_ENV`] and any backend would
//! carry it. **backend cannot.** `workspace.create` takes the environment and
//! *returns* the pane id, so the name does not exist until after the only moment
//! it could have been put there.
//!
//! The two ways out that were written down both fail, and why they fail is the
//! part worth keeping:
//!
//! - **wsp mints the seat** and hands it down in the environment. robustness-044
//!   did exactly this one level in — minting the agent's name rather than
//!   discovering it — but what made that work was `claude -n`: the backend
//!   *accepts* the minted name. `workspace.create` has no such parameter
//!   (checked against `backend api schema --json`, protocol 17: `label`, `cwd`,
//!   `env`, `focus`) and answers with an id of its own. So a minted seat would
//!   have to be translated back to a pane id on every call and on every census
//!   row, from a table that has to survive a restart — for an id backend already
//!   delivers. backend's own place to hang such a table is pane metadata, which
//!   the port deleted as write-only and which expires in 24h anyway. **Minting
//!   pays where the backend takes the minted id, and buys a translation table
//!   where it does not.**
//! - **The agent asks the backend** which seat it is in. backend has one method
//!   that could answer and it answers a different question. Measured 2026-08-17
//!   against backend 0.7.5, called from a shell in `w31:p1`: `pane.current` with
//!   no params replied `w1:pJ` — the **focused** pane, in another workspace, on
//!   somebody else's work. Its only parameter is `caller_pane_id`, which is the
//!   answer being asked for. A socket connection carries no identity, so this
//!   option does not exist here; and had it existed it would be a mechanism that
//!   works only while a human happens to be looking at it, which the focus
//!   decision of 2026-08-17 rules out on its own.
//!
//! What is left is what backend already does, said in the port's words: **the
//! backend delivers the seat to whatever runs in it, and the adapter says under
//! what name.** [`Place::here`] is that reading, and it is the one place that
//! knows. backend's name is `WSP_BACKEND_PANE_ID`; a backend with no convention of its
//! own uses [`SEAT_ENV`], which a supervisor can set without difficulty because
//! it mints the seat and *then* forks — it has the name in hand at the moment
//! the child's environment is fixed. backend's peculiarity is that one call makes
//! the seat and the shell together, and that is what nothing else has to repeat.
//!
//! So [`Order::env`] carries what wsp wants the agent to know and not the seat,
//! and the direction of the requirement is reversed: the port asks the backend
//! *which seat is this*, rather than telling it what to call one.
//!
//! **The answer must be local**, which is a stronger rule than the signature.
//! [`Place::here`] returns an `Option` and cannot refuse, because a backend that
//! would have to ask the network which seat it is in has not delivered the
//! handle: every agent-side verb would pay a round trip, and a failed ask would
//! be indistinguishable from a shell nobody spawned. `None` here is the ordinary
//! truthful answer — a person's own terminal — rather than [`State::Unknown`]'s
//! absence of a fact.
//!
//! # Two verbs, because the claim goes between them
//!
//! [`Place::open`] and [`Place::start`] are separate, and it is not because
//! backend happens to have two methods. `wsp spawn`'s order is **workspace,
//! claim, agent, sentence**, and the claim has to land before the agent starts,
//! because a Claude Code session runs `wsp brief` from its `SessionStart` hook
//! and reads the claim on the way in. Started first, it would open knowing
//! nothing.
//!
//! The claim is keyed on the seat, so the seat has to exist before the agent
//! does. Any backend must honour that window; it is wsp's ordering rule rather
//! than backend's shape.
//!
//! # Ending it: one verb here, and the claim released after it
//!
//! [`Place::stop`] is the other end of that sentence, and it was left out of the
//! first cut of this file on the house rule that a verb with no caller is a tax.
//! The caller exists: wsp puts agents on work one at a time, and a loop that can
//! start work and not end it makes despawning part of the loop rather than an
//! edge case. Measured the same evening, ending the agent that wrote this file:
//! `wsp release --pane w26:p1` and then `backend workspace close w26` — two
//! commands, one of them not wsp. The coordination seat drops out of wsp and
//! into the backend for the one verb wsp did not have.
//!
//! **The order is stop first, release last, and it is the reverse of what was
//! done by hand.** Both halves can fail and the two failures are not
//! comparable. A claim released against a seat that then refuses to close leaves
//! the work looking unowned while an agent is still standing in it — and nothing
//! catches that: `claim`'s live-holder guard reads bindings, so once the binding
//! is gone it will hand the same task to a second agent without a word, which is
//! two agents in one tree. A seat closed whose claim then fails to clear leaves
//! a claim over a workspace that no longer exists, which is exactly the residue
//! `reconcile --reap` exists to sweep. One failure has a sweeper and the other
//! has a way of costing somebody a morning.
//!
//! **Ending a seat ends the claim, not just the binding.** A pane exiting is an
//! accident of process lifetime and must leave the intent standing; this is a
//! decision, and it ends the same way `release` does — the claim goes, and a
//! `worked` record and a line in the task's log say who had it and for how long.
//! Leaving the claim would also not be quiet: `reconcile` rebuilds a binding
//! *from* a claim, so a claim left standing over a closed seat is one a later
//! reconcile tries to bind an agent back onto.
//!
//! None of that is in this file. The port ends a seat; the claim is the caller's
//! and lives in `cmd_spawn::despawn`, which is where the ordering above is
//! carried out and where the store is the only thing that knows a claim exists.
//!
//! # What is deliberately absent
//!
//! - **`available()`.** It is a backend question — is there a socket — and every
//!   guard on it (`cmd_spawn.rs:284`, `cmd_agent.rs:618`) prints a backend
//!   sentence. With a backend the question is answered by the backend existing,
//!   and unreachability is a [`Refusal::Unreachable`] from the call that wanted
//!   it, which arrives at the same moment the guard would have.
//! - **`focus`.** Still absent, and the argument that removed it still holds
//!   (decision on robustness-039): it belongs to the arrange-panes port. What is
//!   here is [`Order::show`], which is scaffolding rather than a verb — `spawn`
//!   has nowhere else to say `--focus` until `arrange` has an implementor.
//!   The removal condition is on the field and is not restated here.
//! - **`stop`** *was* absent, on the grounds that wsp had never despawned
//!   anything, and the trigger named for adding it was the first TTY-less
//!   backend. That was late rather than wrong: the caller was already there and
//!   is `wsp despawn`, above. What has not changed is the rule — the next verb
//!   will need a caller of its own, and this is not a licence to port the rest
//!   of backend's eighty-nine methods on the same reasoning.
//! - **retries and timeouts.** `spawn` used to retry `agent_pane_busy` for five
//!   seconds and retype with a `ctrl-u` after six, because backend types the
//!   agent's name at a shell prompt that may not be ready. That is a backend shell
//!   race and it now sits inside `place_backend`'s [`Place::start`]. It is also
//!   the answer to the one loose end in robustness-022's measurement: the single
//!   `pane.send_text` outside `panel/` and `detail/` is that retype, and it did
//!   not become an eighth port verb.
//!
//! # Ids are opaque, and that is load-bearing
//!
//! A [`Seat`] is a string wsp does not read. backend's are `w0:p3` and
//! `w0:p3@mb2`; strata's D-55 uses an index plus a generation so an id is never
//! silently reused. A seam that passes ids around as strings works; a seam that
//! assumes backend's *shape* of id does not — so nothing here parses one, and
//! `backend::split_host` stays the one place the `@` suffix means anything.
//!
//! So is a machine's address. `Machine::backend_at` is the port's word for
//! *where this machine's backend listens*, and it is opaque for the same reason
//! an id is: backend's is a unix socket path, and something reached over TCP would
//! write a host and a port in the same field without a line of wsp changing.
//! Nothing outside an adapter reads it — the model stores it, `tunnel.rs` hands
//! it to `ssh -L` because a unix socket on both ends is already backend's dialect,
//! and `place_backend::mirrored_socket` is what an empty one means.
//!
//! The `@` suffix belongs to the `Remote` decorator (decision on robustness-017):
//! it holds the connections, strips the machine on the way out and qualifies
//! ids on the way in, and the backend underneath goes on believing it talks to
//! one host. That is why this trait is object-safe — `Remote` holds a
//! `Box<dyn Place>` — and why [`Order::on`] exists at all: **a seat that does
//! not exist yet has no id to route on**, so [`Place::open`] is the single
//! place in the port where a machine is named, exactly as `workspace.create` is
//! today (`cmd_spawn.rs:82`).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::Duration;

pub use crate::base::seat::SEAT_ENV;


/// [`SEAT_ENV`], read — the whole of what a backend with no convention of its
/// own has to do to implement [`Place::here`].
///
/// Here rather than in each adapter so that the fallback is one reading and one
/// name. An empty value is nothing: a seat's environment can only be *overridden*
/// on the wire (see [`shed_env`]), so an empty string is what a stripped variable
/// looks like, and it must not be mistaken for a seat called "".
pub fn seat_from_env() -> Option<Seat> {
    std::env::var(SEAT_ENV).ok().filter(|s| !s.is_empty()).map(Seat::new)
}

/// What a spawned seat must not inherit — the marker, the rule and the list —
/// moved down to `wsp-host` with the in-process host that applies them (wsp-020).
pub use wsp_host::shed::{shed, shed_keys, CHILD_MARKER, KEPT, PI_SESSION};

/// The same list as the entries an [`Order`] carries to shed them.
///
/// **Emptied rather than removed, because removal is not on the wire.** A seat's
/// environment is a string-to-string map on the call that creates it — backend's
/// `workspace.create` takes one and `agent.start` takes none — so the only strip
/// a port can express is an override, and an empty value is the strongest one
/// there is. That it is strong enough was measured rather than assumed; the
/// table on [`CHILD_MARKER`] is that measurement, and an agent started with all
/// of these empty came up, answered and saved its transcript.
pub fn shed_env() -> BTreeMap<String, String> {
    shed_keys().into_iter().map(|k| (k, String::new())).collect()
}

/// The seat handle, moved down to `wsp-host` with the host that answers in it
/// (wsp-020).
pub use wsp_host::port::Seat;

/// What to open a seat for: a name, a place on disk, an environment, and
/// optionally a machine.
///
/// `label` is not decoration and not a key. It is what a person reading a list
/// of seats has to go on, and every backend has some list — a sidebar, a
/// process table, a page. wsp writes `render-109 · <title>` into it
/// (`cmd_agent::task_label`) because that is legible in ten columns. What it is
/// *not*, after this port lands, is how wsp finds the seat again; see [`Seat`].
#[derive(Debug, Clone, Default)]
pub struct Order {
    pub label: String,
    /// Where the work lives. `None` means the backend's own default, which for
    /// backend is wherever it would have opened a shell.
    pub cwd: Option<String>,
    /// Handed to whatever runs in the seat. wsp puts `WSP_PROJECT`, `WSP_TASK`
    /// and the store's own variables here.
    ///
    /// The seat's own name is **not** among them and cannot be: wsp does not
    /// know it yet. That is [`Place::here`]'s half, and the module docs carry
    /// the argument.
    pub env: BTreeMap<String, String>,
    /// Which machine, when it is not this one.
    ///
    /// The only place in the port a machine is named, and only because a seat
    /// that does not exist has no id to route on. The `Remote` decorator reads
    /// this and strips it; the backend beneath never sees it.
    pub on: Option<String>,
    /// Whether to put the new seat in front of the person — where there is a
    /// person and a front.
    ///
    /// **Scaffolding, with a removal condition. It is not part of this port's
    /// shape and must not be reasoned from.** Focus belongs to the arrange-panes
    /// port ([`crate::arrange`], "Focus"), and the decision on robustness-039
    /// settled that it stays there: focus is a state that can be applied
    /// headlessly, and a port every backend must implement without a screen
    /// cannot carry a statement about what a person is looking at.
    ///
    /// This field exists anyway because today it is the only way `spawn` can
    /// express `--focus`. A seat cannot be arranged before it exists, and
    /// there is no spec to declare the arrangement into — `arrange` has no
    /// implementor and `cmd_spawn.rs` holds one port. Without this field the
    /// flag could not be said at all.
    ///
    /// **The removal is not "delete this field", and that is why the trigger is
    /// written here rather than left to be noticed.** Something has to sequence a
    /// spawn that both places work and declares an arrangement, and nothing does.
    /// So the acceptance test for removal is: **`spawn` declares focus into a
    /// [`crate::arrange::Spec`]** — `arrange` has an implementor, `cmd_spawn`
    /// holds both ports, and `--focus` becomes one line of desired state
    /// applied once instead of a call sequenced after another. On that day this
    /// field goes and `cmd_spawn::order` loses its `show` argument. Until then it
    /// stays, and no second caller is added to it.
    ///
    /// Writing the trigger down is the whole point: this field was justified once
    /// as permanent, `arrange` recorded the opposite as settled, and both files
    /// read as closed. Scaffolding with no stated trigger is how that happened.
    ///
    /// It is not backend's `focus` parameter wearing a different name: what wsp
    /// means is *do not drag the screen away from what somebody is reading*, and
    /// a backend with no screen honours it by ignoring it. `false` by default,
    /// so a seat opened by something with no opinion does not steal attention —
    /// which is the same rule read from the other end, and stays true wherever
    /// the flag ends up living.
    pub show: bool,
}

/// Which agent to start, and what to call it.
///
/// `kind` is passed through rather than enumerated. backend refuses an unknown
/// kind with its whole catalogue in the message, which is a better list than
/// one kept here and left to go stale — and a second backend's catalogue is its
/// own. `name` is what the backend should call the running agent; wsp passes
/// the task or project id.
///
/// `args` is the third thing and it is not decoration: what an agent is started
/// with decides what it reads before it does anything, and on 2026-08-17 that
/// was measured at 37,756 tokens re-read on every request of the session
/// (`cmd_spawn::TRIM` holds the numbers and the argument). A port that could
/// only say *which* agent, never *how*, would have had a third of every
/// spawned session's cost outside it. It is passed through like `kind` — these
/// are the agent's own flags, not the backend's, and a backend that inspected
/// them would be enumerating a catalogue that belongs to somebody else.
#[derive(Debug, Clone, Default)]
pub struct Agent {
    pub kind: String,
    pub name: String,
    pub args: Vec<String>,
    /// The row and attempt `seat.spawn` is keyed by (3.3). `None` for a seat
    /// with no row: the host then keys it by its own freshly minted seat id,
    /// attempt 0, which is unique by construction.
    pub key: Option<Key>,
}

pub use wsp_host_api::{FirstOrder, Key, OrderId, RowId};

/// What the host said of an order, heard as its events (`order.acked`,
/// `order.refused`) rather than read off a seat's state
/// (`docs/agent-manager.md` 4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// `order.acked`: the order was delivered and a turn began on it.
    Acked,
    /// `order.refused`, or the seat stopped before an answer: why, worded.
    Refused(String),
    /// Nothing about the order within the wait.
    Unanswered,
}

/// A fresh id for a first order: the seat, the time and a counter, so a
/// re-start of the same seat is a new order and never the answer to the last.
pub fn new_order_id(seat: &Seat) -> OrderId {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    OrderId(format!("{seat}-first-{nanos:x}-{n}"))
}

/// What a seat is doing, moved down to `wsp-host` with the host that reads it
/// off the screen (wsp-020).
pub use wsp_host::port::State;

/// The census row. `session` is the agent's own session id where the backend
/// knows one — Claude Code's, which a binding already records and which is what
/// ties a seat to the JSONL transcript a TTY-less agent leaves behind.
#[derive(Debug, Clone, Default)]
pub struct Seated {
    pub seat: Seat,
    pub label: String,
    pub cwd: String,
    pub agent: Agent,
    pub state: State,
    pub session: String,
}

/// What every backend said when it was asked what it holds.
///
/// **Not a `Vec<Seated>`, and that difference is the whole of this type.** A
/// flat list can say what is running; it cannot say *who was asked*. So a
/// caller that reaps on the strength of one is a network blip away from handing
/// back every task an executor holds: unreachable, answering-with-nothing and
/// never-asked all arrive as the same empty list, and the only party that can
/// tell them apart is the one that made the calls and got the errors.
///
/// wsp has that judgement today and gets it by **parsing the id** —
/// `cmd_agent::machine_of` reads backend's `@mb2` suffix and partitions the live
/// list on it. That works only while wsp knows one backend's shape of id, which
/// is exactly what a [`Seat`] is not allowed to have and what the `Remote`
/// decorator makes private. This type is where the fact comes from instead: the
/// fan-out *already knows* which machine answered — `backend::each` is the line
/// that knows — and until now it threw the answer away and let a caller
/// reconstruct it from the ids that came back.
///
/// [`Census::heard`] and [`Census::silent`] are different constructors and there
/// is no `Default`. The same shape [`crate::arrange::World`] takes, for the same
/// reason: **the cost of this bug is a `Vec::new()` that looks like an answer.**
///
/// **A partial answer is good news.** A census across four machines with one
/// down is three machines' worth of fact, and reading it as a failed call would
/// empty the panel every time a laptop closed — the rule `backend::everywhere`
/// already states as *this machine's failure is an error and a far machine's is
/// not*. [`Refusal::Unreachable`] is kept for the case it names, nobody answered
/// at all, and [`Place::census`] says when that is returned.
///
/// # What this settles, and what has to go with it
///
/// It settles half of the question, and the half it does not settle is the one
/// that would otherwise be discovered late. A census can only speak about seats
/// it can *see*, and a seat a reap is deciding about is by definition in no
/// census — that is what makes it a candidate. So the machine names here have to
/// meet a machine name on wsp's own record of the seat, and an opaque handle
/// cannot be asked which one it is.
///
/// That record can carry it, and not by parsing anything, because wsp names the
/// machine at every door a seat comes in by: [`Order::on`] when wsp opens one,
/// `WSP_MACHINE` — which the executor shim already reads, and today only spends
/// on qualifying ids — when an agent out there claims one, and this type when a
/// seat is adopted from a backend that was asked. The decision is on
/// robustness-059 and the record is robustness-058's.
#[derive(Debug, Clone)]
pub struct Census {
    said: Vec<(String, Result<Vec<Seated>>)>,
}

impl Census {
    /// A backend answered. An empty answer is a fact: it holds nothing.
    ///
    /// `machine` is [`Order::on`]'s name for it and `""` is this one — the same
    /// spelling on both sides of the port, so that what went out as `--on mb2`
    /// comes back as `mb2` without anything in between reading an id.
    pub fn heard(machine: &str, seats: Vec<Seated>) -> Census {
        Census { said: vec![(machine.to_string(), Ok(seats))] }
    }

    /// A backend said nothing, and why.
    ///
    /// The row that has no representation in a `Vec<Seated>` and the reason this
    /// type exists. Keeping the [`Refusal`] rather than a flag is what lets a
    /// reader tell a partition from a machine that is misconfigured, without
    /// either of them being confused with an idle one.
    pub fn silent(machine: &str, why: Refusal) -> Census {
        Census { said: vec![(machine.to_string(), Err(why))] }
    }

    /// Two censuses as one — what a fan-out folds its answers with.
    pub fn and(mut self, other: Census) -> Census {
        self.said.extend(other.said);
        self
    }

    /// Every seat every backend that answered is holding.
    ///
    /// What a caller who only wants the rows reads, and it is safe for exactly
    /// those callers: a seat that is *here* is here whoever else was silent.
    /// Anything concluding from an **absence** must ask [`Census::answered`]
    /// first, and absence is the only question this method cannot answer.
    pub fn seats(&self) -> impl Iterator<Item = &Seated> {
        self.said.iter().filter_map(|(_, s)| s.as_ref().ok()).flatten()
    }

    /// The seats one backend is holding, and none if it said nothing.
    pub fn on<'a>(&'a self, machine: &'a str) -> impl Iterator<Item = &'a Seated> {
        self.said
            .iter()
            .filter(move |(m, _)| m == machine)
            .filter_map(|(_, s)| s.as_ref().ok())
            .flatten()
    }

    /// Whether this machine replied at all.
    ///
    /// **The judgement everything that reaps turns on**, and it is deliberately
    /// only half of one: this says *heard from*, and whether an answer of
    /// nothing is evidence that the work stopped is a policy the caller owns.
    /// `reconcile --reap` says it is not — a backend restoring a session answers
    /// with an empty list for a second or two, and reaping on that is
    /// render-040, every binding in the store gone — so the reap's rule is
    /// this **and** a seat of its own on the same machine. That is one rule in
    /// one place (`cmd_agent::may_reap`), not a second one here; what this type
    /// changes is that the caller can now tell the three silences apart at all.
    pub fn answered(&self, machine: &str) -> bool {
        self.said.iter().any(|(m, s)| m == machine && s.is_ok())
    }

    /// Every machine that answered, and what it holds — for a caller that
    /// keeps a record per machine and may drop a seat only when its own
    /// machine answered without it (`wsp-250`'s clocks). A machine that was
    /// silent, or never asked, is not here.
    pub fn answers(&self) -> impl Iterator<Item = (&str, &[Seated])> {
        self.said.iter().filter_map(|(m, s)| s.as_ref().ok().map(|s| (m.as_str(), s.as_slice())))
    }

    /// Whether anybody answered.
    pub fn was_heard(&self) -> bool {
        self.said.iter().any(|(_, s)| s.is_ok())
    }

    /// Who said nothing, and why. For a reader that draws it or logs it.
    pub fn unheard(&self) -> impl Iterator<Item = (&str, &Refusal)> {
        self.said.iter().filter_map(|(m, s)| s.as_ref().err().map(|e| (m.as_str(), e)))
    }
}

/// Something the backend noticed, without being asked.
///
/// "Tell me when it stops" is the clause of Ed's sentence that has no polling
/// equivalent: a seat that has gone quiet looks exactly like a seat nobody
/// asked about. A backend that cannot push must poll behind [`Place::watch`]
/// and raise these itself — which is what the daemon does today, on a 20s tick,
/// because backend's `pane.agent_status_changed` cannot be subscribed to globally
/// and one entry missing its `pane_id` refuses every other entry with it
/// (`daemon.rs:19`). That fallback becomes the adapter's, and stops being
/// something every caller has to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A seat now exists that did not before, including one a person made.
    Opened(Seat),
    /// An agent has started in a seat.
    Started(Seat),
    /// A running agent changed state.
    Moved(Seat, State),
    /// An agent has stopped. The seat may still be there.
    Stopped(Seat),
    /// The seat itself is gone.
    Closed(Seat),
}

/// Why a call did not happen and what became of a delivery, moved down to
/// `wsp-host` with the host that answers in them (wsp-020).
pub use wsp_host::port::{Delivery, Refusal, Result};


/// A turn's end as a seat's hooks recorded it — [`Place::stopped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    /// The `Stop` hook's time, in epoch seconds.
    pub at: i64,
    /// The transcript the session was writing, where the hook named one.
    pub transcript: Option<String>,
}

/// [`Place::stopped`] off a seat's `said.json`: a `Stop` that is still the
/// last word, with a time that reads.
pub(crate) fn stopped_of(said: &serde_json::Value) -> Option<Stopped> {
    let s = |k: &str| said.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if s("hook") != "Stop" {
        return None;
    }
    let at = crate::util::epoch_of(&s("at"));
    if at == 0 {
        return None;
    }
    let path = s("transcript_path");
    Some(Stopped { at, transcript: (!path.is_empty()).then_some(path) })
}

/// Where wsp puts work.
///
/// Nine methods. Every one of them is a clause of the sentence at the top of
/// this file, and nothing in any signature names a pane, a window, a tab or a
/// terminal. [`Place::here`] is the eighth and is the one clause read from the
/// inside: *give me a handle* is two directions, and it was one for a while.
/// [`Place::nudge`] is the ninth and the only one with a body here, because it
/// is the only one a backend is entitled not to have.
///
/// Object-safe deliberately: `Remote` is a decorator holding a
/// `Box<dyn Place>`, and a backend is chosen at runtime rather than compiled
/// in.
pub trait Place {
    /// Reserve somewhere an agent can run, and give it back by a durable name.
    ///
    /// No agent starts here. That is the whole reason this is its own verb —
    /// the claim is written between [`Place::open`] and [`Place::start`], and
    /// an agent that starts first opens knowing nothing.
    ///
    /// The seat must carry [`Order::env`] to whatever runs in it, must be
    /// findable by the returned [`Seat`] after the backend has restarted, and
    /// must leave its occupant able to answer [`Place::here`] with the same
    /// string this returned.
    fn open(&self, order: &Order) -> Result<Seat>;

    /// Which seat *this* process is standing in, if any.
    ///
    /// The handle's other direction, and the only verb here that is not about
    /// somebody else: every agent-side command — `wsp claim`, `wsp say`,
    /// `wsp release`, `wsp despawn`'s refusal to end the seat it is running in —
    /// is downstream of this one reading.
    ///
    /// Answered from what the backend arranged before the process started, never
    /// by asking it. The module docs carry both halves of that: why backend cannot
    /// be asked, and why a backend that had to be is one that has not delivered
    /// the handle. [`seat_from_env`] is the whole implementation for a backend
    /// with no name of its own for the seat.
    ///
    /// `None` is a real answer — a shell a person opened, a cron job, a build —
    /// and every caller already has a path for it, because `wsp` has always been
    /// runnable from a terminal that is nobody's seat.
    fn here(&self) -> Option<Seat>;

    /// The room a seat is recorded under: the key a custodial slot is written
    /// against ([`crate::cmd_govern::take`]), and a build tree after it.
    ///
    /// Defaulted to the seat itself, because only backend has anything above a
    /// seat. A supervisor's or compound's seat is its own room, and that is
    /// what `wsp govern` has always recorded for one from inside it
    /// (`cmd_govern::room_and_pane`). This used to be backend's `pane.list`, asked
    /// outside the port. It answered nothing for a `cpd-` seat, so a rotation
    /// onto compound confirmed its successor's turn and then refused to move
    /// the slot (`wsp-128`): the seat stayed with a pane that was about to be
    /// ended.
    ///
    /// `None` means the backend has rooms and could not say which one this seat
    /// is in. A caller must not guess one.
    fn room(&self, seat: &Seat) -> Option<String> {
        Some(seat.to_string())
    }

    /// Start an agent in a seat.
    ///
    /// Returns when the agent exists, not when it will take a prompt — those
    /// are different moments and conflating them is the `agent_not_ready` bug.
    /// Poll [`Place::state`] for the second one.
    ///
    /// Whatever a particular backend has to do to make this reliable — retry a
    /// shell that is not ready, clear a half-typed line, wait on a container —
    /// happens in here and is never the caller's business.
    fn start(&self, seat: &Seat, agent: &Agent) -> Result<()>;

    /// Start an agent with its first order in the one `seat.spawn`, and come
    /// back when the host's events say what became of it: `seat.started`,
    /// then `order.acked` or `order.refused` for `order`, within `wait`
    /// (`docs/agent-manager.md` 4.3). Nothing here reads [`Place::state`] or a
    /// screen: the answer is heard, not polled for.
    ///
    /// `None`: this backend has no host whose events it can wait by (the
    /// in-process host), and nothing was started; the caller starts, waits for
    /// ready and tells, as before. `Err` is the start refused, or the host
    /// silent, said as such.
    fn start_ordered(&self, _seat: &Seat, _agent: &Agent, _order: &FirstOrder, _wait: Duration) -> Option<Result<Heard>> {
        None
    }

    /// Go on waiting, up to `wait`, for the answer to an order
    /// [`Place::start_ordered`] sent: after a [`Place::nudge`], say. `None`
    /// where this backend has no events to wait by.
    fn await_order(&self, _seat: &Seat, _order: &OrderId, _wait: Duration) -> Option<Result<Heard>> {
        None
    }

    /// Give the agent in a seat a sentence to act on.
    ///
    /// Refuses with [`Refusal::NotReady`] rather than typing into whatever is
    /// there. Not "send keystrokes": a backend with no terminal has no
    /// keystrokes, and a backend with one should use its own submit rather than
    /// a sleep long enough for a TUI to finish pasting.
    ///
    /// `Ok` is *delivered*, and on most backends it is not *taken*. The backend
    /// accepted the call; whether the agent started a turn on it is
    /// [`Place::state`]'s question and [`Place::nudge`] is what to do about the
    /// answer.
    ///
    /// A backend that can watch for the turn itself is entitled to answer the
    /// second question here instead, and should: [`Refusal::NotTaken`] is
    /// *delivered and no turn started*, which is the reading a caller would
    /// otherwise have had to go and take. It is not a failure and a caller that
    /// treats it as one has thrown away the recovery. What it is not entitled to
    /// do is make `Ok` mean less than it did — a backend that watched and one
    /// that could not both say `Ok` when there is nothing to rescue.
    ///
    /// Which is why the `Ok` says [`Delivery`]. There are three outcomes and not
    /// two: watched and started, watched and stalled ([`Refusal::NotTaken`]),
    /// and delivered with nothing to watch — the last being the agent that was
    /// already mid-turn, which is the ordinary case for anything said to a
    /// governor. A backend that cannot watch answers [`Delivery::Unconfirmed`]
    /// to all of it, which is the truth about what it knows.
    fn tell(&self, seat: &Seat, text: &str) -> Result<Delivery>;

    /// Submit whatever is sitting unsent in the agent's input.
    ///
    /// The one verb here that exists because another verb cannot keep a promise
    /// it looks like it makes. On a backend that reaches an agent by typing at
    /// it, the sentence and the submit are two separate things arriving at a TUI
    /// that is still finishing the first, and the second is dropped: the work
    /// order sits in the composer, the agent reads `idle`, and every reading wsp
    /// has says the spawn went well. Measured through robustness-035: four of
    /// six agents in one burst, then three of three in a quiet moment, then
    /// three of three again on the bare socket with no wsp in the call — so it
    /// is the backend's failure and backend 0.8.0's own wait between the text and
    /// the Enter narrowed the window rather than closing it. A return pressed
    /// afterwards started the turn in 0.15s every time.
    ///
    /// It presses submit and says nothing about what is being submitted, which
    /// is what keeps it from being a keystroke API: there is no key in the
    /// signature and no second use for it. A caller has to have sent something
    /// and watched it not be taken to have any business here.
    ///
    /// [`Refusal::Unsupported`] by default, because a backend where this cannot
    /// happen has nothing to implement. A supervisor writes the prompt into a
    /// pipe the agent is reading — there is no half-arrived state to rescue, and
    /// a caller that gets this answer should say the turn never started rather
    /// than go on pressing.
    fn nudge(&self, _seat: &Seat) -> Result<()> {
        Err(Refusal::Unsupported("submit a prompt that arrived and was not taken"))
    }

    /// Whether the agent's input line is holding typed text, read off its
    /// screen: `Some(true)` it is, `Some(false)` it is empty, `None` this
    /// backend cannot read it — no screen, or an agent whose prompt it does
    /// not know the shape of.
    ///
    /// **The reading [`Place::nudge`]'s rescue was missing** (`wsp-252`).
    /// Without it, a submit that did not start a turn left two stories wsp
    /// could not choose between — the text in the composer, or the text
    /// dropped — and the two have opposite repairs: a return for the first,
    /// a resend for the second, and a resend into the first leaves the order
    /// in the prompt twice. [`submit`] presses only on `Some(true)`.
    fn typed(&self, _seat: &Seat) -> Option<bool> {
        None
    }

    /// End whatever agent is in a seat, and let the seat go.
    ///
    /// **Not the arrange-panes port's `close`.** They are spelled with the same
    /// backend method and they are not the same verb: this one ends somebody's
    /// work, and that one tidies a viewport. A backend is entitled to implement
    /// the two identically; a caller that confuses them shuts down an agent to
    /// close a detail pane.
    ///
    /// One call, and it must be the whole of it — the agent, and the seat it was
    /// standing in. A backend that can only stop the process leaves a seat wsp
    /// will go on counting; one that can only drop the seat leaks the process.
    ///
    /// [`Refusal::NoSeat`] where there was nothing there. That is not a failure
    /// to a caller whose aim is a seat that is gone — an agent whose backend
    /// crashed under it is the ordinary case for this verb — so the caller
    /// treats it as the first half already done, and goes on to the claim.
    fn stop(&self, seat: &Seat) -> Result<()>;

    /// [`Place::stop`], and the pids that ended where the host says so: the
    /// manager's `seat.stopped` under `WSP_HOST=manager`. Empty where the
    /// backend does not name them.
    fn end(&self, seat: &Seat) -> Result<Vec<u32>> {
        self.stop(seat).map(|()| Vec::new())
    }

    /// Whether the host is the manager, whose pids a report names in place of
    /// the ones wsp would find by `ps`.
    fn hosted(&self) -> bool {
        false
    }

    /// What is happening in one seat, right now.
    ///
    /// Separate from [`Place::census`] because `spawn` polls a single seat
    /// every 150ms for up to 30 seconds, and a census is a fan-out across every
    /// reachable machine.
    ///
    /// It is also the reading a backend is expected to be exact about. backend's
    /// census can be exact too, but only because it asks twice — the asymmetry
    /// is between the calls it has, not between one seat and many; see
    /// `place_backend`.
    fn state(&self, seat: &Seat) -> Result<State>;

    /// Whether a turn *began* at this seat at or after `since` (epoch seconds),
    /// for a backend that keeps a record of turns rather than only a present
    /// tense. `None` is *this backend cannot say*, and the caller falls back to
    /// sampling [`Place::state`].
    ///
    /// **Why a sample is not enough.** `wsp-166`: a governor's reply to a typed
    /// batch is one to two seconds long, the pass samples every twenty, and a
    /// turn that starts and ends between two samples is never seen — so the
    /// batch it answered stayed owed and was typed again every `RETYPED` for
    /// a night. A compound seat's hooks write down every turn start, so the
    /// question has an exact answer there.
    fn turn_began_since(&self, _seat: &Seat, _since: i64) -> Option<bool> {
        None
    }

    /// The last moment anything was written about this seat, in epoch seconds
    /// — the clock a census reads to say *how long* a seat has been gone, which
    /// nothing else here keeps. `None` is *this backend has no record to read
    /// it off*, and a caller must treat that as "unknown", never as "long ago":
    /// a seat nobody can date is a seat nobody may sweep (`wsp-169`).
    ///
    /// **A lower bound on when the agent stopped, not the moment.** A pid that
    /// dies is not stamped by anything, and a dead agent writes nothing, so the
    /// last word it left is the nearest thing to a time of death a backend has.
    /// For an agent that was working that is within a turn; for one that sat
    /// idle for hours and was then killed it reads too old, which is why the
    /// only caller sweeps nothing that still holds a claim.
    fn quiet_since(&self, _seat: &Seat) -> Option<i64> {
        None
    }

    /// When this seat's last turn ended — its `Stop` hook, in epoch seconds —
    /// and the transcript that turn was written to, while the last thing the
    /// seat said about itself is that `Stop` (`wsp-606`). `None` is *no stop is
    /// the last word here*, or this backend keeps no hooks: a caller falls back
    /// to whatever clock it had before.
    fn stopped(&self, _seat: &Seat) -> Option<Stopped> {
        None
    }

    /// Every process that is this seat's agent: the one the backend started and
    /// everything under it, read now. `None` is *this backend cannot name one*.
    ///
    /// **The tree and not the one pid, because a closed seat is not an ended
    /// agent.** An agent was found alive at `ppid=1` after its seat had been
    /// despawned, still holding a conversation a person was in. A caller that
    /// means to say "ended" reads this *before* [`Place::stop`] and asks after
    /// it whether any of them are still running; the pid the backend recorded is
    /// only the root, and the root is the one most likely to have gone.
    fn pids(&self, _seat: &Seat) -> Option<Vec<u32>> {
        None
    }

    /// The process this backend started for the seat — the root [`Place::pids`]
    /// walks down from — off its own record, without reading the process table.
    /// `None` is *this backend cannot name one*.
    ///
    /// For a reader that walks many seats over one `ps` rather than one per
    /// seat: [`crate::waiting::Shells`], asked on every `wsp wip`.
    fn root(&self, _seat: &Seat) -> Option<u32> {
        None
    }

    /// Every seat this backend has, and what is in it.
    ///
    /// The one read that stays on this side of the line: what a backend is
    /// running is a question only the thing running it can answer. An error is
    /// not an empty list, and callers reaping anything on the strength of this
    /// must treat the two differently — that rule is older than this port
    /// (`sync.rs:41`) and survives it.
    ///
    /// [`Census`] is that rule made unsayable-wrong rather than remembered: the
    /// answer is per backend, so a machine that was silent is a row of its own
    /// instead of an absence of rows. **A partial answer is an answer** — one
    /// machine down out of four is three machines' worth of fact — so
    /// [`Refusal::Unreachable`] is returned only when *nothing* answered, which
    /// is the sentence it already carries and the moment `backend::available()`
    /// used to be asked in advance.
    fn census(&self) -> Result<Census>;

    /// Block, calling `f` for each event, until `f` returns false.
    ///
    /// The only way to learn that an agent stopped. A backend that cannot push
    /// polls in here.
    fn watch(&self, f: &mut dyn FnMut(Event) -> bool) -> Result<()>;
}

/// How long [`submit`] watches a typed sentence, and how often it presses again.
pub struct Pressing<'a> {
    /// How long each look lasts before the next press, in order. One more
    /// window than presses: the last is the look after the last press.
    ///
    /// Growing, because what a press is waiting on is the TUI getting to it,
    /// and a TUI that was not there at two seconds is more likely there at
    /// five than at four. Eighteen seconds in all, paid only by a seat that is
    /// holding the text unsent the whole way through; a turn that starts ends
    /// the wait at the next poll.
    pub windows: &'a [Duration],
    pub poll: Duration,
    pub clock: &'a dyn crate::util::Clock,
}

impl Default for Pressing<'static> {
    fn default() -> Pressing<'static> {
        const WINDOWS: [Duration; 4] = [
            Duration::from_millis(2_000),
            Duration::from_millis(3_000),
            Duration::from_millis(5_000),
            Duration::from_millis(8_000),
        ];
        Pressing { windows: &WINDOWS, poll: Duration::from_millis(150), clock: &crate::util::Wall }
    }
}

/// [`Place::tell`], and then make sure the sentence was submitted: the one
/// path every send takes — a spawn's work order, `wsp tell`, `govern --tell`,
/// a wake — because each `Kind::tell` that types at a seat comes through here.
///
/// **Why this exists** (`wsp-252`, with `wsp-251` and `wsp-257` folded in).
/// On 2026-10-08 the run's own spawns came up with their work orders typed
/// into the prompt and not submitted, ten times in one list and three in
/// another, at loads from 3 to 540, and once until Ed pressed Enter by hand
/// an hour later. The backend underneath is compound, which types and presses
/// and cannot watch, so `Ok` said *delivered* and nothing looked again.
///
/// **What counts as taken.** A turn that began at the seat since the send
/// ([`Place::turn_began_since`], or a turn in flight where the backend keeps no
/// record), or — for an agent that was already mid-turn, whose sentence is
/// queued and starts nothing — the prompt reading empty. An idle agent's empty
/// prompt is not taken on its own: the screen may not have drawn the text yet,
/// and calling that a delivery is the false success this replaces.
///
/// **What is pressed, and only then.** The prompt read back holding text
/// ([`Place::typed`] is `Some(true)`) at the end of a window, at an agent that
/// was idle when it was typed at. A backend that
/// cannot read its prompt gets no press at all and its own answer back, as
/// before: a return into a prompt nobody has read may submit somebody's draft.
///
/// `Err(Refusal::Unsubmitted)` is the text still sitting there after the last
/// press, and a backend's [`Refusal::NotTaken`] is treated as the first look
/// rather than as an answer, because it is the same story told earlier.
pub fn submit(place: &dyn Place, seat: &Seat, text: &str, how: &Pressing) -> Result<Delivery> {
    // A backend that cannot read the prompt takes the path it always took,
    // call for call: nothing below has anything to go on without the reading.
    if place.typed(seat).is_none() {
        return place.tell(seat, text);
    }
    let busy = place.state(seat).is_ok_and(|s| s.turn_in_flight());
    // Taken before the type: a turn's start is stamped in whole seconds while
    // the type is still returning, so a stamp taken after could postdate the
    // very turn it is looking for. `wake` takes its own the same way.
    let since = crate::util::epoch_secs();
    let first = match place.tell(seat, text) {
        Ok(Delivery::Started) => return Ok(Delivery::Started),
        // The supervisor's own acknowledgement: it owns the retry, so wsp
        // neither watches for a turn nor presses (wsp-477).
        Ok(Delivery::Acked) => return Ok(Delivery::Acked),
        Ok(Delivery::Unconfirmed) => Ok(Delivery::Unconfirmed),
        Err(Refusal::NotTaken) => Err(Refusal::NotTaken),
        Err(e) => return Err(e),
    };
    let turned = || match place.turn_began_since(seat, since) {
        Some(began) => began,
        None => !busy && place.state(seat).is_ok_and(|s| s.turn_in_flight()),
    };
    for (i, window) in how.windows.iter().enumerate() {
        let deadline = how.clock.now() + *window;
        loop {
            if turned() {
                return Ok(Delivery::Started);
            }
            if busy && place.typed(seat) == Some(false) {
                return Ok(Delivery::Unconfirmed);
            }
            if how.clock.now() >= deadline {
                break;
            }
            how.clock.rest(how.poll);
        }
        // A sentence behind a turn is the agent's to take when the turn ends,
        // and a prompt that holds it meanwhile is not a lost submit: every
        // failure this was written for was an idle seat, and a press into a
        // turn is a keystroke at somebody's work.
        if busy {
            return first;
        }
        match place.typed(seat) {
            Some(true) if i + 1 < how.windows.len() => {
                if place.nudge(seat).is_err() {
                    return Err(Refusal::Unsubmitted(seat.clone()));
                }
            }
            Some(true) => return Err(Refusal::Unsubmitted(seat.clone())),
            // Empty, or no longer readable, and no turn: the backend's own
            // answer is all there is, and pressing at nothing is not a rescue.
            _ => return first,
        }
    }
    first
}

/// What the backends and the hook verb ask of the layers above wire, which `main`
/// installs at startup ([`install_above`]): wire does not name them.
pub struct Above {
    /// The program and argv an agent of a kind starts as (`agent_commands::command`).
    pub command: fn(&str, &[String]) -> (String, Vec<String>),
    /// Whether a hook can end a question (`hold_close::closes`).
    pub closes: fn(&str) -> bool,
    /// What a hook says about the questions its session holds (`hold_close::heard`).
    pub heard: fn(&crate::store::Store, &str, &serde_json::Value) -> usize,
    /// Wait on the person for a permission request (`hold::hold`).
    pub hold: fn(&crate::store::Store, &serde_json::Value, u64) -> i32,
    /// How long that waits unless told otherwise (`hold::SECS`).
    pub hold_secs: u64,
}

static ABOVE: std::sync::OnceLock<Above> = std::sync::OnceLock::new();

pub fn install_above(a: Above) {
    let _ = ABOVE.set(a);
}

/// The installed answers. A process that installed none starts the kind's own name, closes and holds nothing.
pub fn above() -> &'static Above {
    {
        static NONE: Above = Above {
            command: |kind, args| (kind.to_string(), args.to_vec()),
            closes: |_| false,
            heard: |_, _, _| 0,
            hold: |_, _, _| 0,
            hold_secs: 86_400,
        };
        ABOVE.get().unwrap_or(&NONE)
    }
}

/// Where a pane is on screen, in cells.
///
/// Read, never written; see rule 5. It is here because two questions need it and
/// neither can be answered from a list: which pane is widest, and which of these
/// is on the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{is_agent_id, new_agent_id};

    /// What a seat's occupant must not find, and where the line is drawn.
    ///
    /// The rule sheds every `CLAUDE_` name so that the one Claude Code adds next
    /// month is shed without anybody noticing it exists — which is the whole
    /// defect, twice: [`CHILD_MARKER`] cost a measurement its transcript, and
    /// `CLAUDE_JOB_DIR` put a dead governor's job name on every seat's status
    /// bar. A configuration directory is a fact about the machine and is kept by
    /// name.
    #[test]
    fn a_new_session_does_not_inherit_the_one_that_spawned_it() {
        for key in [
            "CLAUDECODE",
            CHILD_MARKER,
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_PID",
            "CLAUDE_JOB_DIR",
            "CLAUDE_EFFORT",
            "CLAUDE_CODE_SOMETHING_ADDED_LATER",
            "CLAUDE_SOMETHING_ADDED_LATER",
            // A pi governor's bash tool, spawning: what it hands the command it
            // runs, and what pi/wsp-session.ts reads as being nested.
            "PI_SESSION_ID",
            "PI_SESSION_FILE",
        ] {
            assert!(shed(key), "{key} names the spawning session and would have reached the seat");
        }
        for key in ["CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR", "WSP_TASK", "WSP_BACKEND_SOCKET_PATH", "PATH", SEAT_ENV] {
            assert!(!shed(key), "{key} is not an identity and the seat is poorer without it");
        }
    }

    /// The marker is shed whether or not the caller is carrying it, and the
    /// strip is an empty value rather than an absence.
    ///
    /// Both halves are forced by where the leak can come from. wsp is not the
    /// only carrier — a backend started by hand inside an agent's pane hands the
    /// marker to every seat it will ever open, and a `wsp` run from a clean
    /// shell against that server sees nothing of its own to strip. And a seat's
    /// environment is a map on the call that creates it, so there is no way to
    /// spell "unset" on the wire at all.
    #[test]
    fn the_marker_is_shed_by_a_caller_who_does_not_have_it() {
        let env = shed_env();
        assert_eq!(
            env.get(CHILD_MARKER).map(String::as_str),
            Some(""),
            "a spawn from a clean shell onto a dirty backend still has to say this"
        );
        assert!(env.values().all(String::is_empty), "an override is the only strip on the wire");
        assert!(env.keys().all(|k| shed(k)), "the order carries nothing the rule did not name");
    }

    /// The one question every caller of `state == "idle"` is actually asking,
    /// and the two answers that cost the most.
    ///
    /// An agent still coming up is not idle, however loudly the backend says it
    /// is — that is the `agent_not_ready` bug, and it is why [`State::Starting`]
    /// exists. Silence is not a state either: `Unknown` is refused a prompt
    /// because telling an agent we cannot see is how a work order lands in
    /// somebody's shell.
    ///
    /// *How* a particular backend is read is the adapter's, and the reading that
    /// used to live here now lives in `place_backend`, next to the wire that
    /// produces it — it was wrong about backend twice while it was in this file,
    /// and both times because it could be handed a shape backend does not send.
    #[test]
    fn only_an_agent_known_to_be_idle_is_told_anything() {
        assert!(State::Idle.will_take_a_prompt());
        for silent in
            [State::Starting, State::Unknown, State::Empty, State::Working, State::Blocked, State::Gone]
        {
            assert!(!silent.will_take_a_prompt(), "{silent:?} was told something");
        }
        assert_eq!(State::default(), State::Unknown, "an absence is not a fact");
    }

    /// The distinction robustness-083 exists for: **a process is not a turn.**
    ///
    /// Every census in wsp asked `is_running` and reported the answer as work in
    /// progress. On 2026-08-18 an API overload left seven agents alive with
    /// their turns abandoned, and every one of them answered yes to it — process
    /// alive, conversation intact, nothing happening, and `doctor` saying
    /// `backend up, 12 agents`. These are the two readings side by side, because
    /// the whole failure was that only one of them existed.
    #[test]
    fn a_process_that_exists_is_not_a_turn_that_is_running() {
        assert!(State::Working.turn_in_flight());
        for stopped in [State::Idle, State::Blocked, State::Starting] {
            assert!(stopped.is_running(), "{stopped:?} is an agent that is there");
            assert!(!stopped.turn_in_flight(), "{stopped:?} is an agent that is doing nothing");
        }
        for absent in [State::Empty, State::Gone, State::Unknown] {
            assert!(!absent.turn_in_flight(), "{absent:?}");
        }
        // Not written as `!is_running()`, and this is the assertion that says
        // why: an agent stopped on a permission prompt is running, is not
        // turning, and is the one of the three that a person can clear in a
        // keystroke.
        assert!(State::Blocked.is_running());
        assert!(!State::Blocked.will_take_a_prompt(), "a sentence would be typed at the dialog");

        // And `stopped` is not the complement of `turn_in_flight`, which is the
        // trap in writing it inline at a call site: silence and an empty seat
        // both answer no to both, and neither is an agent that has stalled.
        for stopped in [State::Idle, State::Blocked, State::Starting] {
            assert!(stopped.stopped(), "{stopped:?}");
        }
        for neither in [State::Unknown, State::Empty, State::Gone] {
            assert!(!neither.turn_in_flight(), "{neither:?}");
            assert!(!neither.stopped(), "{neither:?} is not an agent that stopped");
        }
    }

    /// A seat with nothing in it is not an agent that died, and neither of them
    /// is running.
    #[test]
    fn a_seat_with_nothing_in_it_is_not_the_same_as_one_whose_agent_stopped() {
        assert_ne!(State::Empty, State::Gone);
        assert!(!State::Empty.is_running());
        assert!(!State::Gone.is_running());
        assert!(!State::Unknown.is_running(), "not knowing is not evidence of work");
        for running in [State::Starting, State::Idle, State::Working, State::Blocked] {
            assert!(running.is_running(), "{running:?}");
        }
    }

    /// The seat's variable and the executor shim's are two different facts and
    /// must not be one name.
    ///
    /// `executor/wsp` refuses to run without `WSP_SEAT`, where it means the ssh
    /// Host alias of the seat machine, and the install tells you to set it in
    /// every shell an agent might run in. A seat id under the same name would
    /// make one of the two silently wrong in that shell — the shim ssh-ing to
    /// `w0:p3`, or an agent claiming a seat called `seat-mini`. This is the whole
    /// of the guard, because it is a fact about a name rather than about code.
    #[test]
    fn the_seats_variable_does_not_collide_with_the_executor_shims() {
        assert_ne!(SEAT_ENV, "WSP_SEAT", "executor/wsp reads that as an ssh Host alias");
        assert!(SEAT_ENV.starts_with("WSP_"), "it is wsp's contract with the backend");
    }

    /// An emptied variable is not a seat, and there is no other way to spell a
    /// strip on the wire.
    ///
    /// [`shed_env`] says why: a seat's environment is an override-only map, so
    /// anything removed arrives as `""`. A backend that empties [`SEAT_ENV`]
    /// rather than omitting it must not leave every agent-side verb claiming a
    /// seat with no name.
    #[test]
    fn an_emptied_seat_variable_is_not_a_seat_called_nothing() {
        let _env = crate::util::env_lock();
        let saved = std::env::var(SEAT_ENV).ok();
        std::env::set_var(SEAT_ENV, "");
        assert_eq!(seat_from_env(), None, "an override is what a strip looks like");
        std::env::set_var(SEAT_ENV, "sup-7");
        assert_eq!(seat_from_env(), Some(Seat::new("sup-7")));
        std::env::remove_var(SEAT_ENV);
        assert_eq!(seat_from_env(), None, "a shell nobody spawned is in no seat");
        if let Some(v) = saved {
            std::env::set_var(SEAT_ENV, v);
        }
    }

    /// Nothing in the port reads an id. This is the closest a test can get:
    /// a seat is whatever the backend said, unaltered, including the `@mb2`
    /// suffix the `Remote` decorator puts on it.
    /// **An agent id must not look like a task id**, and the reason is the
    /// one `message::new_id` documents: `Store::rename_tasks` rewrites a
    /// renumbering through the raw text of every state file, matching whole
    /// tokens against a map of old task ids. An id that could collide would be
    /// silently rewritten by a `wsp mv`, and the claim naming it would lose its
    /// agent — a failure with no error and no way back.
    ///
    /// Both task shapes end in digits after their last `-`; this ends in `p`.
    #[test]
    fn an_agent_id_cannot_be_mistaken_for_a_task_id_a_renumbering_would_rewrite() {
        let id = new_agent_id();
        assert!(is_agent_id(&id), "{id}");
        assert!(id.starts_with("a-"), "{id}");
        let tail = id.rsplit('-').next().expect("a last segment");
        assert!(
            tail.starts_with('p'),
            "no task id has ever ended in a `p` segment, which is the whole guarantee: {id}"
        );
        for task in ["wsp-105", "t-260817-014", "compound-092"] {
            assert!(!is_agent_id(task), "{task} is a task id and must not read as ours");
        }
    }

    /// Two minted in the same process are two agents. The counter is there
    /// because two inside one nanosecond are still two.
    #[test]
    fn two_agents_minted_together_are_not_one_agent() {
        let a = new_agent_id();
        let b = new_agent_id();
        assert_ne!(a, b, "a seat handed on twice in one tick is two agents");
    }

    #[test]
    fn a_seat_is_carried_rather_than_parsed() {
        for id in ["w0:p3", "w0:p3@mb2", "7", "sess_01HQ", ""] {
            assert_eq!(Seat::new(id).as_str(), id);
            assert_eq!(Seat::new(id).to_string(), id);
        }
        assert!(Seat::default().is_empty());
        assert_ne!(Seat::new("w0:p3"), Seat::new("w0:p3@mb2"));
    }

    fn row(id: &str) -> Seated {
        Seated { seat: Seat::new(id), ..Seated::default() }
    }

    /// The distinction the whole type exists for, and the one a `Vec<Seated>`
    /// cannot hold: a machine that answered and holds nothing, and a machine
    /// that said nothing, are both no rows and are not the same fact.
    ///
    /// Reaping on the second is a network blip handing back every task an
    /// executor holds. Refusing to reap on the first is a claim over a closed
    /// workspace that nothing ever sweeps. One list gets one of them wrong.
    #[test]
    fn a_machine_that_holds_nothing_is_not_a_machine_that_said_nothing() {
        let empty = Census::heard("mb2", Vec::new());
        let quiet = Census::silent("mb2", Refusal::Unreachable("no route to host".into()));

        assert_eq!(empty.seats().count(), 0, "no rows");
        assert_eq!(quiet.seats().count(), 0, "the same no rows");
        assert!(empty.answered("mb2"), "and it was heard from");
        assert!(!quiet.answered("mb2"), "and it was not");

        assert_eq!(quiet.unheard().count(), 1, "who was silent, and why, survives the census");
        assert_eq!(empty.unheard().count(), 0);
        let (machine, why) = quiet.unheard().next().unwrap();
        assert_eq!(machine, "mb2");
        assert!(matches!(why, Refusal::Unreachable(_)), "a partition, not a misconfiguration");
    }

    /// One machine down out of two is one machine's worth of fact.
    ///
    /// The reading `Refusal::Unreachable` from a fan-out would have forced —
    /// the whole call failed — is wrong twice over: it throws away seats that
    /// were reported, and it says nothing about *which* machine went quiet,
    /// which is the only part a reap needs.
    #[test]
    fn a_census_missing_one_machine_still_answers_for_the_others() {
        let c = Census::heard("", vec![row("w0:p1")])
            .and(Census::silent("mb2", Refusal::Unreachable("gone".into())))
            .and(Census::heard("mb3", vec![row("w0:p1@mb3")]));

        assert!(c.was_heard(), "somebody answered");
        assert_eq!(c.seats().count(), 2, "every seat every answering machine holds");
        assert_eq!(c.on("").count(), 1);
        assert_eq!(c.on("mb3").next().unwrap().seat, Seat::new("w0:p1@mb3"));
        assert_eq!(c.on("mb2").count(), 0, "a silent machine holds nothing readable");
        assert!(c.answered("") && c.answered("mb3") && !c.answered("mb2"));
        assert!(!c.answered("mb4"), "a machine nobody asked is as unheard as one that was");
    }

    /// Nobody answered is a census too, and `was_heard` is the only thing that
    /// separates it from a world where nothing is running.
    ///
    /// `arrange::World` draws this line with two constructors and no `Default`,
    /// for the reason robustness-015 paid for: the cost of this bug is a
    /// `Vec::new()` that looks like an answer. A `Census` cannot be made at all
    /// without saying which of the two it is.
    #[test]
    fn a_census_nobody_answered_cannot_be_read_as_an_empty_world() {
        let none = Census::silent("", Refusal::Unreachable("no socket".into()))
            .and(Census::silent("mb2", Refusal::Unreachable("no route".into())));
        assert!(!none.was_heard());
        assert_eq!(none.seats().count(), 0);
        assert_eq!(none.unheard().count(), 2, "both, and each with its own reason");

        let world = Census::heard("", Vec::new());
        assert!(world.was_heard(), "an answer of nothing is an answer");
        assert_eq!(world.seats().count(), 0);
    }

    /// A prompt, and an agent behind it that may or may not take a return.
    ///
    /// What `wsp-252` needed a fake to say and none could: the text arrives,
    /// the submit is lost, and the text stays in the prompt where a reader can
    /// see it. `takes_on` is which press starts the turn — `Some(0)` is the
    /// order's own, `Some(1)` the first press after it, `None` never.
    struct Prompt {
        readable: bool,
        busy: bool,
        /// A turn in flight keeps a queued sentence in the prompt box.
        keeps: bool,
        takes_on: Option<u32>,
        holding: std::cell::Cell<bool>,
        turned: std::cell::Cell<bool>,
        pressed: std::cell::Cell<u32>,
        asked: std::cell::Cell<u32>,
    }

    impl Prompt {
        fn new(takes_on: Option<u32>) -> Prompt {
            Prompt {
                readable: true,
                busy: false,
                keeps: false,
                takes_on,
                holding: std::cell::Cell::new(false),
                turned: std::cell::Cell::new(false),
                pressed: std::cell::Cell::new(0),
                asked: std::cell::Cell::new(0),
            }
        }

        /// A return, the order's own or a press: the text goes if this is the
        /// one that takes, or straight away behind a turn already running.
        fn enter(&self, n: u32) {
            if self.busy {
                self.holding.set(self.keeps);
            } else if self.takes_on == Some(n) {
                self.holding.set(false);
                self.turned.set(true);
            }
        }
    }

    impl Place for Prompt {
        fn open(&self, _: &Order) -> Result<Seat> {
            unreachable!()
        }
        fn here(&self) -> Option<Seat> {
            None
        }
        fn start(&self, _: &Seat, _: &Agent) -> Result<()> {
            unreachable!()
        }
        fn tell(&self, _: &Seat, _: &str) -> Result<Delivery> {
            self.holding.set(true);
            self.enter(0);
            Ok(Delivery::Unconfirmed)
        }
        fn nudge(&self, _: &Seat) -> Result<()> {
            self.pressed.set(self.pressed.get() + 1);
            self.enter(self.pressed.get());
            Ok(())
        }
        fn typed(&self, _: &Seat) -> Option<bool> {
            self.readable.then(|| self.holding.get())
        }
        fn turn_began_since(&self, _: &Seat, _: i64) -> Option<bool> {
            Some(self.turned.get())
        }
        fn stop(&self, _: &Seat) -> Result<()> {
            unreachable!()
        }
        fn state(&self, _: &Seat) -> Result<State> {
            self.asked.set(self.asked.get() + 1);
            Ok(if self.busy || self.turned.get() { State::Working } else { State::Idle })
        }
        fn census(&self) -> Result<Census> {
            unreachable!()
        }
        fn watch(&self, _: &mut dyn FnMut(Event) -> bool) -> Result<()> {
            unreachable!()
        }
    }

    fn pressing<'a>(clock: &'a crate::util::Dial<'_>) -> Pressing<'a> {
        Pressing { clock, ..Pressing::default() }
    }

    /// **The order's own submit is lost, and the press after it starts the
    /// turn** — gpui-migration's ten seats on 2026-10-08, and compound-415's
    /// verifier that sat until Ed pressed Enter. Started, and pressed once.
    #[test]
    fn a_submit_lost_the_first_time_is_pressed_again_and_the_turn_starts() {
        let clock = crate::util::Dial::new();
        let place = Prompt::new(Some(1));
        let said = submit(&place, &Seat::new("cpd-627"), "Verify compound-415", &pressing(&clock));
        assert_eq!(said, Ok(Delivery::Started), "the retry submitted it");
        assert_eq!(place.pressed.get(), 1, "one press was the whole of the repair");
        assert!(!place.holding.get(), "and the prompt is empty again");
    }

    /// A submit that takes is watched for one poll and never pressed: the
    /// healthy send pays a reading, not a wait.
    #[test]
    fn an_order_that_takes_is_not_pressed() {
        let clock = crate::util::Dial::new();
        let place = Prompt::new(Some(0));
        assert_eq!(submit(&place, &Seat::new("cpd-1"), "go", &pressing(&clock)), Ok(Delivery::Started));
        assert_eq!(place.pressed.get(), 0);
        assert_eq!(clock.elapsed(), Duration::ZERO, "the turn was there at the first look");
    }

    /// **Bounded, and said as what it is.** Text that no press sends is
    /// `Unsubmitted` — typed, read back, still there — after three presses
    /// spaced across the windows, and not the old "sitting unsent or dropped".
    #[test]
    fn text_that_stays_in_the_prompt_is_unsubmitted_after_three_spaced_presses() {
        let clock = crate::util::Dial::new();
        let place = Prompt::new(None);
        let how = pressing(&clock);
        let said = submit(&place, &Seat::new("cpd-616"), "Check barrier 14", &how);
        assert_eq!(said, Err(Refusal::Unsubmitted(Seat::new("cpd-616"))));
        assert_eq!(place.pressed.get() as usize, how.windows.len() - 1, "a press between each pair of looks");
        assert!(clock.elapsed() >= how.windows.iter().sum(), "every window was waited out: {:?}", clock.elapsed());
        assert!(clock.elapsed() < Duration::from_secs(20), "and no longer");
    }

    /// A sentence queued behind a turn starts nothing and is still taken:
    /// the prompt empties, and that is the answer. `govern --tell` is this
    /// case, and pressing at a governor mid-turn would be pressing at its work.
    #[test]
    fn a_sentence_queued_behind_a_turn_is_delivered_when_the_prompt_empties_and_never_pressed() {
        let clock = crate::util::Dial::new();
        let place = Prompt { busy: true, ..Prompt::new(None) };
        assert_eq!(submit(&place, &Seat::new("cpd-2"), "the barrier is open", &pressing(&clock)), Ok(Delivery::Unconfirmed));
        assert_eq!(place.pressed.get(), 0);
    }

    /// And one whose prompt holds it until the turn ends is not pressed
    /// either, nor called unsubmitted: a press into a turn is a keystroke at
    /// somebody's work, and every lost submit on record was at an idle seat.
    #[test]
    fn a_sentence_held_in_the_prompt_behind_a_turn_is_not_pressed() {
        let clock = crate::util::Dial::new();
        let place = Prompt { busy: true, keeps: true, ..Prompt::new(None) };
        assert_eq!(submit(&place, &Seat::new("cpd-2"), "the barrier is open", &pressing(&clock)), Ok(Delivery::Unconfirmed));
        assert_eq!(place.pressed.get(), 0);
    }

    /// No reading, no press, and not even the state asked: the path a
    /// backend that cannot see its prompt always took, call for call. A
    /// return into a prompt nobody has read may submit a person's draft.
    #[test]
    fn a_prompt_nobody_can_read_is_never_pressed() {
        let clock = crate::util::Dial::new();
        let place = Prompt { readable: false, ..Prompt::new(None) };
        assert_eq!(submit(&place, &Seat::new("w1:p1"), "go", &pressing(&clock)), Ok(Delivery::Unconfirmed));
        assert_eq!(place.pressed.get(), 0);
        assert_eq!(place.asked.get(), 0, "nothing beyond the tell itself");
    }

    /// An empty prompt and no turn is the backend's own answer, not a press:
    /// there is nothing there to submit, and the run's repair for a dropped
    /// order is a resend, which is not this function's to make.
    #[test]
    fn an_empty_prompt_with_no_turn_is_passed_back_and_not_pressed() {
        let clock = crate::util::Dial::new();
        let place = Prompt::new(None);
        // Dropped: the text never reached the prompt.
        struct Dropped(Prompt);
        let dropped = Dropped(place);
        impl Place for Dropped {
            fn open(&self, o: &Order) -> Result<Seat> { self.0.open(o) }
            fn here(&self) -> Option<Seat> { None }
            fn start(&self, s: &Seat, a: &Agent) -> Result<()> { self.0.start(s, a) }
            fn tell(&self, _: &Seat, _: &str) -> Result<Delivery> { Ok(Delivery::Unconfirmed) }
            fn nudge(&self, s: &Seat) -> Result<()> { self.0.nudge(s) }
            fn typed(&self, s: &Seat) -> Option<bool> { self.0.typed(s) }
            fn turn_began_since(&self, s: &Seat, t: i64) -> Option<bool> { self.0.turn_began_since(s, t) }
            fn stop(&self, s: &Seat) -> Result<()> { self.0.stop(s) }
            fn state(&self, s: &Seat) -> Result<State> { self.0.state(s) }
            fn census(&self) -> Result<Census> { self.0.census() }
            fn watch(&self, f: &mut dyn FnMut(Event) -> bool) -> Result<()> { self.0.watch(f) }
        }
        assert_eq!(submit(&dropped, &Seat::new("cpd-3"), "go", &pressing(&clock)), Ok(Delivery::Unconfirmed));
        assert_eq!(dropped.0.pressed.get(), 0);
    }

    /// A seat on a silent machine is held, not gone: it is not running, will not
    /// take a prompt, and is not `Gone` (`docs/remote-agents.md` section 3).
    #[test]
    fn an_unreachable_seat_is_neither_running_nor_gone_nor_ready_for_a_prompt() {
        let u = State::Unreachable;
        assert_eq!(u.as_str(), "unreachable");
        assert!(!u.is_running() && !u.will_take_a_prompt() && !u.turn_in_flight());
        assert_ne!(u, State::Gone);
        assert_ne!(u, State::Unknown);
    }
}

/// Which seat this process is standing in, or none.
///
/// `place::Place::here` is the verb, and which variable carries the answer is
/// the adapter's business. Every agent-side verb — `claim`, `say`, `release` —
/// is downstream of this, and so is `despawn`'s refusal to end the seat it is
/// running in.
///
/// A `String` because that is what the callers below hold — a claim's `pane`,
/// a binding's key, the `--pane` a person types are all the same string, and
/// converting them all is `place::Seat`'s own migration rather than this one's.
///
/// No socket is opened: the supervisor's `here` reads `place::SEAT_ENV`, set
/// before this process existed, which is a rule of the port and not an
/// optimisation.
pub fn my_pane() -> Option<String> {
    crate::place_super::Supervisor::new().here().map(|s| s.to_string())
}

/// Every backend wsp can spawn onto, for the readers that do not get to ask —
/// `wsp wip` folds all of them into one census (`Wip::live`, `wsp-100`+
/// `compound-078`) and `wsp tell` (`compound-077`) has to find the ONE that
/// answers for a seat before it knows how to reach it. `headless` is left
/// out: `place_super` has no terminal to fold into a listing and no prose to
/// receive that is not already reachable through its own `agent.prompt` —
/// see `robustness-022`'s three fates.
///
/// The same "one copy" argument [`backend`] makes: a reader that built its
/// own array would be a second list to update when another implementor
/// lands.
pub fn local_backends() -> [Box<dyn Place>; 1] {
    [Box::new(crate::place_compound::Compound::new())]
}

/// The name a whole backend's own silence is filed under — `wsp stamp`
/// (`compound-064` item 2), paired with [`local_backends`] by position, so
/// this file stays the one place that ordering is named rather than
/// re-assumed at a call site. `"compound"` rather than `""`: the empty string
/// collides with the MACHINE namespace, where `cmd_stamp::named` reads it as
/// "this machine", and a backend is not a machine.
pub const LOCAL_BACKEND_NAMES: [&str; 1] = ["compound"];

/// The scope this workspace is the custodian of — a project or a worklist — if
/// it is the custodian of one.
///
/// **One agent, one governorship.** Ed, 2026-08-17, reversing what
/// wsp-063 built for. That task argued a night coordinating `robustness`
/// while answering for `wsp` above it was one agent and not two, which
/// described the night accurately and is still the wrong model: it makes two
/// questions unanswerable in principle rather than merely hard. Which row draws
/// the agent, when it is in two positions at once? And what does a vacancy look
/// like when the same occupant fills both? One slot to an agent settles both by
/// construction, and the answer to *"who answers for `wsp` while the
/// `robustness` custodian is busy"* is the chain [`seat_for`] already walks —
/// upwards, to a different agent.
///
/// So this is an `Option` rather than a list, and the invariant lives in the
/// type: [`take`] stands the workspace down from whatever it held before, the
/// way a claim hands off the task it is leaving. A store that already holds two
/// — this one did, the night the rule changed — answers with the first in id
/// order and heals the next time anybody takes a seat.
///
/// No workspace is no seat, never a seat. A pane backend answered for without a
/// workspace id, or a caller outside backend entirely, would otherwise match a
/// record whose own field failed to parse — and the answer to "am I the seat"
/// would come back yes for a process that is not in a workspace at all.
///
/// # `pane` — and why the exact read is the pane's and not the room's
///
/// The record is keyed on the workspace and that is still right: it is what
/// survives an agent being cleared and restarted, and it is what the panel
/// draws under. But **a workspace can hold more than one agent**, and asking
/// this question of the *room* answers yes for every one of them. worklist-035,
/// driven on `796c2d2`: a seat's pane was ended, two later spawns landed in
/// that same workspace, and both were told they were the custodian — which
/// exempted both from [`needs_a_person`] for their whole lives, one of them a
/// member of a running worklist on an unattended night. One line of `wip` held
/// the fact and the silence together.
///
/// So a caller with a pane in hand passes it, and a record that names a pane is
/// only that pane's. `None` is for the caller that genuinely has no pane —
/// naming the *room* after its seat ([`rename_seat`], the workspace token in
/// `sync`) is a question about the workspace, and answering it per pane would
/// be answering a different question.
///
/// **The case for the other answer, stated rather than assumed.** A custodian
/// that splits its own workspace to run something gets told, in the second
/// pane, that it is nobody's seat. That is the cost, it is real, and it is the
/// cheaper of the two: wsp cannot tell that pane from a worker's, and the two
/// failures are not the same size. A seat told *this workspace is nobody's
/// seat* gets a sentence naming the repair and types `wsp govern` again. A
/// worker told it **is** the seat is told nothing at all — it is exempted from
/// the one predicate an unattended run depends on, and stays exempt until
/// somebody happens to read a panel. Silence is the failure this is being
/// repaired for, so the ambiguity is resolved towards the noisy answer.
///
/// A record with no pane on it — hand-written, or written by `wsp govern -w`
/// naming a room this process is not standing in — falls back to the room,
/// because there is nothing better to compare and a seat with no address is
/// still a seat.
pub fn governs(governors: &std::collections::BTreeMap<String, serde_json::Value>, seat: &Seat) -> Option<String> {
    let who = seat.as_str();
    if who.is_empty() {
        return None;
    }
    // Off the record's own fields, read raw, rather than through [`seat_of`]
    // and [`Seat::sat_in`] — deliberately, so this reads as the row that
    // leaves `Seat::workspace` unused: nothing here names the struct field,
    // so deleting it costs this function nothing to keep answering right.
    //
    // A pane id already carries its workspace (`"w1:p1"`), which is what
    // makes one string enough where two used to be needed: an exact pane
    // match no longer has to be confirmed against a separate workspace,
    // because two different rooms cannot mint the same qualified pane id.
    // What a bare workspace comparison bought — matching *any* pane of the
    // room, and matching a `wsp govern -w` record that names no pane at all
    // — is recovered the same way: `who == workspace` catches the caller
    // that is asking about the room by name, and the prefix check catches
    // the caller standing in a pane of a room a `-w` record claimed without
    // ever learning that pane's id.
    governors
        .iter()
        .find(|(_, rec)| {
            let host = rec.get("host").and_then(serde_json::Value::as_str).unwrap_or("");
            if !host.is_empty() && host != crate::util::hostname() {
                return false;
            }
            let workspace = rec.get("workspace").and_then(serde_json::Value::as_str).unwrap_or_default();
            if workspace.is_empty() {
                return false;
            }
            let pane = rec.get("pane").and_then(serde_json::Value::as_str).unwrap_or_default();
            who == pane || who == workspace || (pane.is_empty() && who.starts_with(&format!("{workspace}:")))
        })
        .map(|(p, _)| p.clone())
}

/// The seat a caller means, from what it has in hand: the pane if there is
/// one, the workspace itself otherwise.
///
/// The bridge between the shape most callers still carry — a workspace and
/// maybe a pane, straight off a backend record or a stored claim — and the one
/// [`governs`] now asks for. `pane` wins when it is not empty because it is
/// the more exact of the two; `workspace` alone is the room-wide question
/// `wsp govern -w` and a bare `--clear` ask, and is exactly what a caller
/// passes when it has no pane to be exact about.
pub fn seat_query(workspace: &str, pane: Option<&str>) -> Seat {
    Seat::new(pane.filter(|p| !p.is_empty()).unwrap_or(workspace))
}
