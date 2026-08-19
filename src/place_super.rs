//! A supervisor behind the place-work port: agents with no terminal at all,
//! observed through the hook they fire rather than the screen they draw.
//!
//! The third implementor of `place.rs` and the second real one — `place_herdr`
//! is a multiplexer and `fake` is a socket with a state in it. This one has no
//! terminal anywhere: it forks a process, hands it a pipe, and learns what it is
//! doing because the agent says so.
//!
//! `place.rs` was written for this backend before it existed. Every verb was
//! checked against *could a supervisor with no TTY answer this?* and this file
//! is the answer being collected: eight verbs, no PTY, no rendering, no attach.
//!
//! # The boundary, which is the thing to hold
//!
//! **Not a multiplexer.** The moment it needs to host a terminal it is herdr and
//! the point is lost. A person who wants to sit down in front of an agent this
//! backend is running is a case it refuses, and refusing is correct.
//!
//! Ed's correction of 2026-08-17 is the sharp version of that and is worth
//! having in the file rather than only on the task: *talking* to an agent needs
//! no terminal — twenty-odd work orders went over Claude Code's session channel
//! that night, several to panes nobody had focused — and **attaching** does. A
//! Claude Code in a pane draws permission prompts, an input box and a TUI; an
//! agent hosted here has none of that. So this is a headless agent, which is a
//! different interaction model rather than the same one with less scenery, and
//! what you get is an agent you can start, tell, observe and stop.
//!
//! # How an agent is hosted, and why there is a `tail` in it
//!
//! Measured on 2026-08-17 against Claude Code 2.1.233, twice, before a line of
//! this was written — the pipeline below is the recording rather than a design:
//!
//! ```text
//! tail -n +1 -f <seat>/prompts.jsonl | claude -p --input-format stream-json …
//! ```
//!
//! Claude Code's headless mode reads work orders as JSON lines on stdin and
//! keeps the session alive between them, which is exactly [`Place::tell`]. What
//! it will not survive is **end of file**: whatever writes that stdin has to
//! stay open for the life of the agent, and a supervisor that exits the moment
//! it has forked cannot be that writer. The first probe held the pipe open from
//! a shell and worked; a `wsp tell` that opened, wrote and closed would deliver
//! one sentence and leave the agent deaf.
//!
//! So the durable writer is a `tail -f` on a file, and [`Place::tell`] is an
//! append. Three things fall out of it and each is worth the extra process:
//!
//! - a `tell` cannot block and cannot be refused by a pipe — the refusal comes
//!   from [`Place::state`], which is a fact about the agent rather than about a
//!   file descriptor;
//! - what the seat was told is on disk, in order, which is the record a headless
//!   agent otherwise has nobody to have told;
//! - nothing here opens a FIFO, whose one-writer-at-a-time semantics are the
//!   part of this that would have needed a libc constant and a platform `cfg`.
//!
//! The cost is named rather than discovered later: two processes per seat, in
//! one process group, and [`Place::stop`] ends the group rather than the agent.
//! An agent that exits on its own leaves its feed behind — `tail` only learns
//! that nothing is reading it when it next has something to write — so a seat
//! whose agent has gone holds an idle `tail` until the seat is stopped. That is
//! why `stop` signals the group even when the agent is already [`State::Gone`],
//! and it is the one thing here that would be free to a supervisor that stayed
//! resident.
//!
//! # The eyes: what the agent announces, against what a screen has to be read for
//!
//! This is the half the task was opened for. Claude Code fires a hook at each
//! lifecycle point and the payload names the session and its transcript, so
//! [`Place::state`], [`Place::census`] and [`Place::watch`] — the three verbs a
//! TTY-less backend was expected to find hard — are delivered by the agent
//! rather than inferred from a rendered pane.
//!
//! Measured, one run, milliseconds from the fork:
//!
//! | | |
//! |---|---|
//! | `SessionStart` | +870ms — the launch window, closed exactly |
//! | `UserPromptSubmit` | +45–53ms after the append — the turn began |
//! | `Stop` | end of the turn |
//! | `SessionEnd` | +100ms after `SIGTERM` — it announces its own killing |
//!
//! Compare what that replaces. herdr decides an agent is working by matching a
//! regex against a **spinner glyph** in the terminal title: rule
//! `osc_title_working`, priority 1100, region `osc_title`, regex
//! `^[\x{2800}-\x{28FF}\x{25D0}-\x{25D3}]`, with a hand-written comment saying
//! *"Braille covers <= 2.1.227; half-circles are the 2.1.228 busy spinner"*. The
//! manifest is versioned against Claude Code releases by hand, it was four days
//! old against a build three patch versions ahead of it, and on 2026-08-17 it
//! reported this project's own governor seat as working while it sat waiting for
//! a person. That is not a criticism of herdr's detection — matching spinners is
//! a reasonable way to observe a program that will not tell you anything. It is
//! the argument for not being that program.
//!
//! # What this backend is exact about, and herdr cannot be
//!
//! - **[`State::Gone`].** An agent that has exited leaves herdr a pane that
//!   looks like a shell somebody opened, so `place_herdr` can only raise `Gone`
//!   from the event stream and answers `Empty` from a listing. A supervisor
//!   holds the pid: an agent that has stopped is `Gone`, a seat nobody started
//!   is `Empty`, and there is no reading that confuses them.
//! - **[`State::Starting`].** herdr's launch window is three seconds of looking
//!   exactly like an idle agent, marked only by the *absence* of a field. Here
//!   it is the interval between the fork and `SessionStart`, and it ends when
//!   the agent says so.
//! - **[`Place::start`] returns when the agent exists**, with nothing to wait
//!   for: the fork either produced a pid or failed. herdr's adapter types a name
//!   at a shell that may not be listening, and pays a retry window, a retype and
//!   a thirty-second appearance wait for the privilege.
//!
//! # The limit Ed named, and the seam it leaves honest
//!
//! This is **Claude-Code-specific**, and a *remote* agent is a non-Claude-Code
//! agent even when it is technically Claude Code, because a local hook cannot
//! fire into a local socket across a machine boundary. So remoteness is a
//! property of how an agent can be *observed*. Local Claude Code reports itself,
//! which is this file; remote Claude Code is observed however the far side
//! manages; a kind that offers nothing falls back to reading a screen, which is
//! herdr. [`Place::open`] therefore refuses `Order::on` rather than pretending,
//! and [`Recipe`] is the one place a kind's hosting is written down.
//!
//! # What an agent with nobody in front of it cannot be asked
//!
//! Measured end to end on 2026-08-17, a real spawn onto a real task in a
//! sandbox store: the agent came up, read its brief, took the work order — and
//! then reported itself **blocked**, because the one thing the task asked of it
//! was a `Write`, and a `Write` needs permission. Headless, there is nobody to
//! ask, so the tool call is *denied* rather than queued, and no
//! `PermissionRequest` hook fires: that hook runs before a permission *prompt*,
//! and in print mode there is no prompt to run before.
//!
//! Two things follow, and the second is a decision rather than a defect.
//!
//! First, robustness-051 — *an agent with a question raises nothing* — is only
//! half answered by this backend. The hook is the right channel and
//! [`said_by`] is wired for it, but a headless agent never gets far enough to
//! ask; it is an agent in a **pane** whose question that hook would carry.
//!
//! Second, no permission mode is set here on purpose. `--permission-mode
//! acceptEdits` or `bypassPermissions` would make a headless agent able to do
//! the work, and what it may do to a machine nobody is watching is a decision
//! for a person rather than a default a backend quietly picks. Until it is
//! made, `--headless` gives an agent that can read, think and answer, and not
//! one that can write.
//!
//! # Where the state lives, and why an id is never handed out twice
//!
//! One directory per seat under the store's state directory — beside `bindings`
//! and `claims`, so a sandbox that sets `WSP_STATE` gets its own seats along with
//! its own everything else, which is what makes this measurable without standing
//! on the live store.
//!
//! Two writers, so two files. `seat.json` is what wsp knows — the order, the
//! agent, the pids — and is written by the process that placed the work.
//! `said.json` is what the *agent* knows, written by [`report`] from inside a
//! hook, in another process, at a moment nothing here chose. A single file would
//! be a lost update every time a turn began.
//!
//! Ids are `sup-1`, `sup-2`, allocated by `O_EXCL` on the directory and never
//! reused, and the counter that guarantees it survives the seat being ended. The
//! rule is not tidiness, and it is the one place in this tree where the old
//! belief about herdr was right: herdr does hand workspace ids out again, and
//! the README's oldest complaint is that a claim naming a closed one is *waiting
//! to attach itself to whatever takes the id next*. Measured 2026-08-19
//! (`robustness-084`) — herdr's counter is process-local, so a restart reserves
//! only one above the highest workspace that survived and every id above that
//! mark is reissued. This counter is on disk instead, in the same directory as
//! the claims that could point at it, so an id can only come round again once
//! the state that could name it is gone too.
//!
//! # What a reading costs
//!
//! One `ps` per call, for every pid at once. A supervisor that outlived its
//! seats would know an exit the moment it happened; this one is a CLI that forks
//! and exits, so the agent is reparented and its death is a thing to look for
//! rather than a thing to be told. `wsp spawn`'s readiness loop polls one seat
//! every 150ms and pays one fork for each, which is the price of not being a
//! daemon and is named here so it is chosen rather than inherited.

