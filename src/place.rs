//! The place-work port: what wsp asks of whatever is running its agents.
//!
//! The contract t-260816-081 was opened to write. `wsp spawn` is on it —
//! `place_herdr` is the first backend and the fake behind a socket is the
//! second — and the rest of the call sites are t-260816-061's, still to move.
//! Read it as the answer to one question: **when wsp puts an agent on a piece
//! of work, what does it actually need from the thing underneath?**
//!
//! Ed's sentence, 2026-08-16, and the port is a transcription of it:
//!
//! > start an agent on this task, in this directory; give me a handle; tell me
//! > its state; let me send it a prompt; tell me when it stops.
//!
//! herdr satisfies that by creating a workspace and splitting a pane. A bare
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
//! There are **two** herdr ports, not one (decision on t-260816-083). This is
//! the place-work port. The other is the arrange-panes port — `pane.split`,
//! `pane.close`, `pane.focus`, `pane.layout`, `pane.swap`, `pane.send_text`,
//! `tab.create`, `tab.close`, `tab.focus`, `workspace.focus` — which belongs to
//! t-260816-084. Those ten verbs are what a program does when it *lives inside*
//! a multiplexer as a pane, and every one of their call sites is in `panel/` or
//! `detail/` bar one, so they are the arrange-panes port's herdr adapter rather
//! than anything wsp needs from a backend.
//!
//! The split is between what wsp *means*, not between herdr methods:
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
//! t-260816-078 measured the place-work group at seven herdr methods. They do
//! not have one fate, they have three, and telling them apart is most of the
//! design:
//!
//! | herdr method | fate |
//! |---|---|
//! | `workspace.create` | ported — [`Place::open`] |
//! | `agent.start` | ported — [`Place::start`] |
//! | `agent.prompt` | ported — [`Place::tell`] |
//! | `workspace.rename` | **demoted** into the herdr adapter |
//! | `pane.rename` | **demoted** into the herdr adapter, and split |
//! | `workspace.report_metadata` | **deleted** |
//! | `pane.report_metadata` | **deleted** |
//!
//! **Deleted** is the easy pair and t-260816-061's correction already had it
//! right, understated: the metadata is not merely display-only, it is
//! *write-only*. `Workspace.tokens` is parsed at `herdr.rs:328` and read
//! nowhere in the tree, and `Pane` has no `tokens` field at all — so wsp pushes
//! six workspace tokens and four pane tokens per sync and never asks for one
//! back. They exist to draw herdr's sidebar, which under the four-part
//! partition is the renderer's job (part 4) and not a capability any backend
//! owes us.
//!
//! **Demoted** is the pair t-260816-061 defended, and it was right about the
//! code and wrong about the conclusion. `workspace.rename` and `pane.rename`
//! are not display projection: a claim stores `workspace_label` and four places
//! match on it — `reconcile --reap`'s aliveness test (`cmd_agent.rs:1301`),
//! reconcile's rebinding when an id has changed (`1344`),
//! `resolve::claimed_project` (`resolve.rs:410`) and the `project_for_label`
//! fallback (`resolve.rs:466`). Stop writing it and claims resolve to the wrong
//! workspace. But *why* is it a secondary key? Because herdr's ids are
//! perishable and wsp had nothing durable to hold. The first duty of this port
//! is a durable handle ([`Seat`]) — so the label-as-key is not ported and not
//! deleted, it is demoted: **the herdr adapter owes a durable seat, and if
//! herdr cannot supply one, re-finding it by label is how the adapter keeps its
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
//! herdr expires metadata: `sync.rs:12` says 6h "refreshed by the daemon well
//! inside herdr's 24h ceiling". Delete the tokens and that justification goes
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
//! Not here. `say` is not a port verb, and the answer t-260816-083 gives is
//! confirmed: the sentence belongs to the agent module's ephemeral state.
//!
//! The reason it looks like a herdr call today is an accident of ownership —
//! herdr owns the label the sidebar draws, so the sentence was put where it
//! would be seen. It writes nothing to the store, and is read back out of
//! `pane.list`. Three things that look like three different herdr calls
//! (`report_metadata`, `name_after_task`, `say`) are one thing: a projection of
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
//! This is the half of "give me a handle" that nobody had written down, and it
//! is where a TTY-less backend breaks first.
//!
//! Every agent-side verb — `wsp claim`, `wsp say`, `wsp release` — identifies
//! its own seat by reading `HERDR_PANE_ID` out of the environment
//! (`cmd_agent.rs:164`, `herdr.rs:574`). With no pane there is no
//! `HERDR_PANE_ID`, and the agent cannot say who it is. So the handle has to
//! travel in both directions: wsp gets a [`Seat`] back from [`Place::open`],
//! and whatever runs in that seat must find the same string in its environment
//! under [`SEAT_ENV`].
//!
//! This costs one entry. `cmd_spawn::order` already fills an env map with
//! `WSP_PROJECT`, `WSP_TASK` and the store's own variables, and this joins
//! them — for a backend that can carry it. `place_herdr` records why herdr
//! cannot, which is the open half of this.
//!
//! # Two verbs, because the claim goes between them
//!
//! [`Place::open`] and [`Place::start`] are separate, and it is not because
//! herdr happens to have two methods. `wsp spawn`'s order is **workspace,
//! claim, agent, sentence**, and the claim has to land before the agent starts,
//! because a Claude Code session runs `wsp brief` from its `SessionStart` hook
//! and reads the claim on the way in. Started first, it would open knowing
//! nothing.
//!
//! The claim is keyed on the seat, so the seat has to exist before the agent
//! does. Any backend must honour that window; it is wsp's ordering rule rather
//! than herdr's shape.
//!
//! # Ending it: one verb here, and the claim released after it
//!
//! [`Place::stop`] is the other end of that sentence, and it was left out of the
//! first cut of this file on the house rule that a verb with no caller is a tax.
//! The caller exists: wsp puts agents on work one at a time, and a loop that can
//! start work and not end it makes despawning part of the loop rather than an
//! edge case. Measured the same evening, ending the agent that wrote this file:
//! `wsp release --pane w26:p1` and then `herdr workspace close w26` — two
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
//! - **`available()`.** It is a herdr question — is there a socket — and every
//!   guard on it (`cmd_spawn.rs:284`, `cmd_agent.rs:618`) prints a herdr
//!   sentence. With a backend the question is answered by the backend existing,
//!   and unreachability is a [`Refusal::Unreachable`] from the call that wanted
//!   it, which arrives at the same moment the guard would have.
//! - **`focus`.** Still absent, and the argument that removed it still holds
//!   (decision on t-260817-008): it belongs to the arrange-panes port. What is
//!   here is [`Order::show`], which is scaffolding rather than a verb — `spawn`
//!   has nowhere else to say `--no-focus` until `arrange` has an implementor.
//!   The removal condition is on the field and is not restated here.
//! - **`stop`** *was* absent, on the grounds that wsp had never despawned
//!   anything, and the trigger named for adding it was the first TTY-less
//!   backend. That was late rather than wrong: the caller was already there and
//!   is `wsp despawn`, above. What has not changed is the rule — the next verb
//!   will need a caller of its own, and this is not a licence to port the rest
//!   of herdr's eighty-nine methods on the same reasoning.
//! - **retries and timeouts.** `spawn` used to retry `agent_pane_busy` for five
//!   seconds and retype with a `ctrl-u` after six, because herdr types the
//!   agent's name at a shell prompt that may not be ready. That is a herdr shell
//!   race and it now sits inside `place_herdr`'s [`Place::start`]. It is also
//!   the answer to the one loose end in t-260816-078's measurement: the single
//!   `pane.send_text` outside `panel/` and `detail/` is that retype, and it did
//!   not become an eighth port verb.
//!
//! # Ids are opaque, and that is load-bearing
//!
//! A [`Seat`] is a string wsp does not read. herdr's are `w0:p3` and
//! `w0:p3@mb2`; strata's D-55 uses an index plus a generation so an id is never
//! silently reused. A seam that passes ids around as strings works; a seam that
//! assumes herdr's *shape* of id does not — so nothing here parses one, and
//! `herdr::split_host` stays the one place the `@` suffix means anything.
//!
//! So is a machine's address. `Machine::backend_at` is the port's word for
//! *where this machine's backend listens*, and it is opaque for the same reason
//! an id is: herdr's is a unix socket path, and something reached over TCP would
//! write a host and a port in the same field without a line of wsp changing.
//! Nothing outside an adapter reads it — the model stores it, `tunnel.rs` hands
//! it to `ssh -L` because a unix socket on both ends is already herdr's dialect,
//! and `place_herdr::mirrored_socket` is what an empty one means.
//!
//! The `@` suffix belongs to the `Remote` decorator (decision on t-260816-060):
//! it holds the connections, strips the machine on the way out and qualifies
//! ids on the way in, and the backend underneath goes on believing it talks to
//! one host. That is why this trait is object-safe — `Remote` holds a
//! `Box<dyn Place>` — and why [`Order::on`] exists at all: **a seat that does
//! not exist yet has no id to route on**, so [`Place::open`] is the single
//! place in the port where a machine is named, exactly as `workspace.create` is
//! today (`cmd_spawn.rs:82`).

