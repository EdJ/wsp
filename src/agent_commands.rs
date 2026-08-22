//! The second axis: not *where* the seat is, but *what is standing in it*.
//!
//! `place.rs` abstracts the backend — herdr today, a TTY-less supervisor or a
//! web runtime later. It deliberately does not abstract the agent: `Agent::kind`
//! is a string passed straight through, because herdr owns the catalogue of
//! kinds and a list kept here would go stale. That is right for *starting* one
//! and insufficient for everything after it. How you hand an agent a prompt, how
//! you tell whether it is ready, how you resume its session and where its
//! durable output lives are facts about the **agent**, not about the thing
//! hosting it: a Claude Code in a herdr pane has a session channel of its own,
//! and a `codex` in the identical pane can only be typed at.
//!
//! So the two axes are orthogonal, and this is the other one. The argument for
//! it is the decision of 2026-08-17 on project `wsp` and is not repeated here.
//!
//! # What lives here, and what it was doing before
//!
//! [`Kind::args`] is the first inhabitant and it was provably in the wrong file:
//! `cmd_spawn::preamble` was keyed on `kind` and held [`TRIM`], which is Claude
//! Code's flag spelling and nothing else's, in a module about placing work.
//! Nothing about it changes by moving — [`Plain`] is what every other kind gets,
//! and it is what `cmd_spawn` did before this layer existed.
//!
//! [`Kind::tell`] is the second, and it is the one the layer was opened for.
//! `place.rs` already says `tell` is not "send keystrokes" — *"a backend with no
//! terminal has no keystrokes, and a backend with one should use its own
//! submit"* — so **how** a work order reaches an agent is nobody's business but
//! this file's, and the port does not change to accommodate it.
//!
//! # Addressing a Claude Code, which was the unsolved half
//!
//! Delivering a work order over Claude Code's own session channel was proven to
//! work by hand — repeatedly, on panes that were never focused, on the nights
//! `wsp spawn`'s own delivery failed seven times running. What blocked it was
//! addressing: the channel is addressed by a session **name** (`wsp-f3`), wsp
//! holds a session **id** (`7a188ba8-…`), and the two are not derivable from
//! each other. Sending to the raw id is refused — *"No agent named 7a188ba8-…
//! is reachable"*.
//!
//! `claude agents --json` closes it, and needs no new mechanism on either side.
//! It is documented for scripting and does not require a TTY, and each row
//! carries `sessionId`, `name`, `pid`, `cwd` and `status`. herdr's
//! `agent_session.value` **is** that `sessionId` — `herdr.rs` parses it into
//! `Pane::session_id` and [`Seated::session`] carries it across the port — so
//! wsp already holds the key, and one call turns it into the handle. Verified
//! 2026-08-17 against the exact pair recorded as underivable.
//!
//! The handle therefore travels one way, unlike [`crate::place::SEAT_ENV`]'s,
//! and no `SessionStart` round-trip is needed.
//!
//! # What is still missing is the transport, not the address
//!
//! Stated precisely so the next attempt starts from the right place. The channel
//! is a unix socket — `CLAUDE_CODE_MESSAGING_SOCKET`, which is
//! `/tmp/cc-socks/<pid>.sock` and whose pid is in the same listing —
//! authenticated by `CLAUDE_CODE_MESSAGING_TOKEN`. Its wire protocol is
//! undocumented, and there is no `claude` subcommand that sends on it: the whole
//! command list is `agents`, `auth`, `auto-mode`, `doctor`, `gateway`, `import`,
//! `install`, `mcp`, `plugin`, `project`, `setup-token`, `ultrareview` and
//! `update`. So [`Claude::tell`] delivers the way `place_herdr` always has, and
//! the channel lands in one function when there is a supported way to speak it.
//!
//! What the address buys in the meantime is recovery: [`Kind::address`] is asked
//! when a spawn fails, and the sentence it produces is the one that turned seven
//! hand-recoveries into a guess at which name matched which pane.
//!
//! # Why that sentence was silent on every failure it was written for
//!
//! robustness-049, and it is worth the space because the shape of the answer is
//! the design rule this file is built on rather than a patch. Measured during
//! robustness-041's reproduction: `wsp spawn`'s recovery line printed **nothing,
//! three times out of three**, on exactly the failures it exists for. Two
//! independent causes, and only one of them is the race it looked like.
//!
//! **The deterministic one: the backend's census is the wrong place to recompute
//! the handle from.** `address` used to mint from `Seated::agent.name`, and on
//! the failure path there is no such name. Measured against herdr's live socket
//! on 2026-08-17: an `agent.list` row carries `name` and `agent_session.value`,
//! and a `pane.list` row carries the agent's *kind* and neither of the other
//! two. `Herdr::census` merges them per pane and falls back to the pane row —
//! and a seat herdr has gone blind on is precisely a seat with no `agent.list`
//! row: `State::Empty` is `agent.get` answering that this pane has no agent, and
//! a pane with no agent has no row in the list of them either, the same record
//! read two ways. So on every failure of the sort robustness-041 reproduces, the
//! census offers an empty name; [`mint`] turns that into a seat-only handle
//! nothing answers to, and [`pick`] is left with no session id and a worktree it
//! cannot prove is anybody's. Silence, every time, with no timing in it at all.
//!
//! wsp never needed to ask. It chose the handle before the agent existed, which
//! is the whole point of minting, so [`Kind::address`] now takes the same
//! [`Spawn`] [`Kind::running`] does and recomputes from that. The census is kept
//! for one job only — [`pick`], for agents wsp did not start — and a census wsp
//! cannot read no longer costs the sentence.
//!
//! **The race, which is real but second: the runtime's registry is written after
//! the agent starts.** Measured three times on 2026-08-17, launch to the session
//! file under `~/.claude/sessions/`: **1,306ms, 1,440ms, 1,345ms**, and
//! `claude agents --json` answers about it ~500-700ms later still, because that
//! is what the subprocess costs. A spawn that fails fails early, so an ask that
//! fires once loses. [`Lag`] is the answer: ask again for a few seconds, which
//! costs nothing but a failure path's patience.
//!
//! **And when the wait runs out, say the handle anyway.** [`Address`] carries the
//! confidence with the name, because confirmation answers *is it alive* and not
//! *is this the right name* — see [`Address::Unconfirmed`]. A hedged sentence is
//! strictly better than the silence a person got, and this is the shape the
//! contrast with [`Kind::running`] recommends: being late costs a hedge instead
//! of costing the answer.
//!
//! One measurement fell out of the same probe and is recorded because it makes
//! both readings blind in a case nobody would suspect: a session that inherits
//! [`crate::place::CHILD_MARKER`] **never registers at all**. The first probe of
//! the lag above came up, drew its banner and took a prompt, and no session file
//! or `claude agents --json` row ever appeared for it in twenty seconds; with
//! the caller's identity shed, 1.3s. So [`crate::place::shed`] is what keeps
//! wsp's own spawns visible here, and an agent a person launched by hand from
//! inside another agent's pane has no address for wsp to find.
//!
//! # The next tenants
//!
//! Named rather than absorbed, because the layer is worth more than any one of
//! them and a first pass that took all five would have been unreviewable:
//!
//! - **robustness-041, readiness.** `wait_ready` declares an agent dead while it
//!   is alive, because herdr's claude detection needs a rendered pane and the
//!   focus decision forbids rendering one. `claude agents --json` answers
//!   liveness and `idle`/`busy` per session with no pane involved at all, which
//!   is a per-kind reading of exactly the sort this trait is for.
//! - ~~**robustness-037, environment hygiene.**~~ Refuted, and worth keeping as
//!   the one prediction on this list that was wrong. The guess was that which
//!   variables a kind must not inherit is a fact about that kind. It is not:
//!   what leaks is `CLAUDE_CODE_CHILD_SESSION` and the rest of the *caller's*
//!   session identity, which is wrong to pass on whatever is about to be
//!   started in the seat — including a bare shell, which has no kind at all.
//!   The rule went to [`crate::place::shed`], beside `SEAT_ENV`, where it is
//!   asked once per seat rather than once per agent. A kind-shaped question and
//!   a spawn-shaped one look alike from here; the test is whether the answer
//!   changes when nothing is started.
//! - **render-061, `--resume`.** Recording a session id against a task so an
//!   agent can be picked up again. The same field this file already reads.
//! - ~~**wsp-059, `--model` and `--effort`.**~~ Landed as `wsp-059`, and it
//!   arrived as predicted — more of [`Kind::args`], reading two more fields of
//!   the [`Spawn`] that verb takes instead of a bare flag. What it added that
//!   the prediction did not have is [`Kind::tier`]: a vocabulary is per-kind in
//!   the same way a flag spelling is, so the *check* belongs beside the
//!   spelling and not in `cmd_spawn`.
//!
//! # Minting, which is what the lookup turned out to be a fallback for
//!
//! The lead above — `-n, --name <name>` from `claude --help`, unrun when this
//! file was written — was run on 2026-08-17 against Claude Code 2.1.233, and it
//! holds: [`mint`] is now what names a spawned agent and the lookup below is
//! what answers for agents wsp did not start. The registry those measurements
//! read is `~/.claude/sessions/<pid>.json`, which is the same census
//! `claude agents --json` prints.
//!
//! What made it worth doing was not the flag working. It was the *reason* the
//! derived name cannot be relied on, and that reason was nearly missed, because
//! by the time this ran robustness-010 had given every task a worktree named after
//! it and Claude Code's own derived name had quietly become **almost right**:
//!
//! - A derived name is `<basename of cwd>-<two hex digits>`, stamped
//!   `"nameSource":"derived"`. Standing in `…/.worktrees/robustness-044`, an agent
//!   calls itself `t-260817-014-f6` with nobody asking it to. So the task id was
//!   already the prefix, and minting looked redundant.
//! - **The suffix is not stable.** Two sessions started in one directory,
//!   seconds apart, were `nametest-45` and `nametest-4d`. So the handle is still
//!   unknowable until after the agent exists, which is the whole of what the
//!   lookup was for — a name that is 90% predictable buys nothing, because the
//!   10% is the part you have to address it by.
//! - And the prefix agrees with the task id only because a *path convention*
//!   another task owns says so. wsp would be inferring its addressing scheme
//!   from the shape of somebody else's directory name, and robustness-010 is free
//!   to rename those trees tomorrow without knowing it had broken this.
//!
//! An explicit `-n` overrides derivation completely and leaves `nameSource`
//! unset, so the two are distinguishable after the fact as well as before.
//!
//! # Why the seat is in the handle
//!
//! The one thing derivation does that a task id alone does not: **Claude Code
//! does not enforce unique names.** Two sessions started as `-n dupe-probe` both
//! took it, with no error and no uniquifying suffix, and a duplicate name is
//! ambiguous to address — which is the failure [`pick`] already refuses to
//! commit. The random suffix is derivation's answer to that, and minting a bare
//! task id would throw it away: `spawn --force` onto a task whose previous agent
//! is still idle is two live sessions called `robustness-044`.
//!
//! So the handle is the task **and** the seat, and neither half is decoration.
//! The task id is what a person reads; the seat is what makes it unique, because
//! herdr issues at most one live agent per pane. Both are in wsp's hand *before*
//! [`Place::start`] is called — the port splits `open` from `start` precisely so
//! that the seat exists before the agent does — and both are on the census row
//! afterwards, so [`Kind::address`] can recompute the same string it minted
//! rather than storing it anywhere.
//!
//! `--session-id <uuid>`, the other lead, remains unrun and is now unneeded.

use std::collections::{BTreeMap, HashSet};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::place::{Delivery, Place, Refusal, Result, Seat, Seated};
use crate::util;
use crate::util::Clock;

/// How wsp talks to one sort of agent.
///
/// Object-safe and stateless: [`of`] hands back a `&'static dyn Kind`, so a kind
/// is chosen by the string that came off the command line and nothing has to be
/// constructed to ask it a question.
///
/// Three methods, and each has a caller today. The rule `place.rs` set holds
/// here too — a verb with no caller is a tax — and the four tenants named in the
/// module docs are the ones that will earn the next ones.
pub trait Kind {
    /// The flags an agent of this kind is started with.
    ///
    /// Takes a description of the spawn rather than the bare `full` flag it
    /// started as, which the module docs predicted would happen the first time
    /// this verb needed to know anything about *which* spawn it was arguing
    /// about. Minting the handle is that first time: `-n` is not a constant,
    /// it is a function of the task and the seat.
    fn args(&self, spawn: &Spawn) -> Vec<String>;

    /// Whether this kind will accept the tier it has been asked for, and why
    /// not if it will not.
    ///
    /// The gate in front of [`Spawn::model`] and [`Spawn::effort`], and it is
    /// here for two reasons that pull the same way.
    ///
    /// **An unknown tier fails late and in the wrong place.** herdr starts the
    /// process happily whatever is on its command line; what refuses is the
    /// agent, in a pane nobody is looking at, and `cmd_spawn` reports that as
    /// "claude started but never became ready for input" — a sentence about
    /// readiness for what is a typo. Worse, measured 2026-08-18 against Claude
    /// Code 2.1.234: an unknown `--model` at least refuses, but an unknown
    /// `--effort` prints *"Warning: Unknown --effort value 'bogus' — ignoring
    /// it and using the default effort"* and carries on. So `--effort max` with
    /// a typo in it is a session that ran at default effort and said it ran at
    /// max, which no failure path anywhere would ever have caught. Checking at
    /// this end is what puts the typo in the sentence.
    ///
    /// **And these are the arguments an agent could compose.** `wsp spawn` is a
    /// verb agents drive, so a value that reached the command line unexamined
    /// would be an injection surface with wsp's own hands on it. A fixed
    /// vocabulary per kind, not a passthrough: that is the decision the spawn
    /// strategy already recorded about command templates, applied to the first
    /// flags to test it.
    ///
    /// Both are per-kind facts, which is why this is a trait method and not a
    /// check in `cmd_spawn`: `claude` spells its tiers one way, and the
    /// catalogue of any other kind belongs to that kind.
    fn tier(&self, model: Option<&str>, effort: Option<&str>) -> std::result::Result<(), String>;

    /// Why this tier cannot be left to work on its own, if it cannot.
    ///
    /// Separate from [`Kind::tier`] because it answers a different question.
    /// `tier` asks whether the runtime will *accept* the words; this asks
    /// whether the session they start can be walked away from — and a tier can
    /// pass the first and fail the second, which is exactly the case that was
    /// paid for.
    ///
    /// **Measured 2026-08-18, Claude Code 2.1.234**, two spawns into herdr panes
    /// differing in nothing but `--model`, on the same task and the same
    /// worktree path minutes apart. The Sonnet 5 pane opened on `auto mode on
    /// (shift+tab to cycle)`; the Haiku 4.5 pane opened on `manual mode on`,
    /// with no cycle hint offered and no effort indicator beside the composer.
    /// wsp sends no `--permission-mode` — [`Kind::args`] sends the trim, the
    /// name and the tier and nothing else — so a pane gets Claude Code's own
    /// default, and that default is not the same for the two tiers.
    ///
    /// What that costs is the reason this is a refusal and not a footnote. A
    /// manual-mode pane with nobody in front of it runs only what
    /// `~/.claude/settings.json` already allows, and that allow-list is written
    /// around the wsp verbs. The haiku run stopped twenty-five seconds in, on
    /// its third action, reaching into `~/wsp` with `find` for a record its
    /// brief already held; herdr reported the pane blocked and nothing said why.
    /// A spawn that produces that is worse than a refused flag, because the
    /// refusal is read in the second it is typed and the hang is found later by
    /// somebody wondering why a lane went quiet.
    ///
    /// `None` for a kind that has nothing to say, which is every kind but
    /// `claude` — the honest answer where nobody has measured, and the same
    /// answer [`Kind::running`] gives for the same reason.
    fn unattended(&self, _model: Option<&str>) -> Option<String> {
        None
    }

    /// What this agent's own runtime calls the agent in this seat, if it can be
    /// found out.
    ///
    /// A *second* name for the same agent, and the distinction is the point:
    /// [`Seat`] is what the backend calls the place, and this is what the agent
    /// runtime calls the occupant. They are issued by different systems and
    /// neither can be derived from the other.
    ///
    /// `None` for a kind with no such notion, and — deliberately — for one where
    /// the answer is ambiguous. A wrong handle is worse than no handle: it names
    /// somebody else's agent. An *unnamed* one is a third thing and not that
    /// one: see [`Address::Unconfirmed`], which is a name wsp is certain of and
    /// an agent it is not.
    ///
    /// Takes the [`Spawn`] rather than the seat alone, which is robustness-049's
    /// correction and the module docs carry the measurement. The seat is on the
    /// spawn; what the spawn adds is the name wsp chose, and asking the backend
    /// to remember that instead is what made this silent on every failure it was
    /// written for.
    fn address(&self, place: &dyn Place, spawn: &Spawn) -> Option<Address>;

    /// Give the agent in a seat a sentence to act on.
    ///
    /// Not `Place::tell` renamed. The port's verb is what the *backend* can do
    /// about it; this one chooses between that and whatever the agent itself
    /// offers, and typing at a terminal is only the fallback.
    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<Delivery>;

    /// Whether a session of this kind arrives with its wsp brief already in
    /// context.
    ///
    /// **`false` by default, because the brief is delivered by a hook only
    /// Claude Code runs.** `claude-code/settings.snippet.json` wires
    /// `wsp-session.sh` into Claude Code's own settings file under
    /// `SessionStart`, and nothing else in wsp installs anything anywhere: a
    /// `codex`, a `gemini` or an `opencode` starts with an empty context
    /// whatever wsp has recorded.
    ///
    /// It exists because [`crate::cmd_spawn::work_order`] tells a spawned agent
    /// *"Your brief is already above"*, and for those kinds that is simply
    /// untrue. Driven on 2026-08-21: an opencode spawned with the order in argv
    /// received it, went looking for context with `git status`, and answered
    /// *"I do not want to invent a task for oc2-001, so could you paste the
    /// brief directly"*. The order arrived perfectly and could not be acted on.
    ///
    /// **What answering `false` now buys is the brief itself, not a different
    /// sentence.** It used to select `Handover::Running` — *"please run `wsp
    /// brief --session`"* — and `core-032` replaced that with wsp composing the
    /// brief for the seat and carrying it in front of the order, so the
    /// sentence every spawn says is true for every kind. The round trip that
    /// wording cost is the point: `wsp brief --session` is ~3,300 tokens
    /// fetched at request 1 rather than sitting in the context already, and
    /// under `core-020` d2's brake it is also the agent's first `bash`, so an
    /// unwatched lane stalled on the agent finding out what it was for.
    ///
    /// The old wording is still reached, by a kind that is unbriefed *and* is
    /// told its order rather than handed it in argv. That is
    /// [`crate::cmd_spawn::Route`], and the reason is written there.
    fn briefed(&self) -> bool {
        false
    }

    /// Whether this kind is handed its work order in argv, rather than being
    /// told it once it is listening.
    ///
    /// `false` for every kind but one, and the exception was measured rather
    /// than designed. `core-026` drove `--kind opencode` end to end three
    /// times and the work order failed to arrive all three: opencode's TUI
    /// refuses typed input for **~2s after herdr's `agent.start` returns and
    /// reports `interactive_ready`**, and text sent inside that window is
    /// *discarded* — the composer is empty afterwards, not holding an unsent
    /// sentence. Sweep of two runs × four delays, start→prompt: 0s and 1s
    /// dropped, 2s and 4s landed.
    ///
    /// **That it is discarded rather than left sitting is what makes argv the
    /// fix.** [`crate::cmd_spawn`]'s rescue for a work order that did not start
    /// a turn is to press submit again, and there is nothing to submit; no
    /// number of nudges recovers a sentence the composer never took. Waiting
    /// longer would work and is the wrong shape — it makes every spawn pay for
    /// one kind's startup, and it is a race wsp would be tuning against a
    /// version number it does not control.
    ///
    /// A kind answering `true` promises that [`Kind::args`] puts
    /// [`Spawn::order`] on the command line, so `cmd_spawn` must **not** also
    /// tell it: the sentence is already delivered and telling would say it
    /// twice. What that costs is the one thing argv cannot do — a work order
    /// for an agent that is already running — and nothing wants that here,
    /// because an agent already running is `Handover::Running`'s case and is
    /// reached through [`Kind::tell`] like everything else.
    fn order_in_args(&self) -> bool {
        false
    }

