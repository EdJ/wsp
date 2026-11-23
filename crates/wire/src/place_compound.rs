//! compound behind the place-work port: a real pty per seat, observed the
//! same way [`crate::place_super`] observes a headless one.
//!
//! The fourth implementor of `place.rs` (`compound-064`, `compound` d7/d9).
//! `place_backend` types at a shell inside a multiplexer; `place_super` forks a
//! process with no terminal at all; this one asks `compound-sup` — the
//! resident supervisor `~/claude/compound` builds, one process per session,
//! full libghostty rendering into an IOSurface — to open a real pty and
//! leaves the session running whether or not anything is attached to it.
//!
//! # What compound gives this backend that `place_super` does not have
//!
//! A terminal. An agent placed here can be **sat down in front of** — the
//! compound host discovers the same socket this backend minted
//! (`compound-028`'s directory-of-sockets convention, untouched) and can
//! attach a pane to it — which is the whole reason this backend exists
//! rather than `place_super` growing a screen. See "Identity" below for how
//! a click in that window is supposed to find its way back here.
//!
//! # The eyes are borrowed, not reinvented
//!
//! `compound` d7 asks which signals decide [`State`], not whether they
//! exist, and the answer is: the same ones `place_super` already uses.
//! Claude Code fires the same lifecycle hooks whether its stdin is a `tail
//! -f` or a real pty — a hook does not know what is on the other end of its
//! stdio — so [`crate::place_super::said_by`], [`crate::host::Host::alive`]
//! and [`crate::host::tally_burn`] are reused verbatim rather than
//! copied. What differs is only what a seat *records*: a `compound-sup`
//! socket and pid instead of a `tail`/agent process pair. Two directories,
//! two [`Place`] implementors, one hook vocabulary — `wsp report` (below)
//! is what keeps a hook from having to know which one it landed in.
//!
//! backend's screen-scraped `Blocked` state has no equivalent in
//! `place_super`'s headless world (robustness-051: a headless agent's denied
//! tool call never fires `PermissionRequest`, because there is no prompt to
//! run before). **A pty changes that.** An agent hosted here can genuinely
//! sit in front of a permission dialog, so `said_by`'s
//! `PermissionRequest`/`Elicitation` → `Working` approximation is exactly as
//! honest here as it is there, and no more — a seventh state is
//! robustness-051's, not this row's.
//!
//! # `start`, and what it does not have to build
//!
//! `compound-sup spawn --label <seat> --cwd … --cols … --rows … -- <agent>`
//! is run with **no `--socket`**, so `compound-sup` mints its own name in
//! the one run directory `compound-028` already made canonical
//! (`$COMPOUND_RUN_DIR`, or `~/.compound/run`) — this file does not
//! duplicate that minting, on purpose: two independent generators of "the
//! known socket directory" is exactly the kind of drift `robustness-017`
//! warns a port's adapters not to invent. The label is the seat's own id,
//! written by `compound-sup` itself as the `<name>.label` sidecar
//! `compound-028` d4 built — display-only there, and the join key here: it
//! is how something that already knows the socket (a discovery scan) finds
//! the seat that owns it, without this file or that one gaining a second
//! notion of identity.
//!
//! `compound-sup` daemonizes with `setsid()` and never forks again
//! (`session::daemonize`), so the pid this process sees at `spawn()` is the
//! pid for the session's whole life **and** its own process group leader —
//! [`Place::stop`] signals that pid's group and needs nothing else.
//!
//! Its handshake — `sup <pid>`, `pid <pid>`, `socket <path>` on stdout,
//! before stdio is shed — is the one thing read here rather than reasoned
//! about; `sup`'s pid is `Command::spawn`'s own answer already, so only the
//! socket line is parsed.
//!
//! # `tell`, and why it need not hold a pipe open
//!
//! `place_super`'s module docs name the hard-won lesson: a writer that
//! opens, writes and closes leaves a headless Claude Code deaf, because
//! nothing else holds its stdin open. That constraint does not cross here —
//! a compound session's durability is the resident supervisor's, not this
//! client's connection to it, so [`Place::tell`] dials, sends the sentence
//! as [`supervisor's `Input`][wire] (the paste door, which is what a real
//! terminal's "type this at me" already means), presses Enter as a
//! [`KeyEvent`], and disconnects. Nothing is left holding anything open.
//!
//! [wire]: https://en.wikipedia.org/wiki/Newline_delimited_JSON
//!
//! # The wire is read here, not depended on
//!
//! `~/claude/compound` and this tree are separate repositories with
//! separate histories; there is no crate boundary to depend across even if
//! one wanted to name a path across two checkouts nobody can promise sit
//! beside each other. What is here instead is the handful of wire shapes
//! [`Place::tell`] actually needs — an envelope, `Input`, `Key` — read
//! against `crates/supervisor/src/proto.rs` VERSION 8 at the
//! time of writing and versioned the same way that file is: a refusal rather
//! than a guess if the peer ever disagrees. **This is the one seam in this
//! file that ages by hand** — a wire bump on the compound side is invisible
//! here until something exercises it, which is why the version is checked
//! and refused loudly rather than assumed.
//!
//! **Two sockets since `compound-101`'s split, one process each
//! (`compound-136`).** `<name>.sock`, served by the durable `compound-sup`,
//! answers only `DurableBody` — `Vitals` and `Close` — with no VT and no
//! renderer required; `<name>.rsock`, served by the disposable
//! `compound-render`, answers everything [`Place::tell`] hand-rolls a client
//! for (`Input`/`Key`) plus `screen`/`state`, called as
//! subprocesses the same way `compound-sup screen`/`state` used to be. Both
//! sockets share one version number; what differs by socket is which `Body`
//! it accepts, not which wire it speaks.
//!
//! # Identity: what `Seat` is, and what it is not
//!
//! `compound` d9 settles the question `compound-052` found the cost of:
//! **wsp mints and holds the id.** A `Seat` here is `cpd-1`, `cpd-2` — this
//! backend's own opaque token, exactly as `place_super`'s `sup-N` is —
//! never the compound-sup socket path and never its label. The socket path
//! is an *attribute* recorded in the seat's own record, for this backend's
//! own use; nothing else in wsp reads it, and the port is not widened to
//! carry it.
//!
//! What makes that survive contact with a window that needs to *open* a
//! pane on a census row is the label sidecar, not a new port field: this
//! backend writes the seat's id as the session's label, so anything that
//! already discovers sockets (`compound-028`) can read the label sitting
//! beside one and ask "does wsp have a seat named this" — a compound-side
//! join, using a mechanism compound already built for display, doing one
//! more honest thing with it. Wiring that join into the census strip's
//! click is `compound-064`'s open half; see the task log rather than this
//! file for where that stands.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::host::{self, Host};
use crate::place::{self, Agent, Census, Delivery, Event, Order, Place, Refusal, Result, Seat, Seated, State};
use crate::place_super::{read_json, str_of};
// The OS halves moved to [`crate::host`]. The fixtures below that start and
// end real supervisors for a test, and the tests that drive the moved helpers
// by name, reach them from here.
#[cfg(any(test, feature = "test-support"))]
use crate::host::signal_group;
#[cfg(test)]
use wsp_base::calls::Command;

/// Which of these pids are running, asked of the in-process host.
#[cfg(any(test, feature = "test-support"))]
fn alive(pids: &[u32]) -> std::collections::BTreeSet<u32> {
    use crate::host::Host;
    crate::host::LOCAL.alive(pids)
}
#[cfg(test)]
use {
    crate::host::{
        b64, compound_render_binary, compound_sup_binary, dial, find_render, find_sup, read_deliver, recv_line, send,
        sup_command, wire_health_of, InProcess, CLOSE_AFTER_SECS, DIAL_TIMEOUT, HOOKS_FILE, SAID_FILE,
    },
    crate::place_super::said_by,
    std::net::Shutdown,
    std::os::unix::net::UnixStream,
    std::process::Stdio,
};
use crate::store::{write_atomic, Store};
use crate::util::{self, Clock};

/// compound as a place to put work.
pub struct Compound<'a> {
    /// Where seats live. One directory each, under this — never the
    /// compound-sup run directory, which this backend does not own and does
    /// not mint into.
    pub root: PathBuf,
    /// How long a `SIGTERM`ed session gets before [`Place::stop`] kills it.
    pub linger: Duration,
    pub poll: Duration,
    /// Deliver to a seat that cannot vouch for itself (`compound-097`).
    ///
    /// `false` everywhere except where a person has said so. `state` answers
    /// `Unknown` once a hook's word has expired AND `detected_state` has
    /// nothing either (`compound-109`) — the detector is the floor under a
    /// hook, not a full replacement for one, so `Unknown` is still reachable
    /// whenever it has nothing to read (no socket, no `compound-sup`, an
    /// agent kind it has no manifest for), and `tell` then refuses rather
    /// than typing into whatever is on screen — which is right, and which
    /// would also be a lockout with no way past it in that case.
    ///
    /// So this is the way past: a governor who has LOOKED — `compound-render
    /// screen` (`compound-sup screen` before `compound-101`'s split) is the
    /// honest surface — passes `wsp tell --anyway` and takes
    /// the decision themselves. It is a field on the backend rather than a
    /// widening of `Place`, for the reason `linger` and `poll` are: it
    /// configures this implementation, it does not change what the port
    /// promises. `compound-065` says a row that finds itself widening the
    /// port stops and asks, and this one did not have to.
    pub insist: bool,
    pub clock: &'a dyn Clock,
    /// How a far machine is reached, for a seat opened `--on` one
    /// (`docs/remote-agents.md` section 2). Ssh; a test passes a script.
    pub far: &'a dyn crate::far::Wire,
    /// Every OS operation a seat needs on this machine: processes, sockets,
    /// the renderer, delivery, hook files (`docs/agent-manager.md` 6.2, M0).
    pub host: &'a dyn Host,
}

impl Compound<'static> {
    pub fn new() -> Compound<'static> {
        Compound::at(Store::open().state.join(SEATS))
    }

    /// The one a spawn places work on: its host is `WSP_HOST`'s
    /// ([`crate::host::chosen`]), the in-process host by default. A manager
    /// that does not answer is [`host::HostError::Silent`], "host silent",
    /// and never this process's host in its place.
    pub fn chosen(version: &str) -> std::result::Result<Compound<'static>, host::HostError> {
        let state = Store::open().state;
        let host = host::chosen(&state, version)?;
        Ok(Compound { host, ..Compound::at(state.join(SEATS)) })
    }

    /// One rooted anywhere — a test's temporary directory.
    pub fn at(root: PathBuf) -> Compound<'static> {
        Compound {
            root,
            linger: Duration::from_millis(2_000),
            poll: Duration::from_millis(150),
            insist: false,
            clock: &util::Wall,
            far: &crate::far::Ssh,
            host: &host::LOCAL,
        }
    }

    /// A test's seat made to name `pid` as its process, for a caller that is
    /// asking whose processes are whose. `open` may have started a real
    /// supervisor; it is ended first, since the record that would find it is
    /// the one being replaced (`compound-127`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn record_pid_for_test(&self, seat: &Seat, pid: u32) {
        let mut rec = self.record(seat).unwrap();
        if let Some(old) = rec.get("pid").and_then(|p| p.as_u64()) {
            signal_group(old as u32, "KILL");
        }
        rec["pid"] = json!(pid);
        rec["agent"] = json!({ "kind": "claude", "name": "a", "args": [] });
        write_atomic(&self.dir_of(seat).unwrap().join(SEAT_FILE), &rec.to_string()).unwrap();
    }

    /// The same backend, willing to deliver to a seat that cannot vouch for
    /// itself. See [`Compound::insist`] — this is `wsp tell --anyway` and
    /// nothing else builds it.
    pub fn insisting(self) -> Compound<'static> {
        Compound { insist: true, ..self }
    }
}

impl Compound<'_> {
    /// Every seat's burn record under this backend's root, seat id attached.
    ///
    /// The tally is not this backend's — `heard` calls [`crate::host::tally_burn`] verbatim,
    /// the same call `place_super` makes, so the records are the same shape
    /// either way. The *reader* was, and that is what `wsp-116` is: `wsp burn`
    /// asked `place_super` alone, whose root is a different directory, so it
    /// reported no burn at all for a fleet that was entirely this backend's.
    /// See [`crate::host::burn_under`], which is the shared half and
    /// says why the root has to be a parameter.
    pub fn burn(&self) -> Vec<(String, Value)> {
        self.host.burn_under(&self.root)
    }
}

pub const SEATS: &str = "compound-seats";
const SEAT_FILE: &str = "seat.json";

/// Every hook this seat has ever been heard from, oldest first, one JSON line
/// each — `compound-107`'s answer to the question `said.json` alone cannot
/// answer.
///
/// `said.json` is a single slot, overwritten on every call, by design: it is
/// what `state_of` reads and it should hold nothing but the latest word. That
/// design is exactly what left `compound-107` unable to tell two very
/// different pasts apart from one one look at a live seat: a session whose
/// hooks stopped firing after `SessionStart`, and a session whose hooks fired
/// on every turn but whose seat was inspected between them, long after the
/// last one aged out (`compound-097`'s `VOUCH_SECS`). Both leave `said.json`
/// holding one `SessionStart` record; only this file tells them apart, and
/// only if it was already running when the seat needed it — a governor
/// cannot go back and ask a hook to have logged itself after the fact.
// HOOKS_FILE: see crate::host::HOOKS_FILE, which writes it.

/// How long a hook's word stays evidence about NOW (`compound-097`).
///
/// Not a timeout on the agent — a bound on what this backend is willing to
/// claim on its behalf. Past it the answer is `State::Unknown`, which is
/// honest rather than pessimistic: nothing has told us anything, and the
/// difference between an agent at a prompt, an agent mid-turn and an agent
/// holding a dialog is invisible from here.
///
/// **Two minutes, and the number is chosen against the two real cases rather
/// than picked.** A work order goes out seconds after `SessionStart` fires,
/// so a spawn must still be able to deliver — this is comfortably wide enough
/// for that. And a claim of idleness made two hours ago is worth nothing,
/// which is the case that cost an answer nobody chose. If the turn-boundary
/// hooks ever fire for a compound-hosted agent the way they do elsewhere,
/// every idle seat gets a fresh record each turn and this stops being
/// reachable in normal use — which is the right direction for it to fail in.
const VOUCH_SECS: i64 = 120;
const NEXT_FILE: &str = "next";
/// The prefix of a seat directory moved aside because it would not go
/// (`wsp-092`): a dot-name, so [`Compound::ids`] never reads it as a seat.
const GONE: &str = ".gone-";
const SEAT_PREFIX: &str = "cpd-";

/// How [`Compound::take_away`] removed a seat's directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Took {
    /// The first `remove_dir_all` took it.
    AtOnce,
    /// A later one did, after the directory filled up again: the try it was.
    OnTry(u32),
    /// None did, and it was moved aside to a [`GONE`] name.
    Aside,
}

/// How long `start` waits for `compound-sup`'s handshake before giving up on
/// it. Generous: the supervisor daemonizes, opens libghostty and binds a pty
/// before it prints the third line, and a spawn that is going to fail (a
/// bad cwd, a missing program) says so on the same stdout rather than
/// hanging — so the bound is a safety net, not the expected path.
// Moved to crate::host with the code that waits on it.

/// How long a `tell` or `state`-adjacent dial waits for the socket to
/// accept. A live session answers `Attach` in well under a millisecond
/// (it is a local accept, not a wire round trip); this is room for a
/// session under load, not an expected wait.
// Moved to crate::host with the code that waits on it.

pub use crate::host::RENDER_NAMES;

/// How long after a start [`Compound::at_trust_prompt`] looks: the prompt is the
/// first thing Claude Code draws, and the work order follows within seconds.
const TRUST_PROMPT_WINDOW: i64 = 120;

/// The folder-trust prompt, read off a screen, as the reason not to type.
/// Matched on the option a person would choose, which is the line least
/// likely to be reworded into something else.
fn trust_prompt(seat: &Seat, screen: &str) -> Option<String> {
    if !screen.contains("Yes, I trust this folder") {
        return None;
    }
    let dir = screen
        .lines()
        .skip_while(|l| !l.contains("Accessing workspace:"))
        .skip(1)
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|d| format!(" for {d}"))
        .unwrap_or_default();
    Some(format!(
        "{seat} is at Claude Code's folder-trust prompt{dir} - nothing was typed, because the prompt's \
         default is \"No, exit\" and the order's Enter would end the agent; start it in a project"
    ))
}

/// How long after a start a Claude Code seat that has fired no hook of its own
/// is read as [`State::Starting`] rather than off its screen (`wsp-252`).
///
/// **The screen says idle before Claude Code will take a submit.** Two of the
/// seats that came up on 2026-10-08 with their work orders typed and unsent
/// were looked at afterwards: cpd-616 started at 17:53:51 and fired
/// `SessionStart` at 17:54:22, and cpd-627 started at 04:55:44 and fired it at
/// 04:56:15 — thirty-one seconds both, where a healthy seat takes one or two.
/// Through the whole of that the composer was drawn and the screen read idle,
/// so `wait_ready` passed, the order was typed, and every Enter wsp pressed in
/// those thirty seconds was lost; the text stayed. [`VOUCH_SECS`]'s own doc
/// already assumed "a work order goes out seconds after `SessionStart`" —
/// what let it go out before was the screen answering for a seat whose hooks
/// had not spoken yet.
///
/// Two minutes, the same as the trust prompt's window: past it the screen
/// answers again, so a Claude Code whose hooks never fire is late rather than
/// never told.
const STARTING_WINDOW: i64 = 120;

/// Whether Claude Code's prompt is holding typed text, read off its screen —
/// [`Place::typed`] for a `claude` seat. `None` when the screen holds no
/// prompt this recognises.
///
/// The prompt is the last box on the screen: the line after the second-last
/// horizontal rule, starting `❯`, up to the last rule. A sentence queued
/// behind a turn is drawn above that box and is not in it, which is why the
/// box and not the screen is read. Claude Code's greyed suggestion in an empty
/// prompt (`Try "…"`) reads as text on a screen with no colour in it, and is
/// not.
///
/// A seat Claude Code runs under a session name draws that name in the box's
/// top rule (`──── primary-delivery-cpd-5893 ─`), so a rule is a line of `─`
/// or two runs of it with one short label between (`wsp-659`): read as bare
/// rules only, that box had one rule, the composer read `None`, and the
/// refusal fallback never typed at an idle governor.
fn composer(screen: &str) -> Option<bool> {
    draft(screen).map(|d| !d.is_empty())
}

/// What [`composer`] reads in the box: the text held, or empty for a box
/// holding nothing (or only the greyed suggestion). `None` when no box is drawn.
fn draft(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let rules: Vec<usize> = (0..lines.len()).filter(|&i| is_rule(lines[i])).collect();
    let [.., top, bottom] = rules[..] else { return None };
    let held: String = lines[top + 1..bottom].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
    let held = held.trim_start_matches(['❯', '>']).trim();
    Some(match held.starts_with("Try \"") {
        true => String::new(),
        false => held.to_string(),
    })
}

/// What a refusal says when the seat's own input box is holding a draft
/// (`wsp-694`): the wake path reads it and holds rather than typing again.
pub const DRAFT_STUCK: &str = "a draft is stuck in the seat's input box";

/// A refusal that took nothing, at a box holding text, says what the text is
/// (`wsp-694`). On 2026-11-19 an 81 KB wake sat unsubmitted in a custodian's
/// box and every delivery after it came back `no composer` — true, and no help
/// to anybody reading why the seat had stopped answering for fifteen hours.
fn name_the_draft(answer: Result<Delivery>, draft: impl FnOnce() -> Option<String>) -> Result<Delivery> {
    match &answer {
        Err(Refusal::Failed(why)) if took_nothing(why) && !why.contains(DRAFT_STUCK) => match draft() {
            Some(d) if !d.is_empty() => {
                let head: String = d.chars().take(60).collect();
                let more = if d.chars().count() > 60 { "…" } else { "" };
                Err(Refusal::Failed(format!("{why} — {DRAFT_STUCK} ({} chars: \"{head}{more}\")", d.chars().count())))
            }
            _ => answer,
        },
        _ => answer,
    }
}

/// A supervisor's answer that says nothing reached the seat.
fn took_nothing(why: &str) -> bool {
    ["refused", "no composer", "no_composer", "no-composer"].iter().any(|r| why.contains(r))
}

/// A horizontal rule of Claude Code's prompt box — see [`composer`]: at least
/// 20 wide, and either nearly all `─` or a run of `─` (half the line or more),
/// one label with no `─` in it, and `─` again to the end.
fn is_rule(l: &str) -> bool {
    let t = l.trim();
    let n = t.chars().count();
    let dashes = t.chars().filter(|c| *c == '─').count();
    if n < 20 {
        return false;
    }
    if dashes * 10 >= n * 9 {
        return true;
    }
    let label = t.trim_matches('─').trim();
    t.starts_with('─') && t.ends_with('─') && dashes * 2 >= n && !label.is_empty() && !label.contains('─') && !label.contains('❯')
}