#![allow(dead_code)]

use std::collections::BTreeMap;

/// The environment variable a seat's occupant finds its own handle in.
///
/// The replacement for `HERDR_PANE_ID`. See the module docs — an agent that
/// cannot name its own seat cannot claim, say or release, and a backend with no
/// panes has no pane id to lend it.
///
/// Nothing sets this yet, and the herdr adapter cannot: the environment is fixed
/// by the call that creates the seat, so the seat's name does not exist until
/// after the only moment it could have been put there. `place_herdr` records
/// what that leaves open — either wsp mints the seat itself, or an agent asks
/// the backend which seat it is in — and it is a decision, not an oversight.
pub const SEAT_ENV: &str = "WSP_SEAT";

/// The marker that turns a spawned Claude Code into somebody else's child, and
/// the one variable measured to break a spawn outright.
///
/// A session that finds `CLAUDE_CODE_CHILD_SESSION` in its environment believes
/// it is a sub-session of the one that set it and **writes no transcript at
/// all**. The only evidence is one line in the pane, truncated where it matters:
/// `⚠ Transcript saving is off — inherited CLAUDE_CODE…`. Nothing fails, nothing
/// is logged, and an agent spawned this way works perfectly and leaves no
/// record. Claude Code knows the leak class — it probes `tmux show-environment`
/// to tell a real marker from one that is merely ambient in the multiplexer —
/// and herdr is not tmux, so under herdr the marker is always believed.
///
/// Measured 2026-08-17 against Claude Code 2.1.233, three panes of one sandbox,
/// each started with `agent.start` and prompted:
///
/// | pane | what it inherited | transcript |
/// |---|---|---|
/// | as spawned today | the caller's whole session | **none** |
/// | this one name emptied | — | 36,752 bytes |
/// | every name in [`shed`] emptied | — | 36,882 bytes |
///
/// So it is shed whether or not the caller has it, unlike the rest: the leak
/// does not need wsp to be the carrier. A herdr server started by hand from
/// inside an agent's pane hands the marker to every seat it will ever open, and
/// a `wsp` run from a clean shell against that server can see nothing wrong with
/// its own environment to strip.
pub const CHILD_MARKER: &str = "CLAUDE_CODE_CHILD_SESSION";

