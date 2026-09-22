//! compound behind the place-work port: a real pty per seat, observed the
//! same way [`crate::place_super`] observes a headless one.
//!
//! The fourth implementor of `place.rs` (`compound-064`, `compound` d7/d9).
//! `place_herdr` types at a shell inside a multiplexer; `place_super` forks a
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
//! stdio — so [`crate::place_super::said_by`], [`crate::place_super::alive`]
//! and [`crate::place_super::tally_burn`] are reused verbatim rather than
//! copied. What differs is only what a seat *records*: a `compound-sup`
//! socket and pid instead of a `tail`/agent process pair. Two directories,
//! two [`Place`] implementors, one hook vocabulary — `wsp report` (below)
//! is what keeps a hook from having to know which one it landed in.
//!
//! herdr's screen-scraped `Blocked` state has no equivalent in
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
//! [`Place::tell`] actually needs — an envelope, `Attach`, `Input`, `Key`,
//! `Detach` — read against `crates/supervisor/src/proto.rs` VERSION 6 at
//! the time of writing and versioned the same way that file is: a refusal
//! rather than a guess if the peer ever disagrees. **This is the one seam
//! in this file that ages by hand** — a wire bump on the compound side is
//! invisible here until something exercises it, which is why the version is
//! checked and refused loudly rather than assumed.
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
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::place::{self, Agent, Census, Delivery, Event, Order, Place, Refusal, Result, Seat, Seated, State};
use crate::place_super::{alive, read_json, signal_group, str_of, tally_burn};
#[cfg(test)]
use crate::place_super::said_by;
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
    /// So this is the way past: a governor who has LOOKED — `compound-sup
    /// screen` is the honest surface — passes `wsp tell --anyway` and takes
    /// the decision themselves. It is a field on the backend rather than a
    /// widening of `Place`, for the reason `linger` and `poll` are: it
    /// configures this implementation, it does not change what the port
    /// promises. `compound-065` says a row that finds itself widening the
    /// port stops and asks, and this one did not have to.
    pub insist: bool,
    pub clock: &'a dyn Clock,
}

impl Compound<'static> {
    pub fn new() -> Compound<'static> {
        Compound::at(Store::open().state.join(SEATS))
    }

    /// One rooted anywhere — a test's temporary directory.
    pub fn at(root: PathBuf) -> Compound<'static> {
        Compound {
            root,
            linger: Duration::from_millis(2_000),
            poll: Duration::from_millis(150),
            insist: false,
            clock: &util::Wall,
        }
    }

    /// The same backend, willing to deliver to a seat that cannot vouch for
    /// itself. See [`Compound::insist`] — this is `wsp tell --anyway` and
    /// nothing else builds it.
    pub fn insisting(self) -> Compound<'static> {
        Compound { insist: true, ..self }
    }
}

const SEATS: &str = "compound-seats";
const SEAT_FILE: &str = "seat.json";
const SAID_FILE: &str = "said.json";

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
const HOOKS_FILE: &str = "hooks.jsonl";

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
const SEAT_PREFIX: &str = "cpd-";

/// How long `start` waits for `compound-sup`'s handshake before giving up on
/// it. Generous: the supervisor daemonizes, opens libghostty and binds a pty
/// before it prints the third line, and a spawn that is going to fail (a
/// bad cwd, a missing program) says so on the same stdout rather than
/// hanging — so the bound is a safety net, not the expected path.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a `tell` or `state`-adjacent dial waits for the socket to
/// accept. A live session answers `Attach` in well under a millisecond
/// (it is a local accept, not a wire round trip); this is room for a
/// session under load, not an expected wait.
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);

/// Where `compound-sup` lives: `$COMPOUND_SUP`, or found on `PATH`.
///
/// There is no relative-path fallback the way `crates/host/src/sessions.rs`
/// has one, and cannot be: that fallback is "beside this binary", which
/// means something for the compound host built from its own workspace and
/// means nothing for `wsp`, a separate binary in a separate repository. An
/// operator who wants `wsp` to place compound work sets the variable once,
/// the same shape as `HERDR_SOCKET_PATH` already is for the other backend.
fn compound_sup_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COMPOUND_SUP") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("compound-sup")).find(|p| p.is_file())
}