impl Compound<'_> {
    pub fn dir_of(&self, seat: &Seat) -> Result<PathBuf> {
        let id = seat.as_str();
        let plain = !id.is_empty()
            && !id.starts_with('.')
            && !id.contains('/')
            && !id.contains('\\')
            && id.len() < 128;
        match plain {
            true => Ok(self.root.join(id)),
            false => Err(Refusal::NoSeat(seat.clone())),
        }
    }

    /// Whether compound minted this seat and still has its directory — the
    /// one question a verb that is handed a bare id can ask of this backend
    /// without a census. A stat, so `wsp peek` and `wsp tell` can ask it of
    /// every needle they are given (`wsp-156`).
    pub fn holds(&self, seat: &Seat) -> bool {
        self.dir_of(seat).map(|d| d.is_dir()).unwrap_or(false)
    }

    fn record(&self, seat: &Seat) -> Result<Value> {
        let dir = self.dir_of(seat)?;
        match dir.is_dir() {
            true => Ok(read_json(&dir.join(SEAT_FILE))),
            false => Err(Refusal::NoSeat(seat.clone())),
        }
    }

    fn said(&self, seat: &Seat) -> Value {
        match self.dir_of(seat) {
            Ok(dir) => self.host.said(&dir),
            Err(_) => json!({}),
        }
    }

    /// A seat id nothing has ever been called. See `place_super::mint` —
    /// this is the same `O_EXCL` argument against a directory of this
    /// backend's own, so the two counters can never collide even though the
    /// prefixes already would not.
    fn mint(&self) -> Result<Seat> {
        fs::create_dir_all(&self.root).map_err(|e| Refusal::Backend(e.to_string()))?;
        let mut n = self.next_number();
        loop {
            let id = format!("{SEAT_PREFIX}{n}");
            match fs::create_dir(self.root.join(&id)) {
                Ok(()) => {
                    let _ = write_atomic(&self.root.join(NEXT_FILE), &(n + 1).to_string());
                    return Ok(Seat::new(id));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(Refusal::Backend(e.to_string())),
            }
        }
    }

    fn next_number(&self) -> u64 {
        let counted = fs::read_to_string(self.root.join(NEXT_FILE))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(1);
        let highest = self
            .ids()
            .iter()
            .filter_map(|id| id.strip_prefix(SEAT_PREFIX)?.parse::<u64>().ok())
            .max()
            .map(|n| n + 1)
            .unwrap_or(1);
        counted.max(highest).max(1)
    }

    /// Remove an ended seat's directory, **tolerating the agent's last hooks
    /// writing into it as it goes** (`wsp-092`). `Stop` and `SessionEnd` can
    /// still be putting `said.json` or `hooks.jsonl` there as the session is
    /// ended, and `remove_dir_all` then finds the directory not empty at its
    /// last step: on 2026-11-18 that refused the end of `cpd-1025` and
    /// `cpd-1026`, so their despawns kept the claims, and a rework leg sat
    /// queued behind one for nine minutes.
    ///
    /// So a directory that filled up again (or came back) is tried a few
    /// more times, and one that still will not go is moved aside to a
    /// [`GONE`] dot-name, which [`Self::ids`] never reads as a seat. The
    /// seat is ended either way: its agent has been signalled. Leftovers
    /// are cleared here by the next end, once nothing writes into them.
    /// Any other error refuses as before, and the seat is kept.
    ///
    /// Says how it went, which only a test reads: the end does not care.
    fn take_away(&self, dir: &Path) -> Result<Took> {
        self.clear_gone();
        let mut last = None;
        for n in 0..5 {
            let took = if n == 0 { Took::AtOnce } else { Took::OnTry(n + 1) };
            match fs::remove_dir_all(dir) {
                // A hook that passed its `is_dir` before the removal can still
                // land after it; nothing it writes makes a directory, but
                // asking is cheap.
                Ok(()) if !dir.exists() => return Ok(took),
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(took),
                Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => last = Some(e),
                Err(e) => return Err(Refusal::Backend(e.to_string())),
            }
            self.clock.rest(self.poll);
        }
        let why = last.map_or_else(|| "it came back".to_string(), |e| e.to_string());
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let aside = dir.with_file_name(format!("{GONE}{name}-{}-{}", std::process::id(), util::epoch_nanos()));
        match fs::rename(dir, &aside) {
            Ok(()) => {
                let _ = fs::remove_dir_all(&aside);
                Ok(Took::Aside)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Took::OnTry(6)),
            Err(e) => Err(Refusal::Backend(format!("{why}; and it could not be moved aside: {e}"))),
        }
    }

    /// Every [`GONE`] directory under the root, removed where it will go.
    fn clear_gone(&self) {
        for e in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(GONE) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }

    fn ids(&self) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        out.sort();
        out
    }

    /// One hook, recorded against a seat. Identical in shape to
    /// [`crate::place_super::Supervisor::heard`] — same two files, same
    /// reason for two — because the fact being recorded (what an agent's
    /// hook just said) does not depend on what is on the other end of its
    /// stdio.
    pub fn heard(&self, seat: &Seat, hook: &str, state: State, payload: &Value) {
        let Ok(dir) = self.dir_of(seat) else { return };
        if !dir.is_dir() {
            return;
        }
        let was = self.host.said(&dir);
        // A nested session's hook, not the seat's (`wsp-364`): logged as a
        // child's, so the trail shows it arrived, and kept out of `said.json`.
        if crate::place_super::a_childs(&was, payload) {
            self.append_hooks_log_as(&dir, hook, state, Some(&str_of(payload, "session_id")));
            return;
        }
        let keep = |key: &str| -> String {
            match str_of(payload, key).is_empty() {
                true => str_of(&was, key),
                false => str_of(payload, key),
            }
        };
        self.host.say(
            &dir,
            &json!({
                "state": state.as_str(),
                "hook": hook,
                "needs": crate::place_super::needs_of(hook, payload),
                "at": util::now_iso(),
                "session_id": keep("session_id"),
                "transcript_path": keep("transcript_path"),
            }),
        );
        self.append_hooks_log(&dir, hook, state);
        self.host.tally(&dir, payload);
        // `wsp-637`: an agent that has left for good takes its pane with it.
        if hook == "SessionEnd" && leaves_for_good(payload) {
            if let Ok(rec) = self.record(seat) {
                if rec.get("agent").is_some() && str_of(&rec, "machine").is_empty() {
                    if let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()) {
                        self.host.stop_soon(pid as u32);
                    }
                }
            }
        }
    }

    /// A sign of life from a tool hook (`PreToolUse`/`PostToolUse`): a seat
    /// whose record says `working` has it re-dated, so a turn that runs tools
    /// for longer than [`VOUCH_SECS`] does not age into the screen fallback.
    /// Any other record is left alone: a tool hook that arrives after `Stop`
    /// must not make an idle seat read working, and a nested session's is not
    /// the seat's (`wsp-364`).
    pub fn touched(&self, seat: &Seat, payload: &Value) {
        let Ok(dir) = self.dir_of(seat) else { return };
        let mut was = self.host.said(&dir);
        if str_of(&was, "state") != "working" || crate::place_super::a_childs(&was, payload) {
            return;
        }
        let (was_at, was_hook) = (str_of(&was, "at"), str_of(&was, "hook"));
        was["at"] = json!(util::now_iso());
        // `heard` writes this file whole and does not lock. Look again just
        // before the rename and give up if a hook said something in between:
        // a `Stop` that landed in that gap must not be overwritten with a
        // working record. What is left is a window of microseconds, and its
        // worst case is one late refresh.
        let now = self.host.said(&dir);
        if str_of(&now, "at") != was_at || str_of(&now, "hook") != was_hook {
            return;
        }
        self.host.say(&dir, &was);
    }

    /// Append one line to [`HOOKS_FILE`]. Best-effort, like everything else a
    /// hook touches — a write that fails here must not be the write that
    /// fails the hook, and a reader missing one line is a smaller loss than a
    /// session that stalls on it.
    fn append_hooks_log(&self, dir: &std::path::Path, hook: &str, state: State) {
        self.append_hooks_log_as(dir, hook, state, None)
    }

    /// The same line, marked as a nested session's when `child` names one
    /// (`wsp-364`). Readers of the trail that look at `hook` and `state` alone
    /// must skip a line with `child` set: it is not the seat's.
    fn append_hooks_log_as(&self, dir: &std::path::Path, hook: &str, state: State, child: Option<&str>) {
        let mut line = json!({ "at": util::now_iso(), "hook": hook, "state": state.as_str() });
        if let Some(session) = child {
            line["child"] = json!(session);
        }
        self.host.log_hook(dir, &line);
    }

    /// Whether this seat's `compound-sup` still holds its pid, per the same
    /// `ps` `alive` reads for a bare-forked one. `compound-sup` daemonizes
    /// but never forks again, so the pid this recorded at `start` is the
    /// process for the session's whole life — there is no second pid to
    /// reconcile the way a fork-then-exec backend would have to.
    fn state_of(&self, seat: &Seat, rec: &Value, running: &BTreeSet<u32>) -> State {
        if !str_of(rec, "machine").is_empty() {
            // A seat on another machine has no pid here; the session is there
            // while the forwarded socket answers.
            let socket = str_of(rec, "socket");
            if socket.is_empty() {
                return State::Empty;
            }
            // A machine that has gone silent is a link, not a death: the seat is
            // held and reads `unreachable` (`docs/remote-agents.md` section 3),
            // whether or not the local end of its forward still answers. Only a
            // machine that is answering and whose session is not is the session
            // gone.
            if self.far.silent_for(&str_of(rec, "machine")).is_some() {
                return State::Unreachable;
            }
            if !self.host.answers(Path::new(&socket)) {
                // The forward answers no more, which is what a tunnel that has
                // dropped looks like for the first lease of a link cut, while the
                // machine still reads ready. Ask the link before calling it a death.
                if self.far.link_down(&str_of(rec, "machine")) {
                    return State::Unreachable;
                }
                return State::Gone;
            }
        } else {
            let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()) else { return State::Empty };
            if !running.contains(&(pid as u32)) {
                return State::Gone;
            }
            // A pid that runs is not the session's after a restart: the number
            // is reused (wsp-094). The session is its socket, and one whose
            // socket file is gone is gone, whatever runs under its old pid.
            // Past the start window only, so a socket not bound yet is not read
            // as a death.
            let socket = str_of(rec, "socket");
            let started = util::epoch_of(&str_of(rec, "started_at"));
            if rec.get("agent").is_some()
                && !socket.is_empty()
                && util::epoch_secs().saturating_sub(started) > STARTING_WINDOW
                && !Path::new(&socket).exists()
            {
                return State::Gone;
            }
        }
        // No agent recorded: `compound-081`'s bare terminal — a pty with a
        // shell in it and nothing this backend watches for hooks.
        // `State::Empty`'s own doc names this case — "a terminal somebody
        // opened" — so a live, agent-less pid reads exactly the way no pid
        // at all does, and `said` (which no shell ever writes) is never
        // consulted for one.
        if rec.get("agent").is_none() {
            return State::Empty;
        }
        let said = self.said(seat);
        // **A hook's word is evidence about the moment it was written, and
        // this backend has no second source** (`compound-097`). backend watches
        // a pty and can answer about NOW; compound answers from the last hook
        // that fired, so a record that has not been refreshed says what was
        // true then and nothing about since.
        //
        // The failure that made this a row rather than a nicety: a seat whose
        // only `said` was its own `SessionStart` read `Idle` two hours later
        // while its agent was mid-turn holding a question dialog, and a
        // `wsp tell` was typed AT the dialog and selected an answer nobody
        // chose. `Place::tell` refuses unless `will_take_a_prompt`, and
        // `cmd_agent::tell` refuses on `Blocked` — both guards are sound and
        // both were inert, because they are only as good as this function.
        //
        // So an expired record falls to `detected_state` — the screen this
        // backend can still read even with no hook to trust — and only to
        // `Unknown`, whose own doc is exactly this case — *"the backend did
        // not say, or could not be asked"* — when that has nothing either.
        // `will_take_a_prompt` already refuses `Unknown`, so the guard
        // re-arms without either caller changing; it just has a floor under
        // it now instead of a bare fallback (`compound-109`).
        let at = util::epoch_of(&str_of(&said, "at"));
        // A Claude Code that has not said anything since this start is still
        // starting, whatever its screen draws — see [`STARTING_WINDOW`]. A
        // record older than the start is the last agent's, in a seat reused.
        let started = util::epoch_of(&str_of(rec, "started_at"));
        let kind = rec.get("agent").map(|a| str_of(a, "kind")).unwrap_or_default();
        // Claude-only: the start window is a fact about Claude Code's hooks; a
        // data kind has none and must not be taken for it. pi is left to its
        // screen: its extension may not be installed, and a seat held Starting
        // for want of a hook that never comes is worse than one read off the
        // screen a little early.
        if kind == "claude" && started > 0 && at < started && util::epoch_secs() - started < STARTING_WINDOW {
            // A far seat's hooks fire on the far machine and never write this
            // seat's `said.json`, so the window would hold it `Starting` for its
            // whole two minutes while its renderer served a drawn screen and its
            // agent worked (wsp-589). The screen is the signal `wsp peek` reads;
            // take it when it answers, and stay `Starting` when it does not.
            if !str_of(rec, "machine").is_empty() {
                return self.detected_state(seat, rec).filter(|s| *s != State::Unknown).unwrap_or(State::Starting);
            }
            return State::Starting;
        }
        // A `SessionEnd` is the agent's last word, and any hook after it would
        // have replaced it: it is not a moment's evidence that ages, and a
        // screen read past it (a dead renderer after a restart) says nothing
        // but Unknown (wsp-094).
        if str_of(&said, "state") == "gone" && at >= started {
            return State::Gone;
        }
        if at == 0 || util::epoch_secs().saturating_sub(at) > VOUCH_SECS {
            return self.detected_state(seat, rec).unwrap_or(State::Unknown);
        }
        match said.get("state").and_then(|s| s.as_str()) {
            Some("idle") => State::Idle,
            Some("working") => State::Working,
            Some("gone") => State::Gone,
            _ => State::Starting,
        }
    }

    fn seated(&self, seat: &Seat, rec: &Value, state: State) -> Seated {
        let agent = rec.get("agent").cloned().unwrap_or(json!({}));
        Seated {
            seat: seat.clone(),
            label: str_of(rec, "label"),
            cwd: str_of(rec, "cwd"),
            agent: Agent {
                kind: str_of(&agent, "kind"),
                name: str_of(&agent, "name"),
                key: None,
                args: agent
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
            },
            state,
            session: str_of(&self.said(seat), "session_id"),
        }
    }

    /// A seat as a census row: wsp's record of it, in the state its host's
    /// manager reported. Files only, no `ps` and no render: what `wip` reads
    /// under `WSP_HOST=manager` (`docs/agent-manager.md` 4.3), where the state
    /// is the manager's and not this backend's survey.
    pub fn seated_as(&self, seat: &Seat, state: State) -> Seated {
        let rec = self.record(seat).unwrap_or(json!({}));
        self.seated(seat, &rec, state)
    }

    fn survey(&self) -> Vec<Seated> {
        let seats: Vec<(Seat, Value)> = self
            .ids()
            .into_iter()
            .map(|id| {
                let seat = Seat::new(id);
                let rec = self.record(&seat).unwrap_or(json!({}));
                (seat, rec)
            })
            .collect();
        let pids: Vec<u32> = seats
            .iter()
            .filter_map(|(_, r)| r.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32))
            .collect();
        let running = self.host.alive(&pids);
        seats
            .iter()
            .map(|(seat, rec)| {
                let state = self.state_of(seat, rec, &running);
                self.seated(seat, rec, state)
            })
            .collect()
    }

    /// The socket this seat's session answers on — the attribute d9 keeps
    /// out of [`Seat`] itself. `None` before [`Place::start`] has run, or
    /// after the session is gone.
    pub fn socket_of(&self, seat: &Seat) -> Option<PathBuf> {
        self.record(seat).ok().and_then(|rec| {
            let s = str_of(&rec, "socket");
            (!s.is_empty()).then(|| PathBuf::from(s))
        })
    }

    /// The renderer's own socket for this seat's session (`<name>.rsock`,
    /// `compound-101`) — pixels, keys, mouse, screen text: everything that
    /// needs libghostty. `None` before [`Place::start`]/[`Place::open`] has
    /// run, after the session is gone, or if `compound-sup`'s handshake
    /// never announced one (an older binary, from before the split).
    /// Reachable only while a renderer is actually up; [`Compound::vitals_osc_title`]
    /// is the durable half's own answer for when it is not.
    pub fn render_socket_of(&self, seat: &Seat) -> Option<PathBuf> {
        self.record(seat).ok().and_then(|rec| {
            let s = str_of(&rec, "render_socket");
            (!s.is_empty()).then(|| PathBuf::from(s))
        })
    }

    /// What this seat's pty is showing right now — `compound-render screen
    /// <render-socket>`, the same call a person types by hand.
    ///
    /// **Render-side since `compound-101`'s split (`compound-136`).** `screen`
    /// needs a VT, which only `compound-render` still links; the durable
    /// `compound-sup` half answers `vitals`/`close` and nothing that reads
    /// pixels. A renderer that has died — a seat between renderers, or one
    /// that never got one — answers nothing here; that is a real "cannot
    /// read this pane right now", not this call's to paper over. [`Compound::state_of`]'s
    /// own fallback for that case is `vitals`'s OSC title, not this.
    ///
    /// Not on [`Place`]: reading a pane is the observe half `place.rs`'s
    /// module docs put outside this port, which is why `wsp peek` already
    /// calls backend's `pane.read` directly rather than through a trait method
    /// — this is that seam's compound answer, called the same way. Named in
    /// `compound-111`: with no read of any kind, `compound-sup screen
    /// <socket>` (as it was before the split), typed by a person, was "the
    /// only honest surface all week".
    ///
    /// `compound-render`'s own presentation is a `NNN|text` line per row, a
    /// trailing `size WxH`, and — since the split — a further trailing
    /// `osc_title <title>` line; all three sidecar lines are stripped here
    /// down to bare text so `wsp peek` prints the same shape whichever
    /// backend answered.
    /// Why a seat that has just started must not be typed at: Claude Code is
    /// showing its folder-trust prompt, whose default is "No, exit".
    ///
    /// **Looked for, because the symptom names the wrong thing** (`wsp-219`).
    /// The detector reads that prompt as idle, so the order went in, its Enter
    /// chose "No, exit", the session ended and took the renderer with it, and
    /// the spawn failed on a `.rsock` that "never appeared". That was 57 list
    /// reseats in `cycle.log`. [`crate::cmd_spawn`] now refuses to start an
    /// agent outside a project, which is the cause. This is so that the next
    /// way in says what it is, and leaves the prompt up for a person to answer.
    ///
    /// Only within [`TRUST_PROMPT_WINDOW`] of the start, so a seat being told
    /// its hundredth sentence does not pay a `compound-render` process to look.
    /// A screen that cannot be read is no answer, and the tell goes ahead as
    /// it always has.
    fn at_trust_prompt(&self, seat: &Seat) -> Option<String> {
        let rec = self.record(seat).ok()?;
        let started = rec.get("started_at").and_then(|s| s.as_str()).map(util::epoch_of)?;
        if util::epoch_secs() - started > TRUST_PROMPT_WINDOW {
            return None;
        }
        trust_prompt(seat, &self.read_screen(seat).ok()?)
    }

    pub fn read_screen(&self, seat: &Seat) -> Result<String> {
        let socket = self.render_socket_of(seat).ok_or_else(|| Refusal::NoSeat(seat.clone()))?;
        self.host.read_screen(&socket)
    }

    /// The floor under a hook's word, not a replacement for it
    /// (`compound-109`). [`Compound::state_of`] falls here only once the
    /// hook record cannot answer — absent, or aged past [`VOUCH_SECS`] — and
    /// that is not a rare seam for every agent this backend hosts: opencode
    /// fires none of the hooks `heard` listens for, so a compound-hosted
    /// opencode seat's `said.json` never exists at all, and every call here
    /// used to read `Unknown` for its whole life. This is that seat's only
    /// source of a live answer.
    ///
    /// Two tiers, since `compound-101`'s split (`compound-136`). Screen-based
    /// detection needs a VT, which only `compound-render` holds now; a seat
    /// whose renderer has died is not thereby a seat with nothing to say —
    /// `compound-113` established that compound stands on the screen
    /// *including the title* for both agents, and the title survives on the
    /// durable half (`vitals`) with no renderer required at all. So this
    /// tries the richer, screen-aware answer first and only falls to the
    /// title alone once that cannot be reached — not once it runs and
    /// disagrees, which is a real "unknown" and not this function's to
    /// second-guess.
    ///
    /// `None` when there is nothing to ask at all: no socket minted yet, or
    /// an agent kind the ported engine has no manifest for — only `claude`,
    /// `opencode` and `pi` are compiled (`compound-109`'s overview); anything else
    /// falls back to [`State::Unknown`] exactly as it did before this
    /// existed.
    fn detected_state(&self, seat: &Seat, rec: &Value) -> Option<State> {
        let agent = rec.get("agent")?;
        let kind = str_of(agent, "kind");
        if !reads_kind(&kind) {
            return None;
        }
        if let Some(state) = self.screen_state(seat, &kind) {
            return Some(state);
        }
        // The renderer could not be reached — dead, or never started. Fall to
        // the one signal that survives that (`compound-113`'s title, over
        // `vitals` on the durable socket) rather than reading Unknown when a
        // real answer is still standing.
        self.vitals_osc_title(seat).and_then(|t| title_state(&kind, &t))
    }

    /// The session id on this seat's record, for a reader that has a question
    /// the screen cannot answer.
    ///
    /// **`pub(crate)` because `wsp-160`'s override lives in the reconciler
    /// rather than here**, and the argument is worth leaving written down:
    /// [`Self::detected_state`] is called from `survey`, `wsp wip` and a panel
    /// at 250ms, so an override in it would run `opencode db` on every one of
    /// those. The reconciler runs it once a minute, on one seat at a time, and
    /// can say what it did in `cycle.log` — which a function returning a
    /// `State` cannot.
    pub fn session_of(&self, seat: &Seat) -> String {
        str_of(&self.said(seat), "session_id")
    }

    /// Shells to `compound-render state <render-socket> <kind>` —
    /// `compound-109`'s verb, wrapping the ported half of backend's
    /// agent-detection engine — exactly as [`Compound::read_screen`] shells
    /// to `compound-render screen`: this backend does not link compound's
    /// crates, so this is a wire read the same way that one is, not a
    /// dependency.
    ///
    /// **A process spawn and a socket round trip, not a free read.**
    /// Measured (`compound-109`, `compound-sup state` against a live
    /// opencode session before the split, 20 calls): ~22ms each — the split
    /// moved which binary answers, not the cost. `survey` calls [`state_of`]
    /// once per seat in the directory, serially, so a census of N seats whose
    /// hooks cannot answer costs N × ~22ms here — fine for the sizes `wsp
    /// ls`/`wsp kanban` run against today, and worth re-measuring before this
    /// is anywhere near an input path or a seat count that matters at that
    /// rate.
    fn screen_state(&self, seat: &Seat, kind: &str) -> Option<State> {
        let socket = self.render_socket_of(seat)?;
        self.host.screen_state(&socket, kind)
    }

    /// The child's last OSC window/tab title, straight off `compound-sup
    /// vitals <socket>` — the durable half, answerable with no VT and no
    /// renderer at all (`compound-101`, `compound-109`). `None` when there is
    /// no socket, no `compound-sup` reachable, or the call fails outright;
    /// empty when the durable half is up but no renderer has ever reported a
    /// title, which [`title_state`] also reads as nothing rather than as
    /// idle.
    fn vitals_osc_title(&self, seat: &Seat) -> Option<String> {
        let socket = self.socket_of(seat)?;
        self.host.osc_title(&socket)
    }
}

/// `compound-113`'s two OSC-title rules — `osc_title_working` and
/// `osc_title_idle` in `vendor/backend-detection/manifests/claude.toml`,
/// ported rather than shelled to, because there is no CLI verb for "classify
/// this string" and spawning a process to run a two-line regex would be a
/// strange place to draw that line. **A floor under `screen_state`'s answer,
/// not a second copy of it**: [`Compound::detected_state`] only reaches this
/// once the render socket cannot be asked at all, so this can never disagree
/// with the richer, screen-aware detector while a renderer is actually up —
/// it only has anything to say once that detector has nothing to say at all.
///
/// opencode's manifest carries no `osc_title` rule (`compound-109`'s
/// overview) — it never has, split or not — so this returns `None` for it,
/// same as the render-side detector would with no renderer: opencode fires
/// none of the hooks [`Compound::heard`] listens for either, so a seat whose
/// renderer died genuinely has no answer left, and `Unknown` is the honest
/// one. wsp does not own the manifest this mirrors; a rule changing there is
/// a `compound-113`-shaped row on the compound side, not a silent divergence
/// here — this is two characters, not the engine.
///
/// pi's manifest carries no `osc_title` rule either (only `working_literal`
/// on the screen), so it returns `None` for pi too: a pi seat whose renderer
/// died reads `Unknown`, the honest answer.
fn title_state(kind: &str, osc_title: &str) -> Option<State> {
    // Claude-only: the title glyph rule is Claude Code's; a data kind reads None.
    if kind != "claude" {
        return None;
    }
    let mut chars = osc_title.chars();
    let first = chars.next()?;
    if chars.next() != Some(' ') {
        return None;
    }
    if first == '\u{2733}' {
        return Some(State::Idle);
    }
    if ('\u{2800}'..='\u{28FF}').contains(&first) || ('\u{25D0}'..='\u{25D3}').contains(&first) {
        return Some(State::Working);
    }
    None
}


// ---------------------------------------------------------------------------
// The wire, read rather than depended on. See the module docs' "The wire is
// read here, not depended on".
// ---------------------------------------------------------------------------

/// `compound-sup`'s wire version, moved down to `wsp-host` with the code that
/// speaks it (wsp-020); its history is written there.
pub(crate) use wsp_host::inproc::WIRE_VERSION;

/// The wire version this wsp speaks, for the far-machine checks that compare it
/// with what a machine's `compound-sup` answers (`far`, `spawn --on`, `doctor`).
pub fn wire_version() -> u32 {
    WIRE_VERSION
}

/// What `doctor` says about the two ways this constant has already drifted
/// (`compound-026`), asked of the installed pieces rather than read out of
/// either repository's source — the same discipline the rest of this file
/// already keeps for the wire itself.
///
/// **Two checks, because the two instances were two different shapes.** The
/// August one was a build-ordering trap: `cargo run -p host` rebuilds only
/// `host`, and the two binaries land beside each other in one cargo
/// workspace's `target/<profile>/`, so a `host` newer than the `compound-sup`
/// sitting next to it is exactly that trap, caught by an mtime comparison
/// that needs nothing from either source tree. The September one was this
/// constant itself going stale against a `proto.rs` bump the wsp repository
/// never reads — caught by asking the installed `compound-sup` what it
/// actually speaks, the same way [`crate::host::compound_sup_binary`]'s callers already
/// ask it for `state` rather than trusting a guess.
///
/// Both are notes until they are not: no `compound-sup` on this machine is
/// not a problem — the backend backend needs none of this — and a
/// `compound-sup` that cannot answer `wire-version` yet is an older binary,
/// not a broken one. What one function should never do is fix either: a
/// version check that started rebuilding or restarting somebody else's
/// binaries would be `robustness-090` d1's forbidden act wearing a doctor
/// badge, so this only ever says what is wrong and the verb that fixes it.
pub fn wire_health(problems: &mut Vec<String>, notes: &mut Vec<String>) {
    host::LOCAL.wire_health(problems, notes)
}


/// How long one tick waits on a `deliver` before leaving it pending (wsp-477).
/// compound's `deliver` waits for a busy seat to go idle (minutes) and for an
/// unacknowledged order (about a minute); the governor tick is not a place to
/// sit through either.
const DELIVER_WAIT: Duration = Duration::from_secs(30);

/// A ceiling on [`DELIVER_WAIT`] for this process, in milliseconds; 0 is none
/// (`wsp-666`). `wsp worklist advance` is a step, not a tick's patient pass: it
/// sat the whole 30 s on a `deliver` to a seat that would not take the order
/// (six calls, ~22 s a call over three days of calls.jsonl), when the answer
/// it needs is only "left pending". A pending delivery is asked again under
/// the same id, so a short wait loses nothing.
static DELIVER_CAP_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Cap every `deliver` this process makes at `cap`; see [`DELIVER_CAP_MS`].
pub fn cap_deliver_wait(cap: Duration) {
    DELIVER_CAP_MS.store(cap.as_millis().max(1) as u64, std::sync::atomic::Ordering::Relaxed);
}

