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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::place::{self, Agent, Census, Delivery, Event, Order, Place, Refusal, Result, Seat, Seated, State};
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

    /// Every seat's burn record that has one, seat id attached. This backend's
    /// seats only — see [`burn_under`] for why that is not the whole answer.
    pub fn burn(&self) -> Vec<(String, Value)> {
        burn_under(&self.root)
    }
}

/// The newest modification time among a seat directory and the files in it,
/// in epoch seconds — [`crate::place::Place::quiet_since`] for both backends
/// that keep a directory per seat. The directory counts too: a file replaced
/// by rename moves the directory's time and not always the new file's.
pub(crate) fn last_written(dir: &Path) -> Option<i64> {
    let secs = |p: &Path| {
        let m = std::fs::metadata(p).ok()?.modified().ok()?;
        Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
    };
    let files = std::fs::read_dir(dir).ok()?.flatten().filter_map(|e| secs(&e.path()));
    files.chain(secs(dir)).max()
}

/// Every seat's burn record under one root, seat id attached.
///
/// The reading half of [`tally_burn`] (`core-049`): the ranking is a question
/// about *where the tokens went*, and the answer lives one file per seat.
///
/// **The root is a parameter because two backends write these files and only
/// one of them grew a reader.** `tally_burn` is shared verbatim —
/// `place_compound` calls it at its own `heard` — so a compound seat tallies
/// exactly like a headless one, into a directory of its own
/// (`place_compound::SEATS`). This function living here as a method on
/// [`Supervisor`] therefore read `seats/`, and on a machine where every seat is
/// a compound one — which is the common case since `compound-112` — that
/// directory does not exist at all: `wsp burn` reported no burn for a fleet of
/// eight seats holding real records, and said so in the present tense
/// (`wsp-116`). Nothing about the *shape* was wrong; the reader was pointed at
/// one backend's store in a codebase whose seats are spread over two.
///
/// So the reader is a free function over a root, each backend reaches it
/// through its own `burn()`, and the report folds the two — the same
/// herdr-then-compound fold `sync.rs`, `cmd_watch` and `cmd_checkout` already
/// make, and for the same reason: a source nobody folds is a source nobody is
/// looking at.
///
/// **A seat that tallied zero turns is skipped**, which is not the same thing
/// as a seat with no [`BURN_FILE`] and used to be documented as if it were. The
/// two come apart: a hook carrying a transcript whose lines carry no `usage` —
/// a kind that keeps none, or a format this does not read — writes a record
/// with `turns: 0` and real totals of zero, and it is dropped here. That is
/// right, and deliberately so: a seat at $0.00 is not a rank, and the report is
/// a ranking. A seat with *no* file is the case the doc used to name, and it is
/// dropped by the same line for a better reason — there is nothing there to
/// read.
pub(crate) fn burn_under(root: &Path) -> Vec<(String, Value)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let rec = read_json(&path.join(BURN_FILE));
        if rec.get("turns").and_then(|v| v.as_u64()).unwrap_or(0) == 0 {
            continue;
        }
        let id = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push((id, rec));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The directory under the store's state that holds the seats.
const SEATS: &str = "seats";

/// What wsp knows about a seat: the order it was opened for, and the agent
/// started in it.
const SEAT_FILE: &str = "seat.json";

/// What the agent knows about itself, written by [`report`] from inside a hook.
const SAID_FILE: &str = "said.json";

/// What the session has cost, tallied from the transcript's `usage` fields one
/// hook at a time. See [`tally_burn`].
const BURN_FILE: &str = "burn.json";

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
pub(crate) fn said_by(hook: &str) -> Option<State> {
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

/// Every hook name [`said_by`] answers for. Kept as its own list rather than
/// derived from that match, because nothing can enumerate a match's arms —
/// and named here, beside [`hook_snippet_health`], for the reason
/// `compound-107` exists: a hook `said_by` learns and this forgets to check
/// for reads as installed on every machine until the day a seat goes quiet
/// and nobody can say why.
const HOOK_NAMES: [&str; 7] =
    ["SessionStart", "SessionEnd", "UserPromptSubmit", "Stop", "StopFailure", "PermissionRequest", "Elicitation"];

/// Where Claude Code reads its settings on this machine — `$CLAUDE_CONFIG_DIR`,
/// or `~/.claude`, the same authority [`place::shed`]'s own docs name for that
/// variable: a setting about where configuration lives, not an identity to
/// strip.
fn claude_settings_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| util::expand("~/.claude"))
        .join("settings.json")
}

