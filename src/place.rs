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
//! This is the half of "give me a handle" that nobody had written down, it is
//! where a TTY-less backend breaks first, and it is settled here (decision on
//! robustness-040) — after both of the ways out that were filed turned out not
//! to exist on the only backend there is.
//!
//! Every agent-side verb — `wsp claim`, `wsp say`, `wsp release` — has to name
//! the seat it is standing in, and reads `HERDR_PANE_ID` to do it
//! (`cmd_agent::my_pane`). The port's first answer was that wsp would put the
//! seat's own name into [`Order::env`] under [`SEAT_ENV`] and any backend would
//! carry it. **herdr cannot.** `workspace.create` takes the environment and
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
//!   (checked against `herdr api schema --json`, protocol 17: `label`, `cwd`,
//!   `env`, `focus`) and answers with an id of its own. So a minted seat would
//!   have to be translated back to a pane id on every call and on every census
//!   row, from a table that has to survive a restart — for an id herdr already
//!   delivers. herdr's own place to hang such a table is pane metadata, which
//!   the port deleted as write-only and which expires in 24h anyway. **Minting
//!   pays where the backend takes the minted id, and buys a translation table
//!   where it does not.**
//! - **The agent asks the backend** which seat it is in. herdr has one method
//!   that could answer and it answers a different question. Measured 2026-08-17
//!   against herdr 0.7.5, called from a shell in `w31:p1`: `pane.current` with
//!   no params replied `w1:pJ` — the **focused** pane, in another workspace, on
//!   somebody else's work. Its only parameter is `caller_pane_id`, which is the
//!   answer being asked for. A socket connection carries no identity, so this
//!   option does not exist here; and had it existed it would be a mechanism that
//!   works only while a human happens to be looking at it, which the focus
//!   decision of 2026-08-17 rules out on its own.
//!
//! What is left is what herdr already does, said in the port's words: **the
//! backend delivers the seat to whatever runs in it, and the adapter says under
//! what name.** [`Place::here`] is that reading, and it is the one place that
//! knows. herdr's name is `HERDR_PANE_ID`; a backend with no convention of its
//! own uses [`SEAT_ENV`], which a supervisor can set without difficulty because
//! it mints the seat and *then* forks — it has the name in hand at the moment
//! the child's environment is fixed. herdr's peculiarity is that one call makes
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
//!   has nowhere else to say `--focus` until `arrange` has an implementor.
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