// [`Place::census`] and [`Place::watch`] have no caller outside the tests yet —
// the daemon and `sync` are herdr's for now — for the same reason `place_herdr`
// gives: a trait method a backend has never had to answer is a guess.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::place::{self, Agent, Census, Event, Order, Place, Refusal, Result, Seat, Seated, State};
use crate::store::{write_atomic, Store};
use crate::util::{self, Clock};

/// A supervisor as a place to put work.
///
/// The two durations are `stop`'s, and they are fields for the reason
/// `place_herdr`'s four are: a test that sits through a real two seconds to
/// check what happens after two seconds is a test nobody runs. Everything else
/// here answers out of the filesystem and waits for nothing.
pub struct Supervisor<'a> {
    /// Where seats live. One directory each, under this.
    pub root: PathBuf,
    /// How long a `SIGTERM`ed agent gets to go quietly before it is killed.
    pub linger: Duration,
    /// How often to look — while waiting for a stopped agent to die, and
    /// between passes of [`Place::watch`].
    pub poll: Duration,
    /// What time it is, and how to wait for the next look.
    pub clock: &'a dyn Clock,
}

impl Supervisor<'static> {
    /// The supervisor for this store's state directory, which is what the CLI
    /// uses.
    pub fn new() -> Supervisor<'static> {
        Supervisor::at(Store::open().state.join(SEATS))
    }

    /// One rooted anywhere — a test's temporary directory, or a second store.
    ///
    /// The root is a parameter rather than a constant because every test below
    /// depends on it: a backend that could only be exercised against the one
    /// directory the live agents are running in is one nobody would exercise.
    pub fn at(root: PathBuf) -> Supervisor<'static> {
        Supervisor {
            root,
            // Measured: a `SIGTERM`ed Claude Code was gone, and had fired its
            // own `SessionEnd`, inside 100ms. Two seconds is that with room,
            // and the kill after it is what stops a seat wsp has let go of from
            // leaving an agent running against the same tree.
            linger: Duration::from_millis(2_000),
            poll: Duration::from_millis(150),
            clock: &util::Wall,
        }
    }
}

/// The directory under the store's state that holds the seats.
const SEATS: &str = "seats";

/// What wsp knows about a seat: the order it was opened for, and the agent
/// started in it.
const SEAT_FILE: &str = "seat.json";

/// What the agent knows about itself, written by [`report`] from inside a hook.
const SAID_FILE: &str = "said.json";

/// The work orders, in the order they were given. The agent's stdin is a
/// `tail -f` on this.
const PROMPTS: &str = "prompts.jsonl";

/// The agent's own streams. Kept because a headless agent has nowhere else to
/// have said why it would not start.
const OUT_FILE: &str = "out.jsonl";
const ERR_FILE: &str = "err.log";

/// The next seat number, so that ending a seat does not free its name.
const NEXT_FILE: &str = "next";

/// What a seat is called. `sup-7` is the example `place.rs` reaches for when it
/// wants an id that is plainly not herdr's, and this is where that comes from.
const SEAT_PREFIX: &str = "sup-";

/// How a kind of agent is hosted with no terminal.
///
/// The two halves are one fact and must not drift, which is why they are one
/// struct: a kind started with `--input-format stream-json` **must** be told in
/// JSON lines, and a kind started as itself is told in plain text. Splitting
/// them into two functions is how the flags and the dialect end up disagreeing.
///
/// This is the second axis showing through, and the seam is honest about it.
/// `agent_commands::Kind` owns what an agent of a kind is started *with* — the
/// trim, the minted name — and that is the same wherever it runs. This owns what
/// it takes to run one **without a terminal at all**, which is a fact about the
/// pair. A kind nobody has measured gets [`Recipe::plain`], which is the honest
/// answer rather than a stub: run the program, write lines at its stdin, and do
/// not pretend to know whether it wanted a screen.
struct Recipe {
    /// Prepended to the agent's own arguments, never appended.
    ///
    /// `agent_commands::Claude::args` ends in `--disallowedTools Agent Workflow`,
    /// a space-separated list, and the one argv measured safe against Claude Code
    /// 2.1.233 has `-n <handle>` at the end of it. Going first leaves that exact
    /// argv untouched rather than re-opening a question somebody already answered.
    flags: &'static [&'static str],
    /// Whether a sentence is a JSON line rather than a line of text.
    stream_json: bool,
}

impl Recipe {
    /// Run it and type at it. Every kind but the one below.
    const fn plain() -> Recipe {
        Recipe { flags: &[], stream_json: false }
    }