    /// What this kind's seat needs in its environment, beyond what every seat
    /// gets.
    ///
    /// Empty for every kind that needs nothing, which is all of them but one.
    /// It is here rather than in `cmd_spawn` for the reason [`TRIM`] is here:
    /// the names and the JSON in it are one runtime's spelling, and a module
    /// about placing work should not know them.
    ///
    /// **Reached through [`crate::cmd_spawn::seat_env`] and nowhere else**, and
    /// that is `core-038` rather than tidiness. It had one caller, in `spawn`,
    /// which composed it onto the `WSP_*` set at the call site; `resume` built
    /// the same order out of the same parts and took only the first of them, so
    /// a resumed opencode ran under opencode's shipped policy instead of
    /// `core-020` d1's. Two builders that have to agree is the defect. Anything
    /// that opens a seat now names its occupant or says there is none, and the
    /// composition happens once.
    ///
    /// **Why an environment variable and not a file**, since the obvious
    /// reading of `core-020` d1 is a config file in the worktree. Measured:
    /// `cmd_checkout::dirty` asks git with `--untracked-files=all`, so a file
    /// wsp writes into a working tree makes that tree permanently dirty — every
    /// opencode worktree would refuse `wsp checkout --rm`, be skipped by
    /// `--sweep`, and be kept by `wsp despawn` with "uncommitted work in it".
    /// It would also collide with a repository that ships an `opencode.json` of
    /// its own, and overwriting one is not wsp's to do. The environment carries
    /// exactly the same config — verified against `opencode debug config`, which
    /// resolves the two routes identically — and leaves nothing behind to clean
    /// up.
    ///
    /// **And not argv, which is the third carrier and the one that would have
    /// made `core-038` impossible.** An agent's command line reaches a seat wsp
    /// did not open, since herdr's `agent.start` takes `args`; an environment
    /// does not. It is unavailable rather than declined: opencode's TUI has no
    /// config flag at all — `opencode --help` at 1.18.x offers `--model`,
    /// `--session`, `--prompt` and `--agent`, and nothing that names a config —
    /// so for the one kind that needs configuring there is no argv route to
    /// prefer. That is why the rule in [`crate::cmd_resume`] has to be about
    /// where a seat came from rather than about how it is told.
    ///
    /// `brief` is the file `spawn` will write this seat's brief into, when the
    /// kind is one that reads such a file — see [`Kind::brief_file`]. `None`
    /// for every kind that is not, and for a spawn with nothing to brief about.
    /// It is a path rather than the text because the seat's environment is
    /// fixed when the seat opens, and the brief cannot be composed until the
    /// claim has landed a moment later; the path is knowable in advance and the
    /// content is not.
    fn env(&self, _brief: Option<&Path>) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Whether this kind will read a brief out of a file wsp names in its
    /// configuration, rather than having to be handed one or sent for one.
    ///
    /// **`false` by default, and true for exactly one kind, because it is a
    /// fact about a runtime's config format rather than a thing wsp can arrange.**
    /// opencode has an `instructions` key taking a list of paths, which it loads
    /// as system context before the session's first message. `core-032` d7 drove
    /// it: a brief written to a file outside any working tree, its absolute path
    /// in `instructions`, and an opencode with `bash` and `edit` both **denied**
    /// answered what task it was holding and quoted a token that existed nowhere
    /// but that file. No tool call, so no `bash`, so `core-020` d2's brake is
    /// never met — which is the whole of what this row is for.
    ///
    /// It is what [`crate::cmd_spawn::Route::Inline`] selects on, and it
    /// deliberately is **not** [`Kind::order_in_args`]. Those were the same
    /// predicate for one commit and it was wrong: the brief went into a
    /// `--prompt` argv element, and herdr refuses `agent.start` args holding any
    /// control character — a brief is multi-line, so three spawns of three were
    /// refused with `invalid_agent_argument` and no agent started. Env is not
    /// argv and that guard does not reach it.
    fn brief_file(&self) -> bool {
        false
    }

    /// Whether a sentence delivered to this kind lands in a queue that belongs
    /// to the **agent** rather than to wsp.
    ///
    /// True by default, because the default delivery is [`crate::place::Place::tell`]
    /// — herdr typing at a pane — and what receives it is whatever the agent
    /// does with a prompt that arrives mid-turn. Driven against a real Claude
    /// Code on 2026-08-21 (`core-021` d2): it *queues* it and answers it at the
    /// turn boundary, and nothing is typed at a live composer. So this is not
    /// about corruption.
    ///
    /// It is about **durability**. That queue lives in the agent, so a wake
    /// handed to it and then `/clear`ed, restarted or killed is a wake wsp has
    /// recorded as delivered and nobody ever read — a silent drop with wsp's
    /// name on it, which is the one thing `core-017` forbids. `crate::wake`
    /// holds a wake until the turn ends for that reason and no other, which
    /// costs a wake ninety seconds.
    ///
    /// The candidate for `false` was opencode's ACP server (`core-020`), and it
    /// was driven against a real opencode rather than read — `core-026`,
    /// 2026-08-21, sandbox herdr — and it answers **true** anyway, without ACP
    /// being needed to find that out. What was seen: a mid-turn `wsp tell`
    /// reaches opencode's *durable session record* immediately, as a `user`
    /// message in `opencode export`, and is answered at the turn boundary. Then
    /// the test this predicate actually asks for — killed mid-turn by pid with
    /// a sentence pending, session resumed — and the sentence was still there,
    /// unanswered, with the agent idle. Durable, and **stranded**: nothing
    /// re-drives it.
    ///
    /// So the bar was never durability on its own. `false` says *the transport
    /// will see this read*, and a record that survives with nobody running a
    /// turn on it is `core-017`'s drop wearing a transcript — worse than the
    /// lost shape, because the evidence sits there looking delivered. A kind
    /// that overrides this owes that same drive and the second half of it: not
    /// that the sentence is still in the record afterwards, but that something
    /// on the far side picks it up with wsp saying nothing further.
    ///
    /// Holding for the turn boundary costs a wake ninety seconds, which is the
    /// price of the answer being true, and it is cheap.
    fn queue_is_the_agents(&self) -> bool {
        true
    }

    /// Whether the agent wsp started here is running, as its **own** runtime
    /// sees it — `None` where the kind has no runtime to ask.
    ///
    /// The second opinion, and the only interesting thing about it is who is
    /// being asked. Every other reading of a live agent in wsp comes from the
    /// backend, so a backend that cannot see one has the last word on whether it
    /// exists; this asks the thing that *is* the agent. No `&dyn Place` in the
    /// signature says so, and that is deliberate rather than economical: an
    /// answer that needed the backend would be worth nothing here.
    ///
    /// It can rescue and it must not condemn, which is the rule
    /// [`crate::cmd_spawn`] applies and the reason this returns three answers
    /// rather than two. A session's registry entry is written a moment *after*
    /// the agent starts — measured on 2026-08-17, `wsp spawn`'s own recovery
    /// sentence stayed silent through the first two seconds of a live launch
    /// because of it — so `Some(false)` early in a launch is as likely to mean
    /// "not yet" as "never". Corroborating a seat that has already read empty
    /// for a while is what it is good for.
    fn running(&self, spawn: &Spawn) -> Option<bool>;

    /// What actually served this session, read back off the agent's own
    /// durable record of it — `None` for a kind that keeps none, or when it
    /// cannot be found.
    ///
    /// The other half of [`Kind::tier`], and the reason wsp-060 exists. `tier`
    /// checks what a spawn *asks for*; this reads what a session *got*, and the
    /// two part company in ways no failure path anywhere would catch:
    ///
    /// - An agent types `/model` mid-session and finishes on a tier nothing
    ///   wsp wrote down names. A record labelled with the flag it was spawned
    ///   under would call that session by the tier it left behind, which is a
    ///   router trained on the opposite of what happened.
    /// - A spawn that names nothing at all — today's ordinary case — is
    ///   labelled only by `~/.claude/settings.json`, which is not versioned,
    ///   not shared between machines, and changes under the record.
    /// - `--on <machine>` runs that machine's `claude`, whose settings and
    ///   installed version are its own; the flag states the tier and cannot
    ///   guarantee it.
    ///
    /// So the calibration field is *ran at* and not *spawned at*, and both are
    /// kept: the second is the intent, the first is the fact, and the pair is
    /// what says whether stating a tier changed anything.
    ///
    /// Keyed on the session rather than the seat, because the seat is gone by
    /// the time this is asked — it is read when a claim ends — and the session
    /// is the one handle that outlives it. `cwd` is a hint and not a key: see
    /// [`transcript`], which falls back to a scan when the tree has moved.
    ///
    /// # `since` is what makes this an attempt rather than a session
    ///
    /// Epoch seconds of the claim this is being read for, or `0` for the whole
    /// session. **One session serves several attempts and always has**: an
    /// agent finishes a task, is handed the next, and works both from the same
    /// seat and the same session — which is the ordinary sequence `hand_off`
    /// is written for. Read whole, every attempt after the first is labelled
    /// with the session's running totals, so the second attempt on a seat
    /// reports the first one's turns and tokens as its own and a sum over
    /// attempts counts the early ones once per attempt that followed them.
    ///
    /// Found here rather than in the field, because `core-029` is the first
    /// row to add a number that is *added up*: a turn count read twice looks
    /// like a busy agent, and a token count read twice is a bill.
    ///
    /// Both runtimes stamp every message, so the window costs nothing to apply
    /// and it fails open — a message wsp cannot date is counted, since dropping
    /// evidence is the worse of the two errors and the tier label is right
    /// either way.
    fn ran(&self, session: &str, cwd: &str, since: i64) -> Option<Ran>;

    /// How this kind's own command line says *pick that session back up* —
    /// `None` for one that cannot be told to.
    ///
    /// The smallest possible fact and it is here for the reason the module docs
    /// open with: `--resume` is Claude Code's spelling, opencode's is
    /// `--session`, and until `core-031` the one word was written into
    /// [`crate::cmd_resume::Thread::by_hand`] as though every agent shared it.
    /// What that printed for a live opencode was `claude --resume ses_…` —
    /// wrong binary and wrong flag in one line, offered to a person as the
    /// recovery path for when herdr itself is broken.
    ///
    /// [`Kind::args`] reads it too, so a kind spells its resume in one place
    /// and the argv wsp builds and the line it prints cannot drift apart.
    ///
    /// `None` is not a gap to be filled in with a guess. A kind wsp has no
    /// resume spelling for is one whose thread it cannot pick up, and starting
    /// a *fresh* session while saying "resumed" is the failure this returns
    /// three answers to avoid — see [`crate::cmd_resume::bring_back`], which
    /// refuses rather than starting one.
    fn resume_flag(&self) -> Option<&'static str> {
        None
    }
}

/// What is about to be started, as much of it as a kind is allowed to know.
///
/// No store, no `Args`, no task: a kind decides flags, and a kind that could
/// read the task would start deciding other things. `full` was the whole of
/// this parameter before minting needed the seat and the name, and `model` and
/// `effort` are the third thing to arrive that way — every one of them a fact
/// about *this* spawn that only the kind knows how to spell.
///
/// `name` is the task or project id — the same string [`crate::place::Agent`]
/// carries for the backend's own naming — and `seat` is where it is about to be
/// started. Both are already in the caller's hand at that moment; see the
/// module docs for why the pair is the handle rather than either alone.
pub struct Spawn<'a> {
    /// Keep the whole preamble: the way back from [`TRIM`].
    pub full: bool,
    pub name: &'a str,
    pub seat: &'a Seat,
    /// The tier this spawn was asked for: which model, and how hard it thinks.
    ///
    /// Two `Option`s rather than one enum with a default, because **absent must
    /// send no argument at all**. A spawn that names neither is byte-for-byte
    /// the spawn wsp did before this existed — whatever `~/.claude/settings.json`
    /// says, at whatever effort the model defaults to — and that is the same
    /// compatibility rule `--on` keeps. A default spelled here would be wsp
    /// quietly overriding a person's settings file the first time they upgraded.
    ///
    /// Validated before they arrive: see [`Kind::tier`]. Nothing between there
    /// and the agent's command line inspects them again.
    pub model: Option<&'a str>,
    /// How hard, of [`EFFORTS`] — the cheaper knob and the one to reach for
    /// first, being the same capability class for less spend.
    pub effort: Option<&'a str>,
    /// The work order this agent is being started to act on, for a kind that
    /// takes one **in argv** rather than by being told it afterwards.
    ///
    /// `None` everywhere else, and that is not a default so much as the whole
    /// distinction: [`Claude`] is handed its order by [`Kind::tell`] once it is
    /// listening, and reads this field never. It exists because measuring
    /// `--kind opencode` end to end (`core-026`) found that being told is not
    /// reliable for every kind — see [`Kind::order_in_args`] for what was
    /// measured and why argv is the fix rather than a longer wait.
    pub order: Option<&'a str>,
    /// A session to pick up where it left off, rather than a new one.
    ///
    /// render-061 above, arriving as `render-061`: the id is wsp's — read off
    /// a binding or a seat — and what to *do* with it is the kind's, because
    /// only the kind knows whether its runtime can resume at all. Claude Code
    /// takes `--resume <uuid>`; a kind that cannot ignores this and starts
    /// fresh, which is the same fallback an unknown kind gets for everything
    /// else here.
    pub resume: Option<&'a str>,
}

/// The handle wsp will address this agent by, decided before it exists.
///
/// Deterministic, and that is the entire point — the same inputs give the same
/// string at `agent.start`, at a failed spawn's recovery sentence, and to
/// anything later that wants to send on the session channel without asking who
/// is there first. The module docs carry the argument for the shape; the shape
/// is `<task>-<seat>`, which is deliberately the same `<prefix>-<suffix>`
/// spelling Claude Code derives for itself, with a suffix that means something.
///
/// `None` only when there is nothing to build one from. An empty half is
/// dropped rather than spelled as an empty string, because `t-260817-014-` and
/// `-w2J:p1` are both names somebody would have to explain.
pub fn mint(name: &str, seat: &Seat) -> Option<String> {
    let parts: Vec<&str> = [name.trim(), seat.as_str().trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    match parts.is_empty() {
        true => None,
        false => Some(parts.join("-")),
    }
}

/// The kind a `--kind` names, or [`Plain`] for one this file has nothing to say
/// about.
///
/// Unknown kinds are not refused here. herdr owns the catalogue and refuses an
/// unknown kind with the whole of it in the message, which is a better list than
/// one kept in wsp and left to go stale — so this falls through to the behaviour
/// `cmd_spawn` had before the layer existed, and adding the layer changed
/// nothing for `codex` or `gemini`.
pub fn of(kind: &str) -> &'static dyn Kind {
    match kind.trim() {
        "claude" => &Claude,
        "opencode" => &OpenCode,
        _ => &Plain,
    }
}

/// An agent wsp knows nothing about beyond its name.
///
/// Started with no flags of wsp's, told by whatever the backend can do, and
/// unaddressable by anything but its seat. This is not a stub: it is the honest
/// answer for every kind nobody has measured, and it is what `spawn` did for
/// every kind including `claude` until the trim landed.
pub struct Plain;

impl Kind for Plain {
    fn args(&self, _spawn: &Spawn) -> Vec<String> {
        Vec::new()
    }

    /// Refused rather than dropped.
    ///
    /// `codex` and `gemini` both have models; wsp does not know how either
    /// spells one, and [`Plain::args`] passes nothing on. Accepting the flag
    /// and starting the agent anyway would be a spawn that says it is running
    /// haiku and is not — the same invisible failure the effort warning is,
    /// with wsp doing it on purpose. So say so, and the day somebody measures a
    /// kind's spelling it stops being true.
    fn tier(&self, model: Option<&str>, effort: Option<&str>) -> std::result::Result<(), String> {
        match model.is_some() || effort.is_some() {
            true => Err("wsp knows no tier vocabulary for this kind — only `--kind claude`".into()),
            false => Ok(()),
        }
    }

    fn address(&self, _place: &dyn Place, _spawn: &Spawn) -> Option<Address> {
        None
    }

    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<Delivery> {
        place.tell(seat, text)
    }

    /// Nothing to ask. An agent wsp knows nothing about beyond its name is one
    /// whose backend is the only witness there is.
    fn running(&self, _spawn: &Spawn) -> Option<bool> {
        None
    }

    /// And nothing to read. A kind wsp cannot start at a stated tier is a kind
    /// whose sessions it has no business labelling with one — the record stays
    /// empty rather than being filled in with the default it would have
    /// guessed.
    fn ran(&self, _session: &str, _cwd: &str, _since: i64) -> Option<Ran> {
        None
    }
}

/// opencode.
///
/// The second kind wsp knows by name, and the first that can be *chosen* — see
/// `core-020`, whose point is balancing model usage and which cannot begin until
/// wsp can say which model. Everything here was measured against opencode
/// 1.18.20/1.18.21 in a sandbox herdr rather than read off `--help`; `core-026`
/// holds the driving and `core-027` the decisions.
///
/// [`Kind::running`] stays [`Plain`]'s answer of nothing: the backend is the
/// only witness to whether an opencode is alive. [`Kind::ran`] is not — see
/// there for what `opencode export` publishes, which is a better record than
/// the one wsp derives for Claude Code and is read into the same shape so that
/// neither kind gets a column the other cannot answer.
pub struct OpenCode;

/// The wsp verbs a spawned opencode may run without asking.
///
/// **This list is not wsp's opinion; it is Ed's `~/.claude/settings.json`,
/// verified against it verb for verb.** `core-020` d2 settles that the policy
/// mirrors the shape of that file rather than inventing a second one, and the
/// reason is the one d1 missed: allowing only `wsp brief` past the brake does
/// not remove the stall, it moves it one command later to `wsp show`. An agent
/// reads its brief, then reads the task, then the parent, then what binds it —
/// and every one of those is a person's attention spent on a verb that changes
/// nothing.
///
/// So the line is *read and record freely, stop before changing anything
/// outside your own record*, which is exactly where Ed's own file draws it.
///
/// **What `core-032` narrows, and by exactly how much.** `wsp brief` is still
/// on this list and the list is unchanged — twenty-three allows and four
/// denies, verb for verb what Ed's file has. What changed is that the brief
/// entry is no longer *load-bearing*: the spawn that used to stop on it now
/// arrives with the brief already in front of it, so nothing needs the
/// allowance to get its first turn started. That removes the one reading of d2
/// under which it was an exception for `wsp brief` — it never was, and d2 says
/// so — and leaves the rest exactly where it was, because the reason for the
/// other twenty-two is the one d2 gives: an agent runs `wsp show`, `wsp note`
/// and `wsp review` all day, and every one of those is a person's attention
/// spent on a verb that changes nothing outside the record.
///
/// `say` is deliberately absent, because it is absent there. The handbook asks
/// an agent to use it as its status line, so this is the one verb where a
/// Claude Code is stopped too — noted rather than fixed here, since diverging
/// would be this file inventing the second policy d2 exists to prevent.
const WSP_ALLOWED: &[&str] = &[
    "brief", "ls", "mv", "show", "tree", "wip", "where", "next", "overlap", "inbox", "mandate",
    "add", "claim", "release", "note", "flag", "decide", "edit", "start", "block", "review",
    "reopen", "tag",
];

/// The four a spawned opencode may not run at all, rather than be asked about.
///
/// Ed's file denies exactly these, and `deny` rather than `ask` is the whole
/// point of naming them: `done` is Ed's by the decision of 2026-08-19, and an
/// `ask` is an invitation for somebody to say yes on his behalf. The other three
/// destroy or move records that other work cites.
///
/// Driven rather than read: opencode's permission map takes `deny` beside
/// `allow` and `ask`, and a denied command is refused outright without the pane
/// ever reaching `blocked`.
const WSP_DENIED: &[&str] = &["done", "rm", "project rm", "archive"];

