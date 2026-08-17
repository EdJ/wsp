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
//! # The next tenants
//!
//! Named rather than absorbed, because the layer is worth more than any one of
//! them and a first pass that took all five would have been unreviewable:
//!
//! - **t-260817-010, readiness.** `wait_ready` declares an agent dead while it
//!   is alive, because herdr's claude detection needs a rendered pane and the
//!   focus decision forbids rendering one. `claude agents --json` answers
//!   liveness and `idle`/`busy` per session with no pane involved at all, which
//!   is a per-kind reading of exactly the sort this trait is for.
//! - ~~**t-260817-006, environment hygiene.**~~ Refuted, and worth keeping as
//!   the one prediction on this list that was wrong. The guess was that which
//!   variables a kind must not inherit is a fact about that kind. It is not:
//!   what leaks is `CLAUDE_CODE_CHILD_SESSION` and the rest of the *caller's*
//!   session identity, which is wrong to pass on whatever is about to be
//!   started in the seat — including a bare shell, which has no kind at all.
//!   The rule went to [`crate::place::shed`], beside `SEAT_ENV`, where it is
//!   asked once per seat rather than once per agent. A kind-shaped question and
//!   a spawn-shaped one look alike from here; the test is whether the answer
//!   changes when nothing is started.
//! - **t-260816-068, `--resume`.** Recording a session id against a task so an
//!   agent can be picked up again. The same field this file already reads.
//! - **t-260816-088, `--model` and `--effort`.** More of [`Kind::args`], and the
//!   reason that verb takes a description of the spawn rather than a bare flag
//!   when it grows.
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
//! by the time this ran t-260815-022 had given every task a worktree named after
//! it and Claude Code's own derived name had quietly become **almost right**:
//!
//! - A derived name is `<basename of cwd>-<two hex digits>`, stamped
//!   `"nameSource":"derived"`. Standing in `…/.worktrees/t-260817-014`, an agent
//!   calls itself `t-260817-014-f6` with nobody asking it to. So the task id was
//!   already the prefix, and minting looked redundant.
//! - **The suffix is not stable.** Two sessions started in one directory,
//!   seconds apart, were `nametest-45` and `nametest-4d`. So the handle is still
//!   unknowable until after the agent exists, which is the whole of what the
//!   lookup was for — a name that is 90% predictable buys nothing, because the
//!   10% is the part you have to address it by.
//! - And the prefix agrees with the task id only because a *path convention*
//!   another task owns says so. wsp would be inferring its addressing scheme
//!   from the shape of somebody else's directory name, and t-260815-022 is free
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
//! is still idle is two live sessions called `t-260817-014`.
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

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::place::{Place, Refusal, Result, Seat, Seated};
use crate::util;

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
    /// somebody else's agent.
    fn address(&self, place: &dyn Place, seat: &Seat) -> Option<String>;

    /// Give the agent in a seat a sentence to act on.
    ///
    /// Not `Place::tell` renamed. The port's verb is what the *backend* can do
    /// about it; this one chooses between that and whatever the agent itself
    /// offers, and typing at a terminal is only the fallback.
    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<()>;

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
}

/// What is about to be started, as much of it as a kind is allowed to know.
///
/// Three fields and no store, no `Args`, no task: a kind decides flags, and a
/// kind that could read the task would start deciding other things. `full` was
/// the whole of this parameter before minting needed the other two.
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

    fn address(&self, _place: &dyn Place, _seat: &Seat) -> Option<String> {
        None
    }

    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<()> {
        place.tell(seat, text)
    }

    /// Nothing to ask. An agent wsp knows nothing about beyond its name is one
    /// whose backend is the only witness there is.
    fn running(&self, _spawn: &Spawn) -> Option<bool> {
        None
    }
}

/// Claude Code.
pub struct Claude;

/// What a spawned Claude Code session is *not* given, and why each name is on
/// the list.
///
/// Every request re-reads the whole context, so a token present before the
/// agent has done anything is paid once per request — ~102 times in the session
/// t-260816-096 measured. The preamble is the largest single thing in that
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

impl Kind for Claude {
    /// The trim, and the name.
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
        argv
    }

    /// What wsp called it, confirmed alive — or, for an agent wsp did not start,
    /// what the runtime called it.
    ///
    /// The mint is tried first and it is still *checked* against the listing,
    /// which is not a wasted call. This is asked on the failure path, where the
    /// open question is whether the agent that would not take a work order is
    /// even running: an unconfirmed handle would put "it is reachable as X" on
    /// stderr for a session that never started, and the trait's rule is that a
    /// wrong handle is worse than none.
    ///
    /// What minting buys here is that the answer is **exact** where [`pick`] can
    /// only be probable. `pick` needs herdr to have seen the session id — which
    /// is the whole of t-260817-010, and it often has not — and then falls back
    /// to whoever is alone in the tree. A minted name is matched against itself.
    fn address(&self, place: &dyn Place, seat: &Seat) -> Option<String> {
        let row = place.census().ok()?.into_iter().find(|s| &s.seat == seat)?;
        resolve(&listing().ok()?, &row)
    }

    /// Typed at the terminal, because there is nothing else yet.
    ///
    /// **This is where the session channel lands**, and the module docs say
    /// exactly what is missing: an address is now available for every Claude
    /// Code wsp started, and no supported way to send on the socket it names.
    /// Guessing at the protocol of an authenticated private socket is not a
    /// thing to do inside a spawn verb, so this delegates, and the failure path
    /// in `cmd_spawn` reports the address instead.
    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<()> {
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
}