    /// Claude Code, headless: print mode, streaming in and out.
    ///
    /// `--verbose` is not decoration — `--output-format stream-json` refuses
    /// without it. The stream on stdout is kept in the seat's `out.jsonl`, and
    /// is not what wsp reads state from: the hooks are.
    fn of(kind: &str) -> Recipe {
        match kind.trim() {
            "claude" => Recipe {
                flags: &[
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                ],
                stream_json: true,
            },
            _ => Recipe::plain(),
        }
    }

    /// One sentence, as the line to append to the seat's prompt file.
    fn sentence(&self, text: &str) -> String {
        match self.stream_json {
            true => json!({ "type": "user", "message": { "role": "user", "content": text } })
                .to_string(),
            // A newline is the whole of a submit for a program that reads lines,
            // and is the closest thing to `place.rs`'s "a backend with one
            // should use its own submit" for a kind with no submit of its own.
            false => text.replace('\n', " "),
        }
    }
}

/// Every hook Claude Code fires that says something about what an agent is
/// doing, and what it says.
///
/// The names were checked against the binary rather than remembered: all six
/// appear in Claude Code 2.1.233, and `SessionEnd` and `StopFailure` are both in
/// its own list of recognised hook events.
///
/// Two of them do not have a state of their own and that is recorded rather than
/// smoothed over. `PermissionRequest` and `Elicitation` mean **a person is
/// needed**, which [`State`] has no word for — robustness-051 is the task that
/// wants one, and this is where that signal would land. Until there is a seventh
/// state they are `Working`, which is the honest approximation: `Working` is
/// documented as "nothing to do about it and nothing to ask it", and
/// `will_take_a_prompt` says no to it. The hook's own name is written to
/// `said.json` beside the state, so the fact is kept rather than dropped and
/// nothing has to be re-plumbed to read it.
fn said_by(hook: &str) -> Option<State> {
    Some(match hook.trim() {
        // The launch window closes here, and this is the whole of the readiness
        // question herdr answers by looking for a missing field.
        "SessionStart" => State::Idle,
        "UserPromptSubmit" => State::Working,
        // Fires when Claude stops, which includes clear, resume and compact.
        "Stop" => State::Idle,
        // A turn that ended in an API error rather than an answer. The agent is
        // still there and will take another sentence.
        "StopFailure" => State::Idle,
        "PermissionRequest" | "Elicitation" => State::Working,
        "SessionEnd" => State::Gone,
        _ => return None,
    })
}

/// `wsp report <hook>` — an agent saying what it has just done, from inside its
/// own hook.
///
/// The supervisor's eyes, and the one command in wsp that exists to be called by
/// something that is not a person. Everything about it is shaped by being a
/// hook: it reads the payload on stdin, writes one small file, prints nothing
/// and **always exits 0**. A hook that fails a session, or delays one, is a hook
/// that gets deleted within a week.
///
/// It answers only for the seat named in this process's environment, which is
/// the seat's own [`place::SEAT_ENV`] — so an agent in a herdr pane, a person's
/// shell and a cron job all fall through it in silence, and nothing has to ask
/// which backend is running.
///
/// **A hook that arrives after the seat is gone does not bring it back.** The
/// record is written into an existing directory or not at all, because a
/// `SessionEnd` racing a [`Place::stop`] would otherwise recreate a seat that
/// nothing would ever clear.
pub fn report(args: &crate::Args) -> i32 {
    let hook = args.rest.first().cloned().unwrap_or_default();
    let Some(seat) = place::seat_from_env() else { return 0 };
    let Some(state) = said_by(&hook) else { return 0 };
    let payload: Value = match args.has("payload") {
        // A test's way in, so that the mapping above can be argued about
        // without a hook and a pipe.
        true => serde_json::from_str(&args.get("payload").unwrap_or_default()).unwrap_or(json!({})),
        false => {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
            serde_json::from_str(&buf).unwrap_or(json!({}))
        }
    };
    Supervisor::new().heard(&seat, &hook, state, &payload);
    0
}

/// The seat's environment, as the child will get it.
///
/// **An empty value is a removal here, and that is the port's own rule being
/// kept rather than a liberty.** `place::shed_env` empties rather than unsets
/// because a seat's environment on herdr's wire is an override-only map and
/// there is no way to spell "unset" on it — the emptying is herdr's compromise,
/// not the port's intention, and `place::seat_from_env` already reads an empty
/// value as an absence. A supervisor mints the seat and *then* forks, so it can
/// do the thing the wire could not.
///
/// The caller's own identity is shed whether or not the order mentioned it, for
/// the reason [`place::CHILD_MARKER`] carries: a session that inherits it writes
/// no transcript at all, and the only evidence is one truncated line in a pane
/// this backend does not have.
fn child_env(cmd: &mut Command, seat: &Seat, env: &BTreeMap<String, String>) {
    for (k, v) in env {
        match v.is_empty() {
            true => cmd.env_remove(k),
            false => cmd.env(k, v),
        };
    }
    for k in place::shed_keys() {
        cmd.env_remove(k);
    }
    // Last, and not from the order: the seat's own name is the one thing wsp
    // could not have put in `Order::env`, because it did not exist yet. This is
    // the direction `place.rs` reversed — the backend delivers the seat to
    // whatever runs in it, and says under what name.
    cmd.env(place::SEAT_ENV, seat.as_str());
}

/// Which of these pids are running, in one ask.
///
/// One `ps` rather than one `kill -0` each, because [`Place::census`] would
/// otherwise fork once per seat. An empty list forks nothing.
///
/// **A zombie is not alive**, and that arm is not hypothetical. `wsp spawn`
/// starts the agent and then polls its readiness from the same process, so an
/// agent that dies in its first second is a child nobody has reaped and sits in
/// the process table looking exactly like a running one. Reporting it as alive
/// is the same class of defect as robustness-041 with the sign flipped: an agent
/// declared healthy while its corpse cools.
pub(crate) fn alive(pids: &[u32]) -> BTreeSet<u32> {
    if pids.is_empty() {
        return BTreeSet::new();
    }
    let list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let out = match Command::new("ps").args(["-o", "pid=,state=", "-p", &list]).output() {
        Ok(o) => o,
        // Not an empty set: `ps` failing to run is not evidence that every agent
        // on this machine has stopped, and a caller that reaped on the strength
        // of it would end every claim at once. The rule is `sync.rs:41`'s and is
        // older than this file.
        Err(_) => return pids.iter().copied().collect(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid: u32 = it.next()?.parse().ok()?;
            match it.next().unwrap_or("").starts_with('Z') {
                true => None,
                false => Some(pid),
            }
        })
        .collect()
}