/// The environment variable a seat's occupant finds its own handle in, for a
/// backend that has no name of its own for it.
///
/// **Not a thing wsp writes.** The module docs say why the arrow points the
/// other way: the backend puts this in the seat's environment, because the
/// backend is the only party that knows the seat's name at the moment the
/// environment is fixed. herdr has its own name for the same string and
/// `place_herdr` reads that instead; this is the convention for a supervisor or
/// a runtime arriving without one, so that not every new backend invents a
/// variable and every adapter has to learn it.
///
/// `WSP_SEAT_ID` rather than `WSP_SEAT` because **`WSP_SEAT` is taken, and by
/// the other sense of the word.** `executor/wsp` reads it as the ssh Host alias
/// of the seat *machine* — the one you sit at — and refuses to run without it,
/// and the README's install tells you to set it in every shell an agent might
/// run in. On an executor the two would be one variable in one environment with
/// a pane id on one side and a host alias on the other, and whichever won the
/// other would be silently wrong: the shim would ssh to `w0:p3`, or an agent
/// would claim a seat called `seat-mini`. Renaming the const was one line
/// because nothing read it yet; renaming the shim's would have been every
/// executor's shell profile.
pub const SEAT_ENV: &str = "WSP_SEAT_ID";

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
/// herdr does not quite supply this, and the shortfall is not the one this
/// comment used to claim. Measured 2026-08-19 (`robustness-084`, and see
/// [`crate::place_herdr`] for the transcript): a pane number inside a surviving
/// workspace never comes round again, but a *workspace* id does — herdr's
/// counter is process-local, and a restart reserves only one above the highest
/// workspace that survived. So an id above that mark is handed out again, and
/// with it every pane id qualified by it. A handle can therefore fail in both
/// directions: name nothing, or name somebody else. That is why claims carry a
/// `workspace_label` and a cwd — not to re-find an id that changed, but to tell
/// the workspace we meant from the one that took its name. Under this port the
/// shortfall is the herdr adapter's to make good, by whatever means, and wsp
/// stops carrying the workaround. It is also why
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
/// process table, a page. wsp writes `render-109 · <title>` into it
/// (`cmd_agent::task_label`) because that is legible in ten columns. What it is
/// *not*, after this port lands, is how wsp finds the seat again; see [`Seat`].
#[derive(Debug, Clone, Default)]
pub struct Order {
    pub label: String,
    /// Where the work lives. `None` means the backend's own default, which for
    /// herdr is wherever it would have opened a shell.
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
    /// port ([`crate::arrange`], "Focus"), and the decision on t-260817-008
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
/// The README says why a backend's own vocabulary is not enough: "`idle` is an
/// answer to a question nobody asked". This enum is the answers wsp acts on,
/// each with a caller today.
///
/// **The distinction this is for is [`State::turn_in_flight`], and it is the
/// only one that separates work happening from a slot producing nothing.**
/// Everything else here is about what wsp may *do* to a seat. That one is about
/// whether anything is happening in it, and until robustness-083 nothing in wsp
/// asked it — a census counted processes, and a process that had stopped
/// mid-task counted the same as one three hours into a turn.
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
    /// Busy. Nothing to do about it and nothing to ask it. **The only state in
    /// which a turn is in flight** — see [`State::turn_in_flight`].
    Working,
    /// Stopped in front of a question only a person can answer: a permission
    /// prompt, a trust dialog. Running, and going nowhere until somebody
    /// answers.
    ///
    /// Separate from [`State::Idle`] because the two want opposite handling and
    /// were indistinguishable until robustness-083: an idle agent is told the
    /// next thing, a blocked one is *answered*, and a sentence sent to it lands
    /// in the modal rather than in the composer. It is also the state a Claude
    /// Code reaches by responding to a work order with a permission request,
    /// which is why `fork-015` could not have `agent.prompt` wait for
    /// `working` — a prompt plainly taken passes through here and never
    /// through there.
    Blocked,
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
            State::Blocked => "blocked",
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
        matches!(self, State::Starting | State::Idle | State::Working | State::Blocked)
    }

    /// Whether a turn is actually running — the reading robustness-083 was
    /// opened for, and the one nothing in wsp had.
    ///
    /// [`State::is_running`] answers "is there a process here", which is what
    /// every census in wsp asked and is not the question. Seven agents on
    /// 2026-08-18 answered yes to it with their turns abandoned by an API
    /// overload: process alive, conversation intact, nothing happening, and
    /// `doctor` reporting `herdr up, 12 agents`. **An agent that is not turning
    /// will not start turning on its own**, whatever put it there.
    ///
    /// So this is deliberately not `!is_running()` and deliberately does not
    /// try to say *why* a turn ended. Idle after finishing, idle after an
    /// overload and blocked on a permission are three different repairs and one
    /// identical fact: the slot is costing something and producing nothing.
    /// Telling them apart is a job for whoever reads the census, and wsp does
    /// not have to understand the cause to raise a hand — see
    /// `cmd_agent::Bound`.
    ///
    /// [`State::Unknown`] answers false, on the rule the rest of this file is
    /// built on: an absence is not a fact, and least of all a fact about work
    /// getting done.
    pub fn turn_in_flight(&self) -> bool {
        matches!(self, State::Working)
    }

    /// The same fact from the other side: **there is an agent here and it is
    /// doing nothing.**
    ///
    /// Not merely `!turn_in_flight()`, and the difference is [`State::Unknown`]
    /// and [`State::Empty`]. An agent nobody can see is not an agent that has
    /// stopped, and a seat with nothing in it has not stopped either — it is a
    /// different report with a different verb under it. Only a seat known to
    /// hold something known not to be turning belongs here.
    ///
    /// Written once because four censuses ask it — `wip`'s `needs you`,
    /// `overlap`'s, the panel's `←`, and `doctor`'s quiet check — and until
    /// robustness-083 each of them asked it as `agent_status == "idle"`. That
    /// is one of *three* words herdr has for a pane running no turn, and the
    /// other two are not corner cases: `done` was four of twelve agents on this
    /// machine when it was measured, and `blocked` is a permission prompt,
    /// which is the failure most worth interrupting somebody for.
    pub fn stopped(&self) -> bool {
        self.is_running() && !self.turn_in_flight()
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
/// `cmd_agent::machine_of` reads herdr's `@mb2` suffix and partitions the live
/// list on it. That works only while wsp knows one backend's shape of id, which
/// is exactly what a [`Seat`] is not allowed to have and what the `Remote`
/// decorator makes private. This type is where the fact comes from instead: the
/// fan-out *already knows* which machine answered — `herdr::each` is the line
/// that knows — and until now it threw the answer away and let a caller
/// reconstruct it from the ids that came back.
///
/// [`Census::heard`] and [`Census::silent`] are different constructors and there
/// is no `Default`. The same shape [`crate::arrange::World`] takes, for the same
/// reason: **the cost of this bug is a `Vec::new()` that looks like an answer.**
///
/// **A partial answer is good news.** A census across four machines with one
/// down is three machines' worth of fact, and reading it as a failed call would
/// empty the panel every time a laptop closed — the rule `herdr::everywhere`
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
    /// `reconcile --reap` says it is not — a herdr restoring a session answers
    /// with an empty list for a second or two, and reaping on that is
    /// t-260816-015, every binding in the store gone — so the reap's rule is
    /// this **and** a seat of its own on the same machine. That is one rule in
    /// one place (`cmd_agent::may_reap`), not a second one here; what this type
    /// changes is that the caller can now tell the three silences apart at all.
    pub fn answered(&self, machine: &str) -> bool {
        self.said.iter().any(|(m, s)| m == machine && s.is_ok())
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
    /// The sentence was delivered and the agent never acted on it.
    ///
    /// Only a backend that *watched* can say this, and saying it is the whole
    /// value: it is [`Place::tell`]'s `Ok` and [`Place::nudge`]'s reason
    /// arriving in one answer, so a caller learns in the reply what it
    /// otherwise had to go and find out by looking. Not a failure on its own —
    /// the work order is sitting where the agent can see it, and a submit is
    /// what it is waiting for.
    NotTaken,
    /// This backend cannot do that at all — a seat for a person on a backend
    /// with no terminal, say. Distinct from a failure, and not a bug.
    Unsupported(&'static str),
    /// The backend said no, in its own words.
    Backend(String),
}

/// What became of a sentence [`Place::tell`] delivered.
///
/// `Ok` has always meant *delivered* and never *taken* — the paragraph on
/// [`Place::tell`] is older than this enum and is unchanged by it. What this
/// adds is the answer to the second question where a backend has one, because
/// worklist-010 was a verb that could not tell **delivered and watched** from
/// **delivered and unwatchable** and so said neither, and then said something
/// false instead.
///
/// Two variants and not three. A backend with no way to watch, and a backend
/// that watched and could not tell, are the same fact to every caller: nothing
/// here confirms a turn, nothing here is owed, and a retry is the wrong move.
/// Telling *those* two apart would be a fact about the backend rather than
/// about the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Delivered, and the agent moved in answer to it — watched by something in
    /// a position to see it.
    ///
    /// Moved, rather than *is working*: an agent that answers a work order by
    /// asking for a permission has plainly taken it and is `blocked`. That is
    /// why `cmd_spawn::hand_over` still takes its own reading afterwards — a
    /// folder-trust modal is the same status and the opposite outcome, and this
    /// says the sentence landed on something awake, not what it decided.
    Started,
    /// Delivered, with no turn observed and none claimed.
    ///
    /// Either the backend does not watch, or it watched an agent that was
    /// already mid-turn: a sentence queued behind a turn in progress changes no
    /// status, so there is nothing for a watch to see and the agent has not read
    /// it yet. **This is not a failure and must never be reported as one** —
    /// that report is worklist-010, where a governor retried a message that had
    /// arrived and sent the same paragraph three times.
    Unconfirmed,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreachable(w) => write!(f, "no backend answered: {w}"),
            Refusal::NoSeat(s) => write!(f, "no seat {s}"),
            Refusal::NotReady(s) => write!(f, "not ready — {}", s.as_str()),
            Refusal::NotTaken => write!(f, "the sentence arrived and no turn started"),
            Refusal::Unsupported(w) => write!(f, "this backend cannot {w}"),
            Refusal::Backend(w) => write!(f, "{w}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Refusal>;

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
    /// by asking it. The module docs carry both halves of that: why herdr cannot
    /// be asked, and why a backend that had to be is one that has not delivered
    /// the handle. [`seat_from_env`] is the whole implementation for a backend
    /// with no name of its own for the seat.
    ///
    /// `None` is a real answer — a shell a person opened, a cron job, a build —
    /// and every caller already has a path for it, because `wsp` has always been
    /// runnable from a terminal that is nobody's seat.
    fn here(&self) -> Option<Seat>;

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
    /// is the backend's failure and herdr 0.8.0's own wait between the text and
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
    ///
    /// [`Census`] is that rule made unsayable-wrong rather than remembered: the
    /// answer is per backend, so a machine that was silent is a row of its own
    /// instead of an absence of rows. **A partial answer is an answer** — one
    /// machine down out of four is three machines' worth of fact — so
    /// [`Refusal::Unreachable`] is returned only when *nothing* answered, which
    /// is the sentence it already carries and the moment `herdr::available()`
    /// used to be asked in advance.
    fn census(&self) -> Result<Census>;

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
    /// `herdr up, 12 agents`. These are the two readings side by side, because
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
    /// for the reason t-260816-058 paid for: the cost of this bug is a
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
}