/// How long a `deliver` waits here: [`DELIVER_WAIT`], or the cap if lower.
pub fn deliver_wait() -> Duration {
    match DELIVER_CAP_MS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => DELIVER_WAIT,
        ms => DELIVER_WAIT.min(Duration::from_millis(ms)),
    }
}

/// **What the daemon's tick may spend waiting on `deliver`s, in all** (`wsp-665`).
/// The tick is one thread: every `compound-sup deliver` it asked sat out its
/// whole [`DELIVER_WAIT`] against a seat that was busy or slow to acknowledge,
/// and with four to eight asked per tick (every owed governor, every held
/// answer, every send-back) that was 74–132 s of a 190 s tick in the 11-17
/// `calls.jsonl`, all of it `compound-sup` children aging to 30.0 s. The ask
/// is unchanged and so is its answer: past the wait it is left pending and
/// asked again next tick under the same id, which the supervisor answers once.
/// A tick only waits less. `wsp tell` from a person's shell keeps the 30 s.
const TICK_DELIVER_BUDGET: Duration = Duration::from_secs(12);
/// One ask's share of it: long enough for a seat that is ready to say so.
const TICK_DELIVER_EACH: Duration = Duration::from_secs(5);
/// The least an ask waits once the budget is spent: the supervisor has to be
/// handed the words and answer a seat that took them at once.
const TICK_DELIVER_LEAST: Duration = Duration::from_millis(1500);

/// What is left of this tick's budget, `None` outside the daemon's tick.
static TICK_DELIVER: std::sync::Mutex<Option<Duration>> = std::sync::Mutex::new(None);

/// The daemon calls this as a tick begins (`Some`) and when it ends (`None`).
pub fn tick_deliver_budget(on: bool) {
    *TICK_DELIVER.lock().unwrap_or_else(|e| e.into_inner()) = on.then_some(TICK_DELIVER_BUDGET);
}

/// How long the next `deliver` may wait. Pure in what is left.
fn deliver_wait_of(left: Option<Duration>) -> Duration {
    match left {
        None => DELIVER_WAIT,
        Some(left) => DELIVER_WAIT.min(TICK_DELIVER_EACH).min(left.max(TICK_DELIVER_LEAST)),
    }
}

/// The wait for this ask, and what it took off the tick's budget once it is over.
fn deliver_waited<T>(ask: impl FnOnce(Duration) -> T) -> T {
    let left = *TICK_DELIVER.lock().unwrap_or_else(|e| e.into_inner());
    let began = Instant::now();
    // a process that capped its deliveries (wsp-666) keeps the cap
    let out = ask(deliver_wait_of(left).min(deliver_wait()));
    if let Some(left) = left {
        let mut g = TICK_DELIVER.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_some() {
            *g = Some(left.saturating_sub(began.elapsed()));
        }
    }
    out
}

/// The seat's file naming the delivery still being asked about, so the next
/// tick asks again under the same id (the sup answers an id asked again with
/// the same answer: exactly once).
const DELIVER_PENDING: &str = "deliver.pending";

/// A pending delivery is the same sentence asked again for this long; after it
/// a repeat of the words is a new tell.
const DELIVER_PENDING_FOR: i64 = 900;

/// The words a delivery that did not finish in time opens its refusal with,
/// moved down to `wsp-host` with the delivery (wsp-020).
pub use wsp_host::inproc::DELIVERY_PENDING;

/// The id this sentence is delivered under: the one still pending for the same
/// words, or a fresh one, recorded in the seat's directory.
fn delivery_id(dir: &Path, text: &str) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file = dir.join(DELIVER_PENDING);
    let rec = read_json(&file);
    let id = str_of(&rec, "id");
    let fresh = util::epoch_secs() - rec.get("at").and_then(Value::as_i64).unwrap_or(0) < DELIVER_PENDING_FOR;
    if !id.is_empty() && fresh && rec.get("text").and_then(Value::as_str) == Some(text) {
        return id;
    }
    let id = format!(
        "wsp-{}-{}-{}",
        std::process::id(),
        util::epoch_secs(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let _ = write_atomic(&file, &json!({ "id": id, "text": text, "at": util::epoch_secs() }).to_string());
    id
}

/// A sentence delivered under the id this seat keeps for it (wsp-477): the id
/// is wsp's and stays in the seat's directory until an answer is read, so a
/// wait that ran out is asked again, not doubled. The asking is the host's.
fn deliver_through(host: &dyn Host, session: &Path, dir: &Path, seat: &Seat, text: &str, wait: Duration) -> Result<Delivery> {
    let id = delivery_id(dir, text);
    let answer = host.deliver(session, &id, text, wait, seat);
    // An answer read is the end of that id: `failed` asked again under it would
    // only be answered `failed` again, and a retry is a new delivery.
    if matches!(answer, Ok(Delivery::Acked) | Err(Refusal::Failed(_)) | Err(Refusal::NoSeat(_))) {
        let _ = fs::remove_file(dir.join(DELIVER_PENDING));
    }
    answer
}

/// **A supervisor's refusal of a seat wsp reads as empty and at its prompt is
/// typed past, on the same tick** (`wsp-659`). `wsp-634` retried the `deliver`
/// verb, and a supervisor that refuses an idle seat with an empty composer (its
/// hook never fired, so it reads the seat as holding other text) answers
/// `failed … refused` to every retry: governors read "refused 5 ticks running"
/// and were ended for it, the sentence never landing. When the answer took
/// nothing (`refused`, `no composer`, in any of its spellings) and this backend's own read of the
/// composer is *empty* — not unreadable, not holding text — the sentence goes
/// by the typed path instead, and is `Unconfirmed` like any typed tell, so the
/// wake path waits for the turn that confirms it. Any other answer, or a
/// composer that holds text or cannot be read, is the supervisor's answer.
fn past_a_refusal(
    answer: Result<Delivery>,
    holds_text: impl FnOnce() -> Option<bool>,
    type_it: impl FnOnce() -> Result<()>,
) -> Result<Delivery> {
    match &answer {
        Err(Refusal::Failed(why)) if took_nothing(why) && holds_text() == Some(false) => match type_it() {
            Ok(()) => Ok(Delivery::Unconfirmed),
            Err(_) => answer,
        },
        _ => answer,
    }
}

/// Whether this seat's supervisor is one that can be asked to `deliver`: the
/// seat was spawned for it by this wsp. A record from before (every seat
/// already running) has no such mark and keeps the typed path.
fn delivers(rec: &Value) -> bool {
    rec.get("deliver").and_then(Value::as_bool) == Some(true)
}


impl Compound<'_> {
    /// The text in this seat's input box, for [`name_the_draft`] — `Some("")`
    /// for an empty box, `None` where `Place::typed` is.
    fn draft_in(&self, seat: &Seat) -> Option<String> {
        let rec = self.record(seat).ok()?;
        if rec.get("agent").map(|a| str_of(a, "kind")).as_deref() != Some("claude") {
            return None;
        }
        draft(&self.read_screen(seat).ok()?)
    }

    /// What `tell` and `deliver_order` share: the state check, the trust
    /// prompt, and for a seat that delivers the asked `deliver` (under `order`
    /// where the caller gave one, else under the seat's own pending id).
    fn tell_by(&self, seat: &Seat, text: &str, order: Option<&str>) -> Result<Delivery> {
        let state = self.state(seat)?;
        // `insist` is a person saying they have looked; see its own doc.
        if !self.insist && !state.will_take_a_prompt() {
            return Err(Refusal::NotReady(state));
        }
        if let Some(why) = self.at_trust_prompt(seat) {
            return Err(Refusal::Backend(why));
        }
        // wsp-477: a seat whose supervisor can be asked is told through its
        // `deliver` verb and the answer is read, not inferred from a
        // transcript. The supervisor owns the retry; `failed` is the only
        // thing wsp escalates.
        //
        // **Only a seat spawned for it** (`"deliver": true` in its record, set
        // by [`Place::start`] with `--agent`): a supervisor built before
        // compound-999 drops the connection on a `Deliver` frame, and every seat
        // already running is one. Those keep the typed path below.
        if let (true, Some(session), Ok(rec)) = (self.host.can_deliver(), self.socket_of(seat), self.record(seat)) {
            if delivers(&rec) {
                let dir = self.dir_of(seat)?;
                let answer = deliver_waited(|wait| match order {
                    Some(order) => self.host.deliver(&session, order, text, wait, seat),
                    None => deliver_through(self.host, &session, &dir, seat, text, wait),
                });
                let answer = past_a_refusal(answer, || self.typed(seat), || {
                    let socket = self.render_socket_of(seat).ok_or_else(|| Refusal::NoSeat(seat.clone()))?;
                    self.host.type_in(&socket, text)
                });
                return name_the_draft(answer, || self.draft_in(seat));
            }
        }
        if order.is_some() {
            return Err(Refusal::Unsupported("deliver under an order id to a seat whose supervisor cannot be asked"));
        }
        let socket = self.render_socket_of(seat).ok_or_else(|| Refusal::NoSeat(seat.clone()))?;
        self.host.type_in(&socket, text)?;
        // Typed rather than watched: this backend has not read the reply
        // that would let it say more, so `Unconfirmed` is the honest answer
        // `place.rs` asks for from a backend that delivered without seeing
        // whether a turn started.
        Ok(Delivery::Unconfirmed)
    }

    /// `compound-sup spawn --label <seat> …` and nothing more: no `--socket`
    /// (see the module docs on why this backend does not mint into
    /// compound's run directory itself), `program`/`args` as the pty's own
    /// child so a person who attaches sees exactly that and nothing wrapping
    /// it. Shared by [`Place::start`] (an agent's own argv) and
    /// [`Place::open`] (a bare shell, `compound-081`) — everything below the
    /// choice of what runs in the pty is one path, not two.
    ///
    /// Returns `rec` with `pid`/`socket`/`started_at` folded in; the caller
    /// decides what else changed (`start` adds `agent`; `open` adds
    /// nothing) and writes it.
    fn spawn_sup(&self, seat: &Seat, rec: &Value, key: Option<&wsp_host_api::Key>, program: &str, args: &[String]) -> Result<Value> {
        self.spawn_sup_by(seat, rec, key, program, args, &mut |s| self.host.spawn_session(s))
    }

    /// [`Compound::spawn_sup`], the host asked by `spawn`: its plain
    /// `seat.spawn`, or one carrying the first order ([`Place::start_ordered`]).
    fn spawn_sup_by(
        &self,
        seat: &Seat,
        rec: &Value,
        key: Option<&wsp_host_api::Key>,
        program: &str,
        args: &[String],
        spawn: &mut dyn FnMut(&host::Spawn<'_>) -> Result<host::Session>,
    ) -> Result<Value> {
        let mut rec = rec.clone();
        let key = match key {
            Some(key) => key.clone(),
            None => self.seat_key(seat, &mut rec),
        };
        let host::Session { pid, socket, render } = spawn(&Self::spawn_of(seat, &rec, Some(&key), program, args))?;
        rec["pid"] = json!(pid);
        rec["socket"] = json!(socket.display().to_string());
        if let Some(render) = &render {
            rec["render_socket"] = json!(render.display().to_string());
        }
        rec["started_at"] = json!(util::now_iso());
        Ok(rec)
    }

    /// The key a spawn with no row goes out under: the seat's own id, at the
    /// attempt after the last one the seat's record spent (0 for `open`'s bare
    /// terminal, 1 for the agent `start` puts in its place, 2 for a re-start
    /// after a start judged dead). Spent on the record before it is sent, so a
    /// spawn that fails or is refused is never sent again under the same key.
    /// A fixed `(seat, 0)` was `open`'s key as well as the agent's: under the
    /// manager the shell's run spent it, and every keyless start after it (a
    /// verifier, a rotation onto a project) was refused as a key that had
    /// already run (wsp-040).
    fn seat_key(&self, seat: &Seat, rec: &mut Value) -> wsp_host_api::Key {
        let on_disk = self.record(seat).ok();
        let spent = [Some(&*rec), on_disk.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("attempt").and_then(|a| a.as_u64()))
            .max();
        let attempt = spent.map_or(0, |a| a as u32 + 1);
        rec["attempt"] = json!(attempt);
        if let (Some(mut was), Ok(dir)) = (on_disk, self.dir_of(seat)) {
            was["attempt"] = json!(attempt);
            let _ = write_atomic(&dir.join(SEAT_FILE), &was.to_string());
        }
        wsp_host_api::Key { row: wsp_host_api::RowId(seat.to_string()), attempt }
    }

    /// A local seat's start, its `seat.spawn` asked of the host by `spawn`:
    /// a seat holding a bare terminal is replaced, one holding an agent is
    /// refused, and the record is written with what started.
    fn start_here(
        &self,
        seat: &Seat,
        rec: Value,
        agent: &Agent,
        spawn: &mut dyn FnMut(&host::Spawn<'_>) -> Result<host::Session>,
    ) -> Result<()> {
        let dir = self.dir_of(seat)?;
        if let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()) {
            if self.host.alive(&[pid as u32]).contains(&(pid as u32)) {
                if rec.get("agent").is_some() {
                    return Err(Refusal::Backend(format!("{seat} already has a session running")));
                }
                self.end_process(seat, &rec)?;
            }
        }
        let (program, argv) = (crate::place::above().command)(&agent.kind, &agent.args);
        let mut told = rec.clone();
        told["agent"] = json!({ "kind": agent.kind });
        let mut rec = self.spawn_sup_by(seat, &told, agent.key.as_ref(), &program, &argv, spawn)?;
        rec["agent"] = json!({ "kind": agent.kind, "name": agent.name, "args": agent.args });
        // wsp-477: spawned with `--agent`, so its supervisor can `deliver`.
        if reads_kind(&agent.kind) {
            rec["deliver"] = json!(true);
        }
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string())
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        Ok(())
    }

    /// The `seat.spawn` this seat's record asks for: the agent kind its
    /// supervisor reads a composer for, its directory and its environment.
    fn spawn_of<'s>(seat: &'s Seat, rec: &Value, key: Option<&wsp_host_api::Key>, program: &'s str, args: &'s [String]) -> host::Spawn<'s> {
        let env: BTreeMap<String, String> = rec
            .get("env")
            .and_then(|e| e.as_object())
            .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
            .unwrap_or_default();
        host::Spawn {
            key: key.cloned().unwrap_or_else(|| wsp_host_api::Key { row: wsp_host_api::RowId(seat.to_string()), attempt: 0 }),
            seat: seat.as_str(),
            kind: rec.get("agent").map(|a| str_of(a, "kind")),
            cwd: str_of(rec, "cwd"),
            env,
            program,
            args,
        }
    }

    /// Start an agent on the machine this seat's record names
    /// (`docs/remote-agents.md` sections 2 and 8): `compound-sup spawn` over ssh
    /// with the token exported on the far side, the handshake read from the ssh
    /// stdout, one forward each for the `.sock` and the `.rsock`, and the machine
    /// and both forwarded sockets recorded on the seat. Nothing is left half
    /// done: a forward that fails closes the far session it follows.
    fn start_far(&self, seat: &Seat, rec: &Value, agent: &Agent) -> Result<()> {
        let dir = self.dir_of(seat)?;
        let name = str_of(rec, "machine");
        let bad = |why: String| Refusal::Backend(why);
        let m = self.far.machine(&name).ok_or_else(|| bad(format!("no machine `{name}` — wsp machine add")))?;
        if m.exec_dir.is_empty() {
            return Err(bad(format!("{name} has no exec_dir — wsp machine set {name} exec_dir=…")));
        }
        let cwd = str_of(rec, "cwd");
        if cwd.is_empty() {
            return Err(bad(format!("{seat} has no tree on {name}: the copy up was not done")));
        }
        let up = |key: &str| {
            let s = str_of(rec, key);
            !s.is_empty() && self.host.answers(Path::new(&s))
        };
        if rec.get("agent").is_some() && up("socket") {
            return Err(bad(format!("{seat} already has a session running")));
        }
        let (sock, rsock) = crate::far::forward_paths(&self.far.dir(&m), seat.as_str());
        for p in [&sock, &rsock] {
            if p.as_os_str().len() > crate::tunnel::SUN_PATH_MAX {
                return Err(bad(format!(
                    "{} is {} bytes and a unix socket may be {} — the state directory is too deep to forward into",
                    p.display(),
                    p.as_os_str().len(),
                    crate::tunnel::SUN_PATH_MAX
                )));
            }
        }
        let env: BTreeMap<String, String> = rec
            .get("env")
            .and_then(|e| e.as_object())
            .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
            .unwrap_or_default();
        // The gate goes in before the agent does: a session that starts first has
        // had tool calls the hook never saw.
        self.install_gate(&m, seat, &cwd)?;
        // So is the folder trust: trust on `<exec_dir>/trees` did not cover the
        // row's tree, and an untrusted folder stops claude at "No, exit" (wsp-445).
        self.far
            .run(&m, &crate::far::trust_command(&m, "add", &cwd), crate::far::SESSION_TIMEOUT)
            .map_err(|f| bad(format!("trusting {cwd} on {name}: {}", f.why())))?;
        let (program, argv) = (crate::place::above().command)(&agent.kind, &agent.args);
        let cmd = crate::far::seat_command(&m, seat.as_str(), &cwd, &env, &program, &argv);
        let out = self
            .far
            .run(&m, &cmd, crate::far::SESSION_TIMEOUT)
            .map_err(|f| bad(format!("compound-sup on {name}: {}", f.why())))?;
        let hs = crate::far::parse_handshake(&out)
            .ok_or_else(|| bad(format!("compound-sup on {name} never announced a socket: {}", out.trim())))?;

        let close = |why: String| {
            let _ = self.far.run(&m, &format!("{} close {}", crate::far::quote(&crate::far::sup_of(&m)), crate::far::quote(&hs.socket)), crate::far::SESSION_TIMEOUT);
            bad(why)
        };
        if let Err(e) = self.far.forward(&m, "forward", &sock, &hs.socket) {
            return Err(close(format!("forwarding {name}'s {} failed: {e}", hs.socket)));
        }
        if let Some(render) = &hs.render {
            if let Err(e) = self.far.forward(&m, "forward", &rsock, render) {
                let _ = self.far.forward(&m, "cancel", &sock, &hs.socket);
                self.host.unlink(&sock);
                return Err(close(format!("forwarding {name}'s {render} failed: {e}")));
            }
        }

        let mut rec = rec.clone();
        rec["socket"] = json!(sock.display().to_string());
        rec["far_socket"] = json!(hs.socket);
        match &hs.render {
            Some(render) => {
                rec["render_socket"] = json!(rsock.display().to_string());
                rec["far_render_socket"] = json!(render);
            }
            None => {}
        }
        if let Some(pid) = hs.pid {
            rec["far_pid"] = json!(pid);
        }
        if let Some(sup) = hs.sup {
            rec["far_sup"] = json!(sup);
        }
        rec["started_at"] = json!(util::now_iso());
        rec["agent"] = json!({ "kind": agent.kind, "name": agent.name, "args": agent.args });
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string()).map_err(|e| Refusal::Backend(e.to_string()))?;
        Ok(())
    }

    /// Write the PreToolUse gate into the far tree's `.claude/settings.json`
    /// (`docs/remote-agents.md` section 3): `compound-remote gate --exec-dir <d>
    /// --seat <s> --lease <machine lease>`, merged into whatever the tree
    /// already has there, with a hook timeout above the lease. The seat's first
    /// lease goes down in the same command (wsp-562), so its first tool call
    /// finds a fresh one rather than waiting for a beat that is minutes off.
    /// Refuses rather than start an agent that nothing gates.
    fn install_gate(&self, m: &crate::model::Machine, seat: &Seat, cwd: &str) -> Result<()> {
        let bad = |why: String| Refusal::Backend(why);
        let file = format!("{}/.claude/settings.json", cwd.trim_end_matches('/'));
        let there = self
            .far
            .run(m, &format!("cat {} 2>/dev/null || true", crate::far::quote(&file)), crate::far::SESSION_TIMEOUT)
            .map_err(|f| bad(format!("reading {file} on {}: {}", m.name, f.why())))?;
        let text = crate::far::settings_with_gate(&there, m, seat.as_str()).map_err(|why| bad(format!("{}: {why}", m.name)))?;
        let write = format!(
            "{}mkdir -p {} && cat > {} <<'WSP_GATE_EOF'\n{text}\nWSP_GATE_EOF",
            crate::far::first_lease_command(m, seat.as_str()),
            crate::far::quote(&format!("{}/.claude", cwd.trim_end_matches('/'))),
            crate::far::quote(&file),
        );
        self.far
            .run(m, &write, crate::far::SESSION_TIMEOUT)
            .map_err(|f| bad(format!("writing the gate to {file} on {}: {}", m.name, f.why())))?;
        Ok(())
    }

    /// End a far seat's session: `compound-sup close <far socket>` on the
    /// machine, its lease file removed, then both forwards dropped and the local
    /// sockets swept. A link that is down refuses (the session would be left
    /// running unseen) and says what it left: the seat is kept, so ending it
    /// again once the link is back finishes the job. A command that ran and
    /// failed means the session is already gone.
    fn stop_far(&self, seat: &Seat, rec: &Value) -> Result<()> {
        let name = str_of(rec, "machine");
        let m = self.far.machine(&name);
        let far_sock = str_of(rec, "far_socket");
        let id = seat.as_str().to_string();
        if let Some(m) = &m {
            let left = |why: &str| {
                let mut left = Vec::new();
                if !far_sock.is_empty() {
                    left.push(format!("its session {far_sock}"));
                }
                if !id.is_empty() {
                    left.push(format!("its lease file {}/lease/{id}", m.exec_dir.trim_end_matches('/')));
                }
                Refusal::Backend(format!(
                    "cannot reach {name} to close its session: {why} — left on {name}: {}; the seat is kept, so ending it again once {name} answers closes them",
                    if left.is_empty() { "nothing recorded".to_string() } else { left.join(" and ") }
                ))
            };
            if !far_sock.is_empty() {
                let cmd = format!("{} close {}", crate::far::quote(&crate::far::sup_of(m)), crate::far::quote(&far_sock));
                if let Err(crate::far::Failed::Link(why)) = self.far.run(m, &cmd, crate::far::SESSION_TIMEOUT) {
                    return Err(left(&why));
                }
            }
            if !id.is_empty() && !m.exec_dir.is_empty() {
                if let Err(crate::far::Failed::Link(why)) = self.far.run(m, &crate::far::drop_lease_command(m, &id), crate::far::SESSION_TIMEOUT) {
                    return Err(left(&why));
                }
            }
        }
        for (local, far) in [("socket", "far_socket"), ("render_socket", "far_render_socket")] {
            let (local, far) = (str_of(rec, local), str_of(rec, far));
            if local.is_empty() {
                continue;
            }
            if let Some(m) = &m {
                let _ = self.far.forward(m, "cancel", Path::new(&local), &far);
            }
            self.host.unlink(Path::new(&local));
        }
        Ok(())
    }

    /// `SIGTERM` the group, wait, `SIGKILL` if it lingers, and sweep the
    /// socket once nothing answers it. The shared tail of ending whatever
    /// currently occupies a seat's pty — whether the SEAT is going with it
    /// ([`Place::stop`]) or is about to hold a fresh session in its place
    /// ([`Place::start`] replacing a bare terminal, `compound-081`).
    ///
    /// What ended, as the host says: the manager's own pids under
    /// `WSP_HOST=manager`. A host that cannot say (silent, or not running the
    /// seat) is the error, before anything of the seat is cleared.
    fn end_process(&self, seat: &Seat, rec: &Value) -> Result<Vec<u32>> {
        let mut ended = Vec::new();
        if let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32) {
            let stop = host::Stop { group: pid, watch: Some(pid), grace: self.linger, poll: self.poll, clock: self.clock };
            ended = self.host.end(seat.as_str(), stop)?.pids;
        }
        // A signalled `compound-sup` skips its own unlink, and so does a
        // signalled renderer: each leaves a headstone the host clears.
        if let Some(socket) = self.socket_of(seat) {
            self.host.clear_socket(&socket, &["label"]);
        }
        if let Some(render) = self.render_socket_of(seat) {
            self.host.clear_socket(&render, &[]);
        }
        Ok(ended)
    }
}

/// Whether a `SessionEnd` is the agent leaving, and not `/clear` or a resume
/// that goes on in the same process (`wsp-637`).
pub(crate) fn leaves_for_good(payload: &Value) -> bool {
    !matches!(str_of(payload, "reason").as_str(), "clear" | "resume" | "compact")
}