/// The shell verbs a spawned opencode may read with, without asking.
///
/// **`core-040`, and it is the half of `core-020` d2 that nothing implemented.**
/// d2 draws the line at *read and record freely, stop before changing anything
/// outside your own record*. [`WSP_ALLOWED`] is the record half; there was no
/// read half, because `"*": "ask"` covers `grep`, `cat`, `ls` and `head` alike.
/// A Claude Code in this project never noticed, because it reads with Read,
/// Grep and Glob, which are not Bash and need no allowance — so the same policy
/// mirrored verb for verb landed on a much narrower agent. Measured on
/// `ui-007`: **28 permission prompts in 12 minutes**, the first of them
/// `grep -n "## Source map" README.md`, the agent's second action, doing what
/// the handbook tells every arriving agent to do.
///
/// **Ed settled it as scope rather than enumeration, and opencode has no such
/// scope to set.** The decision named `external_directory` and said to read it
/// before assuming it does the job. Read, in opencode 1.18.21, and it does not:
/// `ShellTool.ask` raises it as a *second, separate* approval covering the path
/// arguments of eight file-touching commands (`rm cp mv mkdir touch chmod chown
/// cat`, plus the `cd` family), and the `bash` ask on the command text happens
/// either way. It is also already doing the only thing it can: a path inside
/// the agent's own location never asks and a path outside it always does, which
/// is the scope the decision wanted and is on by default — four of `ui-007`'s
/// prompts were exactly that, the agent reaching into `~/wsp` and a temp dir.
/// Setting it would be a no-op or a tightening nobody asked for. So this is the
/// fallback the decision named rather than one invented here.
///
/// **And the argument the decision preferred scope *for* turns out not to hold
/// against opencode.** The row's case against pattern lists was
/// `grep -n x $(rm -rf …)` — a read-only pattern with an argument that is not.
/// opencode parses the command with tree-sitter and evaluates *every* command
/// node in the tree, so the substitution is its own pattern and falls to
/// `"*": "ask"` while the `grep` around it is allowed; the request asks, and
/// nothing runs. Driven 2026-08-22 against a real opencode, `$( )` and
/// backticks both, with a file that survived it.
///
/// What earns a place here is one checkable property: **no flag this command
/// accepts can write a file or run another command.** It is not a claim that
/// reading is safe, which is the thing `core-040` says a name cannot carry —
/// it is the narrowest test that a reader can re-apply to a candidate. What it
/// excludes is as much of the argument as what it admits: `find` (`-exec`,
/// `-delete`), `sed` (`-i`), `awk` (`system()`, `print >`), `sort` (`-o`),
/// `uniq` (its second operand is an output file), `file` (`-C` compiles a magic
/// file), `tee` and `xargs` outright — and `rg`, which fails on `--pre` and
/// `--hostname-bin` and is the one exclusion a reader will query. It is not
/// that ripgrep reads worse; it is that opencode's own `grep` tool is ripgrep
/// and is unasked, so the shell copy is the only one that has to pass this.
///
/// `git` is here by subcommand and not as a verb: `git log`, `show`, `status`,
/// `diff` and `blame` and nothing else, so `push`, `checkout` and `reset` stay
/// where the rest of the shell is.
const READ_ONLY: &[&str] = &[
    "cat", "grep", "head", "tail", "ls", "wc", "stat", "du", "pwd", "basename", "dirname", "which",
    "nl", "cut", "tr", "git log", "git show", "git status", "git diff", "git blame",
];

/// The one rule that keeps [`READ_ONLY`] read-only: no redirection, ever.
///
/// `cat README.md > copy.txt` is a write, and an allowlist of heads that did
/// not say so would have allowed it — the pattern opencode matches is the whole
/// redirected statement, so `cat *` swallows the `>` and everything after it.
/// Driven both ways on 2026-08-22: with this rule the command asks and the file
/// is not there afterwards.
///
/// It is deliberately blunt. `2>/dev/null` is a redirect and asks like any
/// other, which cost three of `ui-007`'s otherwise-allowed rows; the precise
/// version needs one exception per allowed head, because a bare
/// `"*>/dev/null": "allow"` sitting after this would hand `rm -rf x >/dev/null`
/// straight through. Twenty extra patterns to save three prompts in twelve
/// minutes is the wrong trade, and the blunt rule is the one that can be stated
/// in a sentence: **nothing wsp allows a spawned opencode may redirect.**
const NO_REDIRECT: &str = "*>*";

/// What a wsp-spawned opencode is configured with, as the JSON that
/// [`Kind::env`] hands it.
///
/// **This is `core-020` d1 as amended by d2, and it is a policy rather than a
/// default.** opencode ships allowing `bash` and `edit` outright: driving one in
/// `core-026`, it ran a shell command and wrote a file without asking anything.
/// herdr's detection for the blocked state is correct and fires the moment
/// something asks — but in the shipped configuration nothing ever asks, so
/// `needs_a_person` is unreachable, and a kind whose only brake can never engage
/// is a kind that cannot be given unattended work.
///
/// The price is accepted deliberately and is not small: more stalls, and each
/// one wants a person. That is the trade Ed made, and the alternative — an agent
/// that cannot be stopped and cannot ask — is the one thing `core-017` and the
/// whole attention path exist to prevent. `core-040` prices the other side of
/// it: replayed against `ui-007`'s own 32 requests, this policy answers 14 of
/// them without a person, and every one of the 18 left is a build, a write, a
/// reach outside the tree, or a verb Ed's file does not allow either.
///
/// Built from the three lists rather than written out, so the correspondence
/// with `~/.claude/settings.json` is one line per verb and checkable by eye, and
/// every pattern and action still goes through `serde_json` so a verb with a
/// character in it that needs escaping cannot produce config that silently
/// parses as something else.
///
/// **Order carries the precedence, and the map is emitted in it.** opencode's
/// `Permission.evaluate` is a `findLast` over the rules in config order — the
/// *last* match wins, not the most specific — so broad goes first and narrow
/// goes last: the catch-all, then the reads, then the wsp verbs, then
/// [`NO_REDIRECT`] over all of them, then the four denies which nothing may
/// overturn. This is why the map cannot be a `serde_json::Map`: that is a
/// `BTreeMap`, and sorting the keys is what the old code was silently relying
/// on — `*` sorts below every letter, so the catch-all landed first and the
/// allows won by accident. The accident held; the sentence written next to it
/// ("opencode resolves the most specific match") did not, and a rule like
/// [`NO_REDIRECT`] that has to beat `cat *` would have been dead on arrival
/// under it.
///
/// Scoped to the seat and not to the machine. Ed's own opencode is untouched by
/// this, which is the other half of the decision: wsp configures the agents it
/// starts and nothing else.
fn config(brief: Option<&Path>) -> String {
    let mut rules: Vec<(String, &str)> = Vec::new();
    // Everything a shell can do, which is the brake itself. First, because
    // everything below it is an exception to it.
    rules.push(("*".to_string(), "ask"));
    // `grep *` and not `grep*`: opencode rewrites a trailing ` *` to an
    // optional argument list, so this matches `grep` and `grep -n x` and does
    // not match a `grepfoo` that happens to be on the PATH.
    rules.extend(READ_ONLY.iter().map(|c| (format!("{c} *"), "allow")));
    rules.extend(WSP_ALLOWED.iter().map(|v| (format!("wsp {v}*"), "allow")));
    rules.push((NO_REDIRECT.to_string(), "ask"));
    rules.extend(WSP_DENIED.iter().map(|v| (format!("wsp {v}*"), "deny")));

    let bash = rules
        .iter()
        .map(|(pattern, action)| format!("{}:{}", Value::from(pattern.as_str()), Value::from(*action)))
        .collect::<Vec<_>>()
        .join(",");
    let mut cfg = format!(r#"{{"permission":{{"bash":{{{bash}}},"edit":"ask"}}"#);
    // The brief, as a path opencode loads for itself. `core-032` d7, and the
    // reason it goes here rather than into the work order is on
    // [`Kind::brief_file`].
    //
    // **This key merges with the machine's own config rather than replacing
    // it**, which is the property the whole scheme rests on and the one worth
    // naming: `~/.config/opencode/plugins/herdr-agent-state.js` is what reports
    // an opencode's state to herdr, and a config that clobbered `plugin` would
    // have turned off state reporting for every agent wsp starts — silently,
    // and with `needs_a_person` among the things it took with it. Driven three
    // times now: `opencode debug config` with this exact value resolves
    // `instructions` to the file below, `plugin` still to the herdr plugin, and
    // — the reason it was run again for `core-040` — the bash map in the order
    // wsp wrote it rather than reordered by the merge.
    if let Some(path) = brief {
        let named = Value::from(vec![path.display().to_string()]);
        cfg.push_str(&format!(r#","instructions":{named}"#));
    }
    cfg.push('}');
    cfg
}

/// Where the model catalogue comes from, and the shape a model name has.
///
/// `provider/model` — `opencode models` prints one per line and there were
/// **365 of them on this machine**, which is the whole argument against a list
/// compiled into wsp. It is not a vocabulary of four words that a person learns
/// once; it is a per-machine fact about which providers are configured and
/// authenticated, and a copy kept here would be stale on the day it was written
/// and wrong on any machine but one. `place.rs` refuses to keep a catalogue of
/// kinds for exactly this reason and this is the same refusal one level down.
fn opencode_models() -> std::result::Result<Vec<String>, String> {
    let out = captured(opencode_bin(), &["models"])?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.contains('/'))
        .collect())
}

/// opencode, wherever it is — the same reasoning as [`claude_bin`], and one
/// extra reason of its own.
///
/// `~/.opencode/bin` reached `PATH` through `.zshrc`, which a **non-interactive**
/// shell does not read, so `Command::new("opencode")` failed from a hook or a
/// script on a machine that plainly had it. That was fixed at the source by
/// moving the export to `~/.zshenv` on 2026-08-21, and this stays anyway: the
/// lookup costs nothing and the failure it prevents reads as "opencode is not
/// installed" on a machine where it is.
fn opencode_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("OPENCODE_BIN") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("opencode");
            if p.is_file() {
                return p;
            }
        }
    }
    util::home().join(".opencode/bin/opencode")
}

/// Everything one of these runtimes wrote to stdout, through a file rather
/// than a pipe.
///
/// **The pipe was losing most of it, and saying nothing.** `opencode export
/// --sanitize` on a 53-turn session writes 165,230 bytes. Asked for down a
/// pipe, wsp received 65,536 of them — one pipe buffer — and an exit status of
/// *zero*, so [`read_export`] was handed a JSON document cut off mid-string,
/// returned nothing, and `ui-007` closed with no cost clause at all. That is
/// `core-039`. It was read at first as `despawn` ending the agent before its
/// runtime could be asked. Driven in a sandbox, both endings wrote the clause —
/// `wsp release` on a live opencode and `wsp despawn` on another — and then the
/// row that had lost one lost it again on a replay through `release`, with no
/// agent left to kill. Nothing about the ending is the variable. The size of
/// the export is.
///
/// The cause belongs to the runtime rather than to the subcommand: these are
/// Bun-compiled binaries, their stdout writes are asynchronous, and the process
/// exits without waiting for a pipe it has filled to drain. A file has no
/// buffer to overrun and its writes complete before exit — the same export
/// redirected to one came back 165,230 bytes three times out of three.
///
/// So every capture in this file goes through here, including the two that are
/// small today: `opencode models` is 13KB and `claude agents --json` is 1.4KB,
/// both grow with what is on the machine, and both fail plausibly rather than
/// loudly when cut — a valid model refused, an agent that cannot be addressed.
/// The defence is one temp file, and waiting to be the second row this costs is
/// not a saving.
///
/// The file is this process's own, is opened `0600` because a sanitized export
/// still carries the shape of one attempt and a temp directory is shared, and
/// is removed however this ends.
fn captured(bin: PathBuf, args: &[&str]) -> std::result::Result<Vec<u8>, String> {
    // Named as the reader would type it — `opencode models`, not the resolved
    // path — because these strings reach a person inside a refusal that is
    // about their `--model`, not about where wsp found the binary.
    let said = format!(
        "{} {}",
        bin.file_name().unwrap_or(bin.as_os_str()).to_string_lossy(),
        args.join(" ")
    );
    // Unique per call and not merely per process. Nothing here runs two at
    // once today, and a name that would silently collide the day something
    // does is a trap laid for a reader with no reason to look here.
    static NTH: AtomicU64 = AtomicU64::new(0);
    let at = std::env::temp_dir().join(format!(
        "wsp-captured-{}-{}",
        std::process::id(),
        NTH.fetch_add(1, Ordering::Relaxed)
    ));
    let sink = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&at)
        .map_err(|e| format!("{said}: {e}"))?;
    let ran = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(sink))
        .stderr(Stdio::null())
        .status();
    let out = match ran {
        Err(e) => Err(format!("{said}: {e}")),
        Ok(s) if !s.success() => Err(format!("{said} failed")),
        Ok(_) => std::fs::read(&at).map_err(|e| format!("{said}: {e}")),
    };
    // Whatever happened, including the failures: a capture that could not be
    // read is exactly the one whose leftovers nobody comes back for.
    let _ = std::fs::remove_file(&at);
    out
}

impl Kind for OpenCode {
    /// The model, the session, and the work order.
    ///
    /// No trim and no handle. There is nothing to trim — opencode has no
    /// equivalent of Claude Code's tool and MCP preamble to shed — and nothing
    /// to name it with: [`Kind::address`] is `None` here, so a handle would be a
    /// string wsp mints and can never use.
    ///
    /// The session goes on through [`Kind::resume_flag`] rather than being
    /// spelled here, so this kind's word for it is written once and the line
    /// `wsp resume --print` offers cannot drift from the argv wsp builds.
    ///
    /// `--prompt` last, and it is the reason [`Kind::order_in_args`] exists. See
    /// there for the measurement; what matters here is that the order is a
    /// **single argv element** and is therefore safe for the prose a work order
    /// actually is. Verified rather than hoped: a prompt containing backticks
    /// and `$(…)` was passed through herdr's `agent.start` and arrived intact,
    /// with nothing executed — herdr does not put these through a shell.
    fn args(&self, spawn: &Spawn) -> Vec<String> {
        let mut argv: Vec<String> = Vec::new();
        if let Some(model) = spawn.model {
            argv.push("--model".into());
            argv.push(model.to_string());
        }
        if let (Some(flag), Some(session)) = (self.resume_flag(), spawn.resume) {
            argv.push(flag.into());
            argv.push(session.to_string());
        }
        if let Some(order) = spawn.order {
            argv.push("--prompt".into());
            argv.push(order.to_string());
        }
        argv
    }

    /// `--session`, which is `core-026`'s finding D: resuming an opencode
    /// through [`Claude`]'s spelling hands it `--resume`, and opencode has no
    /// such flag. This is where knowing the kind pays for itself.
    fn resume_flag(&self) -> Option<&'static str> {
        Some("--session")
    }

    /// A `provider/model` this machine can actually serve, and no effort at all.
    ///
    /// **`--effort` is refused rather than dropped**, which is [`Plain::tier`]'s
    /// rule kept rather than weakened: opencode has no effort concept, so a flag
    /// accepted here would be a spawn that says it is running at `max` and is
    /// not. The refusal names the one knob this kind does have, because a person
    /// who typed `--effort` was reaching for a cheaper run and `--model` is how
    /// they get one.
    ///
    /// **The model is checked in two stages, cheapest first.** The shape check
    /// is free and catches the likeliest mistake by a distance — typing
    /// `--model opus`, which is wsp's own vocabulary and means nothing here. Only
    /// a name that is at least shaped like a model costs the catalogue lookup.
    ///
    /// **On not mapping `opus|sonnet|haiku` onto this kind.** It was a real
    /// choice and this is the argument, because the next person will ask. wsp's
    /// four words are Claude Code's aliases; the set they would have to map onto
    /// is 365 entries that differ per machine with which providers are
    /// configured and authenticated. A mapping compiled into wsp would claim
    /// `opus` means `anthropic/claude-opus-5` on a machine where the `anthropic`
    /// provider is not set up at all — a spawn stating a tier it is not running,
    /// which is the exact defect [`Plain::tier`]'s refusal exists to prevent,
    /// reintroduced with more steps. So: no mapping, `provider/model` only, and
    /// the refusal says where the real names come from.
    ///
    /// **The lookup shells out and that costs ~2s**, measured over three runs.
    /// It is paid only when `--model` is stated — a spawn that names no tier
    /// makes no subprocess and is byte-for-byte the spawn wsp did before — and it
    /// is paid *before* [`crate::place::Place::open`], which is the placement
    /// `cmd_spawn::tier` already requires: two seconds against a workspace, a
    /// claim and a worktree that would otherwise have to be despawned is not a
    /// close call.
    ///
    /// **`--on <machine>` is the known gap.** The catalogue read here is this
    /// machine's, and a far machine's providers are its own. That is the same
    /// asymmetry `cmd_spawn::tier` already records for `--model` generally — the
    /// flag states the tier and cannot guarantee it — and the refusal says so
    /// rather than pretending the answer is authoritative.
    fn tier(&self, model: Option<&str>, effort: Option<&str>) -> std::result::Result<(), String> {
        if effort.is_some() {
            return Err(
                "opencode has no effort setting — say --model provider/model instead, \
                 which is the knob this kind has"
                    .into(),
            );
        }
        let Some(m) = model else { return Ok(()) };
        if !m.contains('/') {
            return Err(format!(
                "no model `{m}` — opencode names models `provider/model`, not by alias. \
                 `opencode models` lists what this machine can serve"
            ));
        }
        // A catalogue wsp could not read is not a licence to start anything: the
        // failure this check exists to prevent is an unknown model refused by
        // the agent in a pane nobody is looking at, and that is exactly as
        // expensive whether or not the probe worked. Saying which of the two
        // happened is the difference between a typo and a broken install.
        let models = opencode_models()
            .map_err(|e| format!("cannot check `{m}` — {e}. Is opencode installed?"))?;
        if !models.iter().any(|k| k == m) {
            return Err(format!(
                "no model `{m}` on this machine — `opencode models` lists {} of them. \
                 If this spawn is --on another machine, its providers are its own",
                models.len()
            ));
        }
        Ok(())
    }

    /// [`permission`], which is the whole of it. The argument is on that
    /// function, on [`WSP_ALLOWED`] and on [`Kind::env`].
    fn env(&self, brief: Option<&Path>) -> BTreeMap<String, String> {
        BTreeMap::from([("OPENCODE_CONFIG_CONTENT".to_string(), config(brief))])
    }

    /// Yes — `instructions`. The measurement is on [`Kind::brief_file`].
    fn brief_file(&self) -> bool {
        true
    }

    /// Yes — `--prompt`. The measurement is on [`Kind::order_in_args`].
    fn order_in_args(&self) -> bool {
        true
    }

    /// Nothing, and it is [`Plain`]'s answer for [`Plain`]'s reason: opencode
    /// has a session id, but herdr is where wsp learns it and only once the
    /// plugin has reported one. Minting a second name here would be a handle
    /// wsp cannot address anything with.
    fn address(&self, _place: &dyn Place, _spawn: &Spawn) -> Option<Address> {
        None
    }

    /// Typed at, like [`Plain`], and `core-028` closed the question of whether
    /// it should be something else. opencode's ACP server is a real transport,
    /// but the fact it was wanted for — whether a sentence handed to a busy
    /// opencode outlives the agent — was established by driving the pane
    /// instead, and the answer does not want a second transport: the sentence
    /// is durable and nothing re-drives it, so
    /// [`Kind::queue_is_the_agents`] stays true and is not overridden here.
    /// The argument is on that method, which is where a later kind will read
    /// it.
    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<Delivery> {
        place.tell(seat, text)
    }

    /// Nothing to ask. The backend is the only witness, as for [`Plain`].
    fn running(&self, _spawn: &Spawn) -> Option<bool> {
        None
    }

    /// `opencode export`, which is a *better* record than the one wsp derives
    /// for Claude Code, and that is the uncomfortable half of `core-029`.
    ///
    /// Every assistant message carries `providerID`, `modelID`, `cost` in
    /// dollars and `tokens {input, output, reasoning, cache{read, write}}` —
    /// published rather than inferred, and in the units `core-014` prices in,
    /// so setting the two kinds against each other is arithmetic. What wsp
    /// does with it is exactly what it does with Claude's transcript, in the
    /// same shape, so that neither kind gets a column the other cannot answer:
    /// see [`Spend`].
    ///
    /// **`--sanitize`, and not only for size.** The flag redacts transcript and
    /// file data, which takes this session's export from 172KB to 47KB with
    /// every number in it unchanged (measured 2026-08-22 on
    /// `ses_fdafaf5dd…`). wsp wants counts and never content, so asking for the
    /// redacted copy means an agent's files do not pass through wsp at all —
    /// the smaller parse is the second reason rather than the first.
    ///
    /// **The session id is the whole join and `cwd` is not used.** `export`
    /// takes an id and finds the session wherever it ran, so unlike
    /// [`transcript`] there is no path to derive and no scan to fall back to.
    /// A cwd fallback was considered and refused: `opencode session list` can
    /// name every session in a directory, but picking one of them by recency
    /// would attribute a person's own opencode in that tree to an agent's
    /// attempt, and the rule this record is built on is that a silently wrong
    /// label is worse than a missing one.
    ///
    /// **What is in an export is one session and nothing else**: every message
    /// in it carries the id that was asked for. So anything that ran under a
    /// session of its own — which is where opencode puts a sub-agent, though
    /// that half is read off its design rather than driven here — is outside
    /// this reading, the same line [`read_ran`] draws at `isSidechain`. Both
    /// columns are a floor for the same reason, and an attempt that delegated
    /// heavily cost more than either record says.
    ///
    /// It shells out, ~1.5s measured over three runs, and that is affordable
    /// exactly where it is paid — once when a claim ends, and on demand from
    /// `wsp attempts`. Never in `brief`, `ls` or `show`.
    fn ran(&self, session: &str, _cwd: &str, since: i64) -> Option<Ran> {
        read_export(&export(session)?, since)
    }
}