/// Whether any live session goes by this name.
///
/// A line of its own so the order above can be argued about in a test without
/// shelling out, which is the same seam [`resolve`] is split on.
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

/// The handle for one seated agent, given the runtime's whole census.
///
/// Split from [`Kind::address`] for the reason the handbook gives about seams:
/// `address` has to shell out for the listing, so anything decided inside it is
/// decided where no test can reach. What is interesting is the *order*, and the
/// order is here, taking the listing as an argument like [`pick`] does.
///
/// Minted first, and only if the runtime confirms a session by that name. Then
/// [`pick`], unchanged, for every agent wsp did not start — a person's own
/// `claude` in a pane, an agent from before this shipped, one restarted by hand.
/// That path is byte-identical to what it was, which is why demoting it is a
/// cheap change rather than a risky one.
fn resolve(live: &[Live], row: &Seated) -> Option<String> {
    let minted = mint(&row.agent.name, &row.seat).filter(|h| live.iter().any(|s| &s.name == h));
    minted.or_else(|| pick(live, &row.session, &row.cwd))
}

/// Which live session is the one in this seat.
///
/// Two keys, and the order is the point. The session id is **exact**: the
/// backend saw the agent and said which session it is, and no two sessions share
/// one. The cwd is a fallback for the case that matters most — herdr failing to
/// detect a live agent is the whole of t-260817-010, and it leaves a seat with a
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
    let out = Command::new(claude_bin())
        .args(["agents", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("claude agents --json: {e}"))?;
    if !out.status.success() {
        return Err("claude agents --json failed".into());
    }
    Ok(parse_listing(&String::from_utf8_lossy(&out.stdout)))
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
pub fn recovery(handle: Option<String>) -> Option<String> {
    handle.map(|h| format!("it is reachable as `{h}` — the work order can be sent there by hand"))
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
    /// `7a188ba8-…` and `wsp-f3` are the exact pair recorded on t-260817-011 as
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
    /// is the whole of t-260817-010 — and one live session in that directory is
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

    /// A seat, as the backend reports one, for arguing about [`resolve`].
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
    /// its session id is empty — which is t-260817-010's blind spot, herdr
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
        let row = seated("w2J:p1", "t-260817-014", "", "/Users/edjames/claude/wsp");
        assert_eq!(pick(&live, &row.session, &row.cwd), None, "three agents in one tree");
        assert_eq!(resolve(&live, &row).as_deref(), Some("t-260817-014-w2J:p1"));
    }

    /// An agent wsp did not start is still found the way it always was.
    ///
    /// The demotion has to be a demotion and not a replacement: `wsp-f3` was
    /// never minted by anything, and the pair below is the one t-260817-011
    /// recorded as underivable. It still resolves.
    #[test]
    fn an_agent_wsp_did_not_name_falls_back_to_the_census_lookup() {
        let live = parse_listing(CAPTURE);
        let row = seated("w1:p1", "t-260817-011", "7a188ba8-7ca6-4743-921f-35fcc7079c11", "");
        assert_eq!(resolve(&live, &row).as_deref(), Some("wsp-f3"), "the lookup still answers");
    }

    /// A handle wsp minted for an agent that is not running is not offered.
    ///
    /// This is the whole reason `resolve` checks the listing instead of trusting
    /// its own arithmetic. It is asked on the failure path, where "the agent
    /// never started" is a live possibility — and `recovery` would otherwise
    /// print "it is reachable as t-260817-014-w2J:p1" about nothing at all.
    #[test]
    fn a_name_wsp_minted_for_an_agent_that_never_started_is_not_reported_as_reachable() {
        let live = parse_listing(CAPTURE);
        let row = seated("w2J:p1", "t-260817-014", "", "/Users/edjames/claude/wsp");
        assert_eq!(resolve(&live, &row), None, "minted, and nothing answers to it");
        // The seat being empty of everything is the same answer, not a panic.
        assert_eq!(resolve(&[], &row), None);
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
        Spawn { full, name, seat }
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

    /// **The second opinion t-260817-010 needed.** A minted handle on the
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
        fn tell(&self, seat: &Seat, text: &str) -> Result<()> {
            self.said.borrow_mut().push((seat.clone(), text.to_string()));
            Ok(())
        }
        fn census(&self) -> Result<Vec<Seated>> {
            Ok(self.seats.clone())
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
        assert_eq!(of("codex").address(&place, &Seat::new("w1:p1")), None);
        assert_eq!(recovery(None), None);
        assert!(recovery(Some("wsp-f3".into())).unwrap().contains("wsp-f3"));
    }
}