/// The forwards the seats on `machine` should have, as `(local socket, far
/// socket)`, read from the seat records. What the tunnel supervisor re-adds when
/// a machine's ControlMaster comes back: a master that died took its forwards
/// with it (`docs/remote-agents.md` section 2).
pub fn forwards_for(store: &Store, machine: &str) -> Vec<(PathBuf, String)> {
    let root = store.state.join(SEATS);
    let mut out = Vec::new();
    for e in fs::read_dir(&root).into_iter().flatten().flatten() {
        let rec = read_json(&e.path().join(SEAT_FILE));
        if str_of(&rec, "machine") != machine {
            continue;
        }
        for (local, far) in [("socket", "far_socket"), ("render_socket", "far_render_socket")] {
            let (local, far) = (str_of(&rec, local), str_of(&rec, far));
            if !local.is_empty() && !far.is_empty() {
                out.push((PathBuf::from(local), far));
            }
        }
    }
    out.sort();
    out
}

/// The machine a seat's record names: `""` for a seat on this machine, or one
/// wsp has no record of.
pub fn seat_machine(store: &Store, seat: &str) -> String {
    str_of(&read_json(&store.state.join(SEATS).join(seat).join(SEAT_FILE)), "machine")
}

/// Whether a compound seat still has a supervisor behind it (`wsp-649`): its
/// record is there and the pid it names is alive. A seat on another machine is
/// taken to have one — its link is the question, and that is not this one.
pub fn supervised(store: &Store, seat: &str) -> bool {
    let rec = read_json(&store.state.join(SEATS).join(seat).join(SEAT_FILE));
    if !str_of(&rec, "machine").is_empty() {
        return true;
    }
    match rec.get("pid").and_then(|p| p.as_u64()) {
        Some(pid) if pid > 0 => host::LOCAL.alive(&[pid as u32]).contains(&(pid as u32)),
        _ => false,
    }
}