/// Signal a whole process group, by the id of the group's leader.
///
/// The group rather than the pid, because an agent's children are its work: a
/// `Bash` tool call is a shell under the agent, and a `stop` that left it running
/// would be the sort of half-ending that leaves a build writing into a tree wsp
/// has told somebody else is free.
fn signal_group(group: u32, sig: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(format!("-{group}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A file read as JSON, or an empty object — a seat whose record is missing is
/// the caller's question rather than this function's.
fn read_json(path: &PathBuf) -> Value {
    fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

impl Supervisor<'_> {
    /// A seat's directory, refusing anything that is not a name this backend
    /// could have issued.
    ///
    /// A [`Seat`] is a string wsp carries and does not read, and it arrives here
    /// out of a claim written some other day — so the one thing this file does
    /// read is whether it is a *file name*. Without it a seat called `../..` is
    /// a `stop` that removes somebody's home directory, which is not a
    /// hypothetical worth leaving to good manners.
    fn dir_of(&self, seat: &Seat) -> Result<PathBuf> {
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

    /// The seat's own record, or [`Refusal::NoSeat`] where there is no seat.
    fn record(&self, seat: &Seat) -> Result<Value> {
        let dir = self.dir_of(seat)?;
        match dir.is_dir() {
            true => Ok(read_json(&dir.join(SEAT_FILE))),
            false => Err(Refusal::NoSeat(seat.clone())),
        }
    }

    /// A seat id nothing has ever been called, and a directory to prove it.
    ///
    /// `create_dir` is the `O_EXCL` here: two spawns racing cannot be handed the
    /// same name whatever the counter says. The counter is what stops the *next*
    /// name being one that has been used and released — see the module docs — and
    /// the scan behind it is belt and braces for a counter file that was lost.
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

    /// The counter, or one past the highest name still on disk, whichever is
    /// larger. Parsing an id, which nothing else in wsp may do and this file
    /// must: these are the ids it issued.
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

    /// Every seat directory, by name.
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

    /// What the agent last said about itself, and the state it implies.
    fn said(&self, seat: &Seat) -> Value {
        match self.dir_of(seat) {
            Ok(dir) => read_json(&dir.join(SAID_FILE)),
            Err(_) => json!({}),
        }
    }

    /// One hook, recorded against a seat. [`report`] is the command; this is the
    /// write, so that a test can make an agent say something without a hook, a
    /// pipe and a process.
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
                // The hook's own name, kept beside the state it was read as.
                // `PermissionRequest` and `Elicitation` both arrive as
                // `working`, and this is the difference a seventh state would
                // be read out of.
                "hook": hook,
                "at": util::now_iso(),
                // Claude Code's session id and the transcript it is writing, both
                // of which arrive in the payload and neither of which wsp could
                // otherwise know. A later hook that does not carry them — and
                // they do all carry them — keeps what the last one said.
                "session_id": keep("session_id"),
                "transcript_path": keep("transcript_path"),
            })
            .to_string(),
        );
    }

    /// The reading, from what is on disk and what is in the process table.
    ///
    /// | on disk | the pid | what it is |
    /// |---|---|---|
    /// | no directory | — | [`Refusal::NoSeat`] |
    /// | a seat, no agent started | — | [`State::Empty`] |
    /// | an agent | not running | [`State::Gone`] |
    /// | an agent | running, and has said nothing yet | [`State::Starting`] |
    /// | an agent | running | whatever it last said |
    ///
    /// The third row is the one herdr cannot do, and the fourth is the launch
    /// window closed by an announcement rather than by a timer.
    ///
    /// An agent that has said `SessionEnd` while its process is still up is
    /// [`State::Gone`] — the session is the agent, and what is left is a process
    /// on its way out.
    fn state_of(&self, seat: &Seat, rec: &Value, running: &BTreeSet<u32>) -> State {
        let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()) else { return State::Empty };
        if !running.contains(&(pid as u32)) {
            return State::Gone;
        }
        let said = self.said(seat);
        match said.get("state").and_then(|s| s.as_str()) {
            Some("idle") => State::Idle,
            Some("working") => State::Working,
            Some("gone") => State::Gone,
            // Started, and not a word from it yet. This is the whole of the
            // launch window, and it is a fact rather than a guess: the fork
            // happened, and `SessionStart` has not.
            _ => State::Starting,
        }
    }

    /// One census row out of a seat's two files.
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

    /// Every seat, read once — the shape [`Place::census`] and [`Place::watch`]
    /// share, so that a watcher's pass costs exactly one census.
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
}