/// One session as opencode publishes it, redacted of everything but the
/// numbers.
///
/// The id is checked before it reaches a command line, for the reason
/// [`transcript`] checks it before it reaches a path: it arrives from herdr,
/// which got it from the agent, and `spawn` is a verb agents drive. opencode
/// spells an id `ses_` and twenty-something alphanumerics; anything with a
/// space, a slash or a dash in it is not one.
///
/// Taken through [`captured`] and not through a pipe, which is the whole of
/// `core-039`: this is the longest thing wsp asks any runtime for, so it is the
/// one that overran a pipe buffer and came back a truncated document under a
/// successful exit status. The argument is there.
fn export(session: &str) -> Option<Vec<u8>> {
    if session.is_empty() || !session.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    captured(opencode_bin(), &["export", "--sanitize", session]).ok()
}

/// That export, reduced to what served the attempt and what it cost.
///
/// Parsed whole rather than streamed, which is the one place this differs from
/// [`read_ran`] and is not a choice: an export is a single JSON document, so
/// there is no line to stop at. `--sanitize` is what keeps that affordable —
/// the numbers are a fraction of a percent of an unredacted export.
fn read_export(src: &[u8], since: i64) -> Option<Ran> {
    let v: Value = serde_json::from_slice(src).ok()?;
    let mut ran = Ran::default();
    let mut micros: u64 = 0;
    for m in v.get("messages")?.as_array()? {
        let info = m.get("info")?;
        if info.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        // Milliseconds here and seconds in the claim: opencode stamps in the
        // unit JavaScript counts in, and the window arrives in the unit the
        // store writes.
        let at = info.pointer("/time/created").and_then(Value::as_i64).map(|ms| ms / 1_000);
        if since > 0 && at.is_some_and(|t| t < since) {
            continue;
        }
        let Some(model) = info.get("modelID").and_then(Value::as_str) else { continue };
        ran.turns += 1;
        // `provider/model`, which is opencode's own spelling of a model and the
        // one `--model` is stated in — so what ran can be set against what was
        // asked for without either being translated.
        let model = match info.get("providerID").and_then(Value::as_str) {
            Some(p) if !p.is_empty() => format!("{p}/{model}"),
            _ => model.to_string(),
        };
        if !ran.models.contains(&model) {
            ran.models.push(model);
        }
        let n = |p: &str| info.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        ran.spend.cache += n("/tokens/cache/read");
        ran.spend.input += n("/tokens/input") + n("/tokens/cache/write");
        // Reasoning is counted *beside* output here and *inside* it by
        // Anthropic, whose `output_tokens` includes thinking. Adding them is
        // what makes the two kinds' out column the same unit — tokens the model
        // produced — rather than two numbers that differ by whichever half the
        // runtime happened to report separately.
        ran.spend.output += n("/tokens/output") + n("/tokens/reasoning");
        // Dollars as a float on the wire, kept as micro-dollars from here on:
        // see `util::money_human` for why nothing downstream sees a float.
        micros += (info.get("cost").and_then(Value::as_f64).unwrap_or(0.0) * 1e6).round() as u64;
    }
    if ran.models.is_empty() {
        return None;
    }
    // Stated even when it is zero, because zero is a fact about this kind that
    // Claude's record cannot state: a free model really did cost nothing, and
    // `None` would say wsp does not know.
    ran.spend.micros = Some(micros);
    // opencode has no effort knob — [`OpenCode::tier`] refuses the flag — so
    // the honest half is `default`, and it is written rather than omitted for a
    // reason that is not cosmetic: `cmd_attempts::agrees` splits a tier at its
    // *last* separator, and a model spelled `provider/model` with no effort
    // after it reads as the provider at `model` effort. The empty half is the
    // one shape that parse cannot survive, and this is the kind that produces
    // it.
    ran.efforts.push("default".to_string());
    Some(ran)
}

/// Claude Code.
pub struct Claude;

/// What a spawned Claude Code session is *not* given, and why each name is on
/// the list.
///
/// Every request re-reads the whole context, so a token present before the
/// agent has done anything is paid once per request — ~102 times in the session
/// robustness-031 measured. The preamble is the largest single thing in that
/// context and almost none of it is wsp's: `wsp brief --session` is ~3,300
/// tokens of it, the rest is Claude Code's own.
///
/// **Measured against a live spawn on 2026-08-17, not estimated.** Two
/// `wsp spawn --agent` runs into a sandbox — its own herdr session, its own
/// store — one with `--full` and one without, read back off the two transcripts:
///
/// | | first-request context |
/// |---|---|
/// | `--full` | 37,756 |
/// | trimmed | 25,306 |
///
/// **12,450 tokens off every request of the session — 33%.** At the ~102
/// requests that session ran, ~1.27M tokens.
///
/// Which is *worth saying plainly*: the parent task's arithmetic hoped for
/// ~28.6K, on the reading that the ~34K preamble was mostly sheddable. It is
/// not. What can be shed through a flag is the tool schemas and the MCP prose;
/// the system prompt underneath them is ~25K and there is no lever here for it.
/// This is 44% of the hoped saving and the rest is not available.
///
/// Attribution, one lever at a time, by running `claude -p` with the flag and
/// summing `input + cache_creation + cache_read` on the reply — the print-mode
/// baseline is 26,816 and each figure is what removing that one thing saved:
///
/// | dropped | tokens | why it is safe to drop |
/// |---|---|---|
/// | `Workflow` | 6,024 | the work order forbids it in as many words |
/// | `Agent` | 2,682 | same, and this task exists because sub-agents are what blew the budget |
/// | every MCP server, and its instruction prose | 831 | two spawned agents made zero MCP calls between them |
///
/// The 831 is the print-mode floor for it: an interactive session also carries
/// `claude-in-chrome`, Gmail, Calendar and Drive, whose instruction blocks are
/// prose rather than schema, which is most of the gap between the 9,537 those
/// three lines account for in print and the 12,450 measured live.
///
/// **Named removals rather than a named allowlist, and the difference is not
/// cosmetic.** `--tools` takes an allowlist of built-ins, and the first attempt
/// at this used one: `Bash,Read,Edit,Write,Glob,Grep,TodoWrite,Task`. Four of
/// those eight do not exist in this build, and unknown names are ignored in
/// silence — so it measured something other than what it said, and quietly
/// withheld several tools that do exist and were never considered. That is
/// precisely the failure this trim is supposed to avoid: an agent that silently
/// lacks a tool does not report it, it works around it, expensively and out of
/// sight. An allowlist withholds every tool nobody thought of, including every
/// one Claude Code gains after this line was written. A denylist withholds only
/// what it names, and what it names is here to read.
///
/// The same rule says what is deliberately *kept*, and it was priced rather
/// than assumed, so nobody has to re-measure to argue with it: `ScheduleWakeup`
/// 1,250, `ReportFindings` 609, `ListAgents` 306. Each is small, each is
/// reachable from a slash command a person might type into this pane, and 2,165
/// tokens is not worth a capability going missing without a sentence about it.
/// `Read`, `Edit`, `Write` and `Bash` are not negotiable: the same measurement
/// found those agents doing all their reading through `sed` and `head` at ~28K,
/// and a trim that pushes more work into Bash has made things worse while
/// appearing to make them better. Both probe agents above reached for `Read`
/// on their first tool call, which is the thing to watch if this list ever
/// grows.
///
/// What the trim leaves behind is legible, which was the other requirement: a
/// trimmed session asked what it has answers "No Workflow tool", "No mcp__
/// prefixed tools", and lists its fourteen skills. It does not silently
/// improvise around an absence it cannot see.
const TRIM: &[&str] = &["--strict-mcp-config", "--disallowedTools", "Agent", "Workflow"];

/// The model aliases `wsp spawn --model` takes, and the whole of the list.
///
/// Aliases and not full names, on purpose. `claude --help` says an alias names
/// *the latest* model of that family — so `opus` follows the upgrade and
/// `claude-opus-5` pins wsp to a string that goes stale the week after it is
/// written, in a file nobody would think to grep. What a person means when they
/// type `--model haiku` is the tier, and the tier is what an alias says.
///
/// `[1m]` may be appended to any of them, which is Claude Code's own spelling
/// for the 1M context window (`~/.claude/settings.json` here says `opus[1m]`);
/// it is a property of the session, not a fifth model, so it is a suffix here
/// too rather than four more entries.
///
/// Measured 2026-08-18 against Claude Code 2.1.234: all four, and all four with
/// `[1m]`, are recognised — an unrecognised name prints *"is not a model this
/// version of Claude Code recognizes"* and the session then fails to start, so
/// the list is exactly what does not do that.
const MODELS: &[&str] = &["fable", "opus", "sonnet", "haiku"];

/// The effort levels, straight off `claude --help`.
///
/// The cheaper of the two knobs and the one to reach for first: the same
/// capability class for less spend, with no new failure mode. Not every model
/// has one — Haiku 4.5 takes no effort parameter at all — but the pair is not
/// refused here, because Claude Code 2.1.234 accepts `--model haiku --effort
/// high` without complaint and it is not wsp's business to be stricter than the
/// runtime about a combination that costs nothing.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// The tiers whose panes open in manual mode, and so cannot be left alone.
///
/// One entry, and the list is a list rather than an `if` because the next
/// Claude Code release could move a tier either way and the measurement in
/// [`Kind::unattended`] is what would move it. Matched on the alias with any
/// `[1m]` already stripped, the way [`MODELS`] is.
const NO_AUTO_MODE: &[&str] = &["haiku"];

impl Kind for Claude {
    /// The one kind that is, and the only one wsp installs a hook for — see
    /// `claude-code/settings.snippet.json`, which is Claude Code's settings
    /// file and nobody else's.
    fn briefed(&self) -> bool {
        true
    }

    /// The trim, the name, and the tier.
    ///
    /// `-n` is outside the `full` arm on purpose. [`TRIM`] is a capability
    /// change and `--full` is the way back from it; a handle is not a
    /// capability, and an agent started with `--full` still has to be
    /// addressable. Naming it costs two argv entries and nothing per request.
    ///
    /// **It goes last, and that it is safe there was measured rather than
    /// assumed.** `--disallowedTools` takes a *space*-separated list, so the
    /// trim ends in two bare words and a flag appended after them is a fair
    /// question: `… --disallowedTools Agent Workflow -n <handle>` could as
    /// easily have denied a tool called `-n`. Run against Claude Code 2.1.233
    /// on 2026-08-17, that exact argv gave a session named `<handle>` which
    /// answered that it has no `Workflow` and does have `Bash` — the name
    /// lands, the trim still bites, and neither ate the other.
    ///
    /// If a flag that takes a value is ever added to [`TRIM`], this is the line
    /// to re-check rather than the one to reorder around.
    fn args(&self, spawn: &Spawn) -> Vec<String> {
        let mut argv: Vec<String> = match spawn.full {
            true => Vec::new(),
            false => TRIM.iter().map(|s| (*s).to_string()).collect(),
        };
        if let Some(handle) = mint(spawn.name, spawn.seat) {
            argv.push("-n".into());
            argv.push(handle);
        }
        // The tier, if one was asked for, and nothing at all if not — which is
        // what keeps an unflagged spawn identical to the spawn wsp did before
        // these existed. Both were validated by `tier` before this was reached.
        if let Some(model) = spawn.model {
            argv.push("--model".into());
            argv.push(model.to_string());
        }
        if let Some(effort) = spawn.effort {
            argv.push("--effort".into());
            argv.push(effort.to_string());
        }
        // Last, after the name, for the reason the name is last: the resume
        // flag takes a value, so nothing may follow it that could be eaten as
        // one. It is not conditional on `full` — a resumed session is a
        // *thread* being picked up, and which tools it has is still this
        // spawn's decision rather than the old one's. The spelling is
        // `Kind::resume_flag`'s and not this function's.
        if let (Some(flag), Some(session)) = (self.resume_flag(), spawn.resume) {
            argv.push(flag.into());
            argv.push(session.to_string());
        }
        argv
    }

    /// `--resume`.
    fn resume_flag(&self) -> Option<&'static str> {
        Some("--resume")
    }

    /// [`MODELS`], with an optional `[1m]`, and [`EFFORTS`].
    ///
    /// The refusal carries the whole list, because the person reading it has
    /// just mistyped one of four words and the cost of printing them is nothing
    /// against another round trip. A full model name — `claude-opus-5` — is
    /// refused by the same line and learns the same way; see [`MODELS`] for why
    /// the list is aliases.
    fn tier(&self, model: Option<&str>, effort: Option<&str>) -> std::result::Result<(), String> {
        if let Some(m) = model {
            // The suffix is stripped before the lookup rather than being four
            // more entries: `opus[1m]` is opus, asked for with a bigger window.
            let alias = m.strip_suffix("[1m]").unwrap_or(m);
            if !MODELS.contains(&alias) {
                return Err(format!(
                    "no model `{m}` — claude takes {}, any of them with [1m] for the 1M window",
                    MODELS.join(", ")
                ));
            }
        }
        if let Some(e) = effort {
            if !EFFORTS.contains(&e) {
                return Err(format!("no effort `{e}` — claude takes {}", EFFORTS.join(", ")));
            }
        }
        Ok(())
    }

    /// [`NO_AUTO_MODE`], and the measurement behind it is on the trait method.
    ///
    /// The sentence names the three things a person needs and no more: which
    /// tier, what is missing, and the two ways on. `--focus` is a real way on
    /// rather than a hedge — a focused spawn puts you at the pane, and the
    /// whole of the objection is that nobody is there.
    fn unattended(&self, model: Option<&str>) -> Option<String> {
        let alias = model?;
        let alias = alias.strip_suffix("[1m]").unwrap_or(alias);
        NO_AUTO_MODE.contains(&alias).then(|| {
            format!(
                "a `{alias}` pane opens in manual mode and stops at the first command \
                 the settings file does not already allow — measured 2026-08-18, and \
                 nothing clears it but a person at the tty. Spawn it with --focus if \
                 you mean to sit there, or --model sonnet to leave it working"
            )
        })
    }

    /// What wsp called it, confirmed alive if the runtime will confirm it — or,
    /// for an agent wsp did not start, what the runtime called it.
    ///
    /// The mint is tried first and it is still *checked* against the listing,
    /// which is not a wasted call. This is asked on the failure path, where the
    /// open question is whether the agent that would not take a work order is
    /// even running, and the difference between "it is reachable as X" and
    /// "nothing answers to X" is the whole of what a person is about to do next.
    ///
    /// What minting buys here is that the answer is **exact** where [`pick`] can
    /// only be probable. `pick` needs herdr to have seen the session id — which
    /// is the whole of robustness-041, and it often has not — and then falls back
    /// to whoever is alone in the tree. A minted name is matched against itself.
    ///
    /// The census is read **once, and only for [`pick`]**. It is the one reading
    /// here that cannot be waited into existence: a seat whose agent herdr has
    /// no record of has no name and no session on its row however long you ask,
    /// which is the measurement in the module docs. A census wsp could not read
    /// at all is no longer fatal to the sentence for the same reason — the
    /// handle does not come from there any more.
    fn address(&self, place: &dyn Place, spawn: &Spawn) -> Option<Address> {
        let row = place.census().ok().and_then(|c| c.seats().find(|s| &s.seat == spawn.seat).cloned());
        // A listing wsp could not take is treated as an empty one, which is the
        // opposite of what [`Kind::running`] does with the same failure and right
        // for the opposite reason: there the difference decides whether to
        // condemn a live agent, and here both answers lead to the same hedge.
        addressed(spawn, row.as_ref(), &Lag::default(), &|| listing().unwrap_or_default())
    }

    /// Typed at the terminal, because there is nothing else yet.
    ///
    /// **This is where the session channel lands**, and the module docs say
    /// exactly what is missing: an address is now available for every Claude
    /// Code wsp started, and no supported way to send on the socket it names.
    /// Guessing at the protocol of an authenticated private socket is not a
    /// thing to do inside a spawn verb, so this delegates, and the failure path
    /// in `cmd_spawn` reports the address instead.
    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<Delivery> {
        place.tell(seat, text)
    }

    /// The census, asked about the handle wsp minted — which is why minting had
    /// to land first.
    ///
    /// Three `?`s and each is a different "cannot say": nothing to build a
    /// handle from, and `claude` neither on the `PATH` nor answering. None of
    /// them is `Some(false)`, because this is read as evidence that an agent is
    /// gone and a listing wsp failed to take is evidence of nothing.
    ///
    /// [`Kind::address`] resolves the same handle and then falls back to
    /// [`pick`]; this deliberately does not. `pick`'s fallback is *one live
    /// session in this tree*, and the seat being asked about here is by
    /// construction one whose agent the backend has lost sight of — so the tree
    /// is this repository, three colleagues are usually standing in it, and a
    /// probable answer to "is mine alive" is worse than no answer.
    fn running(&self, spawn: &Spawn) -> Option<bool> {
        let handle = mint(spawn.name, spawn.seat)?;
        Some(answers_to(&listing().ok()?, &handle))
    }

    /// The transcript, which is the only witness that was there for the whole
    /// session.
    ///
    /// Every other reading of what a Claude Code is running is a reading of
    /// *now*: `claude agents --json` answers about live sessions, herdr's pane
    /// row carries a session id and no tier at all, and both are gone by the
    /// time a claim ends. The transcript is written as the session goes and
    /// stays afterwards, and it carries the model on every assistant turn and
    /// the effort beside it — so a `/model` halfway through is not a fact that
    /// has to be caught as it happens, it is a second entry in the list this
    /// returns.
    ///
    /// Recorded against Claude Code 2.1.234: one JSON object per line under
    /// `~/.claude/projects/<cwd>/<session>.jsonl`, an assistant turn carrying
    /// `message.model` (`claude-opus-5`) and a top-level `effort` (`high`).
    fn ran(&self, session: &str, cwd: &str, since: i64) -> Option<Ran> {
        let file = std::fs::File::open(transcript(session, cwd)?).ok()?;
        read_ran(&mut std::io::BufReader::new(file), since)
    }
}

// ---- what actually ran ----------------------------------------------------