/// What a seat's occupant must **not** find: a variable that names the Claude
/// Code session the caller is sitting in, rather than anything about the seat.
///
/// The other half of [`SEAT_ENV`]. That one is the handle a spawn owes the
/// child; this is the identity a spawn owes it *not* to pass on, and both are
/// facts about placing work rather than about any backend.
///
/// A prefix rather than the four names anybody could list today, for the reason
/// `cmd_sandbox::forgettable` gives about `HERDR_`: a variable Claude Code adds
/// next month is a fact about the caller's session too, and this defect exists
/// precisely because [`CHILD_MARKER`] was a name nobody knew to unset. What the
/// prefix catches beyond the marker is worth naming, because none of it is
/// harmless: `CLAUDE_CODE_SESSION_ID` is the caller's own session id, and
/// `CLAUDE_CODE_MESSAGING_SOCKET` and `..._TOKEN` are the caller's control
/// channel and the credential for it, handed to an unrelated agent.
///
/// `CLAUDE_PID` is named because the prefix misses it and it is the same fact:
/// the control socket is `/tmp/cc-socks/<CLAUDE_PID>.sock`, so shedding the
/// socket and keeping the pid that spells it is half a strip. The prefix stops
/// at `CLAUDE_CODE_` rather than `CLAUDE_` for the opposite reason — a setting
/// like `CLAUDE_CONFIG_DIR` says where this machine keeps its configuration and
/// is *supposed* to be inherited. Identity is shed; preference is not.
pub fn shed(key: &str) -> bool {
    matches!(key, "CLAUDECODE" | "CLAUDE_PID") || key.starts_with("CLAUDE_CODE_")
}