impl Place for Supervisor<'_> {
    /// A directory, an id, and the order written into it. Nothing is started and
    /// nothing is running: this is the window `place.rs` splits `open` from
    /// `start` for, and the claim goes in it.
    ///
    /// `Order::show` is ignored, which is what "a backend with no screen honours
    /// it by ignoring it" looks like in code.
    fn open(&self, order: &Order) -> Result<Seat> {
        if order.on.is_some() {
            // Ed's limit, and the seam left honest rather than faked: a hook on
            // another machine cannot fire into a socket on this one, so a Claude
            // Code over there is not a Claude Code this backend can see. Saying
            // so is what `Refusal::Unsupported` is for.
            return Err(Refusal::Unsupported("run an agent on another machine"));
        }
        let seat = self.mint()?;
        let dir = self.dir_of(&seat)?;
        let mut env: BTreeMap<String, String> = order.env.clone();
        // The seat's own name, on disk, at the moment the seat exists — which is
        // the promise `Place::here` is downstream of and the reason a supervisor
        // can keep it where herdr cannot: it mints the seat and *then* forks.
        env.insert(place::SEAT_ENV.to_string(), seat.to_string());
        let rec = json!({
            "label": order.label,
            "cwd": order.cwd.as_deref().map(|c| util::expand(c).display().to_string()),
            "env": env,
            "opened_at": util::now_iso(),
        });
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string())
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        Ok(seat)
    }

    /// [`place::seat_from_env`], and nothing else.
    ///
    /// This backend is the one that variable was written for. It is read rather
    /// than asked, which is the port's rule and is free here: the supervisor put
    /// it in the child's environment itself.
    ///
    /// herdr's own name is deliberately not consulted as a fallback, for the
    /// reason `place_herdr` gives about the mirror image: two answers to *which
    /// seat is this* is a way to be in two seats. Which backend a process is
    /// standing in is decided by which name its seat arrived under, one level
    /// up, in `cmd_agent::my_pane`.
    fn here(&self) -> Option<Seat> {
        place::seat_from_env()
    }

    /// Fork the agent, and hand it a pipe that will still be open tomorrow.
    ///
    /// **Returns when the agent exists, with nothing to wait for**, which is the
    /// port's promise met by construction rather than by patience: either there
    /// is a pid or the fork failed. Whether it will take a prompt is a different
    /// moment and is [`Place::state`]'s — here it is the ~870ms until
    /// `SessionStart`, and it is announced rather than waited out.
    ///
    /// Two processes, one group. `tail` leads the group so that the agent can
    /// join it, and [`Place::stop`] ends the group — which is also how the shell
    /// an agent left running goes with it.
    fn start(&self, seat: &Seat, agent: &Agent) -> Result<()> {
        let rec = self.record(seat)?;
        let dir = self.dir_of(seat)?;
        if let Some(pid) = rec.get("pid").and_then(|p| p.as_u64()) {
            if alive(&[pid as u32]).contains(&(pid as u32)) {
                return Err(Refusal::Backend(format!("{seat} already has an agent running")));
            }
        }
        let recipe = Recipe::of(&agent.kind);
        let prompts = dir.join(PROMPTS);
        let io = |name: &str| -> Result<fs::File> {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name))
                .map_err(|e| Refusal::Backend(e.to_string()))
        };
        io(PROMPTS)?;
        let out = io(OUT_FILE)?;
        let err = io(ERR_FILE)?;

        // The durable writer on the agent's stdin. `-n +1` so that anything
        // already in the file is delivered rather than skipped, which matters
        // only for a seat being restarted and costs nothing when it is not.
        let mut feeder = Command::new("tail")
            .args(["-n", "+1", "-f"])
            .arg(&prompts)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| Refusal::Backend(format!("no way to feed the agent: {e}")))?;
        let group = feeder.id();
        let Some(feed) = feeder.stdout.take() else {
            signal_group(group, "KILL");
            return Err(Refusal::Backend("the feed had no pipe".into()));
        };

        let mut cmd = Command::new(&agent.kind);
        cmd.args(recipe.flags)
            .args(&agent.args)
            .stdin(Stdio::from(feed))
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .process_group(group as i32);
        let cwd = str_of(&rec, "cwd");
        if !cwd.is_empty() {
            cmd.current_dir(&cwd);
        }
        let env: BTreeMap<String, String> = rec
            .get("env")
            .and_then(|e| e.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        child_env(&mut cmd, seat, &env);

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // The feed is not left running against a seat with no agent in
                // it: a `tail -f` nobody is reading is a leak that would outlive
                // every failed spawn.
                signal_group(group, "KILL");
                return Err(Refusal::Backend(format!("{} did not start: {e}", agent.kind)));
            }
        };

        let mut rec = rec;
        rec["agent"] = json!({ "kind": agent.kind, "name": agent.name, "args": agent.args });
        rec["pid"] = json!(child.id());
        rec["group"] = json!(group);
        rec["started_at"] = json!(util::now_iso());
        write_atomic(&dir.join(SEAT_FILE), &rec.to_string())
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        Ok(())
    }

    /// One line on the end of the seat's prompt file, in the dialect its kind
    /// was started to read.
    ///
    /// Refused unless the agent will take it, which is the port's rule and is
    /// not a formality here: an append to a file nothing is reading succeeds
    /// perfectly and silently, so [`Place::state`] is the only thing standing
    /// between a work order and a seat that has been dead for an hour.
    fn tell(&self, seat: &Seat, text: &str) -> Result<()> {
        let state = self.state(seat)?;
        if !state.will_take_a_prompt() {
            return Err(Refusal::NotReady(state));
        }
        let dir = self.dir_of(seat)?;
        let kind = str_of(&self.record(seat)?.get("agent").cloned().unwrap_or(json!({})), "kind");
        let line = Recipe::of(&kind).sentence(text);
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(PROMPTS))
            .map_err(|e| Refusal::Backend(e.to_string()))?;
        // One `write_all` of one line, which is what makes this safe against a
        // second `tell` arriving at the same moment: `O_APPEND` is atomic for a
        // write this size, so two sentences interleave as two lines rather than
        // as one unparseable one.
        writeln!(f, "{line}").map_err(|e| Refusal::Backend(e.to_string()))
    }

    /// End the agent, and let the seat go.
    ///
    /// `SIGTERM` to the group, a short wait, then `SIGKILL` — and the directory
    /// removed either way. The wait is not politeness: a Claude Code fires its
    /// own `SessionEnd` on the way out, measured at 100ms, and an agent given no
    /// chance to take it would leave a transcript ending mid-sentence.
    ///
    /// [`Refusal::NoSeat`] where there was nothing there, which the caller reads
    /// as the first half already done — an agent whose backend died under it is
    /// the ordinary case for this verb.
    fn stop(&self, seat: &Seat) -> Result<()> {
        let rec = self.record(seat)?;
        let dir = self.dir_of(seat)?;
        let group = rec.get("group").and_then(|g| g.as_u64()).map(|g| g as u32);
        let pid = rec.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32);
        if let Some(group) = group {
            signal_group(group, "TERM");
            let deadline = self.clock.now() + self.linger;
            while let Some(pid) = pid {
                if !alive(&[pid]).contains(&pid) {
                    break;
                }
                if self.clock.now() >= deadline {
                    signal_group(group, "KILL");
                    break;
                }
                self.clock.rest(self.poll);
            }
        }
        // The record goes with the agent. What survives is Claude Code's own
        // transcript, which is where a headless agent's durable output was
        // always going to be — the seat's directory holds a copy of the stream
        // and the orders it was given, and neither is the thing to keep a
        // directory per dead agent for.
        fs::remove_dir_all(&dir).map_err(|e| Refusal::Backend(e.to_string()))?;
        Ok(())
    }

    fn state(&self, seat: &Seat) -> Result<State> {
        let rec = self.record(seat)?;
        let pids: Vec<u32> =
            rec.get("pid").and_then(|p| p.as_u64()).map(|p| vec![p as u32]).unwrap_or_default();
        Ok(self.state_of(seat, &rec, &alive(&pids)))
    }

    /// Every seat this supervisor has, and what is in it.
    ///
    /// One pass over the directory and one `ps`, whatever the number of seats.
    /// An error is not an empty list — this returns rows or the error that
    /// stopped it, and a `ps` that would not run leaves every agent believed
    /// alive rather than reaped.
    fn census(&self) -> Result<Census> {
        // One machine, and it is this one. A supervisor forks locally; there is
        // no fan-out to be partly silent about, so the census is one answer and
        // an empty one is a fact rather than a silence.
        Ok(Census::heard("", self.survey()))
    }

    /// Poll, and say what changed.
    ///
    /// A supervisor could be told — a resident one waits on `SIGCHLD` and the
    /// hook could push down a socket — and this one is a CLI that forks and
    /// exits, so it polls, which is the fallback `place.rs` names: *a backend
    /// that cannot push polls in here*. What it polls is cheap and exact: a
    /// directory of small files and one `ps`.
    ///
    /// **The first pass is the baseline and raises nothing.** A watcher that
    /// announced every seat that already existed would report a world being
    /// created every time anything started listening, and every caller would
    /// have to learn to ignore the first second of the stream.
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
                            // Opened and started between two passes, which is
                            // what `wsp spawn` does every time: both happened,
                            // so both are said.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Dial;

    /// A supervisor on a directory of its own, and the seats it makes cleaned up
    /// after it — including any process still standing in one, which is the one
    /// kind of test rubbish that costs somebody else their machine.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let root = std::env::temp_dir()
                .join(format!("wsp-sup-{name}-{}-{}", std::process::id(), util::epoch_nanos()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Scratch { root }
        }
        fn place(&self) -> Supervisor<'static> {
            Supervisor { poll: Duration::from_millis(10), ..Supervisor::at(self.root.clone()) }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let sup = Supervisor::at(self.root.clone());
            for id in sup.ids() {
                let _ = sup.stop(&Seat::new(id));
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// A stand-in for an agent: a real process, started through the port, that
    /// stays up until it is stopped. `sleep` is not a Claude Code and does not
    /// have to be — what every test below is about is the *supervision*, and the
    /// one thing a live Claude Code adds is the hooks, which arrive as
    /// [`Supervisor::heard`] either way.
    fn a_process() -> Agent {
        Agent { kind: "sleep".into(), name: "t-1".into(), args: vec!["60".into()] }
    }

    /// Wait for something a real process does in its own time.
    ///
    /// A hang-guard rather than a measurement, and generous on purpose: it is
    /// only ever paid in full by a test that is already failing, which is
    /// robustness-054's rule. Nothing here asserts how long anything took —
    /// that is what [`Dial`] is for, and the machine is not on trial.
    fn until(what: impl Fn() -> bool) -> bool {
        for _ in 0..500 {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The window `place.rs` splits two verbs to leave open: a seat exists, has
    /// a name, and has nothing running in it, so the claim can land before the
    /// agent reads its brief.
    #[test]
    fn a_seat_exists_before_the_agent_does_and_knows_its_own_name() {
        let scratch = Scratch::new("open");
        let place = scratch.place();
        let seat = place
            .open(&Order {
                label: "robustness/061".into(),
                env: BTreeMap::from([("WSP_TASK".into(), "robustness-061".into())]),
                ..Order::default()
            })
            .expect("a seat");

        assert_eq!(place.state(&seat).unwrap(), State::Empty, "nothing has been started here");
        let rec = place.record(&seat).unwrap();
        assert_eq!(str_of(&rec, "label"), "robustness/061");
        // The whole of `here` for the agent about to be started, arranged before
        // it exists — which is the direction the port reversed.
        assert_eq!(
            rec["env"][place::SEAT_ENV].as_str(),
            Some(seat.as_str()),
            "the seat does not carry its own name to its occupant"
        );
        assert_eq!(rec["env"]["WSP_TASK"].as_str(), Some("robustness-061"));
    }

    /// A name is not handed out twice, however many seats have come and gone.
    ///
    /// The rule is not tidiness. herdr reissues workspace ids — measured, not
    /// assumed (`robustness-084`) — and the oldest complaint in this repository
    /// is that a claim naming a closed one is waiting to attach itself to
    /// whatever takes the id next. A supervisor is free of that only if it
    /// chooses to be, and this is the choice.
    #[test]
    fn a_seat_id_is_never_handed_out_again_after_the_seat_is_ended() {
        let scratch = Scratch::new("ids");
        let place = scratch.place();
        let first = place.open(&Order::default()).unwrap();
        let second = place.open(&Order::default()).unwrap();
        assert_ne!(first, second);
        place.stop(&first).expect("the seat was there");
        place.stop(&second).expect("the seat was there");
        assert!(place.census().unwrap().seats().next().is_none(), "the seats outlived their stop");

        let third = place.open(&Order::default()).unwrap();
        assert_ne!(third, first, "an id came round again with the claims still naming it");
        assert_ne!(third, second);

        // And with the counter lost — the belt to that braces. What is left on
        // disk is enough on its own.
        let fourth = place.open(&Order::default()).unwrap();
        fs::remove_file(scratch.root.join(NEXT_FILE)).unwrap();
        let fifth = place.open(&Order::default()).unwrap();
        assert_ne!(fifth, fourth);
        assert_ne!(fifth, third);
    }

    /// A seat is a name this backend issued, and a string out of a two-day-old
    /// claim is not a path.
    ///
    /// `stop` removes a directory, so the guard is the difference between a verb
    /// and an accident. Cheap to write down, and the sort of thing that is only
    /// ever written down before it is needed.
    #[test]
    fn a_seat_this_backend_never_issued_cannot_name_a_file_outside_its_own_root() {
        let scratch = Scratch::new("traversal");
        let place = scratch.place();
        for id in ["../..", "/etc", ".hidden", "w1:p1/../..", ""] {
            let seat = Seat::new(id);
            assert_eq!(place.state(&seat), Err(Refusal::NoSeat(seat.clone())), "{id}");
            assert_eq!(place.stop(&seat), Err(Refusal::NoSeat(seat)), "{id}");
        }
        // A herdr id is refused for being unknown rather than for its shape:
        // nothing here parses one, and `w1:p1` is a perfectly good file name.
        let herdrs = Seat::new("w1:p1");
        assert_eq!(place.state(&herdrs), Err(Refusal::NoSeat(herdrs)));
    }

    /// The launch window, closed by an announcement rather than by a timer — and
    /// the three readings herdr cannot tell apart, told apart.
    ///
    /// A seat with nothing in it, an agent still coming up, an agent that will
    /// take a prompt: on herdr the first and third are the same `agent_status`
    /// and the second is marked only by a *missing field*. Here each is a
    /// different fact, and only the last one is told anything.
    #[test]
    fn an_agent_is_starting_until_it_says_it_has_started() {
        let scratch = Scratch::new("window");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        assert_eq!(place.state(&seat).unwrap(), State::Empty);

        place.start(&seat, &a_process()).expect("a process");
        assert_eq!(
            place.state(&seat).unwrap(),
            State::Starting,
            "an agent that has not said anything yet is not idle"
        );
        assert!(!State::Starting.will_take_a_prompt());
        assert_eq!(place.tell(&seat, "go"), Err(Refusal::NotReady(State::Starting)));

        place.heard(&seat, "SessionStart", State::Idle, &json!({ "session_id": "abc" }));
        assert_eq!(place.state(&seat).unwrap(), State::Idle, "it said it was up");
        place.tell(&seat, "go").expect("now it takes one");

        place.heard(&seat, "UserPromptSubmit", State::Working, &json!({}));
        assert_eq!(place.state(&seat).unwrap(), State::Working);
        assert_eq!(place.tell(&seat, "again"), Err(Refusal::NotReady(State::Working)));
        place.heard(&seat, "Stop", State::Idle, &json!({}));
        assert_eq!(place.state(&seat).unwrap(), State::Idle, "the turn ended");
    }

    /// **The reading herdr has to raise from an event stream, because a listing
    /// cannot carry it.** An agent that has stopped is not a seat nobody used.
    ///
    /// And the arm that is easy to leave out: `wsp spawn` starts the agent and
    /// polls it from the same process, so an agent that dies immediately is a
    /// child nobody has reaped and looks, in the process table, exactly like a
    /// running one. Reporting that as alive is robustness-041 with the sign
    /// flipped — a corpse declared healthy — and it is the reason this asks `ps`
    /// for a state rather than for a row.
    #[test]
    fn an_agent_that_has_stopped_is_gone_even_while_its_corpse_is_in_the_process_table() {
        let scratch = Scratch::new("gone");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        place
            .start(&seat, &Agent { kind: "true".into(), name: "t-1".into(), args: Vec::new() })
            .expect("a process");

        assert!(
            until(|| place.state(&seat).unwrap() == State::Gone),
            "a seat whose agent has exited never stopped reading as running"
        );
        assert_ne!(State::Gone, State::Empty, "and it is not a seat nobody ever used");
        assert!(!State::Gone.is_running());
        // The seat is still there. Ending the agent and ending the seat are one
        // verb, and nothing has called it.
        assert!(place.census().unwrap().seats().any(|s| s.seat == seat));
    }

    /// What a seat's occupant must not inherit is *removed* here, not emptied —
    /// and the difference is measured through a real fork rather than argued.
    ///
    /// `place::shed_env` empties because herdr's wire has no way to spell an
    /// unset. That is the multiplexer's compromise and this backend is not
    /// bound by it: it mints the seat and then forks, so the caller's session
    /// identity simply does not exist in the child. The variable that matters
    /// most is the one this proves absent — a session that inherits
    /// `CLAUDE_CODE_CHILD_SESSION` writes no transcript at all.
    #[test]
    fn the_callers_session_identity_is_removed_from_the_seat_rather_than_emptied() {
        let _env = util::env_lock();
        let scratch = Scratch::new("shed");
        let place = scratch.place();
        std::env::set_var(place::CHILD_MARKER, "the-callers-session");
        std::env::set_var("CLAUDE_CODE_MESSAGING_TOKEN", "the-callers-credential");

        let seat = place
            .open(&Order {
                // Exactly what `cmd_spawn::order` builds: the shed list as
                // emptied values, which this backend has to read as removals.
                env: place::shed_env(),
                ..Order::default()
            })
            .unwrap();
        let dir = place.dir_of(&seat).unwrap();
        let out = dir.join("env.txt");
        place
            .start(
                &seat,
                &Agent {
                    kind: "sh".into(),
                    name: "t-1".into(),
                    args: vec!["-c".into(), format!("env > {}", out.display())],
                },
            )
            .expect("a process");
        // Wait on the child exiting, not on the file appearing: the shell
        // creates `env.txt` when it opens the redirection, ~20ms before `env`
        // writes a byte into it, so existence says the child *started*. Run
        // alone this failed 48 times in 50 on an empty env dump; under a loaded
        // suite the first poll got descheduled and handed the child its 20ms,
        // which is why concurrency was hiding the bug rather than causing it.
        assert!(until(|| place.state(&seat).unwrap() == State::Gone), "the child never ran");
        std::env::remove_var(place::CHILD_MARKER);
        std::env::remove_var("CLAUDE_CODE_MESSAGING_TOKEN");

        let env = fs::read_to_string(&out).unwrap();
        let names: Vec<&str> = env.lines().filter_map(|l| l.split('=').next()).collect();
        assert!(
            !names.contains(&place::CHILD_MARKER),
            "the marker reached the seat, and its transcript would never have been written"
        );
        assert!(!names.contains(&"CLAUDE_CODE_MESSAGING_TOKEN"), "the caller's credential reached the seat");
        assert!(
            env.lines().any(|l| l == format!("{}={seat}", place::SEAT_ENV)),
            "the occupant cannot say which seat it is in: {env}"
        );
    }

    /// A sentence reaches the agent in the dialect its kind was started to read,
    /// and only when the agent will listen.
    ///
    /// The two halves are one fact: the flags that make a Claude Code headless
    /// are the flags that make its stdin a stream of JSON objects, so a backend
    /// that got the dialect wrong would be typing English at a parser. That is
    /// why [`Recipe`] is one struct and not two constants.
    #[test]
    fn an_agent_is_told_in_the_dialect_it_was_started_to_read() {
        let scratch = Scratch::new("tell");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        place
            .start(&seat, &Agent { kind: "claude".into(), name: "t-1".into(), args: Vec::new() })
            .expect("a process");
        place.heard(&seat, "SessionStart", State::Idle, &json!({}));
        place.tell(&seat, "pick up robustness-061").expect("it was idle");

        let said = fs::read_to_string(place.dir_of(&seat).unwrap().join(PROMPTS)).unwrap();
        let line: Value = serde_json::from_str(said.trim()).expect("one JSON line: {said}");
        assert_eq!(line["type"], "user", "Claude Code reads work orders as user messages");
        assert_eq!(line["message"]["content"], "pick up robustness-061");

        // A kind nobody has measured is told in the only dialect there is, which
        // is the same thing typing at it would have done.
        assert_eq!(Recipe::of("codex").sentence("go"), "go");
        assert!(Recipe::of("codex").flags.is_empty(), "flags invented for a kind nobody measured");
    }

    /// A census carries what the backend started and what the agent said it was,
    /// which is the pair `place_herdr` needs two calls and a merge to assemble.
    #[test]
    fn a_census_carries_the_agent_wsp_started_and_the_session_it_reported() {
        let scratch = Scratch::new("census");
        let place = scratch.place();
        let empty = place.open(&Order { label: "a seat".into(), ..Order::default() }).unwrap();
        let busy = place.open(&Order { label: "robustness/061".into(), ..Order::default() }).unwrap();
        place.start(&busy, &a_process()).expect("a process");
        place.heard(
            &busy,
            "UserPromptSubmit",
            State::Working,
            &json!({ "session_id": "7a188ba8", "transcript_path": "/tmp/t.jsonl" }),
        );

        let seats = place.census().unwrap();
        let of = |s: &Seat| seats.seats().find(|r| &r.seat == s).cloned().unwrap();
        assert_eq!(of(&empty).state, State::Empty, "a seat somebody could sit in is still a seat");
        assert_eq!(of(&empty).label, "a seat");
        assert_eq!(of(&busy).state, State::Working);
        assert_eq!(of(&busy).agent.name, "t-1", "what it was started as");
        assert_eq!(of(&busy).agent.kind, "sleep");
        // The session id herdr reports as `agent_session.value` and takes a
        // second call to find. Here the agent said it, in its own hook.
        assert_eq!(of(&busy).session, "7a188ba8");
    }

    /// "Tell me when it stops" is the clause no reading carries, and a
    /// supervisor that has exited cannot be told — so it looks, and says what
    /// changed between two looks.
    ///
    /// Driven by [`Dial`], so the world's other events happen *at* a time rather
    /// than on a second thread racing the poll. Nothing here sleeps and nothing
    /// measures the machine.
    #[test]
    fn a_watcher_hears_a_seat_open_start_and_end_without_being_asked() {
        let scratch = Scratch::new("watch");
        let quiet = Supervisor::at(scratch.root.clone());
        // A seat that already existed when the watcher started is the baseline
        // and is not news. What follows it is.
        let seat = quiet.open(&Order::default()).unwrap();
        let s = seat.clone();

        let poll = Duration::from_millis(10);
        let dial = Dial::new()
            // An agent appears in it: a pid this test can be sure is running,
            // which is its own.
            .at(poll, || {
                let mut rec = quiet.record(&s).unwrap();
                rec["pid"] = json!(std::process::id());
                let dir = quiet.dir_of(&s).unwrap();
                write_atomic(&dir.join(SEAT_FILE), &rec.to_string()).unwrap();
            })
            .at(poll * 2, || quiet.heard(&s, "SessionStart", State::Idle, &json!({})))
            .at(poll * 3, || quiet.heard(&s, "UserPromptSubmit", State::Working, &json!({})))
            .at(poll * 4, || quiet.heard(&s, "SessionEnd", State::Gone, &json!({})));
        let place = Supervisor { poll, clock: &dial, ..Supervisor::at(scratch.root.clone()) };

        let mut heard: Vec<Event> = Vec::new();
        place
            .watch(&mut |e| {
                let last = matches!(e, Event::Stopped(_));
                heard.push(e);
                !last
            })
            .unwrap();
        assert_eq!(
            heard,
            vec![
                // The fork, then the agent announcing itself, then the turn,
                // then the end. Only the first of those is something a listing
                // could have told us.
                Event::Started(seat.clone()),
                Event::Moved(seat.clone(), State::Idle),
                Event::Moved(seat.clone(), State::Working),
                Event::Stopped(seat.clone()),
            ],
            "a seat that was already open was announced as news, or a change was missed"
        );
    }

    /// A hook that arrives after the seat has been ended does not bring it back.
    ///
    /// The race is ordinary rather than exotic: `stop` kills the agent, the
    /// agent fires `SessionEnd` on its way out — measured at 100ms — and that
    /// hook runs `wsp report` against a seat wsp has already let go of. A record
    /// written into a directory that is not there would be a seat nothing would
    /// ever clear, listed in every census, with an agent that has been dead
    /// since Tuesday.
    #[test]
    fn a_hook_that_arrives_after_the_seat_is_gone_does_not_bring_it_back() {
        let scratch = Scratch::new("late");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        place.stop(&seat).expect("the seat was there");

        place.heard(&seat, "SessionEnd", State::Gone, &json!({ "session_id": "abc" }));
        assert!(place.census().unwrap().seats().next().is_none(), "a hook recreated the seat");
        assert_eq!(place.state(&seat), Err(Refusal::NoSeat(seat.clone())));
        assert_eq!(place.stop(&seat), Err(Refusal::NoSeat(seat)), "already gone");
    }

    /// Every hook Claude Code fires, as the state wsp acts on — and the two that
    /// mean a person is needed, kept as themselves.
    ///
    /// This is the whole of what replaces a regex against a spinner glyph, so it
    /// is worth reading as a list: six announcements, each exact, none of them a
    /// property of what is on a screen.
    #[test]
    fn what_the_agent_announces_is_what_the_seat_is_doing() {
        assert_eq!(said_by("SessionStart"), Some(State::Idle), "the launch window closes here");
        assert_eq!(said_by("UserPromptSubmit"), Some(State::Working));
        assert_eq!(said_by("Stop"), Some(State::Idle));
        assert_eq!(said_by("StopFailure"), Some(State::Idle), "an API error is not a death");
        assert_eq!(said_by("SessionEnd"), Some(State::Gone));
        // A person is needed, and there is no state for that yet. `Working` is
        // the honest approximation because nothing may be sent to it;
        // robustness-051 is where the seventh state belongs, and the hook's name
        // is kept on disk so it can be read without re-plumbing anything.
        for asking in ["PermissionRequest", "Elicitation"] {
            assert_eq!(said_by(asking), Some(State::Working), "{asking}");
            assert!(!said_by(asking).unwrap().will_take_a_prompt(), "{asking}");
        }
        // A hook wsp has nothing to say about changes nothing, rather than
        // being read as a state it did not name.
        for quiet in ["PreToolUse", "PostToolUse", "SubagentStop", "PreCompact", ""] {
            assert_eq!(said_by(quiet), None, "{quiet}");
        }

        let scratch = Scratch::new("said");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        place.heard(&seat, "PermissionRequest", State::Working, &json!({ "session_id": "abc" }));
        let said = place.said(&seat);
        assert_eq!(str_of(&said, "hook"), "PermissionRequest", "the raised hand was lost");
        // And what the payload carries is kept when the next hook does not
        // repeat it, so a `Stop` does not forget which session it was.
        place.heard(&seat, "Stop", State::Idle, &json!({}));
        assert_eq!(str_of(&place.said(&seat), "session_id"), "abc");
    }

    /// A seat on another machine is refused rather than faked.
    ///
    /// Ed's limit, and the one place this backend says no on purpose: a hook
    /// fires into a socket on the machine it runs on, so a Claude Code over
    /// there cannot be observed from here. `Refusal::Unsupported` is the port's
    /// word for a backend that cannot do a thing at all, as distinct from one
    /// that failed at it.
    #[test]
    fn an_agent_on_another_machine_is_refused_rather_than_opened_here() {
        let scratch = Scratch::new("remote");
        let place = scratch.place();
        let refused = place.open(&Order { on: Some("mb2".into()), ..Order::default() });
        assert!(matches!(refused, Err(Refusal::Unsupported(_))), "{refused:?}");
        assert!(place.census().unwrap().seats().next().is_none(), "a refused order left a seat behind");
    }

    /// Ending a seat ends what was running in it, and ending it twice is not a
    /// failure.
    ///
    /// The second half is the interesting one: an agent whose backend died under
    /// it is the ordinary case for this verb, and `cmd_spawn::despawn` releases
    /// the claim on `NoSeat` alone — so "there was nothing there" and "the
    /// backend said no" must not be the same answer.
    #[test]
    fn a_seat_that_is_ended_takes_its_agent_with_it() {
        let scratch = Scratch::new("stop");
        let place = scratch.place();
        let mine = place.open(&Order::default()).unwrap();
        let theirs = place.open(&Order::default()).unwrap();
        place.start(&mine, &a_process()).expect("a process");
        place.start(&theirs, &a_process()).expect("a process");
        let pid = place.record(&mine).unwrap()["pid"].as_u64().unwrap() as u32;
        assert!(alive(&[pid]).contains(&pid));

        place.stop(&mine).expect("the seat was there");
        assert!(until(|| !alive(&[pid]).contains(&pid)), "the agent outlived its seat");
        assert_eq!(place.state(&mine), Err(Refusal::NoSeat(mine.clone())));
        assert_eq!(place.stop(&mine), Err(Refusal::NoSeat(mine)), "already gone");

        // Somebody else's seat is not swept up with it.
        assert_eq!(place.state(&theirs).unwrap(), State::Starting);
    }
}