/// Every seat of this store that names `machine`, started or not: what owns a
/// session or a lease file on that machine.
pub fn seats_on(store: &Store, machine: &str) -> Vec<String> {
    let root = store.state.join(SEATS);
    let mut out = Vec::new();
    for e in fs::read_dir(&root).into_iter().flatten().flatten() {
        if str_of(&read_json(&e.path().join(SEAT_FILE)), "machine") == machine {
            out.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

/// The far seats on `machine`, as the beat sends to them: every seat record that
/// names this machine and has a session. Every one is beaten on the machine's
/// **controller** socket ([`crate::far::controller_local`]), never on its own
/// session socket, which is compound-sup's and does not speak the lease line
/// (wsp-475). A local seat has no `machine` and is never in it.
pub fn beat_seats(store: &Store, machine: &str) -> Vec<crate::far::BeatSeat> {
    let root = store.state.join(SEATS);
    let controller = crate::far::controller_local(store.machine_socket(machine).parent().unwrap_or(&store.state), machine);
    let held = store.held_seats();
    let mut out = Vec::new();
    for e in fs::read_dir(&root).into_iter().flatten().flatten() {
        let rec = read_json(&e.path().join(SEAT_FILE));
        let (socket, id) = (str_of(&rec, "socket"), e.file_name().to_string_lossy().into_owned());
        if str_of(&rec, "machine") != machine || socket.is_empty() {
            continue;
        }
        let started = str_of(&rec, "started_at");
        out.push(crate::far::BeatSeat {
            held: held.contains(&id),
            age: (!started.is_empty()).then(|| util::since(&started)),
            socket: controller.clone(),
            id,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Whether compound can tell what an agent of this kind is doing, moved down to
/// `wsp-host`, whose spawn passes the kind on (wsp-020).
pub use wsp_host::inproc::reads_kind;


impl Place for Compound<'_> {
    /// Mints the seat's record, then — `compound-081` — puts a real shell in
    /// its pty, the same way backend's `open` leaves a person looking at a
    /// prompt rather than a blank pane: **agents and command lines**, Ed's
    /// own words for what this port has to cover, and a bare `wsp spawn`
    /// giving a task record with nothing to attach to was the half that
    /// answered neither.
    ///
    /// **Best effort, deliberately.** A seat is real the moment its record
    /// is — every other verb here already depends on that being true before
    /// any pty exists — so a `compound-sup` this machine cannot find (no
    /// binary on `PATH`, no `$COMPOUND_SUP`) fails the SHELL, not the open:
    /// the seat comes back exactly as it did before this row, holding no
    /// session, `State::Empty` for the reason its own doc always named
    /// ("a seat opened and never started") rather than for the newer one.
    /// [`Place::start`] still refuses loudly when compound-sup is missing,
    /// because starting an AGENT with nothing to run it in is the failure a
    /// caller asked to hear about.
    fn open(&self, order: &Order) -> Result<Seat> {
        let seat = self.mint()?;
        let dir = self.dir_of(&seat)?;
        let mut env: BTreeMap<String, String> = order.env.clone();
        env.insert(place::SEAT_ENV.to_string(), seat.to_string());
        if let Some(machine) = &order.on {
            // A seat on another machine is a record here and nothing more until
            // `start`: no bare shell is run over the link, and `cwd` is the FAR
            // path (the tree was copied up to it), so it is not expanded here.
            let rec = json!({
                "label": order.label,
                "cwd": order.cwd,
                "env": env,
                "machine": machine,
                "opened_at": util::now_iso(),
            });
            write_atomic(&dir.join(SEAT_FILE), &rec.to_string()).map_err(|e| Refusal::Backend(e.to_string()))?;
            return Ok(seat);
        }
        let rec = json!({
            "label": order.label,
            "cwd": order.cwd.as_deref().map(|c| util::expand(c).display().to_string()),
            "env": env,
            "opened_at": util::now_iso(),
        });
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string())
            .map_err(|e| Refusal::Backend(e.to_string()))?;

        // Under the manager, a seat an agent goes into next starts that agent
        // as its one epoch: a shell first was epoch 1, stopped for the agent's
        // epoch 2, and every healthy start read as a restart (wsp-079).
        if order.agent_next && self.host.hears_orders() {
            return Ok(seat);
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        if let Ok(rec) = self.spawn_sup(&seat, &rec, None, &shell, &[]) {
            let _ = write_atomic(&dir.join(SEAT_FILE), &rec.to_string());
        }
        Ok(seat)
    }

    fn here(&self) -> Option<Seat> {
        place::seat_from_env()
    }

    /// An agent's own argv as the pty's child. A seat already holding a
    /// BARE terminal — `compound-081`'s `open` — is replaced rather than
    /// refused: that pty was never an agent's to begin with, so putting one
    /// there is the same act `start` always was, not a second session
    /// beside the first. A seat already holding an AGENT is still refused,
    /// unchanged.
    fn start(&self, seat: &Seat, agent: &Agent) -> Result<()> {
        let rec = self.record(seat)?;
        if !str_of(&rec, "machine").is_empty() {
            return self.start_far(seat, &rec, agent);
        }
        self.start_here(seat, rec, agent, &mut |s| self.host.spawn_session(s))
    }

    /// `seat.spawn` with the first order, and its answer heard as the
    /// manager's events (4.3): a host with no events (in-process), or a far
    /// seat (M2's), is `None` and nothing is started.
    fn start_ordered(&self, seat: &Seat, agent: &Agent, order: &crate::place::FirstOrder, wait: Duration) -> Option<Result<crate::place::Heard>> {
        if !self.host.hears_orders() {
            return None;
        }
        let rec = match self.record(seat) {
            Ok(rec) => rec,
            Err(e) => return Some(Err(e)),
        };
        if !str_of(&rec, "machine").is_empty() {
            return None;
        }
        let mut heard = crate::place::Heard::Unanswered;
        let started = self.start_here(seat, rec, agent, &mut |s| {
            let (session, h) = self.host.spawn_ordered(s, order, wait)?;
            heard = h;
            Ok(session)
        });
        Some(started.map(|()| heard))
    }

    fn await_order(&self, _seat: &Seat, order: &crate::place::OrderId, wait: Duration) -> Option<Result<crate::place::Heard>> {
        self.host.hears_orders().then(|| self.host.await_order(order, wait))
    }

    /// A local seat's start under a host with events, judged by them: a far
    /// seat (M2's) has none here, and is polled as before.
    fn await_up(&self, seat: &Seat, by_turn: bool, wait: Duration) -> Option<Result<crate::place::Up>> {
        if !self.host.hears_orders() {
            return None;
        }
        if self.record(seat).is_ok_and(|rec| !str_of(&rec, "machine").is_empty()) {
            return None;
        }
        Some(self.host.await_up(seat.as_str(), by_turn, wait))
    }

    /// Paste the sentence, press Enter, disconnect — see the module docs'
    /// "`tell`, and why it need not hold a pipe open".
    ///
    /// **Render socket, not durable, since `compound-101`'s split**
    /// (`compound-136`). `Attach`/`Input`/`Key`/`Detach` are `Body` variants,
    /// answered on `<name>.rsock` by `compound-render` — the durable
    /// `<name>.sock` speaks only `DurableBody` (`Vitals`/`Close`) and would
    /// refuse an `Attach` frame as an unknown shape rather than type into
    /// anything. A seat with no renderer up has nothing to type into either
    /// way; that failure belongs to the dial below, not to a silent no-op.
    fn tell(&self, seat: &Seat, text: &str) -> Result<Delivery> {
        self.tell_by(seat, text, None)
    }

    /// [`Place::tell`] under the store's order id: the same path, `order`
    /// handed to the host as the delivery id, and no `deliver.pending` read or
    /// written. A seat that cannot be asked to `deliver` has no id to carry and
    /// is [`Refusal::Unsupported`], not typed at.
    fn deliver_order(&self, seat: &Seat, order: &str, text: &str) -> Result<Delivery> {
        self.tell_by(seat, text, Some(order))
    }

    /// Press submit on a seat whose composer is sitting on a work order.
    ///
    /// **The second half of `compound-174`, and it was not the same fault as
    /// the first.** This backend had no `nudge` at all, so it took the trait's
    /// default and answered `Unsupported` — and `hand_over` treats that as the
    /// end of the rescue and fails the spawn with *"{seat} took the work order
    /// and started nothing"*. So every `wsp spawn --agent` into a compound seat
    /// reported a turn that had not started, on a seat where `tell` had already
    /// typed **and** submitted the order.
    ///
    /// Implementing it is the right repair rather than teaching `hand_over` to
    /// tolerate a missing press, because that loop's strictness is deliberate
    /// and argued at length: a spawn wrongly called failed is recoverable, one
    /// wrongly called succeeded is not. A backend that can be pressed should
    /// be pressed, and the second Enter is exactly the rescue the loop wants to
    /// try — a folder-trust modal holding the keyboard with the order unsent
    /// behind it is the case it exists for, and on this backend it was
    /// unreachable.
    ///
    /// No `Attach`, for the same reason and by the same argument as
    /// [`crate::host::type_and_submit`] — this is the same conversation with nothing typed
    /// in it, so it inherits the `compound-117` answer rather than keeping a
    /// second copy of the attach the first one dropped.
    fn nudge(&self, seat: &Seat) -> Result<()> {
        let socket = self.render_socket_of(seat).ok_or_else(|| Refusal::NoSeat(seat.clone()))?;
        self.host.press_enter(&socket)
    }

    /// Claude Code's prompt, read off the renderer's screen — see [`composer`].
    /// Only for `claude`: opencode's prompt is another shape, and a reading
    /// this cannot vouch for is `None`, which [`crate::place::submit`] takes
    /// as "do not press".
    fn typed(&self, seat: &Seat) -> Option<bool> {
        let rec = self.record(seat).ok()?;
        // Claude-only: the composer is Claude Code's prompt shape.
        if rec.get("agent").map(|a| str_of(a, "kind")).as_deref() != Some("claude") {
            return None;
        }
        composer(&self.read_screen(seat).ok()?)
    }

    /// End the session and let the seat go. `SIGTERM` the group (which is
    /// the compound-sup pid itself — see the module docs), a short wait for
    /// its own `SessionEnd`-on-the-way-out the same as `place_super`
    /// measures, then `SIGKILL`; the directory goes either way.
    fn stop(&self, seat: &Seat) -> Result<()> {
        self.end(seat).map(|_| ())
    }

    fn hosted(&self) -> bool {
        self.host.hears_orders()
    }

    fn end(&self, seat: &Seat) -> Result<Vec<u32>> {
        let rec = self.record(seat)?;
        let dir = self.dir_of(seat)?;
        if !str_of(&rec, "machine").is_empty() {
            self.stop_far(seat, &rec)?;
            self.take_away(&dir)?;
            return Ok(Vec::new());
        }
        // `compound-031` is where the general case (a session whose host is
        // gone and nobody asked it to stop) is filed; this is the one seat
        // this call just ended.
        let ended = self.end_process(seat, &rec)?;
        self.host.bank(&dir);
        self.take_away(&dir)?;
        Ok(ended)
    }

    fn state(&self, seat: &Seat) -> Result<State> {
        let rec = self.record(seat)?;
        let pids: Vec<u32> =
            rec.get("pid").and_then(|p| p.as_u64()).map(|p| vec![p as u32]).unwrap_or_default();
        Ok(self.state_of(seat, &rec, &self.host.alive(&pids)))
    }

    fn quiet_since(&self, seat: &Seat) -> Option<i64> {
        self.host.quiet_since(&self.dir_of(seat).ok()?)
    }

    fn stopped(&self, seat: &Seat) -> Option<crate::place::Stopped> {
        crate::place::stopped_of(&self.said(seat))
    }

    /// `compound-sup` leads the group and the agent is its pty's child, so the
    /// tree under the recorded pid is the agent and its work.
    fn pids(&self, seat: &Seat) -> Option<Vec<u32>> {
        Some(self.host.tree_of(self.root(seat)?))
    }

    fn root(&self, seat: &Seat) -> Option<u32> {
        self.record(seat).ok()?.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32)
    }

    fn census(&self) -> Result<Census> {
        // One machine, and it is this one — a compound session is a local
        // pty and there is no fan-out here for `Census` to speak for.
        let mut census = Census::heard("", self.survey());
        // A far machine that has gone silent is filed as silent, beside the
        // local answer: its seats read `unreachable` in the rows above, and a
        // reap asks `Census::answered` of the machine before it believes an
        // absence (`docs/remote-agents.md` section 3).
        let mut silent: BTreeSet<String> = BTreeSet::new();
        for id in self.ids() {
            let rec = self.record(&Seat::new(id)).unwrap_or(json!({}));
            let machine = str_of(&rec, "machine");
            if machine.is_empty() || silent.contains(&machine) {
                continue;
            }
            // Silent by its lease, or its forward refusing behind a link that is
            // down: either way a reap has no empty answer to believe.
            let socket = str_of(&rec, "socket");
            let cut = !socket.is_empty() && !self.host.answers(Path::new(&socket)) && self.far.link_down(&machine);
            if cut || self.far.silent_for(&machine).is_some() {
                silent.insert(machine);
            }
        }
        for machine in silent {
            census = census.and(Census::silent(&machine, Refusal::Unreachable("no answer from the far seat's machine".into())));
        }
        Ok(census)
    }

    /// Poll, and say what changed — identical shape to
    /// [`crate::place_super::Supervisor::watch`], because the observable is
    /// the same directory-of-small-files-plus-`ps`, just a different
    /// directory.
    fn watch(&self, f: &mut dyn FnMut(Event) -> bool) -> Result<()> {
        let mut was: BTreeMap<Seat, State> =
            self.survey().into_iter().map(|s| (s.seat, s.state)).collect();
        loop {
            self.clock.rest(self.poll);
            let now: BTreeMap<Seat, State> =
                self.survey().into_iter().map(|s| (s.seat, s.state)).collect();
            let mut events: Vec<Event> = Vec::new();
            for (seat, state) in &now {
                match was.get(seat) {
                    None => {
                        events.push(Event::Opened(seat.clone()));
                        if state.is_running() {
                            events.push(Event::Started(seat.clone()));
                        }
                    }
                    Some(before) if before == state => {}
                    Some(before) => events.push(match (before.is_running(), state.is_running()) {
                        (false, true) => Event::Started(seat.clone()),
                        (true, false) => Event::Stopped(seat.clone()),
                        _ => Event::Moved(seat.clone(), *state),
                    }),
                }
            }
            for seat in was.keys() {
                if !now.contains_key(seat) {
                    events.push(Event::Closed(seat.clone()));
                }
            }
            was = now;
            for e in events {
                if !f(e) {
                    return Ok(());
                }
            }
        }
    }
}

/// Stops a real compound seat when the test that opened one goes out of scope.
///
/// A test that calls [`Compound::open`] has spawned a `compound-sup` holding a
/// pty, and that process outlives the temp directory the test is cleaned up
/// with: the state directory goes, the supervisor does not, and it sits there
/// holding ~10MB until the machine is rebooted. 256 of them had accumulated
/// when this was written, from three tests in three modules, and the first
/// symptom was not memory — it was `start` timing out in the very test that
/// leaks, because the machine was near its process limit.
///
/// An explicit `stop` at the end of a test is not enough and is the reason
/// this is a guard: a leak happens precisely when an assert fails, so the
/// cleanup has to run on unwind. `place_compound`'s own `Scratch` already
/// does this for tests inside this module; this is the same promise for the
/// tests outside it, which had no way to make it.
///
/// Declare it AFTER the env guard so it drops BEFORE one — the seat has to be
/// stopped while the state directory naming it still exists.
#[cfg(any(test, feature = "test-support"))]
pub struct StopsOnDrop {
    seat: Seat,
    // The ROOT, pinned when the guard is made, not read again at drop.
    // `Compound::new()` resolves it from the environment, and `util::isolated`
    // moves the environment under a process-wide lock it releases between
    // tests — so a guard that called `new()` in its own `drop` resolved
    // whichever root happened to be current then, which under a parallel run
    // is often another test's. That is the difference between this leaking
    // nothing in a single test and leaking six per full suite.
    root: PathBuf,
    pid: Option<u32>,
}

#[cfg(any(test, feature = "test-support"))]
impl StopsOnDrop {
    pub fn new(seat: Seat) -> StopsOnDrop {
        let place = Compound::new();
        // The pid as well as the seat, read NOW. `stop` is the right verb and
        // is tried first, but it can only work through the seat record, and a
        // test is free to overwrite that record (several do, to exercise
        // `state`) or to leave the environment pointing elsewhere by the time
        // this drops. The pid cannot be falsified by either, so it is the
        // backstop — and a leaked supervisor holds a pty, which this machine
        // has 511 of (`compound-127`).
        let pid = place.record(&seat).ok().and_then(|r| r["pid"].as_u64()).map(|p| p as u32);
        StopsOnDrop { seat, root: place.root, pid }
    }
}

/// Ends a `compound-sup` by the pid it really has, for the tests that make
/// [`StopsOnDrop`] impossible.
///
/// A test exercising [`Compound::state`] has to put pids in the seat record
/// that nothing holds — a live one with no agent, then one no process could
/// plausibly be. Doing so OVERWRITES the record `stop` reads, so the seat
/// becomes unstoppable by its own backend the moment the test scribbles on it:
/// `end_process` signals a pid nothing holds and the real supervisor lives on,
/// holding its pty, past the temp directory and past the test run.
///
/// That is what leaked 275 supervisors and 480 of this machine's 511 ptys
/// (`compound-127`). The remedy is to read the pid the supervisor really has
/// BEFORE the record is rewritten, and end that process directly rather than
/// through a record the test is about to falsify.
#[cfg(any(test, feature = "test-support"))]
pub struct EndsOnDrop(pub u32);

#[cfg(any(test, feature = "test-support"))]
impl Drop for EndsOnDrop {
    fn drop(&mut self) {
        signal_group(self.0, "KILL");
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for StopsOnDrop {
    fn drop(&mut self) {
        // Best effort by construction: a seat whose supervisor already exited
        // is the ordinary case at the end of a despawn test, and a cleanup
        // that panicked would turn a passing test red for tidying up.
        let _ = Compound::at(self.root.clone()).stop(&self.seat);
        if let Some(pid) = self.pid {
            if alive(&[pid]).contains(&pid) {
                signal_group(pid, "KILL");
            }
        }
    }
}

/// The supervisors a test starts, moved down to `wsp-host` with the spawn that
/// ties them to the test (wsp-020).
#[cfg(any(test, feature = "test-support"))]
pub use wsp_host::test_sups;

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_host_api::OrderFail;

    /// The manager's wording of a no-composer refusal (`host_tcp::fail_word`)
    /// is typed past on an empty composer, as `refused` and `no composer` are.
    #[test]
    fn a_manager_no_composer_refusal_is_typed_past() {
        let typed = std::cell::Cell::new(0);
        let type_it = || {
            typed.set(typed.get() + 1);
            Ok(())
        };
        let why = format!("cpd-1: {}", crate::host_tcp::fail_word(OrderFail::NoComposer));
        assert_eq!(why, "cpd-1: no-composer", "the manager's wording");
        assert_eq!(past_a_refusal(Err(Refusal::Failed(why)), || Some(false), type_it), Ok(Delivery::Unconfirmed));
        assert_eq!(typed.get(), 1);
    }

    /// `wsp-602`: a caller whose `PATH` lacks `~/.local/bin` still finds the
    /// supervisor there, and a set-but-missing `$COMPOUND_SUP` falls through.
    #[test]
    fn compound_sup_is_found_off_path_and_a_missing_variable_falls_through() {
        let d = std::env::temp_dir().join(format!("wsp602-sup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let home = d.join("home");
        let beside = d.join("beside");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::create_dir_all(&beside).unwrap();
        let on_path = d.join("onpath");
        std::fs::create_dir_all(&on_path).unwrap();
        let none = || Some(d.join("empty").into_os_string());
        let missing = Some(d.join("nope").into_os_string());

        assert_eq!(find_sup(None, none(), Some(home.clone()), Some(beside.clone())), None);

        std::fs::write(beside.join("compound-sup"), "").unwrap();
        assert_eq!(find_sup(None, none(), Some(home.clone()), Some(beside.clone())), Some(beside.join("compound-sup")));

        std::fs::write(home.join(".local/bin/compound-sup"), "").unwrap();
        assert_eq!(
            find_sup(missing.clone(), none(), Some(home.clone()), Some(beside.clone())),
            Some(home.join(".local/bin/compound-sup")),
            "a set-but-missing variable falls through, and ~/.local/bin comes before beside wsp"
        );

        std::fs::write(on_path.join("compound-sup"), "").unwrap();
        assert_eq!(
            find_sup(missing, Some(on_path.clone().into_os_string()), Some(home), Some(beside)),
            Some(on_path.join("compound-sup"))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Wait for something a real process does in its own time. Same shape
    /// as `place_super::tests::until`, its own copy for the same reason that
    /// one is not shared: a hang-guard belongs beside what it is guarding.
    fn until(what: impl Fn() -> bool) -> bool {
        for _ in 0..500 {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let root = std::env::temp_dir()
                .join(format!("wsp-cpd-{name}-{}-{}", std::process::id(), util::epoch_nanos()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Scratch { root }
        }
        fn place(&self) -> Compound<'static> {
            Compound { poll: Duration::from_millis(10), ..Compound::at(self.root.clone()) }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let sup = Compound::at(self.root.clone());
            for id in sup.ids() {
                let _ = sup.stop(&Seat::new(id));
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// A `sup` that is a shell script: it reads the text from stdin, records
    /// the id it was asked under, and then runs `answer`.
    fn script_sup(scratch: &Scratch, answer: &str) -> (PathBuf, PathBuf, PathBuf) {
        let sup = scratch.root.join("sup");
        let said = scratch.root.join("said");
        let asked = scratch.root.join("asked");
        let body = format!(
            "#!/bin/sh\n[ \"$1\" = deliver ] || exit 2\necho \"$3\" > '{}'\ncat > '{}'\n{answer}\n",
            asked.display(),
            said.display()
        );
        wsp_testkit::script(&sup, &body);
        (sup, said, asked)
    }

    /// `wsp-665`: outside the daemon's tick a `deliver` waits its 30 s; inside
    /// it, one ask waits 5 s at most and the tick's asks together about 12 s,
    /// and an ask after the budget is spent still waits long enough to be heard.
    #[test]
    fn a_tick_waits_on_deliveries_less_than_a_person_does() {
        let s = Duration::from_secs;
        let table = [
            (None, s(30), "no tick: a person's `wsp tell` keeps the wait"),
            (Some(s(12)), s(5), "the first ask of a tick"),
            (Some(s(3)), s(3), "what is left of the budget is what an ask gets"),
            (Some(Duration::ZERO), TICK_DELIVER_LEAST, "spent: still long enough for a seat that is ready"),
        ];
        for (left, want, why) in table {
            assert_eq!(deliver_wait_of(left), want, "{why}");
        }
    }

    /// The budget is taken down by what each ask waited, and only in a tick.
    #[test]
    fn what_an_ask_waits_comes_off_the_ticks_budget() {
        tick_deliver_budget(true);
        let got = deliver_waited(|w| {
            std::thread::sleep(Duration::from_millis(60));
            w
        });
        let left = TICK_DELIVER.lock().unwrap().unwrap();
        tick_deliver_budget(false);
        assert_eq!(got, TICK_DELIVER_EACH);
        assert!(left <= TICK_DELIVER_BUDGET - Duration::from_millis(60), "{left:?}");
        assert!(TICK_DELIVER.lock().unwrap().is_none(), "the end of a tick clears it");
        assert_eq!(deliver_waited(|w| w), deliver_wait(), "and a caller outside a tick waits as before");
    }

    /// wsp-477: a tell on a seat whose supervisor can be asked is its `deliver`
    /// verb and its one-line answer, read: `delivered` is acknowledged, `failed`
    /// is a failure that names the supervisor's reason, and neither is inferred
    /// from a transcript. The text rides on stdin.
    #[test]
    fn a_delivery_is_the_supervisors_answer_read_and_a_failed_names_its_reason() {
        let scratch = Scratch::new("deliver");
        let (seat, session) = (Seat::new("cpd-1"), scratch.root.join("s.sock"));
        let wait = Duration::from_secs(20);

        let (sup, said, _) = script_sup(&scratch, "echo \"delivered $3\"");
        assert_eq!(deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "go and do it", wait), Ok(Delivery::Acked));
        assert_eq!(fs::read_to_string(&said).unwrap(), "go and do it", "the sentence reaches the supervisor whole");

        script_sup(&scratch, "echo \"failed $3 no composer\"; exit 1");
        assert_eq!(
            deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "go", wait),
            Err(Refusal::Failed("cpd-1: no composer".into()))
        );

        // An answer about some other delivery is not this one's receipt.
        script_sup(&scratch, "echo 'delivered someone-else'");
        assert!(matches!(deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "go", wait), Err(Refusal::Backend(_))));
    }

    /// wsp-634: a supervisor that refuses an idle seat with an empty composer
    /// (its hook never fired, so it reads the seat as holding other text) answers
    /// `failed … refused` every time it is asked. A failed delivery is not held
    /// to its id: the same sentence tried again is a new delivery under a new id,
    /// so the seat that starts taking tells takes the held one on the next try.
    #[test]
    fn a_refused_delivery_is_retried_under_a_new_id_and_taken_when_the_seat_does() {
        let scratch = Scratch::new("deliver-refused");
        let (seat, session) = (Seat::new("cpd-1"), scratch.root.join("s.sock"));
        let wait = Duration::from_secs(20);

        let (sup, _, asked) = script_sup(&scratch, "echo \"failed $3 refused\"; exit 1");
        let refused = Err(Refusal::Failed("cpd-1: refused".into()));
        assert_eq!(deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "tell", wait), refused);
        let first = fs::read_to_string(&asked).unwrap();
        assert_eq!(deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "tell", wait), refused);
        let second = fs::read_to_string(&asked).unwrap();
        assert_ne!(first, second, "a failed id is not asked again: its answer would be `failed` again");

        script_sup(&scratch, "echo \"delivered $3\"");
        assert_eq!(deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "tell", wait), Ok(Delivery::Acked));
    }

    /// wsp-659: an idle governor whose supervisor refuses every `deliver` took
    /// the tell on no tick at all — 634's retry asked the same refusing verb
    /// again. A refusal on a composer this backend reads as empty is typed past
    /// on the first tick; a composer holding text, or one that cannot be read,
    /// keeps the refusal, and an answer that is not a refusal is never retyped.
    #[test]
    fn a_refused_delivery_to_an_empty_composer_is_typed_on_the_first_tick() {
        let typed = std::cell::Cell::new(0);
        let type_it = || {
            typed.set(typed.get() + 1);
            Ok(())
        };
        let refused = || Err(Refusal::Failed("cpd-1: refused".into()));
        assert_eq!(past_a_refusal(refused(), || Some(false), type_it), Ok(Delivery::Unconfirmed));
        assert_eq!(typed.get(), 1, "typed, once");
        let no_composer = Err(Refusal::Failed("cpd-1: no composer".into()));
        assert_eq!(past_a_refusal(no_composer, || Some(false), type_it), Ok(Delivery::Unconfirmed));

        assert_eq!(past_a_refusal(refused(), || Some(true), type_it), refused(), "text in the composer is not typed over");
        assert_eq!(past_a_refusal(refused(), || None, type_it), refused(), "an unread composer is not typed at");
        let busy = Err(Refusal::Failed("cpd-1: timeout".into()));
        assert_eq!(past_a_refusal(busy.clone(), || Some(false), type_it), busy, "a failure that may have typed is not retyped");
        assert_eq!(past_a_refusal(Ok(Delivery::Acked), || Some(false), type_it), Ok(Delivery::Acked));
        assert_eq!(typed.get(), 2, "only the two refusals on an empty composer were typed");

        let dial_fails = || Err(Refusal::NoSeat(Seat::new("cpd-1")));
        assert_eq!(past_a_refusal(refused(), || Some(false), dial_fails), refused(), "a type that fails keeps the answer");
    }

    /// wsp-477, the verifier's block 3: a wait that runs out is an answer that is
    /// not `failed`, and the same words asked again are asked under the same id,
    /// so the sup's exactly-once covers it. An answer read ends that id.
    #[test]
    fn a_delivery_that_outlasts_its_wait_is_asked_again_under_the_same_id() {
        let scratch = Scratch::new("deliver-wait");
        let (seat, session) = (Seat::new("cpd-1"), scratch.root.join("s.sock"));

        let (sup, _, asked) = script_sup(&scratch, "sleep 8; echo \"delivered $3\"");
        let t = Instant::now();
        let r = deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "go", Duration::from_millis(2500));
        assert!(t.elapsed() < Duration::from_secs(6), "bounded, not the sup's own wait");
        assert!(matches!(&r, Err(Refusal::Backend(w)) if w.contains("pending")), "{r:?}");
        let first = fs::read_to_string(&asked).unwrap();

        script_sup(&scratch, "echo \"delivered $3\"");
        assert_eq!(
            deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "go", Duration::from_secs(20)),
            Ok(Delivery::Acked)
        );
        assert_eq!(fs::read_to_string(&asked).unwrap(), first, "the same sentence, the same id");

        assert_eq!(
            deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "go", Duration::from_secs(20)),
            Ok(Delivery::Acked)
        );
        assert_ne!(fs::read_to_string(&asked).unwrap(), first, "an answered id is spent: the next tell is new");
    }

    /// A seat made by hand that delivers: this process as its pid, an agent, a
    /// socket that exists, and a hook's fresh word that it is `state`.
    /// Takes the seat's directory away on drop, ahead of `Scratch`, whose own
    /// drop would otherwise `stop` a seat whose pid is this test process.
    struct Forget(PathBuf);
    impl Drop for Forget {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn seat_that_delivers(place: &Compound<'_>, scratch: &Scratch, state: &str) -> (Seat, Forget) {
        let seat = Seat::new("cpd-1");
        let dir = place.dir_of(&seat).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let socket = scratch.root.join("s.sock");
        fs::write(&socket, "").unwrap();
        let rec = json!({
            "pid": std::process::id(),
            "socket": socket.display().to_string(),
            "deliver": true,
            "agent": { "kind": "claude", "name": "a", "args": [] },
        });
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string()).unwrap();
        let said = json!({ "state": state, "hook": "Stop", "at": util::now_iso() });
        write_atomic(&dir.join(SAID_FILE), &said.to_string()).unwrap();
        (seat, Forget(dir))
    }

    /// wsp-120: `deliver_order` hands the store's order id to the host as the
    /// delivery id. The first ask is one the sup never answers (it takes the id
    /// and goes); asked again under the same id the sup answers it, and has
    /// delivered it once. The seat's directory holds no `deliver.pending`.
    #[test]
    fn deliver_order_asked_again_under_one_id_is_one_delivery_and_keeps_no_pending() {
        let scratch = Scratch::new("deliver-order");
        let log = scratch.root.join("delivered");
        let (sup, _, asked) = script_sup(
            &scratch,
            &format!(
                "grep -qx \"$3\" '{0}' 2>/dev/null && {{ echo \"delivered $3\"; exit 0; }}\n\
                 [ -e '{0}.once' ] || {{ touch '{0}.once'; exit 1; }}\n\
                 echo \"$3\" >> '{0}'; echo \"delivered $3\"",
                log.display()
            ),
        );
        let host = InProcess::with_sup(&sup);
        let place = Compound { host: &host, ..scratch.place() };
        let (seat, _forget) = seat_that_delivers(&place, &scratch, "idle");

        let first = place.deliver_order(&seat, "ord-7", "do the thing");
        assert!(first.is_err(), "the first ask was not answered: {first:?}");
        assert_eq!(fs::read_to_string(&asked).unwrap().trim(), "ord-7", "asked under the order's id");
        assert_eq!(place.deliver_order(&seat, "ord-7", "do the thing"), Ok(Delivery::Acked));
        assert_eq!(place.deliver_order(&seat, "ord-7", "do the thing"), Ok(Delivery::Acked));
        assert_eq!(fs::read_to_string(&log).unwrap(), "ord-7\n", "one delivery under one id");
        assert!(!place.dir_of(&seat).unwrap().join(DELIVER_PENDING).exists(), "no deliver.pending");

        // `tell` is unchanged: it mints its own id and keeps its pending file.
        let _ = place.tell(&seat, "a tell");
        assert_ne!(fs::read_to_string(&asked).unwrap().trim(), "ord-7");
    }

    /// wsp-120: a busy seat is `NotReady` for `deliver_order` as for `tell`, and
    /// the sup is never asked; a refusal on a composer wsp cannot read stays the
    /// sup's answer (the empty-composer case is `past_a_refusal`'s, shared).
    #[test]
    fn deliver_order_refuses_a_busy_seat_and_keeps_a_refusal_it_cannot_type_past() {
        let scratch = Scratch::new("deliver-order-busy");
        let (sup, _, asked) = script_sup(&scratch, "echo \"failed $3 refused\"; exit 1");
        let host = InProcess::with_sup(&sup);
        let place = Compound { host: &host, ..scratch.place() };
        let (seat, _forget) = seat_that_delivers(&place, &scratch, "working");
        assert_eq!(place.deliver_order(&seat, "ord-8", "go"), Err(Refusal::NotReady(State::Working)));
        assert!(!asked.exists(), "a busy seat is not asked");

        let (seat, _forget) = seat_that_delivers(&place, &scratch, "idle");
        assert_eq!(place.deliver_order(&seat, "ord-8", "go"), Err(Refusal::Failed("cpd-1: refused".into())));
        assert_eq!(fs::read_to_string(&asked).unwrap().trim(), "ord-8");
    }

    /// wsp-666: a process that capped its deliveries waits the cap on a supervisor
    /// that never answers, not the 30 s, and the delivery is left pending.
    #[test]
    fn a_capped_process_leaves_a_delivery_pending_at_the_cap() {
        let scratch = Scratch::new("deliver-cap");
        let (seat, session) = (Seat::new("cpd-1"), scratch.root.join("s.sock"));
        let (sup, _, _) = script_sup(&scratch, "sleep 20; echo \"delivered $3\"");
        assert_eq!(deliver_wait(), DELIVER_WAIT, "uncapped, the patient wait");
        cap_deliver_wait(Duration::from_millis(500));
        let wait = deliver_wait();
        DELIVER_CAP_MS.store(0, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(wait, Duration::from_millis(500));
        let t = Instant::now();
        let r = deliver_through(&InProcess::with_sup(&sup), &session, &scratch.root, &seat, "go", wait);
        assert!(t.elapsed() < Duration::from_secs(5), "not the sup's own wait: {:?}", t.elapsed());
        assert!(matches!(&r, Err(Refusal::Backend(w)) if w.contains(DELIVERY_PENDING)), "{r:?}");
    }

    /// wsp-477, the verifier's block 1: only a seat spawned for it is delivered
    /// to. A record from before has no mark, and its supervisor drops the
    /// connection on a `Deliver` frame.
    #[test]
    fn only_a_seat_spawned_for_deliver_is_delivered_to() {
        assert!(!delivers(&json!({ "agent": { "kind": "claude" } })), "a seat already running");
        assert!(delivers(&json!({ "deliver": true })));
    }

    #[test]
    fn an_agent_seat_is_spawned_with_the_kind_its_supervisor_delivers_to() {
        let rec = json!({ "agent": { "kind": "claude" } });
        let args = |rec: &Value| -> Vec<String> {
            sup_command(Path::new("compound-sup"), &Compound::spawn_of(&Seat::new("cpd-1"), rec, None, "claude", &[]))
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert!(args(&rec).windows(2).any(|w| w == ["--agent", "claude"]));
        assert!(!args(&json!({})).contains(&"--agent".to_string()), "a bare shell has no composer");
    }

    #[test]
    fn a_session_that_is_gone_is_no_seat_and_not_a_failed_delivery() {
        let seat = Seat::new("cpd-1");
        assert_eq!(
            read_deliver("", "no session at /x: No such file or directory (os error 2)\n", false, "i", &seat),
            Err(Refusal::NoSeat(seat))
        );
    }

    /// `wsp-388`: the supervisor is started from `/`, not from the directory
    /// `wsp spawn` ran in; the agent's own directory is `--cwd`.
    #[test]
    fn the_supervisor_is_started_standing_nowhere_and_the_agent_where_it_was_told() {
        let rec = json!({ "cwd": "/tmp/somewhere" });
        let cmd = sup_command(Path::new("compound-sup"), &Compound::spawn_of(&Seat::new("cpd-1"), &rec, None, "cat", &[]));
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/")));
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|w| w == ["--cwd", "/tmp/somewhere"]), "{args:?}");
    }

    /// `wsp-432`: a supervisor a test starts is tied to the test process and
    /// stands in a run directory that is not the person's.
    #[test]
    fn a_supervisor_a_test_starts_is_ephemeral_and_runs_outside_the_persons_compound_dir() {
        let rec = json!({ "cwd": "/tmp/somewhere" });
        let cmd = sup_command(Path::new("compound-sup"), &Compound::spawn_of(&Seat::new("cpd-1"), &rec, None, "cat", &[]));
        test_sups::tied(&cmd).unwrap();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[sep + 1], "cat", "the flag is the supervisor's, before the `--`, and not the child's argv: {args:?}");

        let mut bare = Command::new("compound-sup");
        bare.args(["spawn", "--label", "cpd-1", "--", "cat"]);
        assert!(test_sups::tied(&bare).is_err(), "a command built by hand is not tied");
        let mut after = Command::new("compound-sup");
        after.args(["spawn", "--", "cat", "--ephemeral", &std::process::id().to_string()]);
        after.env("COMPOUND_RUN_DIR", test_sups::run_dir());
        assert!(test_sups::tied(&after).is_err(), "after the `--` it is the child's argument");
        let mut home = Command::new("compound-sup");
        home.args(["spawn", "--ephemeral", &std::process::id().to_string(), "--", "cat"]);
        home.env("COMPOUND_RUN_DIR", PathBuf::from(std::env::var_os("HOME").unwrap()).join(".compound/run"));
        assert!(test_sups::tied(&home).is_err(), "the person's run directory is refused");
    }

    /// **`wsp-388`: ending one seat ends that seat's process tree and nothing
    /// else.** Four seats are open — a check, a member, a governor and a second
    /// check — each a real process leading its own group, as `compound-sup`
    /// is; the first check is stopped through the close path `wsp despawn`
    /// uses, and the other three are still running.
    /// A far side from a script: records what it was asked, answers a handshake.
    struct FarScript {
        asked: std::cell::RefCell<Vec<String>>,
        dir: PathBuf,
        link_down: std::cell::Cell<bool>,
        forward_fails: bool,
        /// What `cat .claude/settings.json` finds on the far side.
        settings: std::cell::RefCell<String>,
        /// The lease the machine record carries.
        lease: std::cell::Cell<Option<u32>>,
        /// The machine has gone silent (`Wire::silent_for`).
        silent: std::cell::Cell<bool>,
    }

    impl crate::far::Wire for FarScript {
        fn machine(&self, name: &str) -> Option<crate::model::Machine> {
            (name == "mb2").then(|| {
                let mut m = crate::model::Machine::new("mb2", "mb2");
                m.exec_dir = "/far/exec".into();
                m.lease = self.lease.get();
                m
            })
        }
        fn run(&self, _m: &crate::model::Machine, cmd: &str, _t: Duration) -> std::result::Result<String, crate::far::Failed> {
            self.asked.borrow_mut().push(format!("run {cmd}"));
            if self.link_down.get() {
                return Err(crate::far::Failed::Link("Connection refused".into()));
            }
            // Reading and writing the gate's settings file.
            if cmd.starts_with("cat ") {
                return Ok(self.settings.borrow().clone());
            }
            if cmd.starts_with("mkdir ") {
                return Ok(String::new());
            }
            Ok("sup 11\npid 12\nsocket /far/run/s.sock\nrender /far/run/s.rsock".into())
        }
        fn forward(&self, _m: &crate::model::Machine, op: &str, local: &Path, far: &str) -> std::result::Result<(), String> {
            self.asked.borrow_mut().push(format!("{op} {}:{far}", local.file_name().unwrap().to_string_lossy()));
            match (self.forward_fails, op) {
                (true, "forward") => Err("remote port forwarding failed".into()),
                _ => Ok(()),
            }
        }
        fn dir(&self, _m: &crate::model::Machine) -> PathBuf {
            self.dir.clone()
        }
        fn silent_for(&self, name: &str) -> Option<String> {
            (name == "mb2" && self.silent.get()).then(|| "2026-11-06T10:00:00Z".to_string())
        }
        fn lease(&self, _m: &crate::model::Machine, _s: &Path, _seat: &str, _state: &str) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    fn far_script(name: &str, link_down: bool, forward_fails: bool) -> (FarScript, PathBuf) {
        // Short: a forwarded socket path may be 103 bytes, and `$TMPDIR` is not.
        let root = PathBuf::from("/tmp").join(format!("wf-{name}-{}-{}", std::process::id(), util::epoch_nanos() % 100_000));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("fwd")).unwrap();
        let f = FarScript {
            asked: Default::default(),
            dir: root.join("fwd"),
            link_down: std::cell::Cell::new(link_down),
            forward_fails,
            settings: Default::default(),
            lease: Default::default(),
            silent: Default::default(),
        };
        (f, root)
    }

    /// The gate `spawn --on` writes carries the machine's lease and a hook
    /// timeout above it, and keeps what the far tree's settings already held.
    #[test]
    fn the_gate_written_on_spawn_on_carries_the_machines_lease_and_keeps_the_trees_settings() {
        let (far, root) = far_script("gate", false, false);
        far.lease.set(Some(240));
        *far.settings.borrow_mut() = r#"{"permissions":{"allow":["Bash(ls)"]},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"mine.sh"}]}]}}"#.into();
        let place = Compound { far: &far, ..Compound::at(root.join("seats")) };
        let order = Order {
            label: "t-1".into(),
            cwd: Some("/far/exec/trees/t-1".into()),
            on: Some("mb2".into()),
            env: BTreeMap::from([("WSP_TASK".to_string(), "t-1".to_string())]),
            ..Order::default()
        };
        let seat = place.open(&order).unwrap();
        place.start(&seat, &Agent { kind: "claude".into(), name: "t-1".into(), args: vec![], key: None }).unwrap();

        let asked = far.asked.borrow().clone();
        let write = asked.iter().find(|a| a.starts_with("run mkdir ")).expect("the settings were written");
        assert!(write.contains("'/far/exec/trees/t-1/.claude/settings.json'"), "{write}");
        let body = write.split_once("<<'WSP_GATE_EOF'\n").unwrap().1.trim_end_matches("\nWSP_GATE_EOF");
        let doc: Value = serde_json::from_str(body).unwrap();
        assert_eq!(doc["permissions"]["allow"][0], json!("Bash(ls)"), "the tree's own settings stay");
        let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre[0]["hooks"][0]["command"], json!("mine.sh"), "its own hook stays");
        let gate = &pre[1]["hooks"][0];
        assert_eq!(
            gate["command"],
            json!(format!("compound-remote gate --exec-dir '/far/exec' --seat '{seat}' --lease 240"))
        );
        assert!(gate["timeout"].as_u64().unwrap() > 240, "the hook outlives the lease: {gate}");
        assert!(
            asked.iter().position(|a| a.starts_with("run mkdir ")) < asked.iter().position(|a| a.contains("spawn --label")),
            "before the agent starts"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A far seat whose forward does not answer reads `gone` while its machine
    /// answers and `unreachable` while its machine is silent — the link blip that
    /// used to end a seat — and the census files the silent machine as silent,
    /// so a reap has no empty answer to believe (`docs/remote-agents.md`
    /// section 3). A local seat is never read this way.
    #[test]
    fn a_far_seat_on_a_silent_machine_reads_unreachable_and_not_gone() {
        let (far, root) = far_script("silent", false, false);
        let place = Compound { far: &far, ..Compound::at(root.join("seats")) };
        let order = Order {
            label: "t-1".into(),
            cwd: Some("/far/exec/trees/t-1".into()),
            on: Some("mb2".into()),
            ..Order::default()
        };
        let seat = place.open(&order).unwrap();
        // As `start` leaves it, without starting anything: the forwarded socket is
        // a name nobody listens on, the local end of a forward its master took
        // with it.
        let mut rec = place.record(&seat).unwrap();
        rec["socket"] = json!(root.join("fwd").join("dead.sock").display().to_string());
        rec["agent"] = json!({ "kind": "claude", "name": "t-1", "args": [] });
        write_atomic(&place.dir_of(&seat).unwrap().join(SEAT_FILE), &rec.to_string()).unwrap();
        assert_eq!(place.state(&seat).unwrap(), State::Gone, "an answering machine and no session is a session gone");
        assert_eq!(place.census().unwrap().unheard().count(), 0, "nobody is silent");

        far.silent.set(true);
        assert_eq!(place.state(&seat).unwrap(), State::Unreachable, "a silent machine is a link and not a death");
        let census = place.census().unwrap();
        let row = census.seats().find(|r| r.seat == seat).expect("still drawn");
        assert_eq!(row.state, State::Unreachable);
        assert_eq!(row.state.as_str(), "unreachable");
        let silent: Vec<&str> = census.unheard().map(|(m, _)| m).collect();
        assert_eq!(silent, ["mb2"], "the census says who was silent");
        assert!(!census.answered("mb2"), "and a reap may not believe an absence from it");
        assert!(census.answered(""), "this machine answered all the same");

        far.silent.set(false);
        assert_eq!(place.state(&seat).unwrap(), State::Gone, "back to the ordinary reading when the machine is heard");
        let _ = fs::remove_dir_all(&root);
    }

    /// After a restart (wsp-094): the seat's compound-sup pid is another
    /// process now (this test's own, which runs), its socket file is gone and
    /// its last hook is long past. It reads gone, not unknown — by the socket,
    /// and by a last `SessionEnd` where the socket is still on disk.
    #[test]
    fn a_seat_whose_pid_was_reused_and_socket_is_gone_reads_gone_after_a_restart() {
        let scratch = Scratch::new("restart");
        let place = scratch.place();
        let seat = Seat::new("cpd-1000");
        let dir = place.dir_of(&seat).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let long_ago = util::iso_at(util::epoch_secs() - 3 * 3600);
        let rec = json!({
            "pid": std::process::id(),
            "socket": scratch.root.join("run").join("gone.sock").display().to_string(),
            "started_at": long_ago,
            "agent": { "kind": "claude", "name": "t-1", "args": [] },
        });
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string()).unwrap();
        assert_eq!(place.state(&seat).unwrap(), State::Gone, "a reused pid and no socket is a session gone");
        let row = place.census().unwrap().seats().find(|r| r.seat == seat).cloned().expect("drawn");
        assert_eq!(row.state, State::Gone, "the census says gone, not unknown");

        // The socket file left behind, and the agent's last word a SessionEnd.
        let sock = scratch.root.join("run").join("stale.sock");
        fs::create_dir_all(sock.parent().unwrap()).unwrap();
        fs::write(&sock, "").unwrap();
        let mut rec = rec;
        rec["socket"] = json!(sock.display().to_string());
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string()).unwrap();
        let said = json!({ "state": "gone", "hook": "SessionEnd", "at": util::iso_at(util::epoch_secs() - 3600) });
        write_atomic(&dir.join(SAID_FILE), &said.to_string()).unwrap();
        assert_eq!(place.state(&seat).unwrap(), State::Gone, "its SessionEnd was its last word");
    }

    /// The first lease of a link cut: the forward refuses at once, the machine
    /// still reads ready (not silent by its lease), and the seat must read
    /// `unreachable` and the census file the machine silent — never `gone`.
    #[test]
    fn a_far_seat_behind_a_dropped_link_reads_unreachable_before_the_lease_runs_out() {
        let (far, root) = far_script("cut", false, false);
        let place = Compound { far: &far, ..Compound::at(root.join("seats")) };
        let order = Order {
            label: "t-1".into(),
            cwd: Some("/far/exec/trees/t-1".into()),
            on: Some("mb2".into()),
            ..Order::default()
        };
        let seat = place.open(&order).unwrap();
        let mut rec = place.record(&seat).unwrap();
        rec["socket"] = json!(root.join("fwd").join("dead.sock").display().to_string());
        rec["agent"] = json!({ "kind": "claude", "name": "t-1", "args": [] });
        write_atomic(&place.dir_of(&seat).unwrap().join(SEAT_FILE), &rec.to_string()).unwrap();

        far.link_down.set(true);
        assert!(!far.silent.get(), "the machine is not silent by its lease");
        assert_eq!(place.state(&seat).unwrap(), State::Unreachable, "a dropped link is not a death");
        let census = place.census().unwrap();
        assert_eq!(census.seats().find(|r| r.seat == seat).expect("still drawn").state, State::Unreachable);
        assert!(!census.answered("mb2"), "and a reap may not believe an absence from it");

        far.link_down.set(false);
        assert_eq!(place.state(&seat).unwrap(), State::Gone, "a link that answers and no session is a session gone");
        let _ = fs::remove_dir_all(&root);
    }

    /// `spawn --on`: the seat is a record naming the machine and the far tree,
    /// `start` runs compound-sup there, forwards both sockets and records them,
    /// and `stop` closes the far session by its socket, drops the forwards and
    /// removes the seat.
    #[test]
    fn a_seat_on_another_machine_is_started_forwarded_recorded_and_stopped_over_the_link() {
        let (far, root) = far_script("life", false, false);
        let place = Compound { far: &far, ..Compound::at(root.join("seats")) };
        let order = Order {
            label: "t-1".into(),
            cwd: Some("/far/exec/trees/t-1".into()),
            on: Some("mb2".into()),
            env: BTreeMap::from([("WSP_TASK".to_string(), "t-1".to_string())]),
            ..Order::default()
        };
        let seat = place.open(&order).unwrap();
        let rec = place.record(&seat).unwrap();
        assert_eq!(str_of(&rec, "machine"), "mb2");
        assert_eq!(str_of(&rec, "cwd"), "/far/exec/trees/t-1", "the far path, not expanded here");
        assert_eq!(str_of(&rec["env"], "WSP_SEAT_ID"), seat.as_str());
        assert!(far.asked.borrow().is_empty(), "opening runs nothing over the link");

        place.start(&seat, &Agent { kind: "claude".into(), name: "t-1".into(), args: vec![], key: None }).unwrap();
        let asked = far.asked.borrow().clone();
        assert_eq!(asked.len(), 6, "{asked:?}");
        assert!(asked[0].starts_with("run cat ") && asked[1].starts_with("run mkdir "), "the gate is written first: {asked:?}");
        assert!(
            asked[2].starts_with("run mkdir ") && asked[2].ends_with("'/far/exec/bin/wsp-trust' add '/far/exec/trees/t-1'"),
            "then the row's tree is trusted by name, before claude starts: {}",
            asked[2]
        );
        let (asked_gate, asked) = (asked[..2].to_vec(), asked[3..].to_vec());
        assert!(asked_gate[1].contains(&format!("--seat '{seat}' --lease 180")), "{}", asked_gate[1]);
        assert!(
            asked_gate[1].starts_with("run mkdir -p '/far/exec/lease' && printf ok > '/far/exec/lease/.")
                && asked_gate[1].contains(&format!("mv '/far/exec/lease/.{seat}.wsp.tmp' '/far/exec/lease/{seat}' && mkdir -p ")),
            "the seat's first lease is written with the gate, before the agent: {}",
            asked_gate[1]
        );
        assert!(asked[0].contains(&format!("spawn --label '{seat}' --cwd '/far/exec/trees/t-1'")), "{}", asked[0]);
        assert!(asked[0].contains("WSP_SEAT_ID='") && asked[0].contains("claude-token"), "{}", asked[0]);
        assert_eq!(asked[1], format!("forward {seat}.sock:/far/run/s.sock"));
        assert_eq!(asked[2], format!("forward {seat}.rsock:/far/run/s.rsock"));

        let rec = place.record(&seat).unwrap();
        assert_eq!(str_of(&rec, "socket"), far.dir.join(format!("{seat}.sock")).display().to_string());
        assert_eq!(str_of(&rec, "render_socket"), far.dir.join(format!("{seat}.rsock")).display().to_string());
        assert_eq!((str_of(&rec, "far_socket").as_str(), str_of(&rec, "far_render_socket").as_str()), ("/far/run/s.sock", "/far/run/s.rsock"));
        assert_eq!(rec["far_pid"], json!(12));
        assert!(rec.get("pid").is_none(), "no local pid: it is not a local process");
        assert_eq!(place.socket_of(&seat), Some(far.dir.join(format!("{seat}.sock"))));
        let wanted = place.root.parent().unwrap().to_path_buf();
        let _ = wanted;

        far.asked.borrow_mut().clear();
        place.stop(&seat).unwrap();
        let asked = far.asked.borrow().clone();
        assert!(asked[0].contains("close '/far/run/s.sock'"), "closed by its socket: {asked:?}");
        assert!(asked.contains(&format!("cancel {seat}.sock:/far/run/s.sock")) && asked.contains(&format!("cancel {seat}.rsock:/far/run/s.rsock")), "{asked:?}");
        assert!(place.record(&seat).is_err(), "the seat is gone");
        assert!(
            asked.iter().any(|c| c == &format!("run rm -f '/far/exec/lease/{seat}' '/far/exec/lease/.{seat}.wsp.tmp'")),
            "and its lease file with it (wsp-590): {asked:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_forward_that_fails_closes_the_far_session_and_a_dead_link_does_not_lose_the_seat() {
        let (far, root) = far_script("fail", false, true);
        let place = Compound { far: &far, ..Compound::at(root.join("seats")) };
        let order = Order { label: "t".into(), cwd: Some("/far/exec/trees/t".into()), on: Some("mb2".into()), ..Order::default() };
        let seat = place.open(&order).unwrap();
        let err = place.start(&seat, &Agent { kind: "claude".into(), name: "t".into(), args: vec![], key: None }).unwrap_err();
        assert!(err.to_string().contains("forwarding"), "{err}");
        assert!(far.asked.borrow().last().unwrap().contains("close '/far/run/s.sock'"), "nothing left running there: {:?}", far.asked.borrow());
        assert!(place.socket_of(&seat).is_none(), "and nothing recorded as running");

        // A seat that is up, whose link then drops: stop refuses and keeps the record.
        let (far, root2) = far_script("down", false, false);
        let place = Compound { far: &far, ..Compound::at(root2.join("seats")) };
        let seat = place.open(&order).unwrap();
        place.start(&seat, &Agent { kind: "claude".into(), name: "t".into(), args: vec![], key: None }).unwrap();
        far.link_down.set(true);
        let err = place.stop(&seat).unwrap_err();
        assert!(err.to_string().contains("cannot reach mb2"), "{err}");
        assert!(
            err.to_string().contains("its session /far/run/s.sock") && err.to_string().contains(&format!("its lease file /far/exec/lease/{seat}")),
            "what it left is said: {err}"
        );
        assert!(place.record(&seat).is_ok(), "the seat is kept, so the session is not lost track of");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&root2);
    }

    #[test]
    fn stopping_one_check_seat_leaves_a_member_a_governor_and_a_second_check_running() {
        use std::os::unix::process::CommandExt;
        let scratch = Scratch::new("neighbours");
        let place = scratch.place();
        let mut held = Vec::new();
        let mut seats = Vec::new();
        for label in ["pre-prep · barrier 4", "wsp-385", "pre-prep governor", "other · barrier 2"] {
            let seat = place.open(&Order { label: label.into(), ..Order::default() }).unwrap();
            // The shell under the seat's own group leader, as the supervisor
            // and its pty child are: ended with the group.
            let mut leader = wsp_base::calls::Command::new("sh")
                .args(["-c", "sleep 60 & wait"])
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let pid = leader.id();
            // Ends the real supervisor `open` started before its record is
            // overwritten (`wsp-432`): the record is the only way to it.
            place.record_pid_for_test(&seat, pid);
            // Reaped when it ends, so `alive` reads it as gone and not a zombie.
            held.push((pid, std::thread::spawn(move || leader.wait())));
            seats.push(seat);
        }
        let _ends = held.iter().map(|(pid, _)| EndsOnDrop(*pid)).collect::<Vec<_>>();
        assert!(held.iter().all(|(pid, _)| alive(&[*pid]).contains(pid)));

        place.stop(&seats[0]).expect("the check was there");

        assert!(until(|| alive(&[held[0].0]).is_empty()), "the check's own process tree was ended");
        for (pid, _) in &held[1..] {
            assert!(alive(&[*pid]).contains(pid), "pid {pid} belonged to another seat and was ended with the check");
        }
        for seat in &seats[1..] {
            assert!(place.record(seat).is_ok(), "{seat} lost its record with the check");
        }
    }

    #[test]
    fn an_agent_that_leaves_for_good_takes_its_pane_with_it_and_a_clear_does_not() {
        let scratch = Scratch::new("pane-ends");
        let place = scratch.place();
        let mut held = Vec::new();
        for (label, reason, closed) in [("wsp-1", "prompt_input_exit", true), ("wsp-2", "clear", false)] {
            let seat = place.open(&Order { label: label.into(), ..Order::default() }).unwrap();
            let mut rec = place.record(&seat).unwrap();
            rec["agent"] = json!({ "program": "claude" });
            let mut leader = wsp_base::calls::Command::new("sh")
                .args(["-c", "sleep 60 & wait"])
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let pid = leader.id();
            place.record_pid_for_test(&seat, pid);
            let mut rec2 = place.record(&seat).unwrap();
            rec2["agent"] = rec["agent"].clone();
            write_atomic(&place.dir_of(&seat).unwrap().join(SEAT_FILE), &rec2.to_string()).unwrap();
            held.push((pid, std::thread::spawn(move || leader.wait()), seat, reason, closed));
        }
        let _ends = held.iter().map(|h| EndsOnDrop(h.0)).collect::<Vec<_>>();
        for (_, _, seat, reason, _) in &held {
            place.heard(seat, "SessionEnd", State::Gone, &json!({ "session_id": "a", "reason": reason }));
        }
        // The pane that goes on is judged at the moment the one that leaves
        // would have been killed by, counted from the hook rather than from
        // the end of the other wait, so the two waits overlap.
        let judged_at = std::time::Instant::now() + std::time::Duration::from_secs(CLOSE_AFTER_SECS as u64 + 3);
        for (pid, _, _, reason, closed) in &held {
            match closed {
                true => assert!(until(|| alive(&[*pid]).is_empty()), "{reason}: the pane outlived the agent"),
                false => {
                    std::thread::sleep(judged_at.saturating_duration_since(std::time::Instant::now()));
                    assert!(alive(&[*pid]).contains(pid), "{reason}: a pane that goes on was closed");
                }
            }
        }
    }

    /// The window `open`/`start` leaves for the claim to land in, and the
    /// seat's own name delivered to whatever env it will hand `compound-sup`
    /// — same property `place_super` proves the same way, because it is the
    /// same clause of `place.rs`'s sentence.
    #[test]
    fn a_seat_exists_before_a_session_does_and_knows_its_own_name() {
        let scratch = Scratch::new("open");
        let place = scratch.place();
        let seat = place
            .open(&Order {
                label: "compound-064".into(),
                env: BTreeMap::from([("WSP_TASK".into(), "compound-064".into())]),
                ..Order::default()
            })
            .expect("a seat");

        assert_eq!(place.state(&seat).unwrap(), State::Empty);
        let rec = place.record(&seat).unwrap();
        assert_eq!(str_of(&rec, "label"), "compound-064");
        assert_eq!(rec["env"][place::SEAT_ENV].as_str(), Some(seat.as_str()));
        assert_eq!(rec["env"]["WSP_TASK"].as_str(), Some("compound-064"));
    }

    /// Ids mint distinct and never come round again — `place_super`'s own
    /// test, against this backend's own prefix and directory, so the two
    /// counters are proven independent rather than assumed to be.
    #[test]
    fn a_seat_id_is_never_handed_out_again_after_the_seat_is_ended() {
        let scratch = Scratch::new("ids");
        let place = scratch.place();
        let first = place.open(&Order::default()).unwrap();
        let second = place.open(&Order::default()).unwrap();
        assert_ne!(first, second);
        assert!(first.as_str().starts_with(SEAT_PREFIX));
        place.stop(&first).expect("the seat was there");
        place.stop(&second).expect("the seat was there");
        let third = place.open(&Order::default()).unwrap();
        assert_ne!(third, first);
        assert_ne!(third, second);
    }

    /// The hook path a real Claude Code drives through `wsp report`, taken
    /// here without a hook or a pty: `heard` is the write half and this is
    /// what a `PermissionRequest` — the state `place_super` cannot reach at
    /// all — reads as, now that a pty makes it reachable.
    #[test]
    fn a_permission_prompt_reads_as_working_same_as_headless_does() {
        let scratch = Scratch::new("hook");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let dir = place.dir_of(&seat).unwrap();
        // Read BEFORE the record below overwrites it with this process's own
        // pid — from there on `stop` can no longer find the real supervisor
        // (`compound-127`).
        let _ends = EndsOnDrop(place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid") as u32);
        // A session recorded without actually spawning compound-sup: this
        // test is about the hook reading, not the pty. `agent` is set by
        // hand for the same reason `start` sets it — its absence now reads
        // as `compound-081`'s bare terminal, which is a different test.
        let _ = write_atomic(
            &dir.join(SEAT_FILE),
            &json!({ "pid": std::process::id(), "agent": { "kind": "claude", "name": "a", "args": [] } })
                .to_string(),
        );

        place.heard(&seat, "SessionStart", said_by("SessionStart").unwrap(), &json!({}));
        assert_eq!(place.state(&seat).unwrap(), State::Idle);

        place.heard(&seat, "PermissionRequest", said_by("PermissionRequest").unwrap(), &json!({}));
        assert_eq!(place.state(&seat).unwrap(), State::Working);
        assert!(!place.state(&seat).unwrap().will_take_a_prompt(), "a sentence would land in the dialog");
    }

    /// **`compound-107`: `said.json` cannot tell "one hook ever fired" from
    /// "many fired, and this is just the latest" — this file can.**
    ///
    /// `cpd-5`'s `said.json` held one `SessionStart` record six hours and 128
    /// turns into its session, and that single slot cannot say whether every
    /// later hook was silently lost or whether it simply was not looked at
    /// again until the last one had aged out. Both pasts overwrite the same
    /// slot with the same content. `hooks.jsonl` is the file that was not
    /// there to answer it, so this pins the property it needs to have next
    /// time: every call, in order, kept.
    #[test]
    fn every_hook_call_lands_in_the_log_even_though_said_json_only_ever_shows_the_last_one() {
        let scratch = Scratch::new("hooks-log");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let dir = place.dir_of(&seat).unwrap();
        // Read BEFORE the record below overwrites it with this process's own
        // pid — from there on `stop` can no longer find the real supervisor
        // (`compound-127`).
        let _ends = EndsOnDrop(place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid") as u32);
        let _ = write_atomic(
            &dir.join(SEAT_FILE),
            &json!({ "pid": std::process::id(), "agent": { "kind": "claude", "name": "a", "args": [] } })
                .to_string(),
        );

        for hook in ["SessionStart", "UserPromptSubmit", "Stop", "UserPromptSubmit", "Stop"] {
            place.heard(&seat, hook, said_by(hook).unwrap(), &json!({}));
        }

        // said.json: one slot, the latest word only.
        assert_eq!(str_of(&place.said(&seat), "hook"), "Stop");

        // hooks.jsonl: every call this seat was ever heard from, in order —
        // the answer `said.json` alone cannot give.
        let logged = fs::read_to_string(dir.join(HOOKS_FILE)).unwrap();
        let hooks: Vec<String> = logged
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["hook"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            hooks,
            vec!["SessionStart", "UserPromptSubmit", "Stop", "UserPromptSubmit", "Stop"],
            "every call this seat was ever heard from, in the order it happened"
        );
    }

    /// **A Claude Code seat is starting until its own `SessionStart`, whatever
    /// its screen draws** (`wsp-252`).
    ///
    /// cpd-616 and cpd-627 drew an idle composer thirty-one seconds before
    /// they fired `SessionStart`, the screen said ready, and every Enter wsp
    /// pressed in that window was lost with the order left typed. So a seat
    /// with no word of its own since its start is `Starting`, which no tell
    /// is typed at — and a word left by the last agent in a reused seat is
    /// not its own. Past [`STARTING_WINDOW`] the screen answers again, so a
    /// Claude Code whose hooks never fire is late rather than never told.
    #[test]
    fn a_claude_seat_is_starting_until_its_own_session_start_whatever_its_screen_says() {
        let scratch = Scratch::new("starting");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let dir = place.dir_of(&seat).unwrap();
        let _ends = EndsOnDrop(place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid") as u32);
        let now = util::epoch_secs();
        let started = |at: i64, kind: &str| {
            json!({
                "pid": std::process::id(),
                "started_at": util::iso_at(at),
                "agent": { "kind": kind, "name": "a", "args": [] },
            })
            .to_string()
        };
        let _ = write_atomic(&dir.join(SEAT_FILE), &started(now, "claude"));
        assert_eq!(place.state(&seat).unwrap(), State::Starting, "nothing said since the start");
        assert!(!place.state(&seat).unwrap().will_take_a_prompt(), "so nothing is typed at it");

        let last = json!({ "state": "idle", "hook": "SessionStart", "at": util::iso_at(now - 30) });
        let _ = write_atomic(&dir.join(SAID_FILE), &last.to_string());
        assert_eq!(place.state(&seat).unwrap(), State::Starting, "the last agent's word is not this one's");

        place.heard(&seat, "SessionStart", said_by("SessionStart").unwrap(), &json!({}));
        assert_eq!(place.state(&seat).unwrap(), State::Idle, "its own SessionStart, and the order can go");

        let _ = std::fs::remove_file(dir.join(SAID_FILE));
        let _ = write_atomic(&dir.join(SEAT_FILE), &started(now - STARTING_WINDOW - 10, "claude"));
        assert_ne!(place.state(&seat).unwrap(), State::Starting, "past the window the screen answers again");

        let _ = write_atomic(&dir.join(SEAT_FILE), &started(now, "opencode"));
        assert_ne!(place.state(&seat).unwrap(), State::Starting, "opencode fires no hooks to wait for");
    }

    /// The renderer is found under either of its names, the old one first.
    #[test]
    fn the_renderer_is_found_as_compound_render_or_compound_reader() {
        let tmp = std::env::temp_dir().join(format!("wsp-render-names-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let (path_dir, sup_dir) = (tmp.join("path"), tmp.join("sup"));
        fs::create_dir_all(&path_dir).unwrap();
        fs::create_dir_all(&sup_dir).unwrap();
        let path = Some(std::env::join_paths([&path_dir]).unwrap());
        let [first, second] = RENDER_NAMES;
        assert_eq!(find_render(path.clone(), None), None);
        fs::write(path_dir.join(second), "").unwrap();
        assert_eq!(find_render(path.clone(), None), Some(path_dir.join(second)), "either name is found");
        fs::write(path_dir.join(first), "").unwrap();
        assert_eq!(find_render(path.clone(), None), Some(path_dir.join(first)), "this OS's name wins");
        fs::write(sup_dir.join(second), "").unwrap();
        assert_eq!(
            find_render(path, Some(sup_dir.clone())),
            Some(sup_dir.join(second)),
            "beside the supervisor beats PATH, whatever the name"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    /// **A tool hook keeps a working seat fresh and nothing else** (the pi
    /// extension's tool events, Claude's `PostToolUse`). A seat whose last word
    /// is two minutes old reads off the screen; a tool that ran a second ago
    /// makes it a second old again. An idle record is not made working by a
    /// straggler, and a nested session's tool is not the seat's.
    #[test]
    fn a_tool_hook_refreshes_a_working_record_and_only_a_working_one() {
        let scratch = Scratch::new("touched");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let dir = place.dir_of(&seat).unwrap();
        let _ends = EndsOnDrop(place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid") as u32);
        let now = util::epoch_secs();
        let _ = write_atomic(
            &dir.join(SEAT_FILE),
            &json!({
                "pid": std::process::id(),
                "started_at": util::iso_at(now - 1000),
                "agent": { "kind": "pi", "name": "a", "args": [] },
            })
            .to_string(),
        );
        let said = |state: &str, age: i64| {
            let _ = write_atomic(
                &dir.join(SAID_FILE),
                &json!({ "state": state, "hook": "UserPromptSubmit", "at": util::iso_at(now - age), "session_id": "s1" })
                    .to_string(),
            );
        };
        // Working, but the word is older than VOUCH_SECS: refreshed.
        said("working", VOUCH_SECS + 60);
        place.touched(&seat, &json!({ "session_id": "s1" }));
        assert_eq!(place.state(&seat).unwrap(), State::Working, "a tool ran: the record is fresh");

        // Idle stays idle (and stale stays stale).
        said("idle", VOUCH_SECS + 60);
        place.touched(&seat, &json!({ "session_id": "s1" }));
        let at = util::epoch_of(&str_of(&place.said(&seat), "at"));
        assert!(now - at > VOUCH_SECS, "an idle record is not re-dated");

        // Another session's tool is not the seat's.
        said("working", VOUCH_SECS + 60);
        place.touched(&seat, &json!({ "session_id": "other" }));
        let at = util::epoch_of(&str_of(&place.said(&seat), "at"));
        assert!(now - at > VOUCH_SECS, "a nested session's tool is not the seat's");
    }

    use crate::host::RENDER;

    /// **A far seat's screen is its readiness, and its turn is read off its
    /// state** (wsp-589). The far agent's hooks fire over there and write
    /// nothing here, so the start window held a far seat `Starting` while its
    /// renderer served a working screen (`wsp spawn --on`: *never became ready*;
    /// `wsp tell`: *delivered, no turn seen*, both on a seat that worked).
    #[test]
    fn a_far_seat_reads_ready_off_its_screen_and_its_silent_hooks_are_not_no_turn() {
        use std::os::unix::net::UnixListener;
        let (far, root) = far_script("ready", false, false);
        let place = Compound { far: &far, ..Compound::at(root.join("seats")) };
        let seat = Seat::new("cpd-f1");
        let dir = place.dir_of(&seat).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let sock = root.join("s.sock");
        // Accepts and drops, so the supervisor-side fallback read fails at once
        // instead of waiting on a socket that never answers.
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || for _ in listener.incoming() {});
        let now = util::epoch_secs();
        let _ = write_atomic(
            &dir.join(SEAT_FILE),
            &json!({
                "machine": "mb2", "socket": sock, "render_socket": root.join("s.rsock"),
                "started_at": util::iso_at(now),
                "agent": { "kind": "claude", "name": "a", "args": [] },
            })
            .to_string(),
        );

        // No renderer to ask: still starting, which is the honest word.
        assert_eq!(place.state(&seat).unwrap(), State::Starting, "no screen, no hook");

        let render = root.join("compound-render");
        wsp_testkit::script(&render, "#!/bin/sh\ncase \"$1\" in state) echo working;; *) echo '001|hi';; esac\n");
        RENDER.with(|r| *r.borrow_mut() = Some(render.clone()));
        assert_eq!(place.state(&seat).unwrap(), State::Working, "the renderer serves a working screen: no hook is needed");
        wsp_testkit::script(&render, "#!/bin/sh\ncase \"$1\" in state) echo idle;; *) echo '001|hi';; esac\n");
        assert!(place.state(&seat).unwrap().will_take_a_prompt(), "an idle screen takes a prompt");
        fs::write(&render, "#!/bin/sh\ncase \"$1\" in state) echo unknown;; *) echo '001|hi';; esac\n").unwrap();
        assert_eq!(place.state(&seat).unwrap(), State::Starting, "a screen that says nothing keeps it starting");

        RENDER.with(|r| *r.borrow_mut() = None);
        let _ = fs::remove_dir_all(&root);
    }

    /// Claude Code's prompt is the box under the last rule, and a sentence
    /// queued behind a turn is drawn above it — so the box is read and the
    /// screen is not. The greyed suggestion an empty prompt shows reads as text
    /// on a screen with no colour in it, and is not.
    #[test]
    fn the_prompt_is_read_from_the_box_under_the_last_rule() {
        let rule = "─".repeat(60);
        let screen = |inside: &str| format!("⏺ an earlier answer\n\n{rule}\n{inside}\n{rule}\n  ⏵⏵ auto mode on\n");
        assert_eq!(composer(&screen("❯ ")), Some(false), "empty");
        assert_eq!(composer(&screen("❯ Verify **compound-415** (reattach is flaky")), Some(true), "the order, typed");
        assert_eq!(composer(&screen("❯ Verify compound-415 and read\n  the rest of the order")), Some(true), "wrapped");
        assert_eq!(composer(&screen("❯ [Pasted text #1 +12 lines]")), Some(true), "pasted");
        assert_eq!(composer(&screen("❯ Try \"fix lint errors\"")), Some(false), "the suggestion");
        let queued = format!("⏺ working\n  ⎿ the barrier is open — start group 2\n{rule}\n❯ \n{rule}\n");
        assert_eq!(composer(&queued), Some(false), "queued above the box is not in it");
        assert_eq!(composer("$ claude\nWelcome to Claude Code"), None, "no prompt drawn yet");
    }

    /// The idle governor's screen as `wsp peek` read it off cpd-5893 while its
    /// supervisor answered `refused` (`wsp-659`, the verifier's block): Claude
    /// Code draws the session name in the box's top rule, and a reader that took
    /// only bare rules found one rule and said `None`.
    fn named_session_screen(inside: &str) -> String {
        let top = format!("{} primary-delivery-cpd-5893 ─", "─".repeat(52));
        let bottom = "─".repeat(80);
        format!("⏺ Waiting on group 1.\n\n{top}\n{inside}\n{bottom}\n  ⏵⏵ auto mode on (shift+tab to cycle)\n")
    }

    #[test]
    fn a_prompt_box_whose_top_rule_carries_the_session_name_is_read() {
        assert_eq!(composer(&named_session_screen("❯ ")), Some(false), "empty");
        assert_eq!(composer(&named_session_screen("❯ wsp tell seat:wsp-659 -")), Some(true), "holding text");
        let other = format!("{} sidebar-width-cpd-5900 ─", "─".repeat(55));
        assert_eq!(composer(&format!("{other}\n❯\n{}\n", "─".repeat(80))), Some(false), "cpd-5900's frame");
        assert!(!is_rule("── a sentence that happens to start with a dash and ends ─"), "prose is no rule");
        assert!(!is_rule("─── short ──"), "too narrow to be the box");
    }

    /// `wsp-694`: a refusal that took nothing, at a box holding text, names the
    /// draft; an empty box, an unread one, or another answer is left as it was.
    #[test]
    fn a_no_composer_refusal_at_a_box_holding_a_draft_names_the_draft() {
        let no_composer = || Err(Refusal::Failed("cpd-6187: no composer".into()));
        let held = named_session_screen("❯ wsp · nobody typed this. You are the custodian of `compounding`");
        let Err(Refusal::Failed(why)) = name_the_draft(no_composer(), || draft(&held)) else { panic!("still a refusal") };
        assert!(why.starts_with("cpd-6187: no composer — ") && why.contains(DRAFT_STUCK), "{why}");
        assert!(why.contains("\"wsp · nobody typed this."), "the draft is named: {why}");
        assert_eq!(name_the_draft(no_composer(), || draft(&named_session_screen("❯ "))), no_composer());
        assert_eq!(name_the_draft(no_composer(), || None), no_composer());
        let busy = Err(Refusal::Failed("cpd-1: timeout".into()));
        assert_eq!(name_the_draft(busy.clone(), || Some("text".into())), busy, "only a refusal that took nothing");
    }

    /// Item 3 through the real read, not a stub: a `refused` answer to a seat
    /// whose screen is the labelled empty box is typed past; the same box with
    /// text in it keeps the refusal.
    #[test]
    fn a_refusal_at_a_named_sessions_empty_box_is_typed_past() {
        let typed = std::cell::Cell::new(0);
        let type_it = || {
            typed.set(typed.get() + 1);
            Ok(())
        };
        let refused = || Err(Refusal::Failed("cpd-5893: refused".into()));
        let empty = named_session_screen("❯ ");
        assert_eq!(past_a_refusal(refused(), || composer(&empty), type_it), Ok(Delivery::Unconfirmed));
        let held = named_session_screen("❯ half a sentence");
        assert_eq!(past_a_refusal(refused(), || composer(&held), type_it), refused());
        assert_eq!(typed.get(), 1, "typed at the empty box only");
    }

    /// **A hook's word expires, and the guard it feeds comes back with it**
    /// (`compound-097`).
    ///
    /// The seat that cost an answer nobody chose had exactly one `said`
    /// record — its own `SessionStart` — and read `Idle` two hours later
    /// while its agent was mid-turn on a question dialog. Both guards that
    /// exist to stop a sentence landing in that dialog are built on this
    /// function, so both were inert.
    ///
    /// Asserted in both directions, because a bound that only ever refuses
    /// would break every spawn: a fresh record still vouches, so the work
    /// order that goes out seconds after `SessionStart` is delivered.
    #[test]
    fn a_hook_that_has_not_spoken_recently_says_unknown_rather_than_idle() {
        let scratch = Scratch::new("vouch");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let dir = place.dir_of(&seat).unwrap();
        // Read BEFORE the record below overwrites it with this process's own
        // pid — from there on `stop` can no longer find the real supervisor
        // (`compound-127`).
        let _ends = EndsOnDrop(place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid") as u32);
        let _ = write_atomic(
            &dir.join(SEAT_FILE),
            &json!({ "pid": std::process::id(), "agent": { "kind": "claude", "name": "a", "args": [] } })
                .to_string(),
        );

        // Fresh: the spawn case, and it must keep working.
        place.heard(&seat, "SessionStart", said_by("SessionStart").unwrap(), &json!({}));
        assert_eq!(place.state(&seat).unwrap(), State::Idle);
        assert!(
            place.state(&seat).unwrap().will_take_a_prompt(),
            "a work order goes out seconds after SessionStart and must still be delivered"
        );

        // The same record, aged past the bound. Nothing else changes — the
        // pid is alive and the agent is recorded — so this is the claim
        // expiring and not the seat going away.
        let aged = json!({
            "state": "idle",
            "hook": "SessionStart",
            "at": util::iso_at(util::epoch_secs() - (VOUCH_SECS + 60)),
        });
        let _ = write_atomic(&dir.join(SAID_FILE), &aged.to_string());
        assert_eq!(
            place.state(&seat).unwrap(),
            State::Unknown,
            "nothing has told us anything since, and Unknown is what that is called"
        );
        assert!(
            !place.state(&seat).unwrap().will_take_a_prompt(),
            "so `Place::tell` refuses rather than typing at whatever is on screen"
        );

        // A record with no stamp at all — written before this existed —
        // cannot vouch either, and must not read as 1970 or as idle.
        let _ = write_atomic(&dir.join(SAID_FILE), &json!({ "state": "idle" }).to_string());
        assert_eq!(place.state(&seat).unwrap(), State::Unknown);
    }

    /// **The refusal has a way past it, or it is a lockout** (`compound-097`).
    ///
    /// Nothing refreshes a compound seat's `said` record today, so once the
    /// vouch expires every seat refuses — and a governor who has LOOKED, with
    /// `compound-sup screen`, must still be able to reach an agent that is
    /// plainly waiting. `--anyway` is that, and this asserts the gate opens
    /// rather than that the text arrives: there is no socket behind this
    /// fixture, so the insisting backend gets PAST the readiness check and
    /// fails on the dial instead, which is exactly the distinction worth
    /// pinning.
    #[test]
    fn a_seat_that_cannot_vouch_refuses_until_somebody_insists() {
        let scratch = Scratch::new("insist");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let dir = place.dir_of(&seat).unwrap();
        // Read BEFORE the record below overwrites it with this process's own
        // pid — from there on `stop` can no longer find the real supervisor
        // (`compound-127`).
        let _ends = EndsOnDrop(place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid") as u32);
        let _ = write_atomic(
            &dir.join(SEAT_FILE),
            &json!({ "pid": std::process::id(), "agent": { "kind": "claude", "name": "a", "args": [] } })
                .to_string(),
        );
        let _ = write_atomic(
            &dir.join(SAID_FILE),
            &json!({
                "state": "idle",
                "hook": "SessionStart",
                "at": util::iso_at(util::epoch_secs() - (VOUCH_SECS + 60)),
            })
            .to_string(),
        );

        match place.tell(&seat, "anybody there") {
            Err(Refusal::NotReady(State::Unknown)) => {}
            other => panic!("a seat that cannot vouch must refuse, got {other:?}"),
        }

        let insisting = scratch.place().insisting();
        assert!(
            !matches!(insisting.tell(&seat, "anybody there"), Err(Refusal::NotReady(_))),
            "insisting gets past the readiness gate — what it fails on next is the socket"
        );
    }

    /// `compound-081`: a live pid with no `agent` recorded is `open`'s bare
    /// terminal, and reads `Empty` exactly as no pid at all does — `said`
    /// (which no shell ever writes to) is never consulted for one, so a
    /// stale hook file from a PREVIOUS agent in this seat cannot leak
    /// through a bare shell that replaced it.
    #[test]
    fn a_bare_terminal_reads_empty_while_alive_and_gone_once_its_shell_exits() {
        let scratch = Scratch::new("bare");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let dir = place.dir_of(&seat).unwrap();
        // Read BEFORE the record is falsified below: from here on `stop` can
        // no longer find this supervisor, so the pid is the only handle left.
        let _ends = EndsOnDrop(place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid") as u32);
        // A stale hook file, as if this seat held an agent before — the
        // case the `said`-skip in `state_of` exists to guard.
        let _ = write_atomic(&dir.join(SAID_FILE), &json!({ "state": "working" }).to_string());

        let _ = write_atomic(&dir.join(SEAT_FILE), &json!({ "pid": std::process::id() }).to_string());
        assert_eq!(
            place.state(&seat).unwrap(),
            State::Empty,
            "a live shell with no agent in it reads the same as none opened"
        );

        // A pid nothing alive could plausibly hold.
        let _ = write_atomic(&dir.join(SEAT_FILE), &json!({ "pid": 999_999_991u32 }).to_string());
        assert_eq!(place.state(&seat).unwrap(), State::Gone, "the shell is not running");
    }

    /// `compound-081`, end to end: `open` alone already has a real
    /// `compound-sup` behind it — a bare terminal, `State::Empty`, a socket
    /// on disk — and `start` REPLACES that pty's shell with an agent rather
    /// than refusing because a session already occupies the seat.
    #[test]
    #[ignore]
    fn open_alone_has_a_real_terminal_and_start_replaces_its_bare_shell() {
        let Some(sup) = std::env::var_os("COMPOUND_SUP") else {
            eprintln!("skipped: set COMPOUND_SUP to a built compound-sup to run this");
            return;
        };
        assert!(PathBuf::from(&sup).is_file(), "COMPOUND_SUP is not a file");

        let scratch = Scratch::new("bare-real");
        let place = scratch.place();
        let cwd = std::env::temp_dir();
        let seat = place
            .open(&Order { label: "compound-081 smoke".into(), cwd: Some(cwd.display().to_string()), ..Order::default() })
            .expect("a seat");

        assert!(until(|| place.socket_of(&seat).is_some()), "open minted no socket in time");
        let bare_socket = place.socket_of(&seat).unwrap();
        assert!(until(|| bare_socket.exists()), "compound-sup never bound the bare shell's socket");
        assert_eq!(place.state(&seat).unwrap(), State::Empty, "a shell with nobody's agent in it");
        let bare_pid = place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid");

        place
            .start(&seat, &Agent { kind: "cat".into(), name: "smoke".into(), args: vec![], key: None })
            .expect("start replaces the bare shell rather than refusing");

        let agent_pid = place.record(&seat).unwrap()["pid"].as_u64().expect("a live pid");
        assert_ne!(agent_pid, bare_pid, "a fresh process, not the shell wearing an agent's name");
        assert!(until(|| alive(&[bare_pid as u32]).is_empty()), "the bare shell was actually ended");
        assert_eq!(place.state(&seat).unwrap(), State::Starting, "an agent is recorded now");

        place.stop(&seat).expect("the seat was there");
    }

    /// End to end against a REAL `compound-sup`: open, start, watch it reach
    /// `Idle` off its own `SessionStart` hook (no mock — `wsp report` really
    /// runs inside the pty), tell it a line, read it back off the session's
    /// own screen, stop it, and check the socket and the label sidecar it
    /// wrote are both gone. `$COMPOUND_SUP` names the binary; `--ignored`
    /// because it wants one built (`cargo build -p compound-sup --release`
    /// in `~/claude/compound`) and a real pty, the same bar `ghostty-config`'s
    /// `real_machine.rs` sets for "wants a real machine".
    #[test]
    #[ignore]
    fn a_real_compound_sup_session_is_opened_told_and_stopped() {
        let Some(sup) = std::env::var_os("COMPOUND_SUP") else {
            eprintln!("skipped: set COMPOUND_SUP to a built compound-sup to run this");
            return;
        };
        let sup = PathBuf::from(sup);
        assert!(sup.is_file(), "COMPOUND_SUP={} is not a file", sup.display());

        let scratch = Scratch::new("real");
        let place = scratch.place();
        let cwd = std::env::temp_dir();
        let seat = place
            .open(&Order { label: "compound-064 smoke".into(), cwd: Some(cwd.display().to_string()), ..Order::default() })
            .expect("a seat");
        place
            .start(&seat, &Agent { kind: "cat".into(), name: "smoke".into(), args: vec![], key: None })
            .expect("compound-sup started");

        assert!(until(|| place.socket_of(&seat).is_some()), "no socket announced in time");
        let socket = place.socket_of(&seat).unwrap();
        assert!(until(|| socket.exists()), "compound-sup never bound its socket");

        // No hook to read here — `cat` fires none — so this is the pid-alive
        // half of `state`, proven against a real process rather than a
        // recorded one.
        assert_eq!(place.state(&seat).unwrap(), State::Starting);

        // The label sidecar `Sidebar::reach`'s future join key is — written
        // by compound-sup itself, from `--label`.
        let label_path = socket.with_extension("label");
        assert!(until(|| label_path.is_file()), "compound-sup never wrote the label sidecar");
        assert_eq!(fs::read_to_string(&label_path).unwrap().trim(), seat.as_str());

        place.stop(&seat).expect("the seat was there");
        assert!(until(|| !socket.exists()), "compound-sup left its socket behind");
        assert!(until(|| !label_path.exists()), "the label sidecar outlived its socket");
        assert!(place.record(&seat).is_err(), "the seat directory itself must go with stop");
    }

    #[test]
    fn a_pi_seat_is_readable_but_has_no_title_to_fall_back_on() {
        assert!(reads_kind("pi"));
        assert!(!reads_kind("pie"));
        // pi.toml has no osc_title rule: with no renderer, whatever the title
        // says, there is no answer to give.
        assert_eq!(title_state("pi", "\u{2733} pi"), None);
        assert_eq!(title_state("pi", "\u{280B} pi"), None);
    }

    /// What `state()` says for a pi seat, pinned against screens captured from
    /// pi 1.3.0 (the same ones `compound#1`'s `pi_captured_screens_are_detected_as_the_manifest_does`
    /// pins). With the renderer's updated pi rules a spinner line reads
    /// `Working` and a select prompt `Blocked`. A renderer from before them
    /// reads both `Idle`, so the answer depends on which binary is used: the
    /// test is therefore opt-in and runs only when `COMPOUND_RENDER` names a
    /// renderer explicitly (a build known to carry the pi rules), e.g.
    /// `COMPOUND_RENDER=/path/to/compound-render cargo test -p wsp-wire
    /// detected_state_for_a_pi_seat`. Without it the test prints why and
    /// skips; it never guesses at whatever happens to be installed.
    ///
    /// Also skips without `compound-sup` (not hermetic about it, like the
    /// `#[ignore]`d smokes); a script named `pi`
    /// on the seat's `PATH` stands in for the binary and prints one screen, so
    /// no model runs.
    #[test]
    fn detected_state_for_a_pi_seat_reads_working_and_blocked_screens() {
        if std::env::var_os("COMPOUND_RENDER").is_none() {
            eprintln!("skipped: opt-in; set COMPOUND_RENDER to a renderer that carries the pi rules to run this");
            return;
        }
        if compound_sup_binary().is_none() || compound_render_binary().is_none() {
            eprintln!("skipped: needs compound-sup (set COMPOUND_SUP or put it on PATH) and the renderer COMPOUND_RENDER names");
            return;
        }
        let rule = "\u{2500}".repeat(100);
        let foot = format!("{rule}\n\n{rule}\n~/work\n0.0%/262k (auto)    (local) qwen3.8-27b \u{2022} medium\n");
        let cases = [
            ("idle editor", "\u{2580}\u{2580}\u{2588}  v1.3.0\nescape interrupt \u{b7} ctrl+c/ctrl+d clear/exit", State::Idle),
            ("Working... literal", "Working...", State::Working),
            ("spinner and Working", "122. One hundred and twenty-two\n\n\u{2500}\u{2500} \u{2826} Working \u{2500}\u{2500}", State::Working),
            ("select prompt", " Allow this command?  rm -rf build/\n\n \u{2192} Yes\n   No\n\n \u{2191}\u{2193} navigate  enter select  escape/ctrl+c cancel", State::Blocked),
        ];
        for (name, body, want) in cases {
            let scratch = Scratch::new("pi-detect");
            let bin = scratch.root.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let screen = scratch.root.join("screen.txt");
            fs::write(&screen, format!("{body}\n{foot}")).unwrap();
            let script = bin.join("pi");
            wsp_testkit::script(&script, &format!("#!/bin/sh\ncat '{}'\nexec sleep 600\n", screen.display()));
            let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

            let place = scratch.place();
            let seat = place
                .open(&Order { label: "pi-support-007".into(), env: BTreeMap::from([("PATH".into(), path)]), ..Order::default() })
                .expect("a seat");
            place.start(&seat, &Agent { kind: "pi".into(), name: "pi".into(), args: vec![], key: None }).expect("started");
            let drawn = until(|| place.read_screen(&seat).is_ok_and(|t| t.contains("0.0%/262k")));
            assert!(drawn, "{name}: the screen never drew: {:?}", place.read_screen(&seat));
            let got = place.state(&seat).unwrap();
            assert_eq!(got, want, "{name}");
        }
    }

    /// `compound-109`'s barrier: `idle` is not proof `detected_state`
    /// discriminates anything, because it is also what a screen with no
    /// opinion falls back to — the claude case would look identical with no
    /// manifest at all. opencode is the one worth proving, and not only
    /// because it is the other compiled manifest: opencode fires none of the
    /// hooks [`Compound::heard`] listens for, so a real opencode seat's
    /// `said.json` never exists, and `state()` for one is *entirely*
    /// `detected_state`'s answer for its whole life, not a fallback taking
    /// over once something else expires.
    ///
    /// So this drives a real one through a real prompt: idle before, working
    /// once `opencode`'s `interrupt_hint_working`/`progress_bar_working`
    /// rules see the "esc to interrupt" chrome mid-turn, and something other
    /// than working once it settles — proof the rule set discriminates a
    /// live screen rather than defaulting through it twice.
    #[test]
    #[ignore]
    fn detected_state_discriminates_a_real_opencode_seat_between_working_and_not() {
        let Some(sup) = std::env::var_os("COMPOUND_SUP") else {
            eprintln!("skipped: set COMPOUND_SUP to a built compound-sup to run this");
            return;
        };
        assert!(PathBuf::from(&sup).is_file(), "COMPOUND_SUP is not a file");

        let scratch = Scratch::new("opencode-detect");
        let place = scratch.place();
        let seat = place
            .open(&Order { label: "compound-109 smoke".into(), ..Order::default() })
            .expect("a seat");
        place
            .start(&seat, &Agent { kind: "opencode".into(), name: "smoke".into(), args: vec![], key: None })
            .expect("compound-sup started");

        assert!(until(|| place.socket_of(&seat).is_some()), "no socket announced in time");
        assert!(until(|| place.socket_of(&seat).is_some_and(|s| s.exists())), "compound-sup never bound its socket");

        // No `said.json` will ever exist for this seat — opencode fires no
        // hook — so every read from here on is `detected_state` alone.
        //
        // `will_take_a_prompt` alone is not the gate: a still-loading screen
        // ALSO reads `Idle` — no rule has matched it yet either — so it goes
        // true within the first poll, well before opencode's own input
        // handling is actually live. Measured directly (`compound-109`):
        // typing into that window is silently swallowed, no error, nothing
        // on screen — not a wire bug, `type_and_submit` proved sound against
        // the same seat with `cat` in its place. So this also waits for its
        // own placeholder text, the screen's own claim that it is ready.
        assert!(
            until(|| place.state(&seat).unwrap().will_take_a_prompt()),
            "opencode never reached an idle prompt: {:?}",
            place.state(&seat)
        );
        assert!(
            until(|| place.read_screen(&seat).unwrap_or_default().contains("Ask anything")),
            "opencode's prompt box never actually rendered: {}",
            place.read_screen(&seat).unwrap_or_default()
        );

        place.tell(&seat, "Count slowly from 1 to 50, one number per line, pausing to think between each one. Do not write or edit any files.").expect("delivered");

        assert!(
            until(|| place.state(&seat).unwrap() == State::Working),
            "opencode never read as working after a prompt: {:?}\nscreen:\n{}",
            place.state(&seat),
            place.read_screen(&seat).unwrap_or_default()
        );

        // A real turn can run well past `until`'s 10s budget; give leaving
        // `Working` room without pretending that is the steady-state bound.
        let left_working = (0..1200).any(|_| {
            if place.state(&seat).unwrap() != State::Working {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
            false
        });
        assert!(left_working, "opencode never left working: {:?}", place.state(&seat));

        // `stop` (`compound-136`) has to end BOTH halves of the split and
        // clean up both sockets, not just the durable one `compound-031`
        // predates — `end_process`'s render-socket cleanup is new here and
        // this is what proves it against a real renderer rather than a
        // fixture.
        let socket = place.socket_of(&seat).expect("a durable socket was recorded");
        let render_socket = place.render_socket_of(&seat).expect("a render socket was recorded");
        place.stop(&seat).expect("the seat was there");
        assert!(!socket.exists(), "the durable socket outlived stop");
        assert!(!render_socket.exists(), "the render socket outlived stop");
    }

    /// `compound-111`: `wsp peek` had no way to read a compound seat at all —
    /// "backend would not read cpd-2" — and the workaround all week was a
    /// person typing `compound-sup screen <socket>` by hand. This is that
    /// call, against a real session, proving the text a person would have
    /// read on their own screen comes back through [`Compound::read_screen`]
    /// with `compound-sup`'s own `NNN|` prefixes and trailing `size WxH`
    /// stripped off.
    #[test]
    #[ignore]
    fn read_screen_shows_what_a_person_watching_by_hand_would_see() {
        let Some(sup) = std::env::var_os("COMPOUND_SUP") else {
            eprintln!("skipped: set COMPOUND_SUP to a built compound-sup to run this");
            return;
        };
        assert!(PathBuf::from(&sup).is_file(), "COMPOUND_SUP is not a file");

        let scratch = Scratch::new("read-screen");
        let place = scratch.place();
        let seat = place
            .open(&Order { label: "compound-111 smoke".into(), ..Order::default() })
            .expect("a seat");
        place
            .start(&seat, &Agent { kind: "sh".into(), name: "smoke".into(), args: vec!["-c".into(), "printf UNIQUE-COMPOUND-111-MARK; sleep 5".into()], key: None })
            .expect("compound-sup started");

        assert!(until(|| place.socket_of(&seat).is_some()), "no socket announced in time");
        assert!(until(|| place.socket_of(&seat).is_some_and(|s| s.exists())), "compound-sup never bound its socket");

        let text = until_text(&place, &seat, "UNIQUE-COMPOUND-111-MARK");
        assert!(text.contains("UNIQUE-COMPOUND-111-MARK"), "not what the shell printed: {text:?}");
        assert!(!text.contains('|'), "compound-sup's own line-number prefix leaked through: {text:?}");
        assert!(!text.lines().any(|l| l.starts_with("size ")), "the trailing `size WxH` line leaked through: {text:?}");

        place.stop(&seat).expect("the seat was there");
    }

    /// `compound-174`'s second half, measured: **`nudge` presses submit, and
    /// the thing it presses it for is a composer holding an order nobody sent.**
    ///
    /// The shape is `sh` rather than an agent, because this is about the wire
    /// and not about an agent taking a turn. A shell reading its stdin echoes
    /// every line it is given, so "was that line submitted" is a thing the
    /// screen can answer — which is the only honest way to test a key press on
    /// a backend whose whole fault was that nobody could press one.
    ///
    /// The text is typed by hand, over the render socket, WITHOUT the submit
    /// that [`Compound::tell`] would have sent with it. That is the state
    /// `hand_over`'s rescue loop exists for: an order delivered and unsent
    /// behind whatever is holding the keyboard. `tell` cannot set it up — it
    /// types and submits in one conversation — so the test talks to the
    /// renderer the way the key path does, which is also why this proves the
    /// key event is right rather than merely that the call returns `Ok`.
    ///
    /// Needs a real machine: `COMPOUND_SUP` at a built `compound-sup`, and the
    /// `compound-render` beside it. Run it with
    /// `cargo test --bin wsp -- --ignored a_nudge_presses_submit_on_a_real_renderer --nocapture`.
    #[test]
    #[ignore]
    fn a_nudge_presses_submit_on_a_real_renderer() {
        let Some(sup) = std::env::var_os("COMPOUND_SUP") else {
            eprintln!("skipped: set COMPOUND_SUP to a built compound-sup to run this");
            return;
        };
        assert!(PathBuf::from(&sup).is_file(), "COMPOUND_SUP is not a file");

        let scratch = Scratch::new("nudge");
        let place = scratch.place();
        let seat = place
            .open(&Order { label: "compound-174 nudge".into(), ..Order::default() })
            .expect("a seat");
        place
            .start(&seat, &Agent { kind: "sh".into(), name: "smoke".into(), args: vec!["-c".into(), "while read -r line; do printf 'GOT:%s\\n' \"$line\"; done".into()], key: None })
            .expect("compound-sup started");

        assert!(until(|| place.socket_of(&seat).is_some_and(|s| s.exists())), "compound-sup never bound its socket");
        assert!(
            until(|| place.render_socket_of(&seat).is_some_and(|s| s.exists())),
            "no renderer came up, so there is nothing for nudge to press"
        );

        // Type WITHOUT submitting: the unsent order, by hand. No attach
        // needed — this test types with one connection exactly as
        // `type_and_submit` now does, so it doubles as the check that a bare
        // typing connection is what the code under test sends too.
        let socket = place.render_socket_of(&seat).unwrap();
        let mut stream = dial(&socket, DIAL_TIMEOUT).expect("dial the renderer");
        send(&mut stream, json!({ "input": { "bytes": b64(b"COMPOUND-174-NUDGE") } }))
            .expect("type the order");
        let _ = stream.shutdown(Shutdown::Both);
        // The shell has not seen it: nothing was submitted, so `read` has not
        // run and nothing is on the screen. If that is not true the test is
        // measuring the wrong thing and says so rather than passing.
        assert!(
            !until_text(&place, &seat, "GOT:COMPOUND-174-NUDGE").contains("GOT:COMPOUND-174-NUDGE"),
            "the line arrived without a nudge, so this proves nothing"
        );

        // Now the press, and the same assertion the other way round.
        place.nudge(&seat).expect("nudge reached the renderer");
        let echoed = until_text(&place, &seat, "GOT:COMPOUND-174-NUDGE");
        assert!(
            echoed.contains("GOT:COMPOUND-174-NUDGE"),
            "nudge pressed nothing the shell could see: {echoed:?}"
        );

        place.stop(&seat).expect("the seat was there");
    }

    /// `compound-229`: `tell` and `nudge` must work on a seat whose pane is
    /// **open in a window**, because the renderer allows exactly one attached
    /// client and the window is the one holding it.
    ///
    /// The whole bug lived in one frame. `type_and_submit` and `nudge` both
    /// opened with `Attach`, so whenever any window had the pane open —
    /// focused or not, looking or not — the renderer's gate answered them
    /// "*busy: a client is already attached*" and `wsp tell` failed on the
    /// seats most worth telling. `compound-117` had already made `Input` and
    /// `Key` answerable from any connection, so the attach bought nothing
    /// and cost the one thing that mattered.
    ///
    /// Both halves are here because the fix was two edits in two functions,
    /// and one of them passing says nothing about the other.
    ///
    /// Needs a real machine: `COMPOUND_SUP` at a built `compound-sup`, and the
    /// `compound-render` beside it. Run it with
    /// `cargo test --bin wsp -- --ignored a_tell_and_a_nudge_arrive_while_another_client_holds_the_attach --nocapture`.
    #[test]
    #[ignore]
    fn a_tell_and_a_nudge_arrive_while_another_client_holds_the_attach() {
        let Some(sup) = std::env::var_os("COMPOUND_SUP") else {
            eprintln!("skipped: set COMPOUND_SUP to a built compound-sup to run this");
            return;
        };
        assert!(PathBuf::from(&sup).is_file(), "COMPOUND_SUP is not a file");

        let scratch = Scratch::new("attach-busy");
        // `insisting` only steps over the readiness gate: `sh` fires no hooks,
        // so this seat never reads as one that will take a prompt, and the
        // subject here is the attach and nothing else.
        let place = scratch.place().insisting();
        let seat = place
            .open(&Order { label: "compound-229 busy".into(), ..Order::default() })
            .expect("a seat");
        place
            .start(&seat, &Agent { kind: "sh".into(), name: "smoke".into(), args: vec!["-c".into(), "while read -r line; do printf 'GOT:%s\\n' \"$line\"; done".into()], key: None })
            .expect("compound-sup started");

        assert!(until(|| place.socket_of(&seat).is_some_and(|s| s.exists())), "compound-sup never bound its socket");
        assert!(
            until(|| place.render_socket_of(&seat).is_some_and(|s| s.exists())),
            "no renderer came up, so there is nothing to attach to"
        );

        // The window. Held open for the whole test, which is the point: not a
        // window that opened and closed, but the pane a person is sitting in
        // front of while something else tries to tell that agent something.
        let socket = place.render_socket_of(&seat).unwrap();
        let _window = window_attaches(&socket);

        place
            .tell(&seat, "COMPOUND-229-TELL")
            .expect("delivered while the window holds the attach");
        let echoed = until_text(&place, &seat, "GOT:COMPOUND-229-TELL");
        assert!(echoed.contains("GOT:COMPOUND-229-TELL"), "tell typed nothing the shell could see: {echoed:?}");

        // `nudge`, same attach still held: type the order unsent, then press.
        let mut typing = dial(&socket, DIAL_TIMEOUT).expect("dial the renderer");
        send(&mut typing, json!({ "input": { "bytes": b64(b"COMPOUND-229-NUDGE") } })).expect("type the order");
        let _ = typing.shutdown(Shutdown::Both);
        assert!(
            !until_text(&place, &seat, "GOT:COMPOUND-229-NUDGE").contains("GOT:COMPOUND-229-NUDGE"),
            "the line arrived without a nudge, so this proves nothing"
        );

        place
            .nudge(&seat)
            .expect("nudge reached the renderer while the window holds the attach");
        let pressed = until_text(&place, &seat, "GOT:COMPOUND-229-NUDGE");
        assert!(pressed.contains("GOT:COMPOUND-229-NUDGE"), "nudge pressed nothing the shell could see: {pressed:?}");

        place.stop(&seat).expect("the seat was there");
    }

    /// Hold a renderer's attach the way a window with the pane open does, and
    /// hand back the connection still holding it.
    ///
    /// A granted `Attach` is answered with a frame stream, not a reply — the
    /// only thing the gate ever says in words is its refusal — so this
    /// watches for the refusal and takes any first frame as the proof. A
    /// window that never really got the attach would make
    /// `a_tell_and_a_nudge_arrive_while_another_client_holds_the_attach`
    /// pass for the wrong reason, which is the failure a test like this
    /// cannot have.
    fn window_attaches(render_socket: &PathBuf) -> UnixStream {
        let mut stream = dial(render_socket, DIAL_TIMEOUT).expect("dial the renderer");
        send(&mut stream, json!({ "attach": {} })).expect("send the window's attach");
        // `screen` answers on every connection, attached or not, so it is the
        // one frame that says this exchange reached a live renderer at all.
        send(&mut stream, json!({ "screen": {} })).expect("ask the window's screen");
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let waited = Instant::now();
            if waited >= deadline {
                panic!("the renderer answered the window's attach with nothing at all");
            }
            let Some(v) = recv_line(&mut stream, (deadline - waited).min(Duration::from_millis(200))) else {
                continue;
            };
            assert!(
                v.get("error").is_none(),
                "the window's own attach was refused: {}",
                str_of(&v["error"], "what")
            );
            return stream;
        }
    }

    /// Poll [`Compound::read_screen`] until it contains `mark` or the guard
    /// gives up, and return whatever it last saw — the pty needs a moment to
    /// actually draw what was written to it.
    fn until_text(place: &Compound<'static>, seat: &Seat, mark: &str) -> String {
        let mut last = String::new();
        for _ in 0..500 {
            if let Ok(t) = place.read_screen(seat) {
                if t.contains(mark) {
                    return t;
                }
                last = t;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        last
    }

    /// `census` speaks for whatever this directory holds and nothing beyond
    /// it — the property every implementor of the port owes, proven here
    /// the same way `place_super`'s own test proves it.
    #[test]
    fn census_answers_for_this_directory_and_nothing_else() {
        let scratch = Scratch::new("census");
        let place = scratch.place();
        assert_eq!(place.census().unwrap().seats().count(), 0);
        let seat = place.open(&Order { label: "row".into(), ..Order::default() }).unwrap();
        let rows: Vec<_> = place.census().unwrap().seats().cloned().collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seat, seat);
        assert_eq!(rows[0].label, "row");
        assert_eq!(rows[0].state, State::Empty, "opened, not started");
    }

    // -- wire_health (compound-026) ------------------------------------------
    //
    // Against fake scripts stood in for `compound-sup`, never `$COMPOUND_SUP`
    // — that env var is process-global and every other test in this file
    // shares the process (`wsp-env-breaks-cargo-test`'s lesson, one door
    // over). `wire_health_of` takes the binary as an argument for exactly
    // this reason.

    /// A shell script at `dir/compound-sup`, executable, that prints `reply`
    /// to stdout and exits 0 when called as `wire-version` — or exits 2 with
    /// nothing on stdout otherwise, the same shape a real older binary's
    /// unrecognised-verb arm already takes ([`main`]'s `verb =>` arm in the
    /// compound repository).
    fn fake_sup(dir: &std::path::Path, reply: Option<&str>) -> PathBuf {
        let path = dir.join("compound-sup");
        let body = match reply {
            Some(v) => format!("#!/bin/sh\ncase \"$1\" in\n  wire-version) echo {v}; exit 0 ;;\n  *) exit 2 ;;\nesac\n"),
            None => "#!/bin/sh\nexit 2\n".to_string(),
        };
        // Written by a child process, never by this one: a test thread that
        // forks while this process holds the script open for writing hands the
        // fd to its child, and on Linux exec of the script then fails with
        // ETXTBSY until that child execs (4 runs in 20 in parallel, 0 in 20
        // single-threaded, on Omarchy).
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("cat > \"$1\" && chmod 755 \"$1\"")
            .arg("sh")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), body.as_bytes()).unwrap();
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success(), "writing {}", path.display());
        path
    }

    /// The ordinary case, now that both checks can pass: a `compound-sup`
    /// that answers the wsp-side `WIRE_VERSION` and no `host` beside it to
    /// disagree with — nothing to say.
    #[test]
    fn wire_health_says_nothing_when_the_installed_pieces_agree() {
        // The range a current build prints, and the bare number an older one does.
        for (i, reply) in [format!("9 {WIRE_VERSION}"), WIRE_VERSION.to_string()].iter().enumerate() {
            let scratch = Scratch::new(&format!("wire-ok-{i}"));
            let sup = fake_sup(&scratch.root, Some(reply));
            let (mut problems, mut notes) = (Vec::new(), Vec::new());
            wire_health_of(&sup, &mut problems, &mut notes);
            assert!(problems.is_empty(), "{reply:?}: {problems:?}");
            assert!(notes.is_empty(), "{reply:?}: {notes:?}");
        }
    }

    /// `compound-031`'s shape: the installed binary speaks a version this
    /// hand-copied constant does not, named as a problem — not a note, the
    /// way a spawn this actually blocks deserves — and the fix line says
    /// which file to edit rather than leaving that to be rediscovered.
    #[test]
    fn wire_health_catches_wsps_own_constant_going_stale() {
        let other = WIRE_VERSION + 1;
        for (i, reply) in [format!("{other} {}", other + 1), other.to_string()].iter().enumerate() {
            let scratch = Scratch::new(&format!("wire-stale-const-{i}"));
            let sup = fake_sup(&scratch.root, Some(reply));
            let (mut problems, mut notes) = (Vec::new(), Vec::new());
            wire_health_of(&sup, &mut problems, &mut notes);
            assert_eq!(problems.len(), 1, "{reply:?}: {problems:?}");
            assert!(problems[0].contains(&WIRE_VERSION.to_string()), "{}", problems[0]);
            assert!(problems[0].contains(&other.to_string()), "{}", problems[0]);
            assert!(problems[0].contains("place_compound.rs"), "{}", problems[0]);
            assert!(notes.is_empty(), "{notes:?}");
        }
    }

    /// A compound-sup newer than wsp that still accepts wsp's version: wsp is
    /// answered, so nothing is wrong (the shape a fresh install showed as ✗).
    #[test]
    fn wire_health_is_quiet_when_wsps_version_is_inside_the_accepted_range() {
        let scratch = Scratch::new("wire-in-range");
        let sup = fake_sup(&scratch.root, Some(&format!("{WIRE_VERSION} {}", WIRE_VERSION + 5)));
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        wire_health_of(&sup, &mut problems, &mut notes);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// compound-sup behind wsp: the fix is a compound-sup rebuild, not wsp's
    /// constant; and an answer that is not a range is a problem, not a note.
    #[test]
    fn wire_health_names_the_side_that_is_behind_and_refuses_a_garbled_answer() {
        let scratch = Scratch::new("wire-sup-behind");
        let sup = fake_sup(&scratch.root, Some(&format!("{} {}", WIRE_VERSION - 2, WIRE_VERSION - 1)));
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        wire_health_of(&sup, &mut problems, &mut notes);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("compound-sup is behind"), "{}", problems[0]);
        assert!(!problems[0].contains("bump WIRE_VERSION"), "{}", problems[0]);
        for (i, reply) in [format!("{} {}", WIRE_VERSION + 5, WIRE_VERSION), format!("9 {WIRE_VERSION} x")].iter().enumerate() {
            let scratch = Scratch::new(&format!("wire-garbled-{i}"));
            let sup = fake_sup(&scratch.root, Some(reply));
            let (mut problems, mut notes) = (Vec::new(), Vec::new());
            wire_health_of(&sup, &mut problems, &mut notes);
            assert_eq!(problems.len(), 1, "{reply:?}: {problems:?}");
            assert!(problems[0].contains("not a version range"), "{}", problems[0]);
            assert!(notes.is_empty(), "{notes:?}");
        }
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// An installed `compound-sup` old enough to predate the `wire-version`
    /// verb is a fact this cannot check, not a fault — a note, same as no
    /// `compound-sup` at all reads as one.
    #[test]
    fn wire_health_notes_rather_than_alarms_when_the_verb_is_unknown() {
        let scratch = Scratch::new("wire-no-verb");
        let sup = fake_sup(&scratch.root, None);
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        wire_health_of(&sup, &mut problems, &mut notes);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("wire-version"), "{}", notes[0]);
    }

    /// August's shape, reproduced without needing a real cargo workspace:
    /// `host` sitting beside `compound-sup` with a later mtime is exactly
    /// what a `host`-only rebuild leaves behind, caught with no source tree
    /// read at all.
    #[test]
    fn wire_health_catches_a_host_only_rebuild_by_mtime() {
        let scratch = Scratch::new("wire-stale-host");
        let sup = fake_sup(&scratch.root, Some(&WIRE_VERSION.to_string()));
        std::thread::sleep(Duration::from_millis(50));
        fs::write(scratch.root.join("host"), "not a real binary, only its mtime matters here").unwrap();
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        wire_health_of(&sup, &mut problems, &mut notes);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("is newer than"), "{}", problems[0]);
        assert!(problems[0].contains("cargo build"), "{}", problems[0]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// The reverse of the case above: `compound-sup` rebuilt at least as
    /// recently as `host` is the ordinary state of the world (a full
    /// `cargo build`, or `host` simply untouched since) and says nothing.
    #[test]
    fn wire_health_says_nothing_when_compound_sup_is_not_older_than_host() {
        let scratch = Scratch::new("wire-host-not-stale");
        fs::write(scratch.root.join("host"), "not a real binary, only its mtime matters here").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let sup = fake_sup(&scratch.root, Some(&WIRE_VERSION.to_string()));
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        wire_health_of(&sup, &mut problems, &mut notes);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// The screen a `claude` started in `/` showed on 2026-10-06, as
    /// `compound-render screen` printed it (`wsp-219`). Seen, it is the reason
    /// the order is not typed, and the reason names the prompt and the folder
    /// rather than the renderer socket the agent's exit would have removed.
    #[test]
    fn a_seat_at_the_folder_trust_prompt_is_not_typed_at_and_says_why() {
        let screen = "\
 Accessing workspace:

 /

 Quick safety check: Is this a project you created or one you trust?

 \u{276f} No, exit
   Yes, I trust this folder

 Enter to confirm \u{b7} Esc to cancel";
        let why = trust_prompt(&Seat::new("cpd-420"), screen).expect("the trust prompt was not seen");
        assert!(why.contains("cpd-420") && why.contains("folder-trust prompt for /"), "{why}");
        assert!(why.contains("No, exit") && !why.contains("rsock"), "{why}");
        assert_eq!(trust_prompt(&Seat::new("cpd-1"), "> \n? for shortcuts"), None, "a composer is not a prompt");
    }

    /// `wsp-092`, the hook's half: the real hook ([`Compound::heard`]), run in
    /// a loop from another thread through a seat's end and on past it for a
    /// moment, neither keeps the seat from ending nor **brings its directory
    /// back** once it has gone. On 2026-11-18 `cpd-1025` and `cpd-1026` would
    /// not end: "Directory not empty (os error 66)".
    ///
    /// This one does not pin the removal's retry: the hook guards on the
    /// directory being there and writes about one file a call, so the window
    /// is narrow and trunk failed it about one run in nine. That is
    /// [`a_seat_directory_that_keeps_gaining_files_while_it_is_removed_still_goes`]'s.
    #[test]
    fn a_seat_whose_hooks_write_while_it_is_ended_still_ends_and_does_not_come_back() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let scratch = Scratch::new("hookrace");
        let place = Compound { poll: Duration::from_millis(1), ..scratch.place() };
        for n in 1..=20 {
            let seat = Seat::new(format!("cpd-{n}"));
            let dir = place.dir_of(&seat).unwrap();
            fs::create_dir_all(&dir).unwrap();
            write_atomic(&dir.join(SEAT_FILE), &json!({ "label": "t" }).to_string()).unwrap();
            let (stop, wrote) = (AtomicBool::new(false), AtomicUsize::new(0));
            let ended = std::thread::scope(|s| {
                s.spawn(|| {
                    let hook = Compound::at(scratch.root.clone());
                    while !stop.load(Ordering::Relaxed) {
                        hook.heard(&seat, "Stop", State::Idle, &json!({ "session_id": "s" }));
                        wrote.fetch_add(1, Ordering::Relaxed);
                    }
                });
                while wrote.load(Ordering::Relaxed) < 5 {
                    std::thread::yield_now();
                }
                let ended = place.end(&seat);
                std::thread::sleep(Duration::from_millis(20));
                stop.store(true, Ordering::Relaxed);
                ended
            });
            ended.unwrap_or_else(|e| panic!("{seat} was being written to as it ended, and still ends: {e}"));
            assert!(!dir.exists(), "{seat}: a late hook does not bring the directory back");
            assert!(place.record(&seat).is_err() && !place.ids().contains(&seat.to_string()), "{seat} is no seat any more");
        }
    }

    /// `wsp-092`, the removal's half: a seat directory that keeps gaining new
    /// files for the whole of its removal still goes, and the seat is gone
    /// from every census. Two writers with no guard put a new file in every
    /// few microseconds, so the last step of every `remove_dir_all` finds one:
    /// trunk's single removal refuses here with "Directory not empty", and so
    /// do all of [`Compound::take_away`]'s retries — it is the move aside that
    /// ends these seats. (Checked by hand: with `take_away` cut back to a
    /// plain `remove_dir_all` this failed 15 runs in 15; as written it took
    /// every seat `Aside` and passed 15 in 15.) A hook's burst is shorter, and
    /// a retry is what usually takes it; no test pins that.
    #[test]
    fn a_seat_directory_that_keeps_gaining_files_while_it_is_removed_still_goes() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let scratch = Scratch::new("flood");
        let place = Compound { poll: Duration::from_millis(1), ..scratch.place() };
        let mut took = Vec::new();
        for n in 1..=10 {
            let seat = Seat::new(format!("cpd-{n}"));
            let dir = place.dir_of(&seat).unwrap();
            fs::create_dir_all(&dir).unwrap();
            write_atomic(&dir.join(SEAT_FILE), &json!({ "label": "t" }).to_string()).unwrap();
            let (stop, wrote) = (AtomicBool::new(false), AtomicUsize::new(0));
            let outcome = std::thread::scope(|s| {
                for w in 0..2 {
                    let (dir, stop, wrote) = (&dir, &stop, &wrote);
                    s.spawn(move || {
                        let mut i = 0u64;
                        while !stop.load(Ordering::Relaxed) {
                            // `fs::write` makes no directory: once the seat's has
                            // gone (or been moved aside) these fail, as a hook's would.
                            if fs::write(dir.join(format!("late-{w}-{i}")), "x").is_ok() {
                                wrote.fetch_add(1, Ordering::Relaxed);
                            }
                            i += 1;
                        }
                    });
                }
                while wrote.load(Ordering::Relaxed) < 50 {
                    std::thread::yield_now();
                }
                let outcome = place.take_away(&dir);
                stop.store(true, Ordering::Relaxed);
                outcome
            });
            let how = outcome.unwrap_or_else(|e| panic!("{seat} kept gaining files as it was removed, and still goes: {e}"));
            assert!(!dir.exists(), "{seat}: its directory is gone ({how:?})");
            assert!(place.record(&seat).is_err() && !place.ids().contains(&seat.to_string()), "{seat} is no seat any more");
            took.push(how);
        }
        assert!(
            took.iter().any(|t| *t != Took::AtOnce),
            "the flood made some removal retry or move aside, so this pins that path: {took:?}"
        );
    }

    /// What was moved aside and would not go at once is cleared by a later end,
    /// and is never read as a seat meanwhile (`wsp-092`).
    #[test]
    fn a_leftover_moved_aside_is_no_seat_and_a_later_end_clears_it() {
        let scratch = Scratch::new("leftover");
        let place = scratch.place();
        let left = scratch.root.join(format!("{GONE}cpd-7-1-2"));
        fs::create_dir_all(left.join("deep")).unwrap();
        fs::write(left.join("said.json"), "{}").unwrap();
        fs::write(left.join("deep").join("hooks.jsonl"), "{}\n").unwrap();
        assert!(place.ids().iter().all(|id| !id.starts_with(GONE) && !id.contains("cpd-7")), "{:?}", place.ids());
        let seat = Seat::new("cpd-8");
        fs::create_dir_all(place.dir_of(&seat).unwrap()).unwrap();
        place.end(&seat).unwrap();
        assert!(!left.exists(), "the later end cleared the leftover");
        assert!(!place.dir_of(&seat).unwrap().exists());
    }

    /// Any error but a directory that filled up again still refuses: the seat
    /// is kept, so nothing reads a removal that did not happen as an ending.
    #[test]
    fn a_seat_whose_directory_will_not_go_for_another_reason_is_not_ended() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new("stuck");
        let place = scratch.place();
        let seat = Seat::new("cpd-1");
        let dir = place.dir_of(&seat).unwrap();
        fs::create_dir_all(dir.join("held")).unwrap();
        fs::write(dir.join("held").join("f"), "x").unwrap();
        fs::set_permissions(dir.join("held"), fs::Permissions::from_mode(0o555)).unwrap();
        let outcome = place.end(&seat);
        let _ = fs::set_permissions(dir.join("held"), fs::Permissions::from_mode(0o755));
        let root = wsp_base::calls::Command::new("id").arg("-u").output().is_ok_and(|o| o.stdout == b"0\n");
        if !root {
            let err = outcome.expect_err("a permission refusal is not an ending");
            assert!(matches!(err, Refusal::Backend(_)), "{err}");
            assert!(place.holds(&seat), "and the seat is kept");
        }
    }
}