/// The tier a session was actually served at, and how much of it there was.
///
/// Lists rather than scalars, and that is the whole point of reading this
/// instead of trusting the spawn flag: a session can change model or effort
/// under itself, and the honest record of one that did is *both* names in the
/// order they served. Collapsing them to the first would name the tier a
/// decision was escalated away from; collapsing to the last would erase the
/// cheap attempt that failed, which is exactly the datum a try-cheap-then-
/// escalate policy is calibrated against.
///
/// `turns` is assistant turns on the main thread, and it is here because it is
/// the one number that separates an attempt that ground for an hour from one
/// that was spawned and said nothing — wall-clock cannot, because it counts
/// the agent waiting for a person the same as the agent working.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ran {
    /// Distinct model ids, in the order they first served a turn, with the
    /// `claude-` prefix off: `opus-5`, `haiku-4-5`. opencode's are
    /// `provider/model`, which is that kind's own spelling and the one a spawn
    /// states.
    pub models: Vec<String>,
    /// Distinct effort levels, same ordering rule. Empty where the runtime
    /// recorded none, which is a real answer and not a missing one — an older
    /// Claude Code wrote no `effort` at all.
    pub efforts: Vec<String>,
    /// Requests to the model, which is the same number as assistant turns:
    /// one turn is one call and one line of the bill. See [`read_ran`] for
    /// why counting the transcript's lines gave three times this.
    pub turns: usize,
    pub spend: Spend,
}

/// What an attempt consumed, in the one vocabulary both kinds can answer in.
///
/// **The point of this type is that it is the same shape for every kind.**
/// `core-029` exists because opencode publishes cost and Claude Code does not,
/// and the failure it was written against is opencode arriving as a second,
/// better-looking column that nobody can set against the first. So the fields
/// are the ones *both* runtimes report, and a kind that knows more than this
/// says it in [`Spend::micros`] or not at all.
///
/// # Why three numbers and not five
///
/// Both runtimes report input, output and two kinds of cache separately, and
/// this keeps two of the four apart:
///
/// - **`cache` is cache read**, and it is an order of magnitude cheaper than
///   everything else per token while being a hundred times more of them — 673M
///   against 4k in one session measured here. Folding it into input would price
///   an attempt about ten times too high, which is the one error a routing
///   argument cannot survive.
/// - **`input` is input plus cache write**, which are within 25% of each other
///   per token: both are the model reading something it has not read before.
///   The record loses that 25% and says so here.
///
/// Rounded to three significant figures on the way to disk, because the copy
/// that survives is a clause in a task's log — see [`util::count_human`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Spend {
    /// Tokens read from cache: the cheap half, and nearly all of the volume.
    pub cache: u64,
    /// Tokens the model read for the first time — input plus cache write.
    pub input: u64,
    /// Tokens the model produced, thinking included.
    pub output: u64,
    /// What the runtime says it cost, in millionths of a dollar — `None` where
    /// the runtime says nothing, which is Claude Code, and `Some(0)` for a free
    /// model, which is a different fact and has to read as one.
    pub micros: Option<u64>,
}

impl Spend {
    /// Nothing to say: no tokens and no price. A session that was started and
    /// killed before it answered produces one of these, and writing a clause
    /// of zeroes for it would be the record claiming a measurement it does not
    /// have.
    pub fn nothing(&self) -> bool {
        self.cache == 0 && self.input == 0 && self.output == 0 && self.micros.is_none()
    }

    /// The clause that goes on a release line: `673M cache 21M in 3.5M out $1.20`.
    ///
    /// One clause rather than four, and every term self-describing. The task's
    /// log is the only durable copy of this — a transcript is one machine's
    /// disk, cleaned up on its own schedule and absent altogether for an agent
    /// that ran on the other box — and it is also prose a person reads in
    /// `wsp brief`, four entries of it on every request of every session. So
    /// the cost of the record is four words on a line that was already being
    /// written, and zero terms are left out rather than printed as `0`.
    ///
    /// [`Spend::of_clause`] reads it back, and the two are here together
    /// because a format with its parser in another file drifts.
    pub fn clause(&self) -> Option<String> {
        if self.nothing() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        for (n, what) in [(self.cache, "cache"), (self.input, "in"), (self.output, "out")] {
            if n > 0 {
                parts.push(format!("{} {what}", util::count_human(n)));
            }
        }
        if let Some(m) = self.micros {
            parts.push(util::money_human(m));
        }
        Some(parts.join(" "))
    }

    /// That clause back, and `None` for a clause that is not one.
    ///
    /// Strict on purpose: the log is prose people write in, so anything that is
    /// not exactly pairs of `<count> <unit>` with an optional price is somebody's
    /// sentence and must not be read as a measurement.
    pub fn of_clause(text: &str) -> Option<Spend> {
        let mut spend = Spend::default();
        let mut words = text.split_whitespace().peekable();
        let mut any = false;
        while let Some(w) = words.next() {
            if let Some(m) = util::money_of(w) {
                spend.micros = Some(m);
                any = true;
                continue;
            }
            let n = util::count_of(w)?;
            match words.next()? {
                "cache" => spend.cache = n,
                "in" => spend.input = n,
                "out" => spend.output = n,
                _ => return None,
            }
            any = true;
        }
        any.then_some(spend)
    }
}

impl Ran {
    /// `opus-5/high`, or `haiku-4-5→opus-5/high` for a session that moved.
    ///
    /// One string because it is what goes in a log line and what a report
    /// groups by, and those two must be the same string or the report is
    /// grouping on a rendering. The `/` is the same separator `spawned at`
    /// uses, so the intent and the fact line up column-wise when both are
    /// printed.
    pub fn label(&self) -> String {
        match self.efforts.is_empty() {
            true => self.models.join("→"),
            false => format!("{}/{}", self.models.join("→"), self.efforts.join("→")),
        }
    }
}

/// The file the session was written to, most likely place first.
///
/// Claude Code names the directory after the cwd with every character that is
/// not a letter, a digit or a hyphen replaced by one — `/Users/ed/.local` is
/// `-Users-ed--local` — so the direct hit costs one `stat` and is what happens
/// every time an agent stayed in the tree it was spawned into.
///
/// **The scan is not belt and braces.** A claim's cwd is where the *work* is
/// and the transcript is named after where the *pane* was, `wsp resume` starts
/// a session in a new workspace at a recorded cwd, and `wsp checkout` is free
/// to move a worktree; every one of those makes the derived path wrong while
/// the session id stays exactly right. A session id is a uuid, so a scan
/// cannot find the wrong file — it can only fail to find one.
///
/// The id is checked before it is put in a path. It arrives from herdr, which
/// got it from the agent, and `spawn` is a verb agents drive: a session id with
/// a `/` in it is not a session id, and refusing it here is cheaper than
/// discovering what it opened.
fn transcript(session: &str, cwd: &str) -> Option<PathBuf> {
    if session.is_empty() || !session.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let file = format!("{session}.jsonl");
    let projects = util::home().join(".claude").join("projects");
    let derived = projects.join(mangle(cwd)).join(&file);
    if derived.is_file() {
        return Some(derived);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(&file))
        .find(|p| p.is_file())
}

/// A cwd as Claude Code spells it in a directory name.
fn mangle(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}

/// One transcript, reduced to what served it.
///
/// Read a line at a time rather than into a string: a transcript is as long as
/// the session was, the longest on this machine is 123MB, and nothing here
/// needs two of its turns at once.
///
/// Two filters, and each is a claim about what the record means:
///
/// - **The cheap `contains` is a gate, not the test.** It matches the bare word
///   rather than `"type":"assistant"` on purpose: a filter that spells out the
///   punctuation is a filter that silently passes nothing the day the writer
///   puts a space after a colon, and a gate that fails open costs a parse while
///   a gate that fails closed costs the whole record. What decides is the parse
///   below — a tool result can quote a turn, and somebody's pasted output is
///   not evidence of a tier.
/// - **A sidechain is a different agent.** Sub-agent turns are written to the
///   same file with `isSidechain` set, they can run on a model the session
///   never chose, and the question this answers is what the *claimant* ran at.
///   Counting them would report a tier nobody asked for on a task whose agent
///   spawned one search.
///
/// `None` when nothing served a turn at all, which is a transcript that exists
/// and is not evidence of a tier — an agent that started and was killed before
/// it answered. Empty is not zero here: it must not become "ran at nothing".
///
/// # One request is several lines, and counting lines counted it three times
///
/// Measured 2026-08-22 against the largest transcript on this machine: **2,045
/// assistant lines, 692 distinct requests.** Claude Code writes a line per
/// content block — a thinking block and the tool call it decided on are two
/// lines of the same response — and it repeats lines outright after a fork.
/// Every one of those copies carries the *same* `usage`, so the totals this
/// row was opened to record came out at 673M cache-read tokens for a session
/// that read about 220M.
///
/// So the key is `message.id`, which is the id of the API response: one id is
/// one request, one bill and one turn. It is read once per id and skipped
/// after, which also puts the turn count right — the old count was three
/// times the truth and had been printed on every attempt in the store since
/// wsp-060.
///
/// A line with no id at all is counted as its own turn. That is the fail-open
/// half of the same rule the gate above follows: an unrecognised shape must
/// cost a duplicate at worst, never the whole record.
fn read_ran(src: &mut impl std::io::BufRead, since: i64) -> Option<Ran> {
    let mut ran = Ran::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut line = String::new();
    while {
        line.clear();
        src.read_line(&mut line).unwrap_or(0) > 0
    } {
        if !line.contains("assistant") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        // Before this attempt: the same session served the task this agent was
        // handed before this one. See [`Kind::ran`] for why the window is the
        // difference between an attempt and a session.
        if since > 0 {
            let at = v.get("timestamp").and_then(Value::as_str).map(util::epoch_of);
            if at.is_some_and(|t| t > 0 && t < since) {
                continue;
            }
        }
        let Some(model) = v.pointer("/message/model").and_then(Value::as_str) else { continue };
        // The runtime writes its own messages into the same stream and names
        // the model `<synthetic>` — a cancelled turn, a refusal it composed
        // itself — with every token count zero. Found in a 118MB transcript on
        // 2026-08-18, one line among 2,046: enough to put a third tier in a
        // label and a turn in a count that no model ever served. Angle brackets
        // are the runtime saying *this is not a model*, so they are what is
        // matched, rather than the one spelling seen so far.
        if model.starts_with('<') {
            continue;
        }
        // The same response, written out again: a second content block, or a
        // fork replaying what came before it. The tier it names is already
        // recorded and its usage is already counted.
        if let Some(id) = v.pointer("/message/id").and_then(Value::as_str) {
            if !seen.insert(id.to_string()) {
                continue;
            }
        }
        ran.turns += 1;
        let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        ran.spend.cache += n("/message/usage/cache_read_input_tokens");
        ran.spend.input +=
            n("/message/usage/input_tokens") + n("/message/usage/cache_creation_input_tokens");
        // Thinking is inside `output_tokens` here and beside it in opencode's
        // record; [`read_export`] adds the two so that this column means the
        // same thing on both sides. Claude Code publishes no price at all, so
        // `micros` stays `None` — the record says wsp does not know rather
        // than pricing it from a table that would be wrong the week a rate
        // changed.
        ran.spend.output += n("/message/usage/output_tokens");
        let model = model.strip_prefix("claude-").unwrap_or(model).to_string();
        if !ran.models.contains(&model) {
            ran.models.push(model);
        }
        if let Some(effort) = v.get("effort").and_then(Value::as_str) {
            if !ran.efforts.iter().any(|e| e == effort) {
                ran.efforts.push(effort.to_string());
            }
        }
    }
    (!ran.models.is_empty()).then_some(ran)
}

/// Whether any live session goes by this name.
///
/// A line of its own so the order above can be argued about in a test without
/// shelling out, which is the same seam [`confirmed`] is split on.
fn answers_to(live: &[Live], handle: &str) -> bool {
    live.iter().any(|s| s.name == handle)
}

/// One row of `claude agents --json`.
///
/// Four fields of the several it sends, which are the ones something reads. It
/// also carries `pid`, `kind`, `startedAt` and `state`; `pid` is the socket path
/// the transport would need and is left unparsed until there is a transport.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Live {
    /// `sessionId`, which is the same string herdr reports as
    /// `agent_session.value` and the port carries as `Seated::session`.
    pub session: String,
    /// `name` — the handle the session channel is addressed by.
    pub name: String,
    pub cwd: String,
    pub status: String,
}

/// How the recovery sentence names an agent, and how sure it is of it.
///
/// Two answers rather than one because the failure path has two truths to tell
/// and a reading that could only tell the first told neither. The distinction is
/// **not** the trait's "a wrong handle is worse than no handle": that rule is
/// about naming *somebody else's* agent, and it still holds without exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// A live session answers to this name — the runtime's own census says so.
    Confirmed(String),
    /// The name wsp gave this agent, which no live session answers to.
    ///
    /// **Not a guess at which name.** wsp passed this exact string to that
    /// launch as `-n`, and [`mint`] is task *and* seat precisely so that no
    /// other agent of wsp's can be answering to it — herdr issues at most one
    /// live agent per pane. So what is unconfirmed here is whether the agent
    /// exists, never who it would be, and that is what makes saying it safe: the
    /// only thing this can name wrongly is nothing at all.
    ///
    /// Which is worth having, because "nothing at all" is what it says. A spawn
    /// that failed *early* is the case — the registry lands 1.3-1.4s after
    /// launch and a failure can beat it, measured in the module docs — and an
    /// agent that comes up a moment later comes up under this name. The
    /// alternative on offer was silence, and a person recovering a spawn by hand
    /// at midnight can use a hedged name and cannot use nothing.
    Unconfirmed(String),
}

/// How long to keep asking the runtime before an address is only [`mint`]ed.
///
/// A struct rather than two constants for the reason `cmd_spawn::Patience` gives
/// about its own, and the clock is handed in for the reason [`util::Clock`]
/// gives: a test that waits three real seconds to check what happens after three
/// seconds is a test nobody runs, and one that shortens the window instead is
/// measuring this machine's load.
///
/// The budget is a fact about **Claude Code's registry**, not about the caller's
/// patience, which is why it lives here beside the measurement rather than in
/// `spawn`'s three numbers: the session file appears 1.3-1.4s after launch and
/// `claude agents --json` costs another ~0.5s to read it. Three seconds from the
/// ask covers that from any failure early enough to race it, and it is only ever
/// spent by a spawn that has already printed why it failed — the diagnosis is on
/// stderr before this is asked, so what the wait delays is the advice and never
/// the error.
///
/// `poll` is short beside the ask it throttles on purpose. The subprocess is the
/// real interval here (~0.5s, measured), so this is what stops a *cheap* listing
/// — a fake, an empty `PATH`, a `claude` that fails instantly — from spinning
/// the budget away in a tight loop.
struct Lag<'a> {
    until: Duration,
    poll: Duration,
    clock: &'a dyn Clock,
}

impl Default for Lag<'static> {
    fn default() -> Lag<'static> {
        Lag {
            until: Duration::from_millis(3_000),
            poll: Duration::from_millis(250),
            clock: &util::Wall,
        }
    }
}

/// The handle for one spawned agent: waited for, then said anyway.
///
/// Split from [`Kind::address`] for the reason the handbook gives about seams —
/// `address` has to shell out for the listing, so anything decided inside it is
/// decided where no test can reach. The listing is a closure rather than a slice
/// because what is interesting here is what happens *between* two readings of
/// it, which is the one thing a single slice cannot express.
///
/// Returns as soon as either reading answers, so the budget is only ever spent
/// when nothing is answering at all. A registry that never catches up leaves the
/// mint, unconfirmed; a spawn with nothing to mint from leaves nothing, and that
/// is still the honest answer.
fn addressed(
    spawn: &Spawn,
    row: Option<&Seated>,
    lag: &Lag,
    live: &dyn Fn() -> Vec<Live>,
) -> Option<Address> {
    let deadline = lag.clock.now() + lag.until;
    loop {
        if let Some(handle) = confirmed(&live(), spawn, row) {
            return Some(Address::Confirmed(handle));
        }
        if lag.clock.now() >= deadline {
            return mint(spawn.name, spawn.seat).map(Address::Unconfirmed);
        }
        lag.clock.rest(lag.poll);
    }
}

/// The order, given one reading of the runtime's census and one of the backend's.
///
/// Minted first, and only if the runtime confirms a session by that name. Then
/// [`pick`], unchanged, for every agent wsp did not start — a person's own
/// `claude` in a pane, an agent from before this shipped, one restarted by hand.
/// That path is byte-identical to what it was, which is why demoting it is a
/// cheap change rather than a risky one.
///
/// The mint comes from the [`Spawn`] and no longer from the census row, which is
/// robustness-049 and the module docs carry why. `row` is therefore optional and
/// only [`pick`] reads it: there is nothing left that a missing census row can
/// take away.
fn confirmed(live: &[Live], spawn: &Spawn, row: Option<&Seated>) -> Option<String> {
    let minted = mint(spawn.name, spawn.seat).filter(|h| answers_to(live, h));
    minted.or_else(|| row.and_then(|r| pick(live, &r.session, &r.cwd)))
}

/// Which live session is the one in this seat.
///
/// Two keys, and the order is the point. The session id is **exact**: the
/// backend saw the agent and said which session it is, and no two sessions share
/// one. The cwd is a fallback for the case that matters most — herdr failing to
/// detect a live agent is the whole of robustness-041, and it leaves a seat with a
/// cwd and no session — and it answers only when exactly one live session is in
/// that directory.
///
/// Ambiguity returns `None` rather than the first match. Three agents in one
/// tree is the ordinary state of this repo, so a first-match rule would hand
/// back a colleague's handle, and a work order delivered to the wrong agent is
/// worse than one not delivered at all.
fn pick(live: &[Live], session: &str, cwd: &str) -> Option<String> {
    if !session.trim().is_empty() {
        if let Some(s) = live.iter().find(|s| s.session == session) {
            return Some(s.name.clone());
        }
    }
    if cwd.trim().is_empty() {
        return None;
    }
    let here = util::expand(cwd);
    let mut in_tree = live.iter().filter(|s| util::expand(&s.cwd) == here);
    match (in_tree.next(), in_tree.next()) {
        (Some(only), None) => Some(only.name.clone()),
        _ => None,
    }
}

/// `claude agents --json`, parsed.
///
/// A row missing `sessionId` or `name` is dropped rather than defaulted: an
/// empty session id would match every seat the backend could not read, which is
/// exactly the case [`pick`] is careful about.
fn parse_listing(json: &str) -> Vec<Live> {
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let get = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    rows.iter()
        .map(|r| Live {
            session: get(r, "sessionId"),
            name: get(r, "name"),
            cwd: get(r, "cwd"),
            status: get(r, "status"),
        })
        .filter(|s| !s.session.is_empty() && !s.name.is_empty())
        .collect()
}

/// Claude Code, wherever it is.
///
/// The same reasoning as `cmd_sandbox::herdr_bin`, which records it: a `wsp`
/// started by herdr's plugin runner inherits whatever `PATH` herdr was launched
/// with, so the binary is looked up by hand rather than trusted to be on it.
fn claude_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("CLAUDE_BIN") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("claude");
            if p.is_file() {
                return p;
            }
        }
    }
    util::home().join(".local/bin/claude")
}

/// Every Claude Code session on this machine, as its own runtime sees them.
///
/// `--json` is documented as being for scripting and as not requiring a TTY,
/// which is what makes this safe to call from a spawn: it reads a registry and
/// exits, and no pane has to be rendered or focused for it to answer. That last
/// clause is why it is allowed to exist at all — the decision of 2026-08-17
/// forbids a mechanism that only works when a person happens to be looking.
///
/// This machine only. herdr's ids carry an `@machine` suffix and a far agent's
/// registry is on the far machine, so a seat that is not local has no answer
/// here — [`pick`] gets an empty listing and says nothing, which is the right
/// answer rather than a wrong one.
fn listing() -> std::result::Result<Vec<Live>, String> {
    let out = captured(claude_bin(), &["agents", "--json"])?;
    Ok(parse_listing(&String::from_utf8_lossy(&out)))
}

/// The sentence a caller says when it could not deliver a work order.
///
/// Here rather than in `cmd_spawn` because the *shape* of the advice is
/// kind-specific even though the wording is general: what it names is the
/// handle [`Kind::address`] returned, and only this file knows what that handle
/// is for. Seven spawns in one night were recovered by hand over the session
/// channel, each of them beginning with somebody listing every agent on the
/// machine and guessing which name went with which pane. This is that step,
/// done.
///
/// Two sentences, because a person does two different things next. A confirmed
/// handle is somewhere to send the work order now. An unconfirmed one is a name
/// to look for, and saying so in the first clause rather than the last is the
/// difference between a hedge and a lie — the reader must not have to reach the
/// end of the line to learn that nothing answered.
pub fn recovery(address: Option<Address>) -> Option<String> {
    Some(match address? {
        Address::Confirmed(h) => {
            format!("it is reachable as `{h}` — the work order can be sent there by hand")
        }
        Address::Unconfirmed(h) => format!(
            "nothing is answering to `{h}` — that is the name this spawn asked for, \
             so if the agent comes up at all the work order can be sent there by hand"
        ),
    })
}