impl Compound<'_> {
    pub(crate) fn dir_of(&self, seat: &Seat) -> Result<PathBuf> {
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

    fn record(&self, seat: &Seat) -> Result<Value> {
        let dir = self.dir_of(seat)?;
        match dir.is_dir() {
            true => Ok(read_json(&dir.join(SEAT_FILE))),
            false => Err(Refusal::NoSeat(seat.clone())),
        }
    }

    fn said(&self, seat: &Seat) -> Value {
        match self.dir_of(seat) {
            Ok(dir) => read_json(&dir.join(SAID_FILE)),
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
        let was = read_json(&dir.join(SAID_FILE));
        let keep = |key: &str| -> String {
            match str_of(payload, key).is_empty() {
                true => str_of(&was, key),
                false => str_of(payload, key),
            }
        };
        let _ = write_atomic(
            &dir.join(SAID_FILE),
            &json!({
                "state": state.as_str(),
                "hook": hook,
                "at": util::now_iso(),
                "session_id": keep("session_id"),
                "transcript_path": keep("transcript_path"),
            })
            .to_string(),
        );
        self.append_hooks_log(&dir, hook, state);
        tally_burn(&dir, payload);
    }

    /// Append one line to [`HOOKS_FILE`]. Best-effort, like everything else a
    /// hook touches — a write that fails here must not be the write that
    /// fails the hook, and a reader missing one line is a smaller loss than a
    /// session that stalls on it.
    fn append_hooks_log(&self, dir: &std::path::Path, hook: &str, state: State) {
        use std::io::Write;
        let line = json!({ "at": util::now_iso(), "hook": hook, "state": state.as_str() });
        if let Ok(mut f) =
            fs::OpenOptions::new().create(true).append(true).open(dir.join(HOOKS_FILE))
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// Whether this seat's `compound-sup` still holds its pid, per the same
    /// `ps` `alive` reads for a bare-forked one. `compound-sup` daemonizes
    /// but never forks again, so the pid this recorded at `start` is the
    /// process for the session's whole life — there is no second pid to
    /// reconcile the way a fork-then-exec backend would have to.
    fn state_of(&self, seat: &Seat, rec: &Value, running: &BTreeSet<u32>) -> State {
        let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()) else { return State::Empty };
        if !running.contains(&(pid as u32)) {
            return State::Gone;
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
        // this backend has no second source** (`compound-097`). herdr watches
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
        let running = alive(&pids);
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

    /// What this seat's pty is showing right now — `compound-sup screen
    /// <socket>`, the same call a person types by hand.
    ///
    /// Not on [`Place`]: reading a pane is the observe half `place.rs`'s
    /// module docs put outside this port, which is why `wsp peek` already
    /// calls herdr's `pane.read` directly rather than through a trait method
    /// — this is that seam's compound answer, called the same way. Named in
    /// `compound-111`: with no read of any kind, `compound-sup screen
    /// <socket>`, typed by a person, was "the only honest surface all week".
    ///
    /// `compound-sup`'s own presentation is a `NNN|text` line per row and a
    /// trailing `size WxH`; stripped here down to bare text so `wsp peek`
    /// prints the same shape whichever backend answered.
    pub(crate) fn read_screen(&self, seat: &Seat) -> Result<String> {
        let socket = self.socket_of(seat).ok_or_else(|| Refusal::NoSeat(seat.clone()))?;
        let sup = compound_sup_binary()
            .ok_or_else(|| Refusal::Backend("compound-sup not found — set $COMPOUND_SUP or put it on PATH".into()))?;
        let out = Command::new(&sup)
            .args(["screen"])
            .arg(&socket)
            .output()
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        if !out.status.success() {
            return Err(Refusal::Backend(String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        let raw = String::from_utf8_lossy(&out.stdout);
        let mut lines: Vec<&str> = raw.lines().collect();
        if lines.last().is_some_and(|l| l.starts_with("size ")) {
            lines.pop();
        }
        Ok(lines
            .into_iter()
            .map(|l| l.split_once('|').map_or(l, |(_, t)| t))
            .collect::<Vec<_>>()
            .join("\n"))
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
    /// Shells to `compound-sup state <socket> <kind>` — `compound-109`'s
    /// verb, wrapping the ported half of herdr's agent-detection engine —
    /// exactly as [`Compound::read_screen`] shells to `compound-sup screen`:
    /// this backend does not link compound's crates, so this is a wire read
    /// the same way that one is, not a dependency.
    ///
    /// `None` when there is nothing to ask: no socket minted yet, no
    /// `compound-sup` reachable, or an agent kind the ported engine has no
    /// manifest for — only `claude` and `opencode` are compiled
    /// (`compound-109`'s overview); anything else falls back to
    /// [`State::Unknown`] exactly as it did before this existed.
    ///
    /// **A process spawn and a socket round trip, not a free read.**
    /// Measured (`compound-109`, `compound-sup state` against a live
    /// opencode session, 20 calls): ~22ms each. `survey` calls [`state_of`]
    /// once per seat in the directory, serially, so a census of N seats
    /// whose hooks cannot answer costs N × ~22ms here — fine for the sizes
    /// `wsp ls`/`wsp kanban` run against today, and worth re-measuring
    /// before this is anywhere near an input path or a seat count that
    /// matters at that rate.
    fn detected_state(&self, seat: &Seat, rec: &Value) -> Option<State> {
        let agent = rec.get("agent")?;
        let kind = str_of(agent, "kind");
        if !matches!(kind.as_str(), "claude" | "opencode") {
            return None;
        }
        let socket = self.socket_of(seat)?;
        let sup = compound_sup_binary()?;
        let out = Command::new(&sup).args(["state"]).arg(&socket).arg(&kind).output().ok()?;
        if !out.status.success() {
            return None;
        }
        match String::from_utf8_lossy(&out.stdout).lines().next()? {
            "idle" => Some(State::Idle),
            "working" => Some(State::Working),
            "blocked" => Some(State::Blocked),
            // "unknown", or a shape this backend does not recognise yet —
            // read the same as no answer at all rather than guessed at.
            _ => Some(State::Unknown),
        }
    }
}

/// `compound-sup`'s three-line handshake, read with a deadline off a
/// background thread so a supervisor that never prints one (a bad build, a
/// hung `zig` toolchain, anything short of the ordinary path) cannot hang
/// [`Place::start`] rather than refuse it.
fn read_handshake(stdout: std::process::ChildStdout, timeout: Duration) -> Option<PathBuf> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut socket = None;
        for _ in 0..3 {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if let Some(path) = line.trim().strip_prefix("socket ") {
                socket = Some(PathBuf::from(path));
                break;
            }
        }
        // Best effort: a receiver that has already timed out is a send
        // nobody reads, not an error.
        let _ = tx.send(socket);
    });
    rx.recv_timeout(timeout).ok().flatten()
}

// ---------------------------------------------------------------------------
// The wire, read rather than depended on. See the module docs' "The wire is
// read here, not depended on".
// ---------------------------------------------------------------------------

/// `crates/supervisor/src/proto.rs` VERSION at the time this was written.
/// Bumped there without a matching bump here is exactly the drift
/// `robustness-017` warns an adapter not to risk quietly — so a mismatch is
/// refused rather than sent into a peer that may not parse it.
///
/// **That drift happened, 2026-09-22, and this is what it looks like.**
/// `compound-031` bumped the supervisor to v7 for `Body::Close`, correctly
/// re-reading `proto.rs` at rebase exactly as its decision told it to — but
/// this constant is in the OTHER repository, so nothing it could read named
/// it. The first symptom was `wsp tell` refused with `unsupported ver 6`,
/// which reads as a fault in the seat rather than in a hand-copied number.
/// v7 is purely additive (`Close {}` added, no shape changed), so every
/// message wsp already sends stays valid at it and this is a one-line catch-up
/// rather than a port.
///
/// The duplication itself is `compound-026`, which is where it should be
/// fixed: a constant maintained by hand in two repositories will drift again,
/// and the only reason this cost minutes rather than a day is that the
/// supervisor refuses a version it does not know instead of guessing.
const WIRE_VERSION: u32 = 7;

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
/// actually speaks, the same way [`compound_sup_binary`]'s callers already
/// ask it for `state` rather than trusting a guess.
///
/// Both are notes until they are not: no `compound-sup` on this machine is
/// not a problem — the herdr backend needs none of this — and a
/// `compound-sup` that cannot answer `wire-version` yet is an older binary,
/// not a broken one. What one function should never do is fix either: a
/// version check that started rebuilding or restarting somebody else's
/// binaries would be `robustness-090` d1's forbidden act wearing a doctor
/// badge, so this only ever says what is wrong and the verb that fixes it.
pub fn wire_health(problems: &mut Vec<String>, notes: &mut Vec<String>) {
    let Some(sup) = compound_sup_binary() else {
        notes.push(
            "compound-sup not found ($COMPOUND_SUP or PATH) — nothing to check about the compound wire".into(),
        );
        return;
    };
    wire_health_of(&sup, problems, notes);
}

/// [`wire_health`], given the binary rather than finding it — split out so a
/// test can point this at a fake script instead of racing every other test in
/// this process over `$COMPOUND_SUP` (`wsp-env-breaks-cargo-test`'s lesson,
/// one door over: a global is a global whether it names a store or a binary).
fn wire_health_of(sup: &std::path::Path, problems: &mut Vec<String>, notes: &mut Vec<String>) {
    // Follow the PATH symlink to the real binary, whose directory is the
    // cargo workspace's own `target/<profile>/` — where `host` lands too,
    // built by the same `cargo build` that builds this one. Not a path this
    // file invented: `scripts/supervisor_outlives_host_rebuild.sh` in the
    // compound repository already names both binaries at that convention.
    let real = fs::canonicalize(sup).unwrap_or_else(|_| sup.to_path_buf());
    if let Some(dir) = real.parent() {
        let host = dir.join("host");
        if let (Ok(hm), Ok(sm)) =
            (fs::metadata(&host).and_then(|m| m.modified()), fs::metadata(&real).and_then(|m| m.modified()))
        {
            if hm > sm {
                problems.push(format!(
                    "{} is newer than {} — a host-only rebuild (`cargo run -p host`) leaves compound-sup stale, \
                     and its version refusal will name the session rather than the build — `cargo build` rebuilds both (compound-026)",
                    util::contract(&host),
                    util::contract(&real),
                ));
            }
        }
    }

    match ask_wire_version(&sup) {
        Some(v) if v != WIRE_VERSION => problems.push(format!(
            "wsp's WIRE_VERSION is {WIRE_VERSION}, installed compound-sup speaks {v} — bump WIRE_VERSION in \
             src/place_compound.rs to match crates/supervisor/src/proto.rs (compound-026)"
        )),
        Some(_) => {}
        None => notes.push(format!(
            "{} does not answer `wire-version` — wsp cannot check its WIRE_VERSION ({WIRE_VERSION}) against it (compound-026)",
            util::contract(&sup),
        )),
    }
}

/// `compound-sup wire-version` → the bare number, or `None` if the installed
/// binary predates that verb (an older build, not a broken one) or will not
/// answer at all.
fn ask_wire_version(bin: &std::path::Path) -> Option<u32> {
    let out = Command::new(bin).arg("wire-version").stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten()
}

fn dial(socket: &PathBuf, timeout: Duration) -> Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(socket) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() >= deadline => {
                return Err(Refusal::Backend(format!("no answer from {}: {e}", socket.display())))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn send(stream: &mut UnixStream, body: Value) -> std::io::Result<()> {
    let mut line = json!({ "ver": WIRE_VERSION }).as_object().unwrap().clone();
    line.extend(body.as_object().cloned().unwrap_or_default());
    let mut bytes = serde_json::to_vec(&line)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)
}

/// One reply line, or `None` on EOF/timeout — a compound session that
/// answers nothing within the read timeout is read the same way a `tell`
/// with no watcher already reads: as `Unconfirmed`, not as a hang.
fn recv_line(stream: &mut UnixStream, timeout: Duration) -> Option<Value> {
    let _ = stream.set_read_timeout(Some(timeout));
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => serde_json::from_str(&line).ok(),
    }
}

/// Standard base64, matching `supervisor::proto`'s local encoder — the wire
/// carries `Input`'s bytes this way and there is no third-party dependency
/// worth pulling in for one field.
fn b64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Type at a session and press Enter — [`Place::tell`], stripped of the
/// hook-state bookkeeping so a test can drive it against a bare listener.
/// `Attach` first, because the wire refuses any other frame from an
/// unclaimed connection; `Detach` last, so the session sees this client
/// leave rather than an EOF it has to notice on its own clock.
fn type_and_submit(socket: &PathBuf, text: &str) -> Result<()> {
    let mut stream = dial(socket, DIAL_TIMEOUT)?;
    send(&mut stream, json!({ "attach": {} })).map_err(|e| Refusal::Backend(e.to_string()))?;
    match recv_line(&mut stream, DIAL_TIMEOUT) {
        Some(v) if v.get("error").is_some() => {
            return Err(Refusal::Backend(str_of(&v["error"], "what")))
        }
        None => return Err(Refusal::Backend("no answer to attach".into())),
        _ => {}
    }
    send(&mut stream, json!({ "input": { "bytes": b64(text.as_bytes()) } }))
        .map_err(|e| Refusal::Backend(e.to_string()))?;
    send(
        &mut stream,
        json!({ "key": { "event": {
            "action": "press",
            "key": "enter",
            "mods": { "shift": false, "ctrl": false, "alt": false, "cmd": false },
            "unshifted_codepoint": 0,
        }}}),
    )
    .map_err(|e| Refusal::Backend(e.to_string()))?;
    let _ = send(&mut stream, json!({ "detach": {} }));
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

impl Compound<'_> {
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
    fn spawn_sup(&self, seat: &Seat, rec: &Value, program: &str, args: &[String]) -> Result<Value> {
        let Some(sup) = compound_sup_binary() else {
            return Err(Refusal::Backend(
                "compound-sup not found — set $COMPOUND_SUP or put it on PATH".into(),
            ));
        };

        let mut cmd = Command::new(&sup);
        cmd.arg("spawn").args(["--label", seat.as_str()]);
        let cwd = str_of(rec, "cwd");
        if !cwd.is_empty() {
            cmd.args(["--cwd", &cwd]);
        }
        cmd.arg("--").arg(program).args(args);

        let env: BTreeMap<String, String> = rec
            .get("env")
            .and_then(|e| e.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        // Onto compound-sup's own environment, which its pty child inherits
        // in turn — the same "override, never replace" contract
        // `place_super::child_env` keeps, for the same reason: an empty
        // value is the only strip a spawned process's env accepts.
        for (k, v) in &env {
            match v.is_empty() {
                true => cmd.env_remove(k),
                false => cmd.env(k, v),
            };
        }
        for k in place::shed_keys() {
            cmd.env_remove(k);
        }

        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());

        let mut child = cmd.spawn().map_err(|e| Refusal::Backend(format!("{} did not start: {e}", sup.display())))?;
        let pid = child.id();
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            return Err(Refusal::Backend("compound-sup gave no handshake pipe".into()));
        };
        // The child is daemonized (setsid) and outlives this process by
        // design — dropped rather than waited on, same as
        // `crates/host/src/sessions.rs::Panes::open`.
        drop(child);

        let Some(socket) = read_handshake(stdout, HANDSHAKE_TIMEOUT) else {
            signal_group(pid, "KILL");
            return Err(Refusal::Backend("compound-sup never announced a socket".into()));
        };

        let mut rec = rec.clone();
        rec["pid"] = json!(pid);
        rec["socket"] = json!(socket.display().to_string());
        rec["started_at"] = json!(util::now_iso());
        Ok(rec)
    }

    /// `SIGTERM` the group, wait, `SIGKILL` if it lingers, and sweep the
    /// socket once nothing answers it. The shared tail of ending whatever
    /// currently occupies a seat's pty — whether the SEAT is going with it
    /// ([`Place::stop`]) or is about to hold a fresh session in its place
    /// ([`Place::start`] replacing a bare terminal, `compound-081`).
    fn end_process(&self, seat: &Seat, rec: &Value) {
        if let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32) {
            signal_group(pid, "TERM");
            let deadline = self.clock.now() + self.linger;
            while alive(&[pid]).contains(&pid) {
                if self.clock.now() >= deadline {
                    signal_group(pid, "KILL");
                    break;
                }
                self.clock.rest(self.poll);
            }
        }
        // `compound-sup` unlinks its own socket on the way out of its run
        // loop, but installs no `SIGTERM` handler — measured, not assumed —
        // so a signalled exit skips that cleanup and leaves the headstone
        // `compound-028` d2 already named the remedy for: connect, and an
        // `ECONNREFUSED` says nothing is listening, which is what licenses
        // removing the file rather than a process.
        if let Some(socket) = self.socket_of(seat) {
            if socket.exists() && UnixStream::connect(&socket).is_err() {
                let _ = fs::remove_file(&socket);
                let _ = fs::remove_file(socket.with_extension("label"));
            }
        }
    }
}

impl Place for Compound<'_> {
    /// Mints the seat's record, then — `compound-081` — puts a real shell in
    /// its pty, the same way herdr's `open` leaves a person looking at a
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
        if order.on.is_some() {
            // Nothing here reaches a socket on another machine; a compound
            // session is exactly as local as the pty it owns.
            return Err(Refusal::Unsupported("run a compound session on another machine"));
        }
        let seat = self.mint()?;
        let dir = self.dir_of(&seat)?;
        let mut env: BTreeMap<String, String> = order.env.clone();
        env.insert(place::SEAT_ENV.to_string(), seat.to_string());
        let rec = json!({
            "label": order.label,
            "cwd": order.cwd.as_deref().map(|c| util::expand(c).display().to_string()),
            "env": env,
            "opened_at": util::now_iso(),
        });
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string())
            .map_err(|e| Refusal::Backend(e.to_string()))?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        if let Ok(rec) = self.spawn_sup(&seat, &rec, &shell, &[]) {
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
        let dir = self.dir_of(seat)?;
        if let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()) {
            if alive(&[pid as u32]).contains(&(pid as u32)) {
                if rec.get("agent").is_some() {
                    return Err(Refusal::Backend(format!("{seat} already has a session running")));
                }
                self.end_process(seat, &rec);
            }
        }
        let mut rec = self.spawn_sup(seat, &rec, &agent.kind, &agent.args)?;
        rec["agent"] = json!({ "kind": agent.kind, "name": agent.name, "args": agent.args });
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string())
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        Ok(())
    }

    /// Paste the sentence, press Enter, disconnect — see the module docs'
    /// "`tell`, and why it need not hold a pipe open".
    fn tell(&self, seat: &Seat, text: &str) -> Result<Delivery> {
        let state = self.state(seat)?;
        // `insist` is a person saying they have looked; see its own doc.
        if !self.insist && !state.will_take_a_prompt() {
            return Err(Refusal::NotReady(state));
        }
        let socket = self.socket_of(seat).ok_or_else(|| Refusal::NoSeat(seat.clone()))?;
        type_and_submit(&socket, text)?;
        // Typed rather than watched: this backend has not read the reply
        // that would let it say more, so `Unconfirmed` is the honest answer
        // `place.rs` asks for from a backend that delivered without seeing
        // whether a turn started.
        Ok(Delivery::Unconfirmed)
    }

    /// End the session and let the seat go. `SIGTERM` the group (which is
    /// the compound-sup pid itself — see the module docs), a short wait for
    /// its own `SessionEnd`-on-the-way-out the same as `place_super`
    /// measures, then `SIGKILL`; the directory goes either way.
    fn stop(&self, seat: &Seat) -> Result<()> {
        let rec = self.record(seat)?;
        let dir = self.dir_of(seat)?;
        // `compound-031` is where the general case (a session whose host is
        // gone and nobody asked it to stop) is filed; this is the one seat
        // this call just ended.
        self.end_process(seat, &rec);
        fs::remove_dir_all(&dir).map_err(|e| Refusal::Backend(e.to_string()))?;
        Ok(())
    }

    fn state(&self, seat: &Seat) -> Result<State> {
        let rec = self.record(seat)?;
        let pids: Vec<u32> =
            rec.get("pid").and_then(|p| p.as_u64()).map(|p| vec![p as u32]).unwrap_or_default();
        Ok(self.state_of(seat, &rec, &alive(&pids)))
    }

    fn census(&self) -> Result<Census> {
        // One machine, and it is this one — a compound session is a local
        // pty and there is no fan-out here for `Census` to speak for.
        Ok(Census::heard("", self.survey()))
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
#[cfg(test)]
pub(crate) struct StopsOnDrop {
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

#[cfg(test)]
impl StopsOnDrop {
    pub(crate) fn new(seat: Seat) -> StopsOnDrop {
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
#[cfg(test)]
pub(crate) struct EndsOnDrop(pub(crate) u32);

#[cfg(test)]
impl Drop for EndsOnDrop {
    fn drop(&mut self) {
        signal_group(self.0, "KILL");
    }
}

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
            .start(&seat, &Agent { kind: "cat".into(), name: "smoke".into(), args: vec![] })
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
            .start(&seat, &Agent { kind: "cat".into(), name: "smoke".into(), args: vec![] })
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
            .start(&seat, &Agent { kind: "opencode".into(), name: "smoke".into(), args: vec![] })
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

        place.stop(&seat).expect("the seat was there");
    }

    /// `compound-111`: `wsp peek` had no way to read a compound seat at all —
    /// "herdr would not read cpd-2" — and the workaround all week was a
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
            .start(&seat, &Agent { kind: "sh".into(), name: "smoke".into(), args: vec!["-c".into(), "printf UNIQUE-COMPOUND-111-MARK; sleep 5".into()] })
            .expect("compound-sup started");

        assert!(until(|| place.socket_of(&seat).is_some()), "no socket announced in time");
        assert!(until(|| place.socket_of(&seat).is_some_and(|s| s.exists())), "compound-sup never bound its socket");

        let text = until_text(&place, &seat, "UNIQUE-COMPOUND-111-MARK");
        assert!(text.contains("UNIQUE-COMPOUND-111-MARK"), "not what the shell printed: {text:?}");
        assert!(!text.contains('|'), "compound-sup's own line-number prefix leaked through: {text:?}");
        assert!(!text.lines().any(|l| l.starts_with("size ")), "the trailing `size WxH` line leaked through: {text:?}");

        place.stop(&seat).expect("the seat was there");
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
        fs::write(&path, body).unwrap();
        let mut perm = fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        fs::set_permissions(&path, perm).unwrap();
        path
    }

    /// The ordinary case, now that both checks can pass: a `compound-sup`
    /// that answers the wsp-side `WIRE_VERSION` and no `host` beside it to
    /// disagree with — nothing to say.
    #[test]
    fn wire_health_says_nothing_when_the_installed_pieces_agree() {
        let scratch = Scratch::new("wire-ok");
        let sup = fake_sup(&scratch.root, Some(&WIRE_VERSION.to_string()));
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        wire_health_of(&sup, &mut problems, &mut notes);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// `compound-031`'s shape: the installed binary speaks a version this
    /// hand-copied constant does not, named as a problem — not a note, the
    /// way a spawn this actually blocks deserves — and the fix line says
    /// which file to edit rather than leaving that to be rediscovered.
    #[test]
    fn wire_health_catches_wsps_own_constant_going_stale() {
        let scratch = Scratch::new("wire-stale-const");
        let other = WIRE_VERSION + 1;
        let sup = fake_sup(&scratch.root, Some(&other.to_string()));
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        wire_health_of(&sup, &mut problems, &mut notes);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains(&WIRE_VERSION.to_string()), "{}", problems[0]);
        assert!(problems[0].contains(&other.to_string()), "{}", problems[0]);
        assert!(problems[0].contains("place_compound.rs"), "{}", problems[0]);
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
}