/// `compound-107`, the half the account of it left out: a machine can look
/// configured — the file exists, `SessionStart` is in it, an agent's brief
/// arrives on launch exactly as expected — while six of the other seven hooks
/// are silently absent, and nothing before this said so. That is not a
/// hypothetical: it is `cpd-5`'s whole explanation. Only `SessionStart` was
/// ever in `~/.claude/settings.json`, so only `SessionStart` could ever fire,
/// and a session that starts once writes exactly one record however many
/// turns follow — indistinguishable, from `wsp wip`, from a healthy fleet
/// nobody has looked at recently, which is exactly why it went a week.
///
/// **A check that only fired on total absence would have said nothing on the
/// exact case that cost that week.** So this fires on any of the seven
/// missing, names which, and names the fix — `merge
/// claude-code/settings.snippet.json in` — rather than waiting for all seven
/// to be gone before it has anything to say.
///
/// The file simply not existing is a different fact and a smaller one: a
/// machine that has never merged the snippet in at all is not lying about
/// being configured, so that is a note. A file that exists and is short some
/// of the seven is the sneaky case, and that is a problem.
pub fn hook_snippet_health(problems: &mut Vec<String>, notes: &mut Vec<String>) {
    let path = claude_settings_path();
    let Ok(text) = fs::read_to_string(&path) else {
        notes.push(format!(
            "no Claude Code settings at {} — wsp-session.sh is not installed, so no hook ever tells a headless or compound seat's state; see claude-code/settings.snippet.json",
            util::contract(&path)
        ));
        return;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else {
        problems.push(format!("{}: not valid JSON — could not check which hooks are installed", util::contract(&path)));
        return;
    };
    let installed = |event: &str| -> bool {
        settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|matcher| matcher["hooks"].as_array())
            .flatten()
            .filter_map(|h| h["command"].as_str())
            .any(|c| c.contains("wsp-session.sh"))
    };
    let missing: Vec<&str> = HOOK_NAMES.iter().copied().filter(|e| !installed(e)).collect();
    if !missing.is_empty() {
        problems.push(format!(
            "{}: wsp-session.sh is missing from {} of {} hooks — {} — those turns never reach `wsp report`, and state reads stale or Unknown for them; merge the missing entries in from claude-code/settings.snippet.json (compound-107)",
            util::contract(&path),
            missing.len(),
            HOOK_NAMES.len(),
            missing.join(", ")
        ));
    }
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
/// **Two backends now write this hook's answer, and the hook does not know
/// which one it landed in.** A headless seat lives under `place_super`'s own
/// directory; a compound seat (`compound-064`) lives under
/// `place_compound`'s, one pty instead of none, same six hook names. Rather
/// than teach the hook which backend minted its seat — a second thing
/// `SEAT_ENV` would have to carry — this tries the directory that actually
/// holds the seat's name and is silent if neither does, exactly as it was
/// already silent for a seat named by no backend at all.
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
    let super_place = Supervisor::new();
    if super_place.dir_of(&seat).map(|d| d.is_dir()).unwrap_or(false) {
        super_place.heard(&seat, &hook, state, &payload);
        return 0;
    }
    let compound = crate::place_compound::Compound::new();
    if compound.dir_of(&seat).map(|d| d.is_dir()).unwrap_or(false) {
        compound.heard(&seat, &hook, state, &payload);
    }
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
pub(crate) fn signal_group(group: u32, sig: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(format!("-{group}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A file read as JSON, or an empty object — a seat whose record is missing is
/// the caller's question rather than this function's.
pub(crate) fn read_json(path: &PathBuf) -> Value {
    fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

pub(crate) fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// Add this hook's slice of the transcript to the seat's running total.
///
/// Every request an agent makes re-reads its whole context, so **where the
/// tokens are is the question `core-049` was filed to answer** — and Claude
/// Code already writes the answer: every assistant message in the transcript
/// carries a `usage` object with the input, output and cache counts of the
/// request that produced it. Nothing else in wsp sees a transcript — they are
/// session-private — so the hook is where this is read, one append at a time.
///
/// The tally is incremental and survives being wrong about nothing: a byte
/// offset into the file says how far the last look got, only new *whole* lines
/// are parsed and only whole lines advance the offset (a Stop fires every turn,
/// so re-reading a whole night's transcript each time would be quadratic in
/// exactly the sessions this exists to measure), a shorter file than remembered
/// means it was rotated or truncated and the count restarts rather than
/// double-counts, and a different `session_id` means the seat was cleared —
/// `/clear` ends an accounting as surely as it ends a context. A transcript
/// with no `usage` lines (a kind that keeps none, or a format wsp does not
/// read) tallies zero and costs one pass.
///
/// Two things it counts *once* that the first draft counted wrong, both of them
/// silent: the fragment at the end of a read, and the repeated `usage` object
/// Claude Code writes per content block. The argument for each is at the line
/// that does it.
///
/// Written for [`heard`], which every hook reaches; best-effort like everything
/// else on that path.
pub(crate) fn tally_burn(dir: &PathBuf, payload: &Value) {
    let path = str_of(payload, "transcript_path");
    if path.is_empty() {
        return;
    }
    let sid = str_of(payload, "session_id");
    let was = read_json(&dir.join(BURN_FILE));
    // Same session continues the running total; anything else starts one. An
    // empty stored id is a seat that has not been tallied yet, which is also a
    // start.
    let same = !sid.is_empty() && str_of(&was, "session_id") == sid;
    let mut offset = if same {
        was.get("offset").and_then(|v| v.as_u64()).unwrap_or(0)
    } else {
        0
    };
    let mut input = if same { u_at(&was, "input") } else { 0 };
    let mut output = if same { u_at(&was, "output") } else { 0 };
    let mut cache_read = if same { u_at(&was, "cache_read") } else { 0 };
    let mut cache_write = if same { u_at(&was, "cache_write") } else { 0 };
    let mut turns = if same { u_at(&was, "turns") } else { 0 };
    let mut model = if same { str_of(&was, "model") } else { String::new() };
    // Not `u_at`: a record from before the tally priced anything has totals and
    // no cost, and starting from zero would report a whole night as costing
    // whatever the next few turns did.
    let mut cost = if same { crate::cmd_burn::cost_of(&was) } else { 0 };
    // The last request counted, carried between hooks so the dedupe below
    // survives a slice boundary landing in the middle of one.
    let mut last = if same { str_of(&was, "last_request") } else { String::new() };

    let Ok(file) = fs::File::open(&path) else {
        return;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < offset {
        // Rotated, swept or truncated: whatever happened, counting from a hole
        // would count somebody twice.
        offset = 0;
        (input, output, cache_read, cache_write, turns, cost) = (0, 0, 0, 0, 0, 0);
        last.clear();
    }
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut reader = std::io::BufReader::new(file);
    if reader.seek(SeekFrom::Start(offset)).is_err() {
        return;
    }
    let mut fresh = Vec::new();
    if reader.read_to_end(&mut fresh).is_err() {
        return;
    }
    let text = match String::from_utf8(fresh) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Only whole lines are counted, and only whole lines are consumed. A hook
    // fires on the writer's clock, not on the writer's line endings, so a read
    // can land inside a record that is still being written — and an offset
    // advanced past the fragment would leave the next read starting inside a
    // record it can no longer parse, dropping it for good. Silent undercount in
    // an instrument whose whole job is counting.
    let whole = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
    for line in text[..whole].lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(message) = line.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        // One request, one bill. Claude Code writes a transcript line per
        // *content block* and repeats the request's whole `usage` object on
        // each of them, so an assistant turn that spoke and then called a tool
        // is three identical lines. Measured on this store on 2026-08-26: 49
        // usage lines over 30 requests, which a naive sum reports as 1.6x the
        // real bill. The blocks of one request are written together, so
        // remembering the last id counted is enough to tell a repeat from a
        // new request.
        let id = match str_of(&line, "requestId") {
            // `message.id` is the same key one level in, and the fallback for a
            // transcript that carries no request id of its own.
            s if s.is_empty() => str_of(message, "id"),
            s => s,
        };
        if !id.is_empty() && id == last {
            continue;
        }
        if !id.is_empty() {
            last = id;
        }
        let n = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let (i, o) = (n("input_tokens"), n("output_tokens"));
        let (cr, cw) = (n("cache_read_input_tokens"), n("cache_creation_input_tokens"));
        input += i;
        output += o;
        cache_read += cr;
        cache_write += cw;
        turns += 1;
        // Priced against the model *this request* ran on, not against the one
        // the session opened with: an agent that types `/model` mid-session is
        // billed at both tiers and neither total is a lie about the other. The
        // stored `model` is therefore what the seat is on now, and the cost is
        // not derivable from it — see [`crate::cmd_burn::cost`].
        let ran_on = str_of(message, "model");
        cost += crate::cmd_burn::cost(&ran_on, i, o, cr, cw);
        if !ran_on.is_empty() {
            model = ran_on;
        }
    }

    let _ = write_atomic(
        &dir.join(BURN_FILE),
        &json!({
            "session_id": sid,
            "transcript": path,
            "offset": offset + whole as u64,
            "model": model,
            "last_request": last,
            "cost": cost,
            "input": input,
            "output": output,
            "cache_read": cache_read,
            "cache_write": cache_write,
            "turns": turns,
            "updated": util::now_iso(),
        })
        .to_string(),
    );
}

/// A counter out of a burn record, absent meaning zero — the shape a first
/// tally reads back as.
fn u_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
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
         tally_burn(&dir, payload);
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
    fn tell(&self, seat: &Seat, text: &str) -> Result<Delivery> {
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
        writeln!(f, "{line}").map_err(|e| Refusal::Backend(e.to_string()))?;
        // [`Delivery::Unconfirmed`] and never anything stronger, on the same
        // grounds as the refusal above: an append to a file is not evidence
        // that anything read it. A backend that cannot watch says so.
        Ok(Delivery::Unconfirmed)
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

    fn quiet_since(&self, seat: &Seat) -> Option<i64> {
        last_written(&self.dir_of(seat).ok()?)
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
    /// A transcript line Claude Code writes per assistant message, with the
    /// `usage` object the tally reads and nothing else it needs.
    fn usage_line(
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        model: &str,
    ) -> String {
        json!({
            "type": "assistant",
            "message": {
                "model": model,
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": cache_read,
                    "cache_creation_input_tokens": cache_write,
                },
            },
        })
        .to_string()
    }

    /// The tally answers *where the tokens went* (`core-049`), so its own
    /// arithmetic has to hold: only lines past where the last look stopped are
    /// counted (a Stop fires every turn — re-reading a whole transcript each
    /// time would be quadratic in exactly the sessions this exists to measure),
    /// the running total survives between hooks, and the offset lands on the
    /// end of what was read. The model is the last one seen, because a ranking
    /// column wants the tier the seat is on now — the cost is priced per
    /// request and does not come from it.
    #[test]
    fn every_hook_tallies_only_the_transcript_slice_it_has_not_read() {
        let scratch = Scratch::new("burn");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let transcript = scratch.root.join("transcript.jsonl");
        let first = usage_line(100, 50, 1_000, 10, "claude-test");
        fs::write(&transcript, format!("{first}\n")).unwrap();

        let payload = json!({
            "session_id": "s1",
            "transcript_path": transcript.to_string_lossy(),
        });
        let burn = || read_json(&scratch.root.join(seat.as_str()).join(BURN_FILE));

        place.heard(&seat, "Stop", State::Idle, &payload);
        assert_eq!(u_at(&burn(), "input"), 100);
        assert_eq!(u_at(&burn(), "output"), 50);
        assert_eq!(u_at(&burn(), "cache_read"), 1_000);
        assert_eq!(u_at(&burn(), "turns"), 1);
        assert_eq!(str_of(&burn(), "model"), "claude-test");

        // A second turn appends to the file; only the new line may count.
        let second = usage_line(200, 20, 2_000, 0, "claude-test");
        let mut grown = std::fs::read_to_string(&transcript).unwrap();
        grown.push_str(&second);
        grown.push('\n');
        fs::write(&transcript, &grown).unwrap();

        place.heard(&seat, "Stop", State::Idle, &payload);
        assert_eq!(
            u_at(&burn(), "input"),
            300,
            "the second turn's input, not the file re-read"
        );
        assert_eq!(u_at(&burn(), "cache_read"), 3_000);
        assert_eq!(u_at(&burn(), "turns"), 2);

        // And a cleared session starts over rather than carrying the last one's
        // bill into it — `/clear` ends an accounting as surely as it ends a
        // context. Live, a cleared session also gets a fresh transcript, so
        // this points at one.
        let other = scratch.root.join("transcript-2.jsonl");
        fs::write(&other, format!("{}\n", usage_line(7, 0, 0, 0, "m"))).unwrap();
        let fresh = json!({ "session_id": "s2", "transcript_path": other.to_string_lossy() });
        place.heard(&seat, "SessionStart", State::Idle, &fresh);
        let b = burn();
        assert_eq!(str_of(&b, "session_id"), "s2");
        // The tally rides every hook including this one, so the new session
        // has already counted its own file — seven, and not the old three
        // hundred it inherited nothing from.
        assert_eq!(
            u_at(&b, "input"),
            7,
            "a new session starts from its own transcript"
        );
        assert_eq!(u_at(&b, "turns"), 1);

        // The synthetic corner a live run never produces — the old path handed
        // in under the new id — still must not *add* to anything: the file is
        // counted once, under whichever session owns it now.
        place.heard(&seat, "Stop", State::Idle, &payload);
        assert_eq!(
            u_at(&burn(), "input"),
            300,
            "re-attributed whole, never summed across sessions"
        );
    }

    /// The reader is over a *root*, not over this backend — which is the whole
    /// of `wsp-116`. A record this backend never wrote, sitting in a directory
    /// it has no name for, is still a seat that spent money, and a reader that
    /// can only be pointed at its own root is a reader that will report a
    /// machine full of agents as a machine that spent nothing.
    #[test]
    fn the_burn_reader_reads_any_root_it_is_given_rather_than_only_its_own() {
        let scratch = Scratch::new("burn-any-root");
        // A root with no relationship to `Supervisor::new()` — the compound
        // seats directory, under the same isolated state.
        let elsewhere = scratch.root.join("some-other-backend-seats");
        let seat = elsewhere.join("cpd-7");
        fs::create_dir_all(&seat).unwrap();
        fs::write(
            seat.join(BURN_FILE),
            json!({ "turns": 12, "model": "claude-opus-5-5", "input": 40, "output": 900 })
                .to_string(),
        )
        .unwrap();

        let read = burn_under(&elsewhere);
        assert_eq!(read.len(), 1, "the record is there to be found");
        assert_eq!(read[0].0, "cpd-7");
        assert_eq!(u_at(&read[0].1, "turns"), 12);
        assert_eq!(u_at(&read[0].1, "output"), 900);

        // And pointed at its own root it still reads only that: a reader with
        // no root parameter would have made this second assertion the first.
        assert!(
            burn_under(&scratch.root).is_empty(),
            "one root's seats are not another's"
        );
    }

    /// A seat that tallied nothing is not a row. The two cases come apart — a
    /// record with `turns: 0` and a directory with no record at all — and both
    /// are dropped, for the reason the reader's own docs now give rather than
    /// the one they used to.
    #[test]
    fn a_seat_that_tallied_no_turns_is_not_a_row_and_a_directory_with_no_record_is_not_either() {
        let scratch = Scratch::new("burn-zero-turns");
        for (seat, rec) in [
            ("sup-1", json!({ "turns": 3, "output": 100 })),
            // A hook fired, a transcript existed, and no line in it carried a
            // `usage` — real, and worth zero of everything.
            ("sup-2", json!({ "turns": 0, "output": 0, "offset": 19_230 })),
            ("sup-3", json!({})),
        ] {
            let dir = scratch.root.join(seat);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(BURN_FILE), rec.to_string()).unwrap();
        }
        // A file where a directory belongs is not a seat either.
        fs::write(scratch.root.join("stray"), "not a seat").unwrap();

        let read = burn_under(&scratch.root);
        assert_eq!(
            read.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["sup-1"],
            "one seat spent something; the other two are not ranks"
        );
    }

    /// A transcript that shrank under the tally — swept, rotated, truncated —
    /// restarts rather than counting from a hole, which would count somebody
    /// twice.
    #[test]
    fn a_transcript_shorter_than_remembered_starts_over() {
        let scratch = Scratch::new("burn-truncated");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let transcript = scratch.root.join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                usage_line(100, 0, 0, 0, "m"),
                usage_line(100, 0, 0, 0, "m")
            ),
        )
        .unwrap();
        let payload = json!({
            "session_id": "s1",
            "transcript_path": transcript.to_string_lossy(),
        });
        place.heard(&seat, "Stop", State::Idle, &payload);
        let burn_path = scratch.root.join(seat.as_str()).join(BURN_FILE);
        assert_eq!(u_at(&read_json(&burn_path), "turns"), 2);

        fs::write(&transcript, format!("{}\n", usage_line(7, 0, 0, 0, "m"))).unwrap();
        place.heard(&seat, "Stop", State::Idle, &payload);
        let b = read_json(&burn_path);
        assert_eq!(
            u_at(&b, "turns"),
            1,
            "counted the shrunken file from its start"
        );
        assert_eq!(
            u_at(&b, "input"),
            7,
            "and not the two old lines plus the new one"
        );
    }

    /// The pricing judgements, pinned: the rate is the model's, cache reads
    /// bill at a tenth of input and cache writes at a quarter above it. If the
    /// table changes, this changes with it — stated here rather than left
    /// implicit in a ranking nobody could reproduce.
    #[test]
    fn a_request_is_priced_at_the_rate_of_the_model_that_served_it() {
        use crate::cmd_burn::cost;
        // A million input tokens on opus is five dollars; on haiku, one.
        assert_eq!(cost("claude-opus-5", 1_000_000, 0, 0, 0), 5_000_000);
        assert_eq!(cost("claude-haiku-4-5-20251001", 1_000_000, 0, 0, 0), 1_000_000);
        assert_eq!(cost("claude-sonnet-5", 0, 1_000_000, 0, 0), 10_000_000);
        // The two multipliers, on the tier that makes them easiest to read.
        assert_eq!(
            cost("claude-opus-5", 0, 0, 1_000_000, 1_000_000),
            500_000 + 6_250_000,
            "reads at a tenth of input, writes at a quarter above it"
        );
        // The whole finding in one line: volume alone ranks these the wrong way
        // round. Eight million haiku input tokens cost less than two million
        // opus ones, and the old column said the opposite.
        assert!(
            cost("claude-haiku-4-5", 8_000_000, 0, 0, 0)
                < cost("claude-opus-5", 2_000_000, 0, 0, 0)
        );
        // A name the table does not know is not cheap. It prices at the dearest
        // row, so an unrecognised spender cannot hide at the bottom.
        assert_eq!(
            cost("gpt-something", 1_000_000, 0, 0, 0),
            cost("claude-fable-5", 1_000_000, 0, 0, 0)
        );
    }

    /// A hook fires on the writer's clock, not on its line endings. The tally
    /// used to advance its offset by everything it read, so a record caught
    /// half-written was consumed, failed to parse, and was never seen again —
    /// a silent undercount in the one thing here whose job is counting.
    #[test]
    fn a_record_still_being_written_is_counted_once_it_is_finished() {
        let scratch = Scratch::new("burn-partial");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let transcript = scratch.root.join("transcript.jsonl");
        let payload = json!({
            "session_id": "s1",
            "transcript_path": transcript.to_string_lossy(),
        });
        let burn = || read_json(&scratch.root.join(seat.as_str()).join(BURN_FILE));

        // One whole record, and the first half of the next.
        let whole = usage_line(100, 0, 0, 0, "claude-test");
        let next = usage_line(500, 0, 0, 0, "claude-test");
        let (head, tail) = next.split_at(next.len() / 2);
        fs::write(&transcript, format!("{whole}\n{head}")).unwrap();
        place.heard(&seat, "Stop", State::Idle, &payload);
        assert_eq!(u_at(&burn(), "input"), 100, "the finished record only");
        assert_eq!(u_at(&burn(), "turns"), 1);

        // The writer finishes it. Nothing was consumed that did not parse, so
        // the second read starts at the record rather than inside it.
        fs::write(&transcript, format!("{whole}\n{head}{tail}\n")).unwrap();
        place.heard(&seat, "Stop", State::Idle, &payload);
        assert_eq!(
            u_at(&burn(), "input"),
            600,
            "the completed record counted exactly once"
        );
        assert_eq!(u_at(&burn(), "turns"), 2);
    }

    /// Claude Code writes a transcript line per *content block* and repeats the
    /// request's whole `usage` object on each one, so a turn that spoke and then
    /// called a tool is two identical bills. Measured on this store on
    /// 2026-08-26: 49 usage lines over 30 requests. A naive sum reported 1.6x
    /// the real spend, which is the wrong direction for a report whose only job
    /// is to be believed.
    #[test]
    fn one_request_is_billed_once_however_many_blocks_it_wrote() {
        let scratch = Scratch::new("burn-blocks");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let transcript = scratch.root.join("transcript.jsonl");
        let payload = json!({
            "session_id": "s1",
            "transcript_path": transcript.to_string_lossy(),
        });
        let burn = || read_json(&scratch.root.join(seat.as_str()).join(BURN_FILE));

        let block = |req: &str| {
            let mut v: Value = serde_json::from_str(&usage_line(100, 10, 0, 0, "claude-test")).unwrap();
            v["requestId"] = json!(req);
            v.to_string()
        };
        fs::write(&transcript, format!("{}\n{}\n", block("req-1"), block("req-1"))).unwrap();
        place.heard(&seat, "Stop", State::Idle, &payload);
        assert_eq!(u_at(&burn(), "turns"), 1, "two blocks, one request");
        assert_eq!(u_at(&burn(), "input"), 100);

        // The blocks of one request can straddle a read, so the last id counted
        // is remembered between hooks — otherwise the dedupe would fire only
        // inside a slice and the boundary would double-bill.
        let mut grown = std::fs::read_to_string(&transcript).unwrap();
        grown.push_str(&format!("{}\n{}\n", block("req-1"), block("req-2")));
        fs::write(&transcript, &grown).unwrap();
        place.heard(&seat, "Stop", State::Idle, &payload);
        assert_eq!(u_at(&burn(), "turns"), 2, "the third block was the same request");
        assert_eq!(u_at(&burn(), "input"), 200);
    }

    /// A session that changes tier partway through is billed at both, because
    /// the price belongs to the request and not to the session. First-seen-wins
    /// billed a whole night to whatever tier the agent opened on; the stored
    /// model is now what the seat is *on*, and the cost does not come from it.
    #[test]
    fn a_session_that_changes_model_is_billed_at_both_tiers() {
        let scratch = Scratch::new("burn-switch");
        let place = scratch.place();
        let seat = place.open(&Order::default()).unwrap();
        let transcript = scratch.root.join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                usage_line(1_000_000, 0, 0, 0, "claude-haiku-4-5"),
                usage_line(1_000_000, 0, 0, 0, "claude-opus-5")
            ),
        )
        .unwrap();
        let payload = json!({
            "session_id": "s1",
            "transcript_path": transcript.to_string_lossy(),
        });
        place.heard(&seat, "Stop", State::Idle, &payload);
        let b = read_json(&scratch.root.join(seat.as_str()).join(BURN_FILE));
        assert_eq!(str_of(&b, "model"), "claude-opus-5", "the tier it is on now");
        assert_eq!(
            u_at(&b, "cost"),
            1_000_000 + 5_000_000,
            "a dollar of haiku and five of opus, not six of either"
        );
    }

    /// A settings dir of its own, so the check can be pointed somewhere real
    /// without touching whatever the machine running the suite actually has.
    struct SettingsScratch {
        dir: PathBuf,
        saved: Option<std::ffi::OsString>,
    }

    impl SettingsScratch {
        fn write(hooks: &Value) -> SettingsScratch {
            let dir = std::env::temp_dir()
                .join(format!("wsp-cc-settings-{}-{}", std::process::id(), util::epoch_nanos()));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("settings.json"), json!({ "hooks": hooks }).to_string()).unwrap();
            let saved = std::env::var_os("CLAUDE_CONFIG_DIR");
            std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
            SettingsScratch { dir, saved }
        }

        fn missing() -> SettingsScratch {
            let dir = std::env::temp_dir()
                .join(format!("wsp-cc-settings-{}-{}", std::process::id(), util::epoch_nanos()));
            let saved = std::env::var_os("CLAUDE_CONFIG_DIR");
            std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
            SettingsScratch { dir, saved }
        }
    }

    impl Drop for SettingsScratch {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
                None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// One entry naming `wsp-session.sh` under a hook is what "installed" means
    /// — anything else under that key (herdr's own `SessionStart` entry,
    /// beside it) is not this backend's business.
    fn hook_entry(command: &str) -> Value {
        json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": command }] }])
    }

    /// `compound-107`, reproduced rather than described: `SessionStart` alone,
    /// exactly `cpd-5`'s settings. This is the case a check that only fires on
    /// total absence would have missed.
    #[test]
    fn only_session_start_installed_is_named_a_problem_not_silence() {
        let _s = SettingsScratch::write(&json!({
            "SessionStart": hook_entry("sh '/x/wsp-session.sh' SessionStart"),
        }));
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        hook_snippet_health(&mut problems, &mut notes);
        assert_eq!(problems.len(), 1, "silent here is exactly the bug this row exists to end");
        for name in ["SessionEnd", "UserPromptSubmit", "Stop", "StopFailure", "PermissionRequest", "Elicitation"] {
            assert!(problems[0].contains(name), "{name} missing from the finding: {}", problems[0]);
        }
        assert!(!problems[0].contains("SessionStart,"), "the one hook that IS installed should not be named missing");
    }

    /// All seven, correctly installed: nothing to say, which is doctor's own
    /// convention for a check that passed.
    #[test]
    fn all_seven_installed_is_silent() {
        let entries: serde_json::Map<String, Value> = HOOK_NAMES
            .iter()
            .map(|h| (h.to_string(), hook_entry(&format!("sh '/x/wsp-session.sh' {h}"))))
            .collect();
        let _s = SettingsScratch::write(&Value::Object(entries));
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        hook_snippet_health(&mut problems, &mut notes);
        assert!(problems.is_empty());
        assert!(notes.is_empty());
    }

    /// No settings file at all is a smaller, different fact than a short one —
    /// a machine that never merged the snippet in is not lying about being
    /// configured, so it is a note rather than a problem.
    #[test]
    fn no_settings_file_is_a_note_not_a_problem() {
        let _s = SettingsScratch::missing();
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        hook_snippet_health(&mut problems, &mut notes);
        assert!(problems.is_empty());
        assert_eq!(notes.len(), 1);
    }
}