/// The same rule against this process's environment: what a caller would have
/// to `unset` before spawning by hand, which is the workaround this replaces.
///
/// [`CHILD_MARKER`] is always in the list and the rest is only what is actually
/// here, so a spawn from a shell with none of this in it is one name long.
pub fn shed_keys() -> Vec<String> {
    let mut keys: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| shed(k) && k != CHILD_MARKER)
        .collect();
    keys.sort();
    keys.insert(0, CHILD_MARKER.into());
    keys
}

/// The same list as the entries an [`Order`] carries to shed them.
///
/// **Emptied rather than removed, because removal is not on the wire.** A seat's
/// environment is a string-to-string map on the call that creates it — herdr's
/// `workspace.create` takes one and `agent.start` takes none — so the only strip
/// a port can express is an override, and an empty value is the strongest one
/// there is. That it is strong enough was measured rather than assumed; the
/// table on [`CHILD_MARKER`] is that measurement, and an agent started with all
/// of these empty came up, answered and saved its transcript.
pub fn shed_env() -> BTreeMap<String, String> {
    shed_keys().into_iter().map(|k| (k, String::new())).collect()
}

/// A durable handle to somewhere an agent can run.
///
/// **Durable** is the requirement and it is the one the port will not bend on:
/// a seat outlives the agent standing in it and the backend restarting. wsp
/// writes it into a claim and expects to find the same place behind it
/// tomorrow.
///
/// herdr does not quite supply this. Workspace ids survive a session restore
/// but not a workspace being rebuilt, and pane ids are reissued on every
/// restart — which is why claims are keyed on workspaces rather than panes, and
/// why a claim carries a `workspace_label` to re-find one whose id has changed.
/// Under this port that shortfall is the herdr adapter's to make good, by
/// whatever means, and wsp stops carrying the workaround. It is also why
/// nothing here is a `pane` or a `workspace`: today's two ids, of two different
/// lifetimes, become one handle of one lifetime.
///
/// Opaque on purpose. Constructed from whatever the backend says, compared, and
/// never parsed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Seat(String);