/// The refusal a kind gives when asked for something it has no notion of.
///
/// Unused while every kind's `tell` falls back to the backend, and kept because
/// it is the shape the channel arrives in: a Claude Code with no reachable
/// session is [`Refusal::Unsupported`], not a backend failure, and a caller must
/// be able to tell those apart to know whether retrying is worth anything.
#[allow(dead_code)]
pub fn no_channel() -> Refusal {
    Refusal::Unsupported("reach this agent except through its terminal")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::{Agent, Event, Order, Seated, State};

    /// A capture of `claude agents --json` from this machine on 2026-08-17,
    /// trimmed to the rows that matter and otherwise byte-for-byte as sent.
    ///
    /// A recording rather than a hand-written fixture, for the reason the fake
    /// backend's docs give: a parser tested against a shape its subject does not
    /// produce is a parser that has been tested against itself. The first row is
    /// a background session, which carries `id` and `state` and **no `pid`**,
    /// and is the row a `pid`-keyed reading would have dropped.
    const CAPTURE: &str = r#"[
      {
        "id": "a508019d",
        "cwd": "/Users/edjames/claude/vst/Trance",
        "kind": "background",
        "startedAt": 1784113109222,
        "sessionId": "a508019d-11af-46fd-a804-0e49ba1b8651",
        "name": "Discuss performance optimizations",
        "state": "blocked"
      },
      {
        "pid": 6389,
        "cwd": "/Users/edjames/claude",
        "kind": "interactive",
        "startedAt": 1786784991521,
        "sessionId": "e0d0eb3f-51b6-4194-ae6d-03f26b8c7470",
        "name": "vst-3a",
        "status": "idle"
      },
      {
        "pid": 70214,
        "cwd": "/Users/edjames/claude/wsp",
        "kind": "interactive",
        "startedAt": 1786964684840,
        "sessionId": "7a188ba8-7ca6-4743-921f-35fcc7079c11",
        "name": "wsp-f3",
        "status": "busy"
      },
      {
        "pid": 41295,
        "cwd": "/Users/edjames/claude/wsp",
        "kind": "interactive",
        "startedAt": 1786964926194,
        "sessionId": "21bfdf04-c779-4752-9359-3ea5d2456f80",
        "name": "wsp-4b",
        "status": "busy"
      }
    ]"#;

    /// **The measurement this task turned on.** The id wsp holds resolves to the
    /// name the session channel is addressed by.
    ///
    /// `7a188ba8-…` and `wsp-f3` are the exact pair recorded on robustness-042 as
    /// underivable from each other — the id herdr reported for that agent, and
    /// the name a work order actually reached it under after wsp's own delivery
    /// had failed for the fifth time running. They are one row of one documented
    /// listing apart.
    #[test]
    fn the_session_id_herdr_reports_is_the_key_to_the_name_the_channel_wants() {
        let live = parse_listing(CAPTURE);
        assert_eq!(live.len(), 4, "{live:?}");
        assert_eq!(
            pick(&live, "7a188ba8-7ca6-4743-921f-35fcc7079c11", "/Users/edjames/claude/wsp")
                .as_deref(),
            Some("wsp-f3")
        );
        // A background session has no `pid` and is still addressable, which is
        // why nothing here is keyed on one.
        assert_eq!(
            pick(&live, "a508019d-11af-46fd-a804-0e49ba1b8651", "").as_deref(),
            Some("Discuss performance optimizations")
        );
    }

    /// The fallback, and the case it must refuse.
    ///
    /// A seat whose agent herdr cannot see carries a cwd and no session — that
    /// is the whole of robustness-041 — and one live session in that directory is
    /// an exact answer. Two are not, and this repo normally has three: a
    /// first-match rule would hand back a colleague's handle, and a work order
    /// delivered to the wrong agent is worse than one not delivered.
    #[test]
    fn a_seat_with_no_session_is_named_by_its_tree_only_when_nobody_shares_it() {
        let live = parse_listing(CAPTURE);
        assert_eq!(pick(&live, "", "/Users/edjames/claude").as_deref(), Some("vst-3a"));
        assert_eq!(pick(&live, "", "/Users/edjames/claude/wsp"), None, "two agents in one tree");
        assert_eq!(pick(&live, "", "/Users/edjames/nowhere"), None);
        assert_eq!(pick(&live, "", ""), None);
        // An id that is not in the listing falls through to the tree rather than
        // failing outright: a session that ended leaves a seat somebody may
        // still be sitting in.
        assert_eq!(pick(&live, "no-such-session", "/Users/edjames/claude").as_deref(), Some("vst-3a"));
    }

    /// A listing wsp cannot read says nothing, and never says the wrong thing.
    ///
    /// The failure mode being guarded is specific: a row with no `sessionId`
    /// defaulted to `""` would match every seat the backend could not read,
    /// which is precisely the seat the fallback exists for.
    #[test]
    fn nothing_that_is_not_a_listing_produces_a_handle() {
        for junk in ["", "null", "{}", "not json at all", "[]", r#"[{"pid":1}]"#] {
            assert!(parse_listing(junk).is_empty(), "{junk}");
        }
        let nameless = parse_listing(r#"[{"sessionId":"s-1","cwd":"/tmp"}]"#);
        assert!(nameless.is_empty(), "a session with no name cannot be addressed");
        assert_eq!(pick(&parse_listing("[]"), "s-1", "/tmp"), None);
    }

    /// A seat, as the backend reports one, for arguing about [`confirmed`].
    fn seated(seat: &str, task: &str, session: &str, cwd: &str) -> Seated {
        Seated {
            seat: Seat::new(seat),
            agent: Agent { kind: "claude".into(), name: task.into(), ..Agent::default() },
            session: session.into(),
            cwd: cwd.into(),
            ..Seated::default()
        }
    }

    /// The name wsp minted wins over the name the runtime would be searched for,
    /// and it wins in exactly the case the lookup cannot answer.
    ///
    /// The seat here is in `/Users/edjames/claude/wsp` with two other agents, and
    /// its session id is empty — which is robustness-041's blind spot, herdr
    /// unable to see a live agent without a rendered pane. `pick` has nothing to
    /// go on and correctly refuses. A minted handle is matched against itself
    /// and needs neither.
    #[test]
    fn a_minted_handle_answers_where_the_lookup_has_to_give_up() {
        let mut live = parse_listing(CAPTURE);
        live.push(Live {
            session: "d28a4237-8811-42ef-b89e-34ffc2d0df9f".into(),
            name: "t-260817-014-w2J:p1".into(),
            cwd: "/Users/edjames/claude/wsp".into(),
            status: "busy".into(),
        });
        let seat = Seat::new("w2J:p1");
        let row = seated("w2J:p1", "t-260817-014", "", "/Users/edjames/claude/wsp");
        assert_eq!(pick(&live, &row.session, &row.cwd), None, "three agents in one tree");
        assert_eq!(
            confirmed(&live, &spawn(false, "t-260817-014", &seat), Some(&row)).as_deref(),
            Some("t-260817-014-w2J:p1")
        );
    }

    /// **The silence that had no timing in it.** A seat the backend has no agent
    /// record for is still addressed, because the handle never needed the record.
    ///
    /// This is robustness-049's deterministic half and the row below is what was
    /// measured on 2026-08-17: herdr's `agent.list` carries an agent's `name` and
    /// its session, `pane.list` carries neither, and `Herdr::census` falls back
    /// to the pane row for exactly the seat whose agent it has lost sight of —
    /// which is the seat every failed spawn asks about. So the old reading minted
    /// from an empty name, got a handle that is only the seat, and matched
    /// nothing; `pick` had no session id and a worktree it could not prove was
    /// this agent's. Three failures, three silences, and no wait would have
    /// helped any of them.
    #[test]
    fn a_seat_the_backend_kept_no_record_of_is_named_by_what_wsp_called_it() {
        let live = vec![Live {
            session: "d28a4237-8811-42ef-b89e-34ffc2d0df9f".into(),
            name: "robustness-049-w32:p1".into(),
            cwd: "/Users/edjames/claude/wsp/.worktrees/robustness-049".into(),
            status: "idle".into(),
        }];
        // A `pane.list` row: the kind, the cwd, and nothing that names the agent.
        let blind = seated("w32:p1", "", "", "/Users/edjames/claude/wsp/.worktrees/robustness-049");
        assert_ne!(
            mint(&blind.agent.name, &blind.seat).as_deref(),
            Some("robustness-049-w32:p1"),
            "the census cannot rebuild a handle it was never told"
        );
        let seat = Seat::new("w32:p1");
        assert_eq!(
            confirmed(&live, &spawn(false, "robustness-049", &seat), Some(&blind)).as_deref(),
            Some("robustness-049-w32:p1"),
            "wsp chose this name before the agent existed and still holds it"
        );
    }

    /// An agent wsp did not start is still found the way it always was.
    ///
    /// The demotion has to be a demotion and not a replacement: `wsp-f3` was
    /// never minted by anything, and the pair below is the one robustness-042
    /// recorded as underivable. It still resolves.
    #[test]
    fn an_agent_wsp_did_not_name_falls_back_to_the_census_lookup() {
        let live = parse_listing(CAPTURE);
        let seat = Seat::new("w1:p1");
        let row = seated("w1:p1", "t-260817-011", "7a188ba8-7ca6-4743-921f-35fcc7079c11", "");
        assert_eq!(
            confirmed(&live, &spawn(false, "t-260817-011", &seat), Some(&row)).as_deref(),
            Some("wsp-f3"),
            "the lookup still answers"
        );
    }

    /// A handle wsp minted for an agent that is not running is not *confirmed*.
    ///
    /// This is the whole reason the order checks the listing instead of trusting
    /// its own arithmetic. It is asked on the failure path, where "the agent
    /// never started" is a live possibility — and `recovery` must not print "it
    /// is reachable as t-260817-014-w2J:p1" about nothing at all.
    ///
    /// What it may print is the other sentence, and the two tests below are why
    /// that is a different claim rather than the same one weakened.
    #[test]
    fn a_name_wsp_minted_for_an_agent_that_never_started_is_not_reported_as_reachable() {
        let live = parse_listing(CAPTURE);
        let seat = Seat::new("w2J:p1");
        let row = seated("w2J:p1", "t-260817-014", "", "/Users/edjames/claude/wsp");
        let spawned = spawn(false, "t-260817-014", &seat);
        assert_eq!(confirmed(&live, &spawned, Some(&row)), None, "minted, nothing answers to it");
        // The seat being empty of everything is the same answer, not a panic.
        assert_eq!(confirmed(&[], &spawned, Some(&row)), None);
        assert_eq!(confirmed(&[], &spawned, None), None, "and a census wsp could not read");
    }

    /// **The race, and the wait that wins it.** A registry written after the
    /// spawn has already failed is asked again rather than believed once.
    ///
    /// The 1,400ms below is the measurement and not a round number: launch to
    /// session file was 1,306ms, 1,440ms and 1,345ms over three runs on
    /// 2026-08-17, and `claude agents --json` needs another ~500ms to read it.
    /// The clock is wound rather than waited on, so this asserts the rule instead
    /// of this machine's load — `util::Clock` holds that argument.
    #[test]
    fn an_address_the_runtime_has_not_written_yet_is_asked_for_again() {
        let seat = Seat::new("w32:p1");
        let handle = "robustness-049-w32:p1";
        let registered = std::cell::Cell::new(false);
        let asks = std::cell::Cell::new(0);
        let dial = util::Dial::new().at(Duration::from_millis(1_400), || registered.set(true));
        let live = || {
            asks.set(asks.get() + 1);
            match registered.get() {
                true => {
                    vec![Live { name: handle.into(), session: "s-1".into(), ..Live::default() }]
                }
                false => Vec::new(),
            }
        };
        // The budget the CLI ships, on a clock the test winds — so a change to
        // either number has to survive the arithmetic below rather than a copy
        // of it kept here.
        let lag = Lag { clock: &dial, ..Lag::default() };
        assert_eq!(
            addressed(&spawn(false, "robustness-049", &seat), None, &lag, &live),
            Some(Address::Confirmed(handle.to_string())),
            "the sentence a person got nothing of"
        );
        let took = dial.elapsed();
        assert!(took >= Duration::from_millis(1_400), "gave up before the registry: {took:?}");
        assert!(took < Duration::from_millis(1_700), "kept asking after it answered: {took:?}");
        // Arithmetic rather than a guess about scheduling: the polls it takes to
        // reach the registry, and then the ask that finds it.
        let poll = lag.poll.as_millis();
        assert_eq!(asks.get() as u128, 1_400u128.div_ceil(poll) + 1, "one ask per poll, no more");
    }

    /// A registry that never catches up costs a hedge, not the sentence.
    ///
    /// The budget is spent exactly once and then the handle is said anyway, which
    /// is the whole design note on `Address::Unconfirmed`: what is unconfirmed is
    /// whether the agent exists, never which agent it would be. Silence is what a
    /// person got on all three failures of 2026-08-17 and it is strictly worse.
    #[test]
    fn an_agent_the_runtime_never_confirms_is_still_named_once_the_waiting_is_done() {
        let seat = Seat::new("w32:p1");
        let dial = util::Dial::new();
        let asks = std::cell::Cell::new(0);
        let live = || {
            asks.set(asks.get() + 1);
            Vec::new()
        };
        let lag = Lag { clock: &dial, ..Lag::default() };
        assert_eq!(
            addressed(&spawn(false, "robustness-049", &seat), None, &lag, &live),
            Some(Address::Unconfirmed("robustness-049-w32:p1".to_string()))
        );
        assert_eq!(dial.elapsed(), lag.until, "the budget, and no more of it");
        let (until, poll) = (lag.until.as_millis(), lag.poll.as_millis());
        assert_eq!(asks.get() as u128, until / poll + 1, "the polls, and the ask that ends it");
        // Nothing to mint from is still nothing to say. A spawn with neither a
        // task nor a seat has no name to offer, hedged or otherwise.
        let nowhere = Seat::new("");
        assert_eq!(addressed(&spawn(false, "", &nowhere), None, &lag, &live), None);
    }

    /// The trim moved and did not change, and it is still Claude Code's alone.
    ///
    /// Asserted as names rather than as a count, because the point of a denylist
    /// is that it is legible: a test that only checked the length would pass on
    /// a list that had quietly become something else. `Read`, `Edit`, `Write`
    /// and `Bash` appearing here would be the bad change — the measurement that
    /// prompted the trim found agents doing all their reading through `sed` at
    /// ~28K, and a trim that pushes work into Bash costs more than it saves.
    /// A spawn description, so the tests below argue about one thing each.
    fn spawn<'a>(full: bool, name: &'a str, seat: &'a Seat) -> Spawn<'a> {
        Spawn { full, name, seat, model: None, effort: None, order: None, resume: None }
    }

    /// The same, at a stated tier.
    fn at<'a>(name: &'a str, seat: &'a Seat, model: Option<&'a str>, effort: Option<&'a str>) -> Spawn<'a> {
        Spawn { model, effort, ..spawn(false, name, seat) }
    }

    #[test]
    fn an_opencode_is_started_with_the_model_and_the_work_order_it_was_given() {
        let seat = Seat::new("w1:p1");
        let sp = Spawn {
            model: Some("opencode/x-preview-f-free"),
            order: Some("You have been claimed onto core-027."),
            ..spawn(false, "core-027", &seat)
        };
        let argv = of("opencode").args(&sp);
        assert_eq!(
            argv,
            vec![
                "--model",
                "opencode/x-preview-f-free",
                "--prompt",
                "You have been claimed onto core-027."
            ],
            "the tier reaches the command line, and so does the order it was started to act on"
        );
    }

    #[test]
    fn an_opencode_work_order_is_one_argument_however_much_prose_is_in_it() {
        let seat = Seat::new("w1:p1");
        // The hazard is real and is why prose goes through a file everywhere
        // else in wsp: a work order is long, and it quotes identifiers.
        let prose = "read `wsp show core-027` and $(nothing) — then \"begin\"";
        let sp = Spawn { order: Some(prose), ..spawn(false, "core-027", &seat) };
        let argv = of("opencode").args(&sp);
        assert_eq!(argv.len(), 2, "one flag and one value, never a split sentence: {argv:?}");
        assert_eq!(argv[1], prose, "nothing quoted, escaped or eaten on the way");
    }

    #[test]
    fn a_resumed_opencode_is_given_the_flag_opencode_actually_has() {
        let seat = Seat::new("w1:p1");
        let sp = Spawn { resume: Some("ses_fdaf7f65cffe"), ..spawn(false, "core-027", &seat) };
        let argv = of("opencode").args(&sp);
        assert_eq!(
            argv,
            vec!["--session", "ses_fdaf7f65cffe"],
            "opencode has no --resume; handing it Claude Code's spelling is what core-031 is for"
        );
    }

    #[test]
    fn only_the_kind_wsp_installs_a_hook_for_is_told_its_brief_is_already_there() {
        assert!(of("claude").briefed(), "claude-code/settings.snippet.json is Claude Code's");
        assert!(
            !of("opencode").briefed() && !of("codex").briefed(),
            "no other kind runs wsp's SessionStart hook, so no other kind arrives briefed"
        );
    }

    #[test]
    fn only_a_kind_that_takes_its_order_in_argv_says_so() {
        assert!(of("opencode").order_in_args(), "measured: typed input is discarded for ~2s");
        assert!(!of("claude").order_in_args(), "Claude Code is told, and the trim is not an order");
        assert!(!of("codex").order_in_args(), "an unmeasured kind claims nothing");
    }

    #[test]
    fn no_kind_yet_has_a_queue_that_reads_what_is_put_in_it() {
        // The tripwire on `core-028`: opencode was the candidate for `false`
        // and was driven, and a sentence pending across a kill came back
        // durable but with nothing to re-drive it. Flipping either of these
        // stops holding a wake for the turn boundary and hands it over as
        // delivered, which is `core-017`'s drop — so the argument on
        // `Kind::queue_is_the_agents` has to be disagreed with before this
        // line moves.
        assert!(of("opencode").queue_is_the_agents(), "durable is not read — core-026 drove it");
        assert!(of("claude").queue_is_the_agents(), "the queue dies with a /clear");
        assert!(of("codex").queue_is_the_agents(), "an unmeasured kind claims nothing");
    }

    #[test]
    fn a_spawned_opencode_can_read_and_record_freely_and_asks_before_anything_else() {
        let env = of("opencode").env(None);
        let cfg = env.get("OPENCODE_CONFIG_CONTENT").expect("the brake is armed on the seat");
        let v: Value = serde_json::from_str(cfg).expect("valid config, composed not concatenated");
        let bash = &v["permission"]["bash"];
        // The catch-all is the brake: everything not named is a person's call.
        assert_eq!(bash["*"], "ask", "core-020 d1 — {cfg}");
        assert_eq!(v["permission"]["edit"], "ask", "core-020 d1 — {cfg}");
        // And the named verbs are Ed's own file, which is what d2 settles: the
        // stall that mattered was not `wsp brief`, it was every read after it.
        assert_eq!(bash["wsp brief*"], "allow");
        assert_eq!(bash["wsp show*"], "allow", "the verb the brief-only exception stalled on next");
        assert_eq!(bash["wsp note*"], "allow", "recording is reading's other half");
        // `done` is Ed's, so it is refused rather than offered — an ask is an
        // invitation for somebody to say yes on his behalf.
        assert_eq!(bash["wsp done*"], "deny", "the decision of 2026-08-19");
        assert_eq!(bash["wsp archive*"], "deny");
        // The read half, which is `core-040`: the command that stopped `ui-007`
        // on its second action, and the pipeline tail that would have stopped
        // it anyway. opencode asks once per request and needs *every* segment
        // allowed, so a `grep` without a `head` buys nothing.
        assert_eq!(bash["grep *"], "allow", "the first of ui-007's 28 prompts — {cfg}");
        assert_eq!(bash["head *"], "allow", "and the tail of the same pipeline");
        assert_eq!(bash["git show *"], "allow", "`git show HEAD:<path>` is the handbook's own idiom");
        // But not with a redirect on the end of it, which is a write.
        assert_eq!(bash["*>*"], "ask", "core-040 — a head that can redirect is not read-only");
    }

    /// The order the map is written in, because opencode reads it as one.
    ///
    /// `Permission.evaluate` is a `findLast` over the rules in config order, so
    /// a rule only means anything if it comes after everything it has to beat.
    /// Asserted on the raw string rather than the parsed value, because parsing
    /// it into a `serde_json::Value` puts it back in a `BTreeMap` and throws
    /// away the one property under test. Driven the same day against opencode
    /// 1.18.21: `opencode debug config` with this value resolves the bash map
    /// in the order wsp wrote it, and a spawned agent evaluated
    /// `grep -n "## Source map" README.md | head -5` as two allows and never
    /// asked.
    #[test]
    fn the_broad_rules_are_written_before_the_narrow_ones_that_have_to_beat_them() {
        let env = of("opencode").env(None);
        let cfg = env.get("OPENCODE_CONFIG_CONTENT").expect("nothing on the seat");
        let at = |needle: &str| cfg.find(needle).unwrap_or_else(|| panic!("no {needle} in {cfg}"));
        assert!(at(r#""*":"ask""#) < at(r#""grep *""#), "the catch-all would bury the reads: {cfg}");
        assert!(at(r#""grep *""#) < at(r#""*>*""#), "a redirect would be allowed: {cfg}");
        assert!(at(r#""*>*""#) < at(r#""wsp done*""#), "the four denies answer to nothing: {cfg}");
    }

    /// What is not on the read list, and the test each entry had to pass.
    ///
    /// The property is checkable and the exclusions are where it does its work:
    /// a command earns a place only if no flag it accepts can write a file or
    /// run another command. These five are the ones a reader would expect to
    /// find and would then have to un-fix.
    #[test]
    fn a_command_is_on_the_read_list_only_if_nothing_it_accepts_writes_or_runs() {
        for (cmd, why) in [
            ("find", "-exec and -delete"),
            ("sed", "-i"),
            ("awk", "system() and print >"),
            ("sort", "-o"),
            ("rg", "--pre runs a command of the caller's choosing"),
        ] {
            assert!(!READ_ONLY.contains(&cmd), "{cmd} is not read-only — {why}");
        }
        assert!(READ_ONLY.contains(&"grep"), "the one ui-007 stopped on");
        assert!(
            READ_ONLY.iter().all(|c| !c.starts_with("git ") || c.split(' ').count() == 2),
            "git is allowed by subcommand, so `push` and `reset` stay where the rest of the shell is"
        );
    }

    /// The brief rides in on the same variable as the brake, and does not
    /// disturb it or the machine's own config.
    ///
    /// `core-032` d7. Two things are asserted because two things could break:
    /// the key has to be there and absolute, and the config has to still be the
    /// policy it was — a spawned opencode that got a brief and lost its brake
    /// would be the worst trade in this file. What cannot be asserted from here
    /// is the third and most important one, because it is opencode's behaviour
    /// and not wsp's: that this **merges** with the machine config rather than
    /// replacing it, so `~/.config/opencode/plugins/herdr-agent-state.js`
    /// survives. Driven instead — see the note in [`config`].
    #[test]
    fn the_brief_arrives_on_the_same_variable_as_the_brake_and_does_not_disturb_it() {
        let cfg = |brief| {
            let env = of("opencode").env(brief);
            let raw = env.get("OPENCODE_CONFIG_CONTENT").expect("nothing on the seat").clone();
            serde_json::from_str::<Value>(&raw).expect("valid config")
        };

        let with = cfg(Some(Path::new("/some/where/t-260815-033.md")));
        assert_eq!(with["instructions"][0], "/some/where/t-260815-033.md");
        // The brake, unchanged beside it.
        assert_eq!(with["permission"]["bash"]["*"], "ask");
        assert_eq!(with["permission"]["edit"], "ask");
        assert_eq!(with["permission"]["bash"]["wsp done*"], "deny");

        // And a spawn with nothing to brief about says nothing rather than
        // naming a file that is not there: opencode ignores a missing
        // `instructions` path in silence, so a wrong one is invisible.
        let without = cfg(None);
        assert!(without.get("instructions").is_none(), "a path to nothing: {without}");
        assert_eq!(without["permission"], with["permission"], "the brake is not a function of the brief");
    }

    /// Which kinds can be handed a brief this way, and it is a fact about a
    /// runtime's config format rather than about the work.
    #[test]
    fn only_a_kind_with_somewhere_to_read_a_brief_from_is_given_one() {
        assert!(of("opencode").brief_file(), "core-032 d7 drove it");
        assert!(!of("claude").brief_file(), "it has a session hook and must not get it twice");
        assert!(!of("codex").brief_file(), "wsp knows no config channel for it");
    }

    /// The two lists are a copy of somebody else's file, and the only thing
    /// that keeps a copy honest is saying what it is a copy of.
    #[test]
    fn the_verbs_a_spawned_opencode_may_run_are_the_ones_claude_code_is_allowed() {
        assert_eq!(WSP_ALLOWED.len(), 23, "~/.claude/settings.json allows 23 wsp verbs");
        assert_eq!(WSP_DENIED.len(), 4, "and denies four");
        // `mv` is allowed there and `say` is not, both of which read as
        // mistakes and are neither: diverging here would be a second policy.
        assert!(WSP_ALLOWED.contains(&"mv"), "allowed in the file this mirrors");
        assert!(!WSP_ALLOWED.contains(&"say"), "absent there, so absent here — see WSP_ALLOWED");
        assert!(
            WSP_DENIED.iter().all(|d| !WSP_ALLOWED.contains(d)),
            "nothing may be both, whatever the precedence rule turns out to be"
        );
    }

    #[test]
    fn no_other_kinds_seat_is_touched() {
        assert!(
            of("claude").env(Some(Path::new("/tmp/b.md"))).is_empty()
                && of("codex").env(Some(Path::new("/tmp/b.md"))).is_empty(),
            "the compatibility rule everywhere here: an unmeasured kind is left exactly as it was, \
             and a brief it has no way to read does not change that"
        );
    }

    #[test]
    fn wsps_own_tier_words_are_refused_by_a_kind_that_does_not_speak_them() {
        // The likeliest mistake by a distance, and it must not cost a
        // subprocess to catch: `opus` has no `/` and never reaches the lookup.
        let why = of("opencode").tier(Some("opus"), None).expect_err("an alias is not a model here");
        assert!(why.contains("provider/model"), "say the shape that would work — {why}");
        assert!(why.contains("opencode models"), "and where the real names come from — {why}");
    }

    #[test]
    fn an_effort_is_refused_for_a_kind_that_has_no_such_setting() {
        let why = of("opencode").tier(None, Some("high")).expect_err("accepted and dropped is the defect");
        assert!(
            why.contains("--model"),
            "somebody reaching for a cheaper run is told the knob that exists — {why}"
        );
    }

    #[test]
    fn an_opencode_that_states_no_tier_makes_no_subprocess_and_starts_plain() {
        let seat = Seat::new("w1:p1");
        assert!(of("opencode").tier(None, None).is_ok(), "no flags, no catalogue, no cost");
        assert!(
            of("opencode").args(&spawn(false, "core-027", &seat)).is_empty(),
            "an unflagged spawn is byte-for-byte the one wsp did before this kind existed"
        );
    }

    #[test]
    fn a_spawned_claude_is_not_given_the_two_tools_it_is_told_not_to_use() {
        let seat = Seat::new("w2J:p1");
        let trim = of("claude").args(&spawn(false, "t-1", &seat));
        assert!(trim.contains(&"--strict-mcp-config".to_string()), "{trim:?}");
        assert!(trim.contains(&"--disallowedTools".to_string()), "{trim:?}");
        assert!(trim.contains(&"Agent".to_string()), "sub-agents are what blew the budget: {trim:?}");
        assert!(trim.contains(&"Workflow".to_string()), "6,024 tokens of tool nobody may call: {trim:?}");
        for kept in ["Bash", "Read", "Edit", "Write"] {
            assert!(!trim.contains(&kept.to_string()), "{kept} is how the work gets done: {trim:?}");
        }
    }

    /// A spawn that names no tier is the spawn wsp did before tiers existed.
    ///
    /// The compatibility rule, asserted rather than trusted: absent flags send
    /// no argument at all, so the model is still whatever
    /// `~/.claude/settings.json` says and the effort is still the default. A
    /// tier spelled here as a default would be wsp overriding a person's
    /// settings file, silently, on the day they upgraded the binary.
    #[test]
    fn a_spawn_that_states_no_tier_states_nothing() {
        let seat = Seat::new("w2J:p1");
        let argv = of("claude").args(&spawn(false, "t-1", &seat));
        assert!(!argv.contains(&"--model".to_string()), "{argv:?}");
        assert!(!argv.contains(&"--effort".to_string()), "{argv:?}");
    }

    /// And a spawn that names one puts it on the command line, as a pair.
    ///
    /// The value follows its own flag and neither knob implies the other:
    /// `--effort` alone is the common case, being the same capability class for
    /// less spend. Position matters only in that nothing may follow `--resume`,
    /// which is why both sit above it.
    #[test]
    fn the_tier_asked_for_reaches_the_agents_command_line() {
        let seat = Seat::new("w2J:p1");

        let argv = of("claude").args(&at("t-1", &seat, Some("haiku"), Some("low")));
        let pair = |flag: &str, value: &str| {
            let i = argv.iter().position(|a| a == flag).unwrap_or_else(|| panic!("no {flag}: {argv:?}"));
            assert_eq!(argv.get(i + 1).map(String::as_str), Some(value), "{argv:?}");
        };
        pair("--model", "haiku");
        pair("--effort", "low");

        let effort_only = of("claude").args(&at("t-1", &seat, None, Some("max")));
        assert!(effort_only.contains(&"--effort".to_string()), "{effort_only:?}");
        assert!(!effort_only.contains(&"--model".to_string()), "the cheaper knob turns alone: {effort_only:?}");
    }

    /// The vocabulary, and the two shapes that are not in it.
    ///
    /// `[1m]` is a suffix on any alias rather than a fifth model, because it is
    /// a property of the session's context window and not of the model; a full
    /// name is refused so that the list stays aliases, which is what follows an
    /// upgrade. Measured against Claude Code 2.1.234 on 2026-08-18 — every name
    /// this accepts is one that build recognises.
    #[test]
    fn the_tier_vocabulary_is_four_aliases_and_five_levels() {
        let ok = |m: Option<&str>, e: Option<&str>| of("claude").tier(m, e);
        for alias in MODELS {
            assert!(ok(Some(alias), None).is_ok(), "{alias}");
            assert!(ok(Some(&format!("{alias}[1m]")), None).is_ok(), "{alias}[1m]");
        }
        for level in EFFORTS {
            assert!(ok(None, Some(level)).is_ok(), "{level}");
        }
        assert!(ok(Some("claude-opus-5"), None).is_err(), "an alias follows the upgrade, a full name does not");
        assert!(ok(Some("opus[2m]"), None).is_err(), "only the window Claude Code spells");
        assert!(ok(None, Some("higher")).is_err(), "an unknown effort is warned about and ignored");

        assert!(of("codex").tier(Some("opus"), None).is_err(), "Plain::args would send nothing on");
        assert!(of("codex").tier(None, None).is_ok(), "and an unflagged spawn on any kind is untouched");
    }

    /// Accepting the words and being able to walk away are two different
    /// questions, and haiku answers them differently.
    ///
    /// It is a real alias — `tier` takes it, and has to, because a person at a
    /// pane can use it — so the second question cannot be folded into the
    /// first. A kind wsp has not measured says nothing rather than guessing,
    /// which is what `Plain` does everywhere else it is asked something only a
    /// measurement could answer.
    #[test]
    fn a_tier_the_runtime_accepts_can_still_be_one_nobody_can_leave_alone() {
        assert!(of("claude").tier(Some("haiku"), None).is_ok(), "haiku is a model claude takes");

        let why = of("claude").unattended(Some("haiku")).expect("and one that stops at the first prompt");
        assert!(why.contains("manual mode"), "the sentence says what is missing: {why}");
        assert!(of("claude").unattended(Some("haiku[1m]")).is_some(), "a bigger window is the same tier");

        assert!(of("claude").unattended(Some("sonnet")).is_none(), "and the others are left alone");
        assert!(of("claude").unattended(None).is_none(), "as is a spawn that states no tier at all");
        assert!(of("codex").unattended(Some("haiku")).is_none(), "a kind nobody has measured says nothing");
    }

    /// Two ways back to the whole preamble, and both are needed.
    ///
    /// `--full` is the person's: a trim is a capability change, and the agent
    /// that needs the design MCP server to draw an artefact is a spawn on this
    /// backlog rather than a hypothesis. The kind is the machine's: these are
    /// Claude Code's flag spellings, and handing them to `codex` or `gemini`
    /// buys a workspace with a shell in it and no agent.
    #[test]
    fn the_trim_is_claude_codes_alone_and_one_flag_undoes_it() {
        let seat = Seat::new("w2J:p1");
        assert_eq!(
            of("claude").args(&spawn(true, "t-1", &seat)),
            vec!["-n", "t-1-w2J:p1"],
            "--full is the way back from the trim, and from nothing else"
        );
        for kind in ["codex", "gemini", "nonesuch", ""] {
            assert!(
                of(kind).args(&spawn(false, "t-1", &seat)).is_empty(),
                "{kind} has no notion of a handle and gets no flags of wsp's"
            );
        }
    }

    /// **The change this task exists for.** wsp names the agent; it does not
    /// wait to be told what the agent called itself.
    ///
    /// Both halves are asserted because both are load-bearing. The task id is
    /// what a person reads off a failed spawn, and the seat is what stops two
    /// agents on one task from answering to one name — Claude Code accepts a
    /// duplicate `-n` in silence, which is measured in the module docs.
    #[test]
    fn wsp_names_a_claude_after_the_task_and_the_seat_it_is_started_in() {
        let seat = Seat::new("w2J:p1");
        let argv = of("claude").args(&spawn(false, "t-260817-014", &seat));
        let at = argv.iter().position(|a| a == "-n").unwrap_or_else(|| panic!("unnamed: {argv:?}"));
        assert_eq!(argv.get(at + 1).map(String::as_str), Some("t-260817-014-w2J:p1"));
        // And it is the same string the recovery path will recompute, from a
        // census row rather than from anything wsp had to remember.
        assert_eq!(mint("t-260817-014", &seat).as_deref(), Some("t-260817-014-w2J:p1"));
    }

    /// A handle is built from what there is, and never from nothing.
    ///
    /// The empty cases are real: `spawn` on a project passes the project id and
    /// `unwrap_or_default`s to `""` when there is neither, and a `Seated` the
    /// backend could only half read has an empty agent name. `-n ""` would set
    /// a session's display name to the empty string, which is worse than
    /// letting it derive one.
    #[test]
    fn a_handle_with_no_halves_is_not_minted_and_no_flag_is_passed() {
        assert_eq!(mint("t-1", &Seat::new("")).as_deref(), Some("t-1"));
        assert_eq!(mint("", &Seat::new("w1:p1")).as_deref(), Some("w1:p1"));
        assert_eq!(mint("  ", &Seat::new("  ")), None);
        assert_eq!(mint("", &Seat::new("")), None);
        let nowhere = Seat::new("");
        let argv = of("claude").args(&spawn(false, "", &nowhere));
        assert!(!argv.contains(&"-n".to_string()), "nothing to name it after: {argv:?}");
        assert_eq!(argv, TRIM, "an unnameable agent is still a trimmed one");
    }

    /// **The second opinion robustness-041 needed.** A minted handle on the
    /// runtime's own census answers "is this agent alive" without a pane, a
    /// render or a backend.
    ///
    /// The two rows that matter are the two the lookup cannot use: an agent
    /// alone under a name nobody else answers to, and a *third* agent in a tree
    /// two colleagues are already standing in — where [`pick`] correctly refuses
    /// and a minted name is still exact. That is the seat this is asked about, by
    /// construction: one whose agent the backend has lost sight of.
    #[test]
    fn a_minted_handle_is_matched_against_the_runtimes_own_census_and_nothing_else() {
        let mut live = parse_listing(CAPTURE);
        live.push(Live {
            session: "d28a4237-8811-42ef-b89e-34ffc2d0df9f".into(),
            name: "t-260817-010-w2C:p1".into(),
            cwd: "/Users/edjames/claude/wsp".into(),
            status: "idle".into(),
        });
        assert!(answers_to(&live, "t-260817-010-w2C:p1"));
        // The seat is what makes it exact. The same task in a different seat is
        // a different agent, and saying otherwise would keep a spawn waiting on
        // an agent that is somebody else's.
        assert!(!answers_to(&live, "t-260817-010-w2D:p1"));
        assert!(!answers_to(&live, "t-260817-010"), "the task id alone is not a handle");
        assert!(!answers_to(&[], "t-260817-010-w2C:p1"), "a listing wsp could not take");
        // And a kind with no runtime says nothing rather than no.
        let seat = Seat::new("w2C:p1");
        assert_eq!(of("codex").running(&spawn(false, "t-260817-010", &seat)), None);
    }

    /// A backend that records what it was asked, so the fallback can be caught
    /// doing what it says it does.
    struct Typed {
        said: std::cell::RefCell<Vec<(Seat, String)>>,
        seats: Vec<Seated>,
    }

    impl Place for Typed {
        fn tell(&self, seat: &Seat, text: &str) -> Result<Delivery> {
            self.said.borrow_mut().push((seat.clone(), text.to_string()));
            Ok(Delivery::Started)
        }
        fn census(&self) -> Result<crate::place::Census> {
            Ok(crate::place::Census::heard("", self.seats.clone()))
        }
        fn open(&self, _: &Order) -> Result<Seat> {
            panic!("telling an agent does not open seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> Result<()> {
            panic!("telling an agent does not start one")
        }
        fn stop(&self, _: &Seat) -> Result<()> {
            panic!("telling an agent does not end it")
        }
        fn state(&self, _: &Seat) -> Result<State> {
            panic!("readiness is the caller's question")
        }
        fn watch(&self, _: &mut dyn FnMut(Event) -> bool) -> Result<()> {
            panic!("telling an agent does not wait")
        }
        fn here(&self) -> Option<Seat> {
            panic!("telling an agent is about the seat it is in, not ours")
        }
    }

    /// Until the channel exists, every kind delivers the way the backend can —
    /// and the seam is what makes that one line to change rather than a call
    /// site to find.
    ///
    /// Asserted for `claude` as well as for an unknown kind, because the
    /// interesting claim is that the trim moving into this layer did not quietly
    /// move delivery with it.
    #[test]
    fn every_kind_still_delivers_through_the_backend_and_says_so() {
        for kind in ["claude", "codex", "nonesuch"] {
            let place = Typed { said: std::cell::RefCell::new(Vec::new()), seats: Vec::new() };
            of(kind).tell(&place, &Seat::new("w1:p1"), "go").expect("delivered");
            assert_eq!(
                place.said.borrow().as_slice(),
                &[(Seat::new("w1:p1"), "go".to_string())],
                "{kind} did not deliver through the backend"
            );
        }
    }

    /// A kind that has no notion of a handle says so, and the seat is never
    /// offered as one.
    ///
    /// A `Seat` is what the backend calls the place and a handle is what the
    /// runtime calls the occupant. Returning the first where the second was
    /// asked for would produce a sentence telling somebody to send a work order
    /// to `w1:p1`, which nothing can receive.
    #[test]
    fn a_kind_with_no_runtime_of_its_own_has_no_handle_to_offer() {
        let place = Typed {
            said: std::cell::RefCell::new(Vec::new()),
            seats: vec![Seated {
                seat: Seat::new("w1:p1"),
                cwd: "/Users/edjames/claude/wsp".into(),
                session: "7a188ba8-7ca6-4743-921f-35fcc7079c11".into(),
                ..Seated::default()
            }],
        };
        let seat = Seat::new("w1:p1");
        assert_eq!(of("codex").address(&place, &spawn(false, "t-260817-011", &seat)), None);
        assert_eq!(recovery(None), None);
    }

    /// The two sentences say different things, and the hedged one says its hedge
    /// first.
    ///
    /// Asserted on the wording because the wording is the whole deliverable here:
    /// a person reading a failed spawn at midnight acts on the first clause, and
    /// "it is reachable as X" about an agent that never started would send them
    /// looking for something that is not there.
    #[test]
    fn a_handle_the_runtime_would_not_confirm_is_offered_as_one_that_may_not_exist() {
        let sure = recovery(Some(Address::Confirmed("wsp-f3".into()))).expect("a sentence");
        assert!(sure.contains("wsp-f3") && sure.contains("reachable"), "{sure}");

        let hedged = recovery(Some(Address::Unconfirmed("wsp-f3".into()))).expect("a sentence");
        assert!(hedged.contains("wsp-f3"), "a hedge with no name in it is silence: {hedged}");
        assert!(!hedged.contains("is reachable"), "it is not reachable — the point: {hedged}");
        let doubt = hedged.find("nothing is answering").expect("the hedge, up front");
        assert!(doubt < hedged.find("wsp-f3").expect("the name"), "the hedge is last: {hedged}");
    }

    /// One assistant line, as Claude Code 2.1.234 writes it — the fields this
    /// reads and nothing else, so the test says what the coupling actually is.
    ///
    /// A *line*, not a turn: the two are not the same thing and that is the
    /// defect `core-029` found. `id` is `message.id`, the id of the API
    /// response, and every line of one response carries the same one.
    fn line(id: &str, model: &str, effort: &str, sidechain: bool, at: &str, usage: &str) -> String {
        let stamp = match at.is_empty() {
            true => String::new(),
            false => format!(r#""timestamp":"{at}","#),
        };
        format!(
            r#"{{"type":"assistant","isSidechain":{sidechain},"effort":"{effort}",{stamp}"message":{{"id":"{id}","model":"{model}","role":"assistant","usage":{usage},"content":[]}}}}"#
        )
    }

    /// One turn, with an id of its own so that a test which says nothing about
    /// requests gets one request per call.
    fn turn(model: &str, effort: &str, sidechain: bool) -> String {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        line(&format!("msg_{n}"), model, effort, sidechain, "", "{}")
    }

    fn ran_of(lines: &[String]) -> Option<Ran> {
        read_ran(&mut std::io::Cursor::new(lines.join("\n")), 0)
    }

    /// The ordinary session: one tier, and the turn count is what says how much
    /// of it there was.
    #[test]
    fn a_transcript_says_what_served_it_and_how_many_turns_it_took() {
        let ran = ran_of(&[
            turn("claude-opus-5", "high", false),
            r#"{"type":"user","message":{"role":"user","content":"go on"}}"#.into(),
            turn("claude-opus-5", "high", false),
        ])
        .expect("three lines, two of them turns");
        assert_eq!(ran.models, ["opus-5"], "the runtime's prefix is not part of the tier");
        assert_eq!(ran.efforts, ["high"]);
        assert_eq!(ran.turns, 2);
        assert_eq!(ran.label(), "opus-5/high");
    }

    /// The case the whole field exists for. A `/model` mid-session leaves both
    /// names in the file, in order, and a record that kept only the flag it was
    /// spawned under would name the tier the session was escalated *away* from.
    #[test]
    fn a_session_that_changed_tier_under_itself_is_both_tiers_in_order() {
        let ran = ran_of(&[
            turn("claude-haiku-4-5", "medium", false),
            turn("claude-opus-5", "high", false),
            turn("claude-opus-5", "high", false),
        ])
        .expect("three turns");
        assert_eq!(ran.label(), "haiku-4-5→opus-5/medium→high");
        assert_eq!(ran.turns, 3, "every turn counts, whichever model took it");
    }

    /// A sub-agent writes to the same file and can run on a model the session
    /// never chose. The question this answers is what the *claimant* ran at, so
    /// a task whose agent spawned one search must not be reported as having run
    /// on whatever that search used.
    #[test]
    fn a_sidechain_is_a_different_agent_and_is_not_counted() {
        let ran = ran_of(&[
            turn("claude-opus-5", "high", false),
            turn("claude-haiku-4-5", "low", true),
        ])
        .expect("one turn on the main thread");
        assert_eq!(ran.models, ["opus-5"]);
        assert_eq!(ran.turns, 1);
    }

    /// A tool result can quote JSON, so the cheap `contains` that keeps the
    /// parse off nine lines in ten is a gate and never the test.
    #[test]
    fn a_line_that_merely_quotes_a_turn_is_not_one() {
        let quoted = r#"{"type":"user","message":{"role":"user","content":"I saw {\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\"}} in the log"}}"#;
        let ran = ran_of(&[quoted.into(), turn("claude-opus-5", "high", false)]).expect("one turn");
        assert_eq!(ran.models, ["opus-5"], "somebody's pasted output is not evidence of a tier");
    }

    /// Empty is not zero. An agent that started and was killed before it
    /// answered leaves a transcript that is not evidence of a tier, and the
    /// record of that has to be *nothing* rather than a label nobody earned.
    #[test]
    fn a_transcript_with_no_turns_in_it_is_no_answer_rather_than_an_empty_one() {
        assert!(ran_of(&[r#"{"type":"summary","summary":"a session that never ran"}"#.into()]).is_none());
    }

    /// An older runtime wrote no `effort` at all, and the honest label for that
    /// is the model on its own — not the default effort it might have been.
    #[test]
    fn a_transcript_that_records_no_effort_is_labelled_by_its_model_alone() {
        let ran = read_ran(
            &mut std::io::Cursor::new(
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","role":"assistant"}}"#,
            ),
            0,
        )
        .expect("one turn");
        assert_eq!(ran.label(), "sonnet-5");
    }

    /// A cancelled turn is not a tier. The runtime composes messages of its own
    /// into the transcript and names the model `<synthetic>`; counted, it puts
    /// a model nobody ran in the label and a turn nobody took in the count.
    #[test]
    fn a_message_the_runtime_wrote_itself_is_not_a_turn() {
        let ran = ran_of(&[
            turn("claude-opus-5", "high", false),
            turn("<synthetic>", "high", false),
        ])
        .expect("one real turn");
        assert_eq!(ran.models, ["opus-5"]);
        assert_eq!(ran.turns, 1);
    }

    /// The gate matches a bare word rather than the punctuation around it, and
    /// this is the failure it is written against: a gate spelling out
    /// `"type":"assistant"` reads a pretty-printed line as not a turn, comes
    /// back with nothing, and looks exactly like a session that never ran.
    #[test]
    fn a_turn_written_with_spaces_in_it_is_still_a_turn() {
        let ran = read_ran(
            &mut std::io::Cursor::new(
                r#"{"type": "assistant", "effort": "max", "message": {"model": "claude-fable-5"}}"#,
            ),
            0,
        )
        .expect("whitespace is not a format change");
        assert_eq!(ran.label(), "fable-5/max");
    }

    /// The directory rule, which is the one thing here that is a guess about
    /// somebody else's layout — hence the scan behind it in [`transcript`].
    #[test]
    fn a_cwd_becomes_a_directory_name_the_way_claude_code_spells_one() {
        assert_eq!(mangle("/Users/ed/claude/wsp/.worktrees/wsp-060"), "-Users-ed-claude-wsp--worktrees-wsp-060");
        assert_eq!(mangle("/Users/ed/.local/state"), "-Users-ed--local-state");
    }

    /// A session id arrives from herdr, which got it from the agent, and
    /// `spawn` is a verb agents drive. One with a path separator in it is not a
    /// session id, and refusing it is cheaper than finding out what it opened.
    #[test]
    fn a_session_id_that_is_not_one_never_reaches_the_filesystem() {
        assert!(transcript("../../etc/passwd", "/tmp").is_none());
        assert!(transcript("", "/tmp").is_none());
    }

    /// The same, one layer along: an opencode id reaches a command line rather
    /// than a path, and the check is why a session that is really an argument
    /// never gets there.
    #[test]
    fn a_session_id_that_is_not_one_never_reaches_a_command_line() {
        assert!(export("--help").is_none());
        assert!(export("ses_a b").is_none());
        assert!(export("").is_none());
    }

    /// The defect this row found, and the reason every token total in wsp was
    /// nearly three times the truth: Claude Code writes a line per content
    /// block, each carrying the *same* `usage`, and a fork writes some of them
    /// out twice. One `message.id` is one API response, one bill and one turn.
    ///
    /// Measured against the largest transcript on this machine, 2026-08-22:
    /// 2,045 assistant lines, 692 distinct ids, and 673M cache-read tokens
    /// counted where 242M were read.
    #[test]
    fn two_lines_of_one_response_are_one_request_and_one_bill() {
        let usage = r#"{"input_tokens":10,"cache_read_input_tokens":900,"output_tokens":50}"#;
        let ran = ran_of(&[
            line("msg_a", "claude-opus-5", "high", false, "", usage),
            line("msg_a", "claude-opus-5", "high", false, "", usage),
            line("msg_b", "claude-opus-5", "high", false, "", usage),
        ])
        .expect("two responses over three lines");
        assert_eq!(ran.turns, 2, "a thinking block and the tool call it decided on are one turn");
        assert_eq!(ran.spend.cache, 1_800, "and one bill, not one per line");
        assert_eq!(ran.spend.input, 20);
        assert_eq!(ran.spend.output, 100);
    }

    /// Cache write is counted with input and cache read is kept apart, because
    /// the first pair differ in price by a quarter and the second by an order
    /// of magnitude. Folding all three would price an attempt about ten times
    /// too high, which is the one error a routing argument cannot survive.
    #[test]
    fn what_the_model_read_for_the_first_time_is_not_what_it_read_from_cache() {
        let ran = ran_of(&[line(
            "msg_a",
            "claude-opus-5",
            "high",
            false,
            "",
            r#"{"input_tokens":7,"cache_creation_input_tokens":2000,"cache_read_input_tokens":90000,"output_tokens":300}"#,
        )])
        .expect("one turn");
        assert_eq!(ran.spend.input, 2_007, "cache write is input the model had not read before");
        assert_eq!(ran.spend.cache, 90_000);
        assert_eq!(ran.spend.micros, None, "Claude Code publishes no price and wsp invents none");
    }

    /// One session serves several attempts — an agent finishes a task and is
    /// handed the next from the same seat — so a reading that starts at the
    /// session start bills the second attempt for the first one's work.
    #[test]
    fn a_transcript_is_read_from_the_claim_and_not_from_the_start_of_the_session() {
        let usage = r#"{"input_tokens":100,"output_tokens":10}"#;
        let lines = [
            line("msg_a", "claude-haiku-4-5", "low", false, "2026-08-22T09:00:00Z", usage),
            line("msg_b", "claude-opus-5", "high", false, "2026-08-22T11:00:00Z", usage),
        ];
        let whole = read_ran(&mut std::io::Cursor::new(lines.join("\n")), 0).expect("both");
        assert_eq!(whole.turns, 2);
        let second = read_ran(
            &mut std::io::Cursor::new(lines.join("\n")),
            util::epoch_of("2026-08-22T10:00:00Z"),
        )
        .expect("the attempt that claimed at ten");
        assert_eq!(second.turns, 1, "the earlier attempt's turns are not this one's");
        assert_eq!(second.spend.input, 100);
        assert_eq!(second.models, ["opus-5"], "nor is the tier it ran at");
    }

    /// A line wsp cannot date is counted rather than dropped. Losing evidence
    /// is the worse of the two errors, and the tier is right either way.
    #[test]
    fn a_turn_with_no_stamp_on_it_is_counted_into_whatever_attempt_is_asking() {
        let ran = read_ran(
            &mut std::io::Cursor::new(turn("claude-opus-5", "high", false)),
            util::epoch_of("2026-08-22T10:00:00Z"),
        )
        .expect("undated is not undone");
        assert_eq!(ran.turns, 1);
    }

    /// opencode's export, as opencode 1.18.21 writes it — the fields this
    /// reads, in the shape it reads them from.
    fn export_of(messages: &str) -> Vec<u8> {
        format!(r#"{{"info":{{"id":"ses_x"}},"messages":[{messages}]}}"#).into_bytes()
    }

    fn message(model: &str, cost: &str, at: i64, tokens: &str) -> String {
        format!(
            r#"{{"info":{{"role":"assistant","providerID":"opencode","modelID":"{model}","cost":{cost},"time":{{"created":{at}}},"tokens":{tokens}}},"parts":[]}}"#
        )
    }

    /// What an opencode attempt cost, which is `core-029` itself: the numbers
    /// are published per message rather than derived, and they arrive in the
    /// units the Claude column is already in.
    #[test]
    fn an_opencode_attempt_reports_its_cost_in_the_units_the_other_kind_reports() {
        let tokens = r#"{"input":300,"output":40,"reasoning":10,"cache":{"read":9000,"write":200}}"#;
        let ran = read_export(
            &export_of(&[
                message("claude-sonnet-5", "0.012", 1_787_327_482_000, tokens),
                message("claude-sonnet-5", "0.008", 1_787_327_500_000, tokens),
            ]
            .join(",")),
            0,
        )
        .expect("two turns");
        assert_eq!(ran.turns, 2);
        assert_eq!(ran.spend.cache, 18_000);
        assert_eq!(ran.spend.input, 1_000, "input and cache write, as on the other side");
        assert_eq!(
            ran.spend.output, 100,
            "reasoning is beside output here and inside it there, so the column adds them"
        );
        assert_eq!(ran.spend.micros, Some(20_000), "$0.02, kept as an integer");
        assert_eq!(
            ran.spend.clause().as_deref(),
            Some("18k cache 1.0k in 100 out $0.02"),
            "and the clause that lands on the release line says all of it"
        );
    }

    /// A model with a separator in it and no effort half is the one label
    /// `cmd_attempts::agrees` cannot parse — it splits a tier at its last `/`,
    /// and `opencode/x-preview-f-free` alone reads as the provider at
    /// `x-preview-f-free` effort. This kind is the one that produces that
    /// shape, so it states the half it does not have.
    #[test]
    fn an_opencode_tier_states_the_effort_it_has_not_got() {
        let ran = read_export(
            &export_of(&message("x-preview-f-free", "0", 1_787_327_482_000, "{}")),
            0,
        )
        .expect("one turn");
        assert_eq!(ran.label(), "opencode/x-preview-f-free/default");
        assert_eq!(
            ran.spend.micros,
            Some(0),
            "a free model cost nothing, which is a fact and not a missing reading"
        );
    }

    /// The window again, on the kind whose sessions outlive an attempt just as
    /// readily — opencode stamps in milliseconds and the claim is in seconds.
    #[test]
    fn an_opencode_session_is_read_from_the_claim_too() {
        let tokens = r#"{"input":100,"output":10,"cache":{"read":0,"write":0}}"#;
        let src = export_of(
            &[
                message("x-preview-f-free", "0", 1_787_327_000_000, tokens),
                message("x-preview-f-free", "0", 1_787_328_000_000, tokens),
            ]
            .join(","),
        );
        let ran = read_export(&src, 1_787_327_500).expect("the second message only");
        assert_eq!(ran.turns, 1);
        assert_eq!(ran.spend.input, 100);
    }

    /// The property `core-039` cost a row to learn: what these runtimes write
    /// must not land in a pipe. They are Bun binaries, they exit without
    /// draining one they have filled, and the reader is left with 64KiB of a
    /// 165KB export under a successful exit status — a truncated document is a
    /// missing clause and nothing says so. Asserted on the *child's* end of the
    /// handle rather than on the size of what came back, because a capture that
    /// happens to fit tells nothing apart.
    #[test]
    fn what_a_runtime_writes_never_goes_into_a_pipe() {
        let out = captured(
            PathBuf::from("/bin/sh"),
            &["-c", "if [ -p /dev/stdout ]; then echo pipe; else echo file; fi"],
        )
        .expect("/bin/sh");
        assert_eq!(String::from_utf8_lossy(&out).trim(), "file");
    }

    /// Everything the child wrote, and only that. The whole reason for the file
    /// is that the last byte arrives, so the test that matters is the tail.
    #[test]
    fn a_capture_is_every_byte_the_child_wrote() {
        let out = captured(
            PathBuf::from("/bin/sh"),
            &["-c", "i=0; while [ $i -lt 4000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done; echo tail"],
        )
        .expect("/bin/sh");
        assert_eq!(out.len(), 4000 * 41 + 5, "a capture that stopped at a buffer boundary");
        assert!(String::from_utf8_lossy(&out).ends_with("tail\n"), "the last line never arrived");
    }

    /// A child that failed is an error naming the command the way a person
    /// would type it — these strings reach somebody inside a refusal about
    /// their `--model`, not about where wsp found the binary — and it leaves
    /// nothing behind, because the capture nobody could read is exactly the one
    /// whose leftovers nobody comes back for.
    ///
    /// Looked for by what this child wrote rather than by counting the
    /// directory: temp is shared with every other test in the run and with
    /// every other wsp on the machine, and a count is a test that fails for
    /// somebody else's reasons.
    #[test]
    fn a_failed_capture_says_what_was_run_and_keeps_no_file() {
        let mark = format!("half-{}-{:?}", std::process::id(), std::thread::current().id());
        let e = captured(PathBuf::from("/bin/sh"), &["-c", &format!("echo {mark}; exit 3")])
            .expect_err("exit 3");
        assert!(e.contains("sh -c"), "it named a path instead of the command — {e}");
        assert!(e.ends_with("failed"), "{e}");
        let left: Vec<PathBuf> = std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("wsp-captured-"))
                    && std::fs::read_to_string(p).is_ok_and(|s| s.contains(&mark))
            })
            .collect();
        assert!(left.is_empty(), "a temp file outlived the capture: {left:?}");
    }

    /// An export with nothing that served a turn is no answer rather than an
    /// empty one, the same rule [`read_ran`] keeps: it must not become "ran at
    /// nothing, for nothing".
    #[test]
    fn an_export_with_no_assistant_turn_in_it_is_no_answer() {
        let user = r#"{"info":{"role":"user","time":{"created":1}},"parts":[]}"#;
        assert!(read_export(&export_of(user), 0).is_none());
        assert!(read_export(b"not json", 0).is_none());
    }

    /// The clause is the only durable copy of any of this — a transcript is one
    /// machine's disk — so what is written has to read back. It round-trips to
    /// the three significant figures it prints, which is the precision the
    /// record claims and no more.
    #[test]
    fn what_an_attempt_cost_reads_back_off_the_line_it_was_written_on() {
        let spend = Spend { cache: 242_000_000, input: 4_700_000, output: 875_000, micros: Some(1_234_567) };
        let clause = spend.clause().expect("something to say");
        assert_eq!(clause, "242M cache 4.7M in 875k out $1.23");
        let back = Spend::of_clause(&clause).expect("its own inverse");
        assert_eq!(back.cache, 242_000_000);
        assert_eq!(back.input, 4_700_000);
        assert_eq!(back.output, 875_000);
        assert_eq!(back.micros, Some(1_230_000), "to the figures the clause printed");
    }

    /// A session that was started and killed before it answered has no spend,
    /// and the record of that is no clause at all rather than a line of
    /// zeroes claiming a measurement nobody took.
    #[test]
    fn nothing_measured_is_no_clause_and_not_a_row_of_zeroes() {
        assert!(Spend::default().clause().is_none());
        assert!(Spend { micros: Some(0), ..Spend::default() }.clause().is_some());
    }

    /// The log is prose people write in. A clause that is somebody's sentence
    /// must not come back as a measurement, however many numbers are in it.
    #[test]
    fn a_sentence_in_the_log_is_not_a_measurement() {
        assert!(Spend::of_clause("workspace closed").is_none());
        assert!(Spend::of_clause("96 turns").is_none());
        assert!(Spend::of_clause("ran opus-5/high").is_none());
        assert!(Spend::of_clause("").is_none());
        assert!(Spend::of_clause("12k cache and a note about it").is_none());
    }
}
