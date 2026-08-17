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
//! - **t-260817-006, environment hygiene.** `CLAUDE_CODE_*` leaking into a
//!   spawned pane disables its transcript. Which variables a kind must not
//!   inherit is a fact about that kind.
//! - **t-260816-068, `--resume`.** Recording a session id against a task so an
//!   agent can be picked up again. The same field this file already reads.
//! - **t-260816-088, `--model` and `--effort`.** More of [`Kind::args`], and the
//!   reason that verb takes a description of the spawn rather than a bare flag
//!   when it grows.
//!
//! Two unverified leads on addressing, both from `claude --help` and neither run
//! live, because spawning a Claude Code to test them was outside what this
//! session was permitted: `-n, --name <name>` sets a session's display name, and
//! `--session-id <uuid>` fixes its id. If `-n` populates the `name` field of the
//! listing, wsp can **mint** the handle at `agent.start` — `place.rs`'s "wsp
//! mints the seat" move, one entry in [`Kind::args`] — and the lookup below
//! stops being needed at all.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::place::{Place, Refusal, Result, Seat};
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
    /// `full` is the way back from any trim. A trim is a capability change, so
    /// there has to be one and it has to be a flag rather than an edit: the
    /// agent that needs the design MCP server to draw an artefact is a real
    /// spawn on this backlog rather than a hypothetical.
    fn args(&self, full: bool) -> Vec<String>;

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
    fn args(&self, _full: bool) -> Vec<String> {
        Vec::new()
    }

    fn address(&self, _place: &dyn Place, _seat: &Seat) -> Option<String> {
        None
    }

    fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> Result<()> {
        place.tell(seat, text)
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
    fn args(&self, full: bool) -> Vec<String> {
        match full {
            true => Vec::new(),
            false => TRIM.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn address(&self, place: &dyn Place, seat: &Seat) -> Option<String> {
        let row = place.census().ok()?.into_iter().find(|s| &s.seat == seat)?;
        pick(&listing().ok()?, &row.session, &row.cwd)
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

    /// The trim moved and did not change, and it is still Claude Code's alone.
    ///
    /// Asserted as names rather than as a count, because the point of a denylist
    /// is that it is legible: a test that only checked the length would pass on
    /// a list that had quietly become something else. `Read`, `Edit`, `Write`
    /// and `Bash` appearing here would be the bad change — the measurement that
    /// prompted the trim found agents doing all their reading through `sed` at
    /// ~28K, and a trim that pushes work into Bash costs more than it saves.
    #[test]
    fn a_spawned_claude_is_not_given_the_two_tools_it_is_told_not_to_use() {
        let trim = of("claude").args(false);
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
        assert!(of("claude").args(true).is_empty(), "--full is the way back");
        assert!(of("codex").args(false).is_empty(), "not codex's spelling");
        assert!(of("gemini").args(false).is_empty());
        assert!(of("nonesuch").args(false).is_empty(), "an unknown kind is herdr's to refuse");
        assert!(of("").args(false).is_empty());
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