impl Seat {
    pub fn new(id: impl Into<String>) -> Seat {
        Seat(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for Seat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What to open a seat for: a name, a place on disk, an environment, and
/// optionally a machine.
///
/// `label` is not decoration and not a key. It is what a person reading a list
/// of seats has to go on, and every backend has some list — a sidebar, a
/// process table, a page. wsp writes `render/109 · <title>` into it
/// (`cmd_agent::task_label`) because that is legible in ten columns. What it is
/// *not*, after this port lands, is how wsp finds the seat again; see [`Seat`].
#[derive(Debug, Clone, Default)]
pub struct Order {
    pub label: String,
    /// Where the work lives. `None` means the backend's own default, which for
    /// herdr is wherever it would have opened a shell.
    pub cwd: Option<String>,
    /// Handed to whatever runs in the seat. wsp puts `WSP_PROJECT`, `WSP_TASK`
    /// and the store's own variables here; the backend adds [`SEAT_ENV`].
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
    /// port ([`crate::arrange`], "Focus"), and the decision on t-260817-008
    /// settled that it stays there: focus is a state that can be applied
    /// headlessly, and a port every backend must implement without a screen
    /// cannot carry a statement about what a person is looking at.
    ///
    /// This field exists anyway because today it is the only way `spawn` can
    /// express `--no-focus`. A seat cannot be arranged before it exists, and
    /// there is no spec to declare the arrangement into — `arrange` has no
    /// implementor and `cmd_spawn.rs` holds one port. Without this field the
    /// flag could not be said at all.
    ///
    /// **The removal is not "delete this field", and that is why the trigger is
    /// written here rather than left to be noticed.** Something has to sequence a
    /// spawn that both places work and declares an arrangement, and nothing does.
    /// So the acceptance test for removal is: **`spawn` declares focus into a
    /// [`crate::arrange::Spec`]** — `arrange` has an implementor, `cmd_spawn`
    /// holds both ports, and `--no-focus` becomes one line of desired state
    /// applied once instead of a call sequenced after another. On that day this
    /// field goes and `cmd_spawn::order` loses its `show` argument. Until then it
    /// stays, and no second caller is added to it.
    ///
    /// Writing the trigger down is the whole point: this field was justified once
    /// as permanent, `arrange` recorded the opposite as settled, and both files
    /// read as closed. Scaffolding with no stated trigger is how that happened.
    ///
    /// It is not herdr's `focus` parameter wearing a different name: what wsp
    /// means is *do not drag the screen away from what somebody is reading*, and
    /// a backend with no screen honours it by ignoring it. `false` by default,
    /// so a seat opened by something with no opinion does not steal attention —
    /// which is the same rule read from the other end, and stays true wherever
    /// the flag ends up living.
    pub show: bool,
}

/// Which agent to start, and what to call it.
///
/// `kind` is passed through rather than enumerated. herdr refuses an unknown
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
}

/// What an agent in a seat is doing, as far as the backend can tell.
///
/// herdr reports two states, working or idle, and the README already says why
/// that is not enough: "`idle` is an answer to a question nobody asked". This
/// enum is the six answers wsp actually acts on, each with a caller today.
///
/// The two that look like luxuries are the two that have cost the most:
///
/// - [`State::Starting`] is separate from [`State::Idle`] because herdr reports
///   `agent_status: idle` while an agent is still coming up, and `agent.prompt`
///   refuses in that window with `agent_not_ready`. Waiting for "idle" returned
///   in half a second every time and the work order went into a pane still
///   drawing its banner. Every existing caller that tests `state == "idle"`
///   before telling an agent something is one reading of this away from the
///   same bug.
/// - [`State::Unknown`] exists because **an absence is not a fact**. It is the
///   README's `·`, and it is the same rule as `sync`'s "`Err` is not an empty
///   list" and `reconcile`'s unreachable-is-not-empty: one `pane.list` that
///   timed out once unbound every agent on the seat. A backend that does not
///   know must be able to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// The seat exists and nothing is running in it. A terminal somebody
    /// opened, or a seat opened and never started.
    Empty,
    /// An agent is coming up and will refuse a prompt.
    Starting,
    /// Waiting for input. The only state in which [`Place::tell`] is expected
    /// to succeed.
    Idle,
    /// Busy. Nothing to do about it and nothing to ask it.
    Working,
    /// There was an agent here and it has stopped. The seat may still exist.
    ///
    /// herdr cannot distinguish this from [`State::Empty`] in a listing — an
    /// exited agent leaves a pane whose `agent` field is empty, which is what a
    /// plain shell looks like — so its adapter answers `Empty` from a census
    /// and `Gone` from the event stream, where a released agent is named. A
    /// supervisor that watched a pid exit answers it exactly, which is the point
    /// of writing the port in wsp's vocabulary rather than herdr's.
    Gone,
    /// The backend did not say, or could not be asked.
    #[default]
    Unknown,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Empty => "empty",
            State::Starting => "starting",
            State::Idle => "idle",
            State::Working => "working",
            State::Gone => "gone",
            State::Unknown => "unknown",
        }
    }

    /// Whether [`Place::tell`] is worth attempting.
    ///
    /// The one question every caller of `state == "idle"` is actually asking,
    /// written once. `Unknown` is false: telling an agent we cannot see is how
    /// a prompt lands in somebody's shell.
    pub fn will_take_a_prompt(&self) -> bool {
        matches!(self, State::Idle)
    }

    /// Whether something is running here, whatever it is doing.
    pub fn is_running(&self) -> bool {
        matches!(self, State::Starting | State::Idle | State::Working)
    }
}

/// One seat, as the backend currently sees it.
///
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

/// Something the backend noticed, without being asked.
///
/// "Tell me when it stops" is the clause of Ed's sentence that has no polling
/// equivalent: a seat that has gone quiet looks exactly like a seat nobody
/// asked about. A backend that cannot push must poll behind [`Place::watch`]
/// and raise these itself — which is what the daemon does today, on a 20s tick,
/// because herdr's `pane.agent_status_changed` cannot be subscribed to globally
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

/// Why a call did not happen, in wsp's terms.
///
/// The point of naming these is that a caller should never match on a backend's
/// error string. `agent_not_ready`, `agent_pane_busy` and `pane_not_found` are
/// herdr's words for three of these, and `spawn` matching on `agent_pane_busy`
/// is the kind of coupling that survives a port unless the port has a word of
/// its own. It has one now, and the match is in `place_herdr` and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No backend answered. This is what `herdr::available()` becomes: not a
    /// question asked in advance, but the answer to the call that wanted it.
    Unreachable(String),
    /// No such seat. It may have been closed under us.
    NoSeat(Seat),
    /// The agent is not in a state to be told anything.
    NotReady(State),
    /// This backend cannot do that at all — a seat for a person on a backend
    /// with no terminal, say. Distinct from a failure, and not a bug.
    Unsupported(&'static str),
    /// The backend said no, in its own words.
    Backend(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreachable(w) => write!(f, "no backend answered: {w}"),
            Refusal::NoSeat(s) => write!(f, "no seat {s}"),
            Refusal::NotReady(s) => write!(f, "not ready — {}", s.as_str()),
            Refusal::Unsupported(w) => write!(f, "this backend cannot {w}"),
            Refusal::Backend(w) => write!(f, "{w}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Refusal>;

/// Where wsp puts work.
///
/// Seven methods. Every one of them is a clause of the sentence at the top of
/// this file, and nothing in any signature names a pane, a window, a tab or a
/// terminal.
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
    /// The seat must carry [`Order::env`] plus [`SEAT_ENV`] to whatever runs in
    /// it, and must be findable by the returned [`Seat`] after the backend has
    /// restarted.
    fn open(&self, order: &Order) -> Result<Seat>;

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

    /// Give the agent in a seat a sentence to act on.
    ///
    /// Refuses with [`Refusal::NotReady`] rather than typing into whatever is
    /// there. Not "send keystrokes": a backend with no terminal has no
    /// keystrokes, and a backend with one should use its own submit rather than
    /// a sleep long enough for a TUI to finish pasting.
    fn tell(&self, seat: &Seat, text: &str) -> Result<()>;

    /// End whatever agent is in a seat, and let the seat go.
    ///
    /// **Not the arrange-panes port's `close`.** They are spelled with the same
    /// herdr method and they are not the same verb: this one ends somebody's
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

    /// What is happening in one seat, right now.
    ///
    /// Separate from [`Place::census`] because `spawn` polls a single seat
    /// every 150ms for up to 30 seconds, and a census is a fan-out across every
    /// reachable machine.
    ///
    /// It is also the reading a backend is expected to be exact about. herdr's
    /// census can be exact too, but only because it asks twice — the asymmetry
    /// is between the calls it has, not between one seat and many; see
    /// `place_herdr`.
    fn state(&self, seat: &Seat) -> Result<State>;

    /// Every seat this backend has, and what is in it.
    ///
    /// The one read that stays on this side of the line: what a backend is
    /// running is a question only the thing running it can answer. An error is
    /// not an empty list, and callers reaping anything on the strength of this
    /// must treat the two differently — that rule is older than this port
    /// (`sync.rs:41`) and survives it.
    fn census(&self) -> Result<Vec<Seated>>;

    /// Block, calling `f` for each event, until `f` returns false.
    ///
    /// The only way to learn that an agent stopped. A backend that cannot push
    /// polls in here.
    fn watch(&self, f: &mut dyn FnMut(Event) -> bool) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a seat's occupant must not find, and where the line is drawn.
    ///
    /// The rule is a prefix so that the name Claude Code adds next month is shed
    /// without anybody noticing it exists — which is the whole defect, since
    /// [`CHILD_MARKER`] was such a name until it cost a measurement its
    /// transcript. It stops short of every `CLAUDE_` because a configuration
    /// directory is a fact about the machine and is meant to be inherited.
    #[test]
    fn a_new_session_does_not_inherit_the_one_that_spawned_it() {
        for key in [
            "CLAUDECODE",
            CHILD_MARKER,
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_PID",
            "CLAUDE_CODE_SOMETHING_ADDED_LATER",
        ] {
            assert!(shed(key), "{key} names the spawning session and would have reached the seat");
        }
        for key in ["CLAUDE_CONFIG_DIR", "WSP_TASK", "HERDR_SOCKET_PATH", "PATH", SEAT_ENV] {
            assert!(!shed(key), "{key} is not an identity and the seat is poorer without it");
        }
    }

    /// The marker is shed whether or not the caller is carrying it, and the
    /// strip is an empty value rather than an absence.
    ///
    /// Both halves are forced by where the leak can come from. wsp is not the
    /// only carrier — a herdr started by hand inside an agent's pane hands the
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
            "a spawn from a clean shell onto a dirty herdr still has to say this"
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
    /// used to live here now lives in `place_herdr`, next to the wire that
    /// produces it — it was wrong about herdr twice while it was in this file,
    /// and both times because it could be handed a shape herdr does not send.
    #[test]
    fn only_an_agent_known_to_be_idle_is_told_anything() {
        assert!(State::Idle.will_take_a_prompt());
        for silent in [State::Starting, State::Unknown, State::Empty, State::Working, State::Gone] {
            assert!(!silent.will_take_a_prompt(), "{silent:?} was told something");
        }
        assert_eq!(State::default(), State::Unknown, "an absence is not a fact");
    }

    /// A seat with nothing in it is not an agent that died, and neither of them
    /// is running.
    #[test]
    fn a_seat_with_nothing_in_it_is_not_the_same_as_one_whose_agent_stopped() {
        assert_ne!(State::Empty, State::Gone);
        assert!(!State::Empty.is_running());
        assert!(!State::Gone.is_running());
        assert!(!State::Unknown.is_running(), "not knowing is not evidence of work");
        for running in [State::Starting, State::Idle, State::Working] {
            assert!(running.is_running(), "{running:?}");
        }
    }

    /// Nothing in the port reads an id. This is the closest a test can get:
    /// a seat is whatever the backend said, unaltered, including the `@mb2`
    /// suffix the `Remote` decorator puts on it.
    #[test]
    fn a_seat_is_carried_rather_than_parsed() {
        for id in ["w0:p3", "w0:p3@mb2", "7", "sess_01HQ", ""] {
            assert_eq!(Seat::new(id).as_str(), id);
            assert_eq!(Seat::new(id).to_string(), id);
        }
        assert!(Seat::default().is_empty());
        assert_ne!(Seat::new("w0:p3"), Seat::new("w0:p3@mb2"));
    }
}
