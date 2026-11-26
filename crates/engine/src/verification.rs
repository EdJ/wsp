//! A verification is a pass on the member's own row: `wsp-188`.
//!
//! Every verification used to be filed as a row of its own — a child of the
//! member, tagged `verify`, whose overview was the verifier's work order. Ed,
//! 2026-10-06: that doubled the task count for no reason. wsp-148 alone had
//! five Verify rows and compound-289 had four, and every one of them was read
//! for exactly one fact: what the newest verdict on its parent said.
//!
//! So the verdict lives where it is read. A member carries a
//! `## Verification` section, one entry per pass, oldest first:
//!
//! ```text
//! - 2026-10-06T10:00:00Z holds · read 55c4839abcde · pane cpd-12 · on opus/high · ended
//!   what was checked, what held, what did not — indented under the entry
//! ```
//!
//! The word after the instant is the state ([`State`]); the clauses after it
//! are keyed by their first word, so a clause nobody wrote is simply absent
//! and a reader of an old entry never mistakes one clause for another.
//!
//! # The entry is the record, and the only one
//!
//! A verifier has no row and holds no claim. The `running` entry is written
//! under the store lock *before* its agent is spawned — the idempotence key the
//! verifier row used to be — and `wsp spawn --verify` writes the seat onto it.
//! Everything that asks about a verifier asks the entry: the barrier's gate
//! ([`verified`]), the tick that ends a verifier once ([`Ending`]), `wsp wip`
//! naming the seat against its member ([`seats`]), and the brief that seat is
//! given. A binding or a claim would have been a second record of the same
//! fact, and the store has paid for that shape before: two readers of one
//! record disagreeing is how wsp-148 got five rows.
//!
//! # Bookkeeping does not touch the member
//!
//! Opening an entry, naming its seat and marking it ended are written without
//! moving `updated`. A member with no landing to key on is re-verified when it
//! is touched ([`crate::cycle`]'s `moved_since`), and a re-verify bought by the
//! verifier's own bookkeeping would be one bought by nobody. A verdict that
//! blocks moves the member to `doing`, which is a real change and does touch it.
//!
//! # The rows that came before
//!
//! A Verify row wsp filed before this is read as a pass ([`legacy`]) until
//! [`migrate`] has moved it onto its parent and archived it — so a binary that
//! is installed before the migration runs reads the same verdicts the rows
//! held, and starts nothing because a row it could not see looked like a
//! member never verified.

use std::collections::BTreeMap;

use crate::model::{Status, Task};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::base::args::Args;

/// The heading the passes sit under on the member's row.
pub const SECTION: &str = "Verification";

/// Where a pass is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Opened, and no verdict yet. Its seat may not exist yet either.
    Running,
    Holds,
    Blocks,
    /// A newer landing came in while this pass was reading. It was ended and
    /// its verdict, if one arrives, is refused.
    Superseded,
    /// The verifier could not verify (`cause: env`: its tree lost, the load,
    /// a tool missing — compounding-449). **Not a block of the work** (`wsp-614`):
    /// the member stays at review, nothing is sent back, and the run starts a
    /// new verifier, as it does after a superseded pass.
    CouldNotVerify,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Holds => "holds",
            State::Blocks => "blocks",
            State::Superseded => "superseded",
            State::CouldNotVerify => "could not verify",
        }
    }

    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "running" => State::Running,
            "holds" => State::Holds,
            "blocks" => State::Blocks,
            "superseded" => State::Superseded,
            "could not verify" => State::CouldNotVerify,
            _ => return None,
        })
    }
}

/// Whether the pass's seat has been ended, and how that went.
///
/// **Failed is terminal.** wsp-176 item 1: a despawn that failed was retried
/// every tick, each one a line in `cycle.log` and none of them a different
/// answer. It is tried once, the failure is written here and told, and a
/// person ends the seat if it is still standing.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Ending {
    #[default]
    Standing,
    Ended,
    Failed(String),
}

/// One verification of a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    /// When it was opened, as `util::now_iso` writes it.
    pub at: String,
    pub state: State,
    /// The commit it was asked to read: the member's landing when the pass was
    /// opened. `None` for work with no repository behind it.
    pub read: Option<String>,
    /// The member's own commit when `read` is another (`wsp-350`): a by-merge
    /// row's pass reads the PR's head, and "has the member moved" is asked of
    /// this one ([`Pass::own`]).
    pub ready: Option<String>,
    /// Where a by-merge row's `read` came from: the PR, or the remote branch.
    pub from: Option<String>,
    /// The commit that reached the trunk when it is not the one read: a hold
    /// lands the branch rebased onto a trunk that moved.
    pub landed: Option<String>,
    /// The seat its agent was started in.
    pub pane: Option<String>,
    /// What it ran on: the kind, or the kind and its tier.
    pub on: Option<String>,
    /// The Verify row this pass was migrated from, or is being read from.
    pub was: Option<String>,
    pub ending: Ending,
    /// When the verdict was recorded, as `util::now_iso` writes it. `wsp-193`
    /// measures "somebody typed to the member since" from here: a turn after
    /// `at` and before the verdict is the member answering its own verifier.
    pub decided: Option<String>,
    /// The verdict, in the verifier's words.
    pub text: String,
    /// The verdict's tail (`wsp-241`): the reason a glance reads after the
    /// `holds` or `blocks` wsp puts in front. `None` on every pass written
    /// before captions, which reads as the word and its time.
    pub caption: Option<String>,
    /// What the verifier's seat consumed, written when the seat is ended
    /// (wsp-290). `None` on a pass still reading, on every pass written before
    /// this, and on one whose seat had no session to read.
    pub spent: Option<Spent>,
    /// What the verifier did, read off its transcript when its seat was ended
    /// (`wsp-325`). `None` on a pass still reading, on every pass written
    /// before this, and on one whose transcript could not be read.
    pub did: Option<crate::did::Did>,
    /// The done-when items the verifier says it checked, each held or not.
    pub checked: Vec<crate::did::Checked>,
    /// On a hold: the land it owes, and why it could not happen yet
    /// (`wsp-605`) — a dirty tree, which is no block of the work. Cleared when
    /// the run lands it. One line.
    pub owed: Option<String>,
}

/// A pass's spend: the clauses a leg's end line carries, as clauses of the
/// entry. `ran` is the tier that served it (`sonnet-5-5/high`), which `on`
/// cannot say: that is what was asked for.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Spent {
    pub ran: String,
    pub turns: usize,
    pub peak: u64,
    pub spend: crate::agent_kind::Spend,
    /// Seconds the verifier's model calls took, and seconds its tools ran
    /// (`compounding-104`), read off its transcript when its seat is ended. `0`
    /// is unread: a kind with no transcript wsp can read.
    pub model_secs: i64,
    pub tool_secs: i64,
    /// Seconds its test and build commands ran, and the time of its transcript's
    /// first line (`wsp-429`), as `util::now_iso` writes it: the seat's real
    /// start, which wsp's `started` line lags. `None` is unread.
    pub test_secs: i64,
    pub first_line: Option<String>,
    /// The one-minute load average when its seat was named, in hundredths: the
    /// condition a timing was taken under. `None` where it could not be read.
    pub load1: Option<u32>,
}

impl Spent {
    /// What the transcript says of the fixed parts, kept on the reading.
    pub fn with_session(spent: Option<Spent>, read: Option<(i64, String)>) -> Option<Spent> {
        let Some((test, first)) = read else { return spent };
        let mut s = spent.unwrap_or_default();
        s.test_secs = test;
        s.first_line = Some(first);
        Some(s)
    }

    pub fn of(ran: &crate::agent_kind::Ran) -> Spent {
        Spent { ran: ran.label(), turns: ran.turns, peak: ran.peak, spend: ran.spend, ..Spent::default() }
    }

    /// The reading at the seat's end, with what was written at its start kept:
    /// the load, and the seconds when a reading of the transcript gave them.
    pub fn ending(start: Option<&Spent>, ran: Option<&crate::agent_kind::Ran>, secs: Option<(i64, i64)>) -> Option<Spent> {
        let mut s = match ran {
            Some(r) => Spent::of(r),
            None if start.is_some_and(|s| s.load1.is_some()) => Spent::default(),
            None => return None,
        };
        s.load1 = start.and_then(|s| s.load1);
        if let Some((model, tool)) = secs {
            s.model_secs = model;
            s.tool_secs = tool;
        }
        Some(s)
    }
}

impl Pass {
    /// A pass opened now, on the commit the member's work is on.
    /// The member's own commit the pass was opened at: what its landing, ready
    /// or reading line is compared against.
    pub fn own(&self) -> Option<&str> {
        self.ready.as_deref().or(self.read.as_deref())
    }

    pub fn opened(read: Option<String>, on: Option<String>) -> Pass {
        Pass {
            at: util::now_iso(),
            state: State::Running,
            read: read.map(|s| short(&s)),
            ready: None,
            from: None,
            landed: None,
            pane: None,
            on,
            was: None,
            ending: Ending::Standing,
            decided: None,
            text: String::new(),
            caption: None,
            spent: None,
            did: None,
            checked: Vec::new(),
            owed: None,
        }
    }

    /// Whether this pass still has an agent wsp has not ended.
    pub fn standing(&self) -> bool {
        self.pane.is_some() && self.ending == Ending::Standing
    }

    fn line(&self) -> String {
        let mut out = format!("- {} {}", self.at, self.state.as_str());
        let mut clause = |k: &str, v: &Option<String>| {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                out.push_str(&format!(" · {k} {v}"));
            }
        };
        clause("read", &self.read);
        clause("ready", &self.ready);
        clause("from", &self.from);
        clause("landed", &self.landed);
        clause("pane", &self.pane);
        clause("on", &self.on);
        clause("was", &self.was);
        clause("decided", &self.decided);
        // One clause: a ` · ` inside the reason would end it.
        clause("owed", &self.owed.as_deref().map(|o| crate::util::fold(o).replace(" · ", " — ")));
        if let Some(s) = &self.spent {
            clause("ran", &Some(s.ran.clone()));
            clause("turns", &Some(s.turns.to_string()));
            clause("peak", &Some(util::count_human(s.peak)).filter(|_| s.peak > 0));
            clause("spent", &s.spend.clause());
            clause("model", &Some(format!("{}s", s.model_secs)).filter(|_| s.model_secs > 0));
            clause("tools", &Some(format!("{}s", s.tool_secs)).filter(|_| s.tool_secs > 0));
            clause("tested", &Some(format!("{}s", s.test_secs)).filter(|_| s.first_line.is_some()));
            clause("first", &s.first_line);
            clause("load1", &s.load1.map(|l| format!("{}.{:02}", l / 100, l % 100)));
        }
        match &self.ending {
            Ending::Standing => {}
            Ending::Ended => out.push_str(" · ended"),
            // One line: a reason from stderr can run to several, and a line
            // break inside a clause would end the entry.
            Ending::Failed(why) => out.push_str(&format!(" · end failed: {}", crate::util::fold(why))),
        }
        // Last, because it is the one clause made of somebody's words: a ` · `
        // inside a caption would otherwise end it and start a clause nobody
        // wrote. [`Pass::parse`] takes everything after the key.
        if let Some(c) = self.caption.as_deref().filter(|c| !c.is_empty()) {
            out.push_str(&format!(" · {CAPTION}{c}"));
        }
        for l in self.text.trim().lines() {
            out.push_str("\n  ");
            out.push_str(l.trim_end());
        }
        // The lines wsp wrote come last, and a reader takes them off the end:
        // a verdict whose own last line looks like one is closed first.
        let ours: Vec<String> = self
            .did
            .iter()
            .flat_map(|d| d.lines())
            .chain(self.checked.iter().map(|c| crate::did::line("checked", &c.text())))
            .collect();
        if !ours.is_empty() && crate::did::ends_like_ours(&self.text) {
            out.push_str("\n  (end of verdict)");
        }
        for l in ours {
            out.push_str("\n  ");
            out.push_str(&l);
        }
        out
    }

    /// Take the trailing lines wsp wrote off the verdict's text.
    fn peel(&mut self) {
        let lines: Vec<&str> = self.text.lines().collect();
        let mut start = lines.len();
        while start > 0 && crate::did::split_line(lines[start - 1]).is_some() {
            start -= 1;
        }
        if start == lines.len() {
            return;
        }
        let mut did = crate::did::Did::default();
        let mut read = false;
        for l in &lines[start..] {
            let Some((key, body)) = crate::did::split_line(l) else { continue };
            if key == "checked" {
                self.checked.extend(crate::did::Checked::parse(body));
            } else {
                read = true;
                did.absorb(key, body);
            }
        }
        if read {
            self.did = Some(did);
        }
        if start > 0 && lines[start - 1] == "(end of verdict)" {
            start -= 1;
        }
        self.text = lines[..start].join("\n");
    }

    fn parse(line: &str) -> Option<Pass> {
        let rest = line.strip_prefix("- ")?;
        let (at, rest) = rest.split_once(' ')?;
        let (rest, caption) = match rest.split_once(&format!(" · {CAPTION}")) {
            Some((head, c)) => (head, Some(c.trim().to_string())),
            None => (rest, None),
        };
        let mut clauses = rest.split(" · ");
        let state = State::parse(clauses.next()?.trim())?;
        let mut p = Pass {
            at: at.to_string(),
            state,
            read: None,
            ready: None,
            from: None,
            landed: None,
            pane: None,
            on: None,
            was: None,
            ending: Ending::Standing,
            decided: None,
            text: String::new(),
            caption,
            spent: None,
            did: None,
            checked: Vec::new(),
            owed: None,
        };
        for c in clauses {
            let c = c.trim();
            if c == "ended" {
                p.ending = Ending::Ended;
            } else if let Some(why) = c.strip_prefix("end failed:") {
                p.ending = Ending::Failed(why.trim().to_string());
            } else if let Some((k, v)) = c.split_once(' ') {
                let v = Some(v.trim().to_string());
                match k {
                    "read" => p.read = v,
                    "ready" => p.ready = v,
                    "from" => p.from = v,
                    "landed" => p.landed = v,
                    "pane" => p.pane = v,
                    "on" => p.on = v,
                    "was" => p.was = v,
                    "decided" => p.decided = v,
                    "owed" => p.owed = v,
                    "ran" => p.spent.get_or_insert_with(Spent::default).ran = v.unwrap_or_default(),
                    "turns" => p.spent.get_or_insert_with(Spent::default).turns = v.and_then(|v| v.parse().ok()).unwrap_or(0),
                    "peak" => p.spent.get_or_insert_with(Spent::default).peak = v.and_then(|v| util::count_of(&v)).unwrap_or(0),
                    "model" => p.spent.get_or_insert_with(Spent::default).model_secs = v.and_then(|v| v.trim_end_matches('s').parse().ok()).unwrap_or(0),
                    "tools" => p.spent.get_or_insert_with(Spent::default).tool_secs = v.and_then(|v| v.trim_end_matches('s').parse().ok()).unwrap_or(0),
                    "tested" => p.spent.get_or_insert_with(Spent::default).test_secs = v.and_then(|v| v.trim_end_matches('s').parse().ok()).unwrap_or(0),
                    "first" => p.spent.get_or_insert_with(Spent::default).first_line = v,
                    "load1" => p.spent.get_or_insert_with(Spent::default).load1 = v.and_then(|v| v.parse::<f64>().ok()).map(|l| (l * 100.0).round() as u32),
                    "spent" => p.spent.get_or_insert_with(Spent::default).spend = v.and_then(|v| crate::agent_kind::Spend::of_clause(&v)).unwrap_or_default(),
                    _ => {}
                }
            }
        }
        Some(p)
    }
}

/// The key the caption clause is written under, space included.
const CAPTION: &str = "caption ";

/// A commit as an entry writes it: twelve characters, which is unambiguous in
/// any repository wsp works in and short enough to read in `wsp show`.
fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// Whether two spellings of a commit name the same one. An entry writes twelve
/// characters and a landing records forty, so the test is a prefix either way
/// round — never shorter than git's own seven.
pub fn same_commit(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len());
    n >= 7 && a[..n] == b[..n]
}

/// The passes written on a member's own row, oldest first.
pub fn passes(t: &Task) -> Vec<Pass> {
    entries(t).0
}

/// A mechanical check run, written beside the passes (`wsp-324`): what `wsp
/// check` ran at a landing and how it went.
///
/// ```text
/// - 2026-11-01T15:30:00Z checked failed · read 55c4839abcde · 141s
///   ✓ build · cargo build --release · 41s
///   ✗ tests · cargo test · 100s · exit 101
///     | test result: FAILED. 1 passed; 1 failed
/// ```
///
/// **Not a [`Pass`]**: no agent read anything, so it is not a verdict, holds no
/// seat, and the barrier's gate and the verifier's keys never see it. It shares
/// the section because a reader of the member wants one place to look, and the
/// two kinds of entry are told apart by the word after the instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub at: String,
    pub state: RunState,
    /// The commit it ran at.
    pub read: Option<String>,
    /// Seconds from start to finish; `None` while it is still running.
    pub took: Option<u64>,
    /// The step lines, indented under the entry.
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    Passed,
    Failed,
    /// Timed out while the machine was over its load line: no verdict on the
    /// change, and a rerun is owed once the load falls (`wsp-595`).
    Waiting,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Passed => "passed",
            RunState::Failed => "failed",
            RunState::Waiting => "waiting",
        }
    }
}

/// What follows the instant on a run's line, before its state.
const CHECKED: &str = "checked ";

impl Run {
    fn line(&self) -> String {
        let mut out = format!("- {} {CHECKED}{}", self.at, self.state.as_str());
        if let Some(r) = self.read.as_deref().filter(|r| !r.is_empty()) {
            out.push_str(&format!(" · read {r}"));
        }
        if let Some(n) = self.took {
            out.push_str(&format!(" · {n}s"));
        }
        for l in self.text.trim_end().lines() {
            out.push_str("\n  ");
            out.push_str(l.trim_end());
        }
        out
    }

    fn parse(line: &str) -> Option<Run> {
        let rest = line.strip_prefix("- ")?;
        let (at, rest) = rest.split_once(' ')?;
        let rest = rest.strip_prefix(CHECKED)?;
        let mut clauses = rest.split(" · ");
        let state = match clauses.next()?.trim() {
            "running" => RunState::Running,
            "passed" => RunState::Passed,
            "failed" => RunState::Failed,
            "waiting" => RunState::Waiting,
            _ => return None,
        };
        let mut run = Run { at: at.to_string(), state, read: None, took: None, text: String::new() };
        for c in clauses {
            let c = c.trim();
            if let Some(r) = c.strip_prefix("read ") {
                run.read = Some(r.trim().to_string());
            } else if let Some(n) = c.strip_suffix('s').and_then(|n| n.parse().ok()) {
                run.took = Some(n);
            }
        }
        Some(run)
    }
}

/// The check runs written on a member's own row, oldest first.
pub fn runs(t: &Task) -> Vec<Run> {
    entries(t).1
}

/// Both kinds of entry in one walk: an indented line belongs to whichever kind
/// of entry came last.
fn entries(t: &Task) -> (Vec<Pass>, Vec<Run>) {
    let Some(text) = t.section(SECTION) else { return (Vec::new(), Vec::new()) };
    let mut passes: Vec<Pass> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut in_run = false;
    for line in text.lines() {
        if let Some(r) = Run::parse(line) {
            runs.push(r);
            in_run = true;
        } else if let Some(p) = Pass::parse(line) {
            passes.push(p);
            in_run = false;
        } else if let Some(more) = line.strip_prefix("  ") {
            if in_run {
                if let Some(last) = runs.last_mut() {
                    if !last.text.is_empty() {
                        last.text.push('\n');
                    }
                    last.text.push_str(more);
                }
            } else if let Some(last) = passes.last_mut() {
                if !last.text.is_empty() {
                    last.text.push('\n');
                }
                last.text.push_str(more);
            }
        }
    }
    for p in &mut passes {
        p.peel();
    }
    (passes, runs)
}

/// The passes as `wsp show --json` gives them (`wsp-241`), oldest first.
///
/// `verdict` is the verifier's words and `state` the word, as the entry has
/// them. `caption` is §17.6's shape on a pass that has a verdict, and null on
/// one still reading or superseded: those have no verdict to caption, and
/// what they are is `wsp-242`'s stage.
#[cfg(test)]
fn json(t: &Task) -> serde_json::Value {
    wsp_view::task::verification_json(&view(t))
}

/// The passes as `wsp-view` takes them, oldest first.
pub fn view(t: &Task) -> Vec<wsp_view::task::Pass> {
    passes(t)
        .iter()
        .map(|p| {
            let decided = matches!(p.state, State::Holds | State::Blocks);
            let at = p.decided.as_deref().unwrap_or(&p.at);
            wsp_view::task::Pass {
                state: p.state.as_str().to_string(),
                verdict: p.text.clone(),
                caption: decided.then(|| crate::stage::told_view(&crate::caption::of_pass(p.state.as_str(), p.caption.as_deref(), at))),
                at: p.at.clone(),
                decided: p.decided.clone(),
                read: p.read.clone(),
                landed: p.landed.clone(),
                pane: p.pane.clone(),
                did: p.did.as_ref().map(did_view),
                checked: p.checked.iter().map(|c| (c.item.clone(), c.held)).collect(),
            }
        })
        .collect()
}

/// What a verifier did, as `wsp-view` takes it (`wsp-325`).
fn did_view(d: &crate::did::Did) -> wsp_view::task::Did {
    wsp_view::task::Did {
        ran: d
            .ran
            .iter()
            .map(|c| wsp_view::task::Ran { command: c.text.clone(), test: c.is_test(), outcome: c.last().word().to_string() })
            .collect(),
        ran_more: d.more_ran,
        drove: d.drove.clone(),
        files: d.read.files.iter().map(|f| wsp_view::task::FileRead { path: f.path.clone(), lines: f.lines.clone() }).collect(),
        files_more: d.read.more_files,
        diffs: d.read.diffs.clone(),
        diffs_more: d.read.more_diffs,
    }
}

/// Write the passes back under the heading, in time order, with the check runs
/// that were already there.
///
/// Does not touch the member — see the module docs for why bookkeeping must
/// not move `updated`.
pub fn write(t: &mut Task, passes: &[Pass]) {
    let kept = runs(t);
    write_entries(t, passes, &kept);
}

/// [`write`] for the check runs, keeping the passes.
fn write_runs(t: &mut Task, runs: &[Run]) {
    let kept = passes(t);
    write_entries(t, &kept, runs);
}

/// The verb other modules call to record check runs: the runs go under the
/// heading, the passes stay, and the row is saved. This module is the only one
/// that edits `## Verification`.
pub fn save_runs(store: &Store, t: &mut Task, runs: &[Run]) -> std::io::Result<()> {
    write_runs(t, runs);
    store.save_task(t)
}

/// A check that passed at `from` stands for the same work rebased to `to`: the
/// rebase added only what the trunk gained, and the barrier runs the whole
/// suite on the result. Written as a passed entry that says so, so the gate
/// does not start a second check for the new tip. `false` when `from` has no
/// passed check, or `to` already has an entry.
pub fn carry_pass(t: &mut Task, from: &str, to: &str) -> bool {
    let mut runs = runs(t);
    let at = |r: &Run, sha: &str| r.read.as_deref().is_some_and(|x| same_commit(x, sha));
    if runs.iter().any(|r| at(r, to)) {
        return false;
    }
    let Some(passed) = runs.iter().rev().find(|r| at(r, from)).filter(|r| r.state == RunState::Passed) else { return false };
    let took = passed.took;
    let mut secs = util::epoch_secs();
    while runs.iter().any(|r| util::epoch_of(&r.at) >= secs) {
        secs += 1;
    }
    let (from12, to12): (String, String) = (from.chars().take(12).collect(), to.chars().take(12).collect());
    runs.push(Run {
        at: util::iso_at(secs),
        state: RunState::Passed,
        read: Some(to12),
        took,
        text: format!("✓ carried · passed at {from12} (rebased onto the trunk since) · not run again"),
    });
    write_runs(t, &runs);
    true
}

fn write_entries(t: &mut Task, passes: &[Pass], runs: &[Run]) {
    let mut all: Vec<(String, String)> = passes.iter().map(|p| (p.at.clone(), p.line())).collect();
    all.extend(runs.iter().map(|r| (r.at.clone(), r.line())));
    all.sort_by(|a, b| a.0.cmp(&b.0));
    let text = all.into_iter().map(|(_, l)| l).collect::<Vec<_>>().join("\n");
    crate::model::set_section_in(&mut t.body, SECTION, &text);
}

/// The tag a Verify row carries. Kept for [`legacy`] and [`migrate`]: nothing
/// files one any more.
pub const VERIFY_TAG: &str = "verify";

/// The phrase every work order wsp wrote onto a Verify row carries, and the
/// thing that tells a row wsp filed from one a person wrote and tagged.
const FILED_BY_WSP: &str = "wsp spawned you when";

/// Whether a row is a Verify row wsp filed: the tag, a parent, and wsp's own
/// work order in the overview. Not the title — `wsp-137` is titled
/// "Re-verify" and is somebody's own review, and must be left alone.
pub fn filed_by_wsp(t: &Task) -> bool {
    t.tags.iter().any(|g| g == VERIFY_TAG)
        && t.parent.is_some()
        && t.section("Overview").is_some_and(|o| o.contains(FILED_BY_WSP))
}

/// A Verify row read as the pass it recorded.
pub fn legacy(row: &Task) -> Pass {
    let state = match row.status() {
        Status::Review | Status::Done => State::Holds,
        Status::Blocked => State::Blocks,
        _ => State::Running,
    };
    let attempt = crate::attempt::attempts_of(row).into_iter().last();
    let on = attempt.as_ref().and_then(|a| {
        let ran = crate::attempt::Row(a.clone()).tier();
        (!ran.is_empty()).then_some(ran)
    });
    Pass {
        at: row.created.clone(),
        state,
        read: crate::repair::landed(row).map(|s| short(&s)),
        ready: None,
        from: None,
        landed: None,
        pane: attempt.map(|a| a.pane).filter(|p| !p.is_empty()),
        on,
        was: Some(row.id.clone()),
        ending: Ending::Ended,
        // The row's last touch: for a row at its verdict, the verdict.
        decided: (state != State::Running).then(|| row.updated.clone()),
        text: verdict_of(row),
        caption: None,
        spent: None,
        did: None,
        checked: Vec::new(),
        owed: None,
    }
}

/// The verdict a Verify row's own log recorded: its last `review:` or
/// `blocked:` line, which is what `wsp review <id> -` and `wsp block` write.
fn verdict_of(row: &Task) -> String {
    let Some(log) = row.section("Log") else { return String::new() };
    log.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("- ")?;
            let (_, said) = rest.split_once(' ')?;
            said.strip_prefix("review: ").or_else(|| said.strip_prefix("blocked: "))
        })
        .last()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Every pass on a member, oldest first: the entries on its row, and any Verify
/// row under it that has not been migrated yet.
pub fn all(tasks: &[Task], member: &Task) -> Vec<Pass> {
    let mut out = passes(member);
    out.extend(
        tasks
            .iter()
            .filter(|t| t.parent.as_deref() == Some(member.id.as_str()) && filed_by_wsp(t))
            .map(legacy),
    );
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.was.cmp(&b.was)));
    out
}

/// The newest pass on a member, by id.
pub fn latest(tasks: &[Task], member: &str) -> Option<Pass> {
    let m = tasks.iter().find(|t| t.id == member)?;
    all(tasks, m).pop()
}

/// Whether a member's newest pass holds — the predicate the barrier is gated on.
pub fn verified(tasks: &[Task], member: &str) -> bool {
    latest(tasks, member).is_some_and(|p| p.state == State::Holds)
}

/// The member's newest pass, when it blocked.
pub fn blocked(tasks: &[Task], member: &str) -> Option<Pass> {
    latest(tasks, member).filter(|p| p.state == State::Blocks)
}

/// Every seat a pass still stands in, to the member it is verifying — what
/// `wsp wip` and `wsp brief` name a verifier's seat by.
pub fn seats(tasks: &[Task]) -> BTreeMap<String, String> {
    seated(tasks).into_iter().map(|(seat, (member, _))| (seat, member)).collect()
}

/// [`seats`], with which of the member's passes stands there, counting from 1
/// — so a pass after the first is a member being verified again (`wsp-246`).
pub fn seated(tasks: &[Task]) -> BTreeMap<String, (String, usize)> {
    let mut out = BTreeMap::new();
    for t in tasks.iter().filter(|t| t.body.contains("## Verification")) {
        for (i, p) in passes(t).into_iter().enumerate().filter(|(_, p)| p.standing()) {
            if let Some(pane) = p.pane {
                out.insert(pane, (t.id.clone(), i + 1));
            }
        }
    }
    out
}

/// What the pass standing in each seat records it ran on — its `on` clause, a
/// [`crate::model::Policy::label`] — for `wip`'s kind, model and effort
/// (`wsp-248`), by seat. `None` for a seat whose pass recorded nothing.
///
/// Every seat from one walk of the passes, since `wsp-320`: asked one seat at
/// a time, it was a parse of every task's `## Verification` per agent, on
/// every attention pass the daemon makes.
pub fn ran_on_by_seat(tasks: &[Task]) -> BTreeMap<String, Option<String>> {
    let mut out = BTreeMap::new();
    for p in tasks.iter().filter(|t| t.body.contains("## Verification")).flat_map(passes) {
        if p.standing() {
            if let Some(pane) = p.pane.clone() {
                // The last standing pass in a seat is the one that answers.
                out.insert(pane, p.on);
            }
        }
    }
    out
}

/// The member a seat has ever verified, and the pass it did it in: the newest
/// one, of any state and any ending. `wsp-197`.
///
/// **Ever, not now, because the seat that broke this was finished.** cpd-332
/// had given its verdict on wsp-179 and was idle when it ran a bare `wsp
/// govern` and was written into two seats; [`seats`] is what is still standing
/// and would have let it through. So every pass is asked, and every Verify row
/// wsp filed before `wsp-188` — by each pane that ever held a claim on it, not
/// only the last one [`legacy`] reads, because a released claim was still a
/// verifier's.
///
/// Archived members are passed in too: a member archived after its verdict
/// does not make the pane that verified it fit to govern.
pub fn verified_in(tasks: &[Task], seat: &str) -> Option<(String, Pass)> {
    if seat.is_empty() {
        return None;
    }
    let mut found: Vec<(String, Pass)> = Vec::new();
    for t in tasks {
        if t.body.contains("## Verification") {
            found.extend(passes(t).into_iter().filter(|p| p.pane.as_deref() == Some(seat)).map(|p| (t.id.clone(), p)));
        }
        if filed_by_wsp(t) {
            if crate::attempt::attempts_of(t).iter().any(|a| a.pane == seat) {
                let member = t.parent.clone().unwrap_or_default();
                found.push((member, Pass { pane: Some(seat.to_string()), ..legacy(t) }));
            }
        }
    }
    found.into_iter().max_by(|a, b| a.1.at.cmp(&b.1.at))
}

/// What a verifier's seat is called, wherever a seat is named.
pub fn seat_label(member: &str) -> String {
    format!("{member} · verifying")
}

/// Write the seat onto the newest pass that has none. `wsp spawn --verify`'s
/// half of the record, written the moment the seat exists and before the agent
/// in it reads its brief.
pub fn name_seat(store: &Store, member: &str, seat: &str) -> bool {
    let wrote = store.locked(|| {
        let Some(mut t) = store.find_task(member) else { return false };
        let mut ps = passes(&t);
        let Some(p) = ps.iter_mut().rev().find(|p| p.state == State::Running && p.pane.is_none()) else {
            return false;
        };
        p.pane = Some(seat.to_string());
        // The load the verifier starts under (`compounding-104`): a timing is
        // read against it, and VS-c02 was a false block at load 108.
        if let Some(one) = crate::throttle::load1() {
            p.spent.get_or_insert_with(Spent::default).load1 = Some((one * 100.0).round() as u32);
        }
        write(&mut t, &ps);
        store.save_task(&t).is_ok()
    });
    if wrote {
        store.git_commit(&format!("wsp: {member} is verified in {seat}"));
    }
    wrote
}

/// Mark the running pass in `seat` superseded, so the next pass opens a fresh
/// one and [`crate::cycle::end_passes`] ends this one's seat. Bookkeeping, so
/// the member's `updated` does not move. Whether it wrote.
pub fn supersede(store: &Store, member: &str, seat: &str) -> bool {
    let wrote = store.locked(|| {
        let Some(mut t) = store.find_task(member) else { return false };
        let mut ps = passes(&t);
        let Some(p) = ps.iter_mut().rev().find(|p| p.state == State::Running && p.pane.as_deref() == Some(seat)) else {
            return false;
        };
        p.state = State::Superseded;
        write(&mut t, &ps);
        store.save_task(&t).is_ok()
    });
    if wrote {
        store.git_commit(&format!("wsp: {member}'s pass in {seat} took no turn — superseded"));
    }
    wrote
}

/// `wsp verified <member> --holds|--blocks --from FILE --caption "…"` — a
/// verifier's verdict, recorded on the member it read.
///
/// **The caption is required, with no exemption** (`wsp-241`). Only an agent
/// wsp spawned runs this, its work order says how to write one, and a verdict
/// is long and opens with bookkeeping, so there is no sentence of it to stand
/// in. It is refused before anything is read or written.
///
/// **Onto the caller's own pass**: the newest `running` entry whose seat is
/// this pane, or the newest `running` entry at all for a caller with no seat.
/// A governor who answers a block by hand — the finding was wrong, the work
/// stands — has no pass open, and gets a new one in their own name.
///
/// **A superseded pass is refused**, because a newer landing has already
/// started a fresh verifier on the work this one was reading, and a verdict
/// about the old commit landing after it would read as the newest.
///
/// Holds leaves the member where it is. Blocks is `wsp reopen` with the verdict
/// as what is owed — the member goes back to `doing`, and its pane is told —
/// and the run tells its governor once, as the decision it is.
pub fn cmd_verified(store: &Store, args: &Args) -> i32 {
    let me = crate::place::my_pane();
    // Given no verdict: the one the verifier wrote at the place wsp keeps for it
    // (`wsp-374`, `$WSP_VERDICT`), when it was written in this pass.
    if let Some((from_file, written)) = crate::closing::verifier_args(store, args, me.as_deref()) {
        let code = verified_as(store, &from_file, me);
        if code == 0 {
            crate::closing::note_close(store, &args.rest[0], "verified", written, None);
        }
        return code;
    }
    verified_as(store, args, me)
}

/// [`cmd_verified`] as the seat `me`, which is how wsp closes for a verifier that
/// did not (`wsp-374`): the process is not the verifier's, so its pass is named.
///
/// **An alias of `wsp submit` for the verifier kind** (`wsp-614`): the verdict
/// it is given runs the same validator. Written as an exit (a `summary:` line
/// and its sections), it is read as one, with the word typed as its `verdict:`;
/// written as prose, its `checked:` lines are taken out as before and a
/// `cause:` line (or `--cause`) is taken out too. Either way `--blocks` with no
/// cause is refused, a hold with an item not held is refused, and `cause: env`
/// is no block: the pass is recorded `could not verify` and the member stays
/// at review.
pub fn verified_as(store: &Store, args: &Args, me: Option<String>) -> i32 {
    const USAGE: &str = "wsp verified <member> --holds|--blocks --from FILE --caption \"…\" [--cause \"work — …\"]   (or `-` for stdin)";
    let word = match (args.has("holds"), args.has("blocks")) {
        (true, false) => crate::exit::Verdict::Holds,
        (false, true) => crate::exit::Verdict::Blocks,
        _ => {
            eprintln!("usage: {USAGE}");
            eprintln!("       one of --holds or --blocks, and the verdict to go with it");
            return 2;
        }
    };
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let caption = match crate::caption::flag(args, false) {
        Ok(c) => c.filter(|c| !c.is_empty()),
        Err(code) => return code,
    };
    let text = match crate::cmd_task::prose_kept(args, USAGE) {
        Ok(t) => t,
        Err(code) => return code,
    };
    // An exit carries its own caption (its summary); prose needs one typed.
    let exit_form = crate::cmd_submit::looks_like_exit(&text);
    if caption.is_none() && !exit_form {
        return crate::caption::refuse(if word == crate::exit::Verdict::Holds { "holds" } else { "blocks" });
    }
    let member = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    // Another user leased the row while this spoke was offline: its verdict is
    // refused, and nothing is written (wsp-469, wsp-483).
    if let Some(why) = crate::hub_journal::refusal(store, &member.id, "verified") {
        eprintln!("{why}");
        return 1;
    }
    let cause = args.get("cause").filter(|c| c != "true");
    let read = match exit_form {
        true => crate::cmd_submit::verifier_exit(word, &text),
        false => crate::cmd_submit::verifier_prose(word, &text, cause.as_deref()),
    };
    let judged = match read {
        Ok(j) => j,
        Err(lines) => {
            for l in &lines {
                eprintln!("{l}");
            }
            eprintln!("wsp: {} — the verdict is refused ({} refusal(s)); nothing recorded. Fix it and give it again", member.id, lines.len());
            return 2;
        }
    };
    let mut judged = judged;
    if let Some(c) = caption.filter(|c| !(exit_form && c.starts_with("summary:"))) {
        judged.caption = c;
    }
    verdict_core(store, member, judged, me)
}

/// What [`verdict_core`] records: the pass's state, its text and caption, the
/// done-when items it checked, and — on a block from an exit — what is owed,
/// which becomes the `sent back:` line in place of the verdict's prose.
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub state: State,
    pub text: String,
    pub caption: String,
    pub checked: Vec<crate::did::Checked>,
    pub owed: Option<String>,
    /// A line for the member's log besides the pass: a `tree` block's defect.
    pub note: Option<String>,
}

/// A verdict once read and validated: the superseded case, the land a hold
/// does, the pass written, the member sent back on a block.
pub fn verdict_core(store: &Store, member: Task, judged: Judged, me: Option<String>) -> i32 {
    let Judged { state, text, caption, checked, owed: owes, note } = judged;
    // A row sent back while this verifier read it (a failed check, a person) is
    // not landed by the hold: the send-back owns the row until it returns to
    // review, where a fresh verifier reads what comes back.
    if state == State::Holds && member.status() == Status::Doing && crate::repair::ready(&member).is_some() {
        let said = format!("{}\n\n(not landed: {} was sent back to doing while this read it)", text.trim(), member.id);
        let _ = record_checked(store, &member.id, State::Superseded, &said, me.as_deref(), Some(&caption), checked, None);
        store.git_commit(&format!("wsp: verified {} superseded — sent back while read", member.id));
        eprintln!(
            "wsp: {} was sent back to doing while you read it, so your hold lands nothing and is recorded as superseded; \
             a fresh verifier reads it when it returns to review",
            member.id
        );
        return 1;
    }
    // A hold on a member that is ready and unlanded lands it, here, so a land
    // that cannot happen turns the hold into a block in the same write.
    let mut state = state;
    let mut text = text;
    let mut caption = caption;
    let mut owes = owes;
    let mut landing = None;
    let mut reran = Vec::new();
    let mut land_owes = None;
    // A hold on a row whose `## Checks` runs its own check is a block: the line
    // can never pass, and the barrier would spend a full suite finding that out.
    if state == State::Holds {
        let calls = crate::check::calls_itself_in(&member.id, &crate::check::task_steps(&member));
        if !calls.is_empty() {
            state = State::Blocks;
            caption = "a check line calls the row's own check".to_string();
            let why = format!("its ## Checks has line(s) that call its own check, which is always refused as already running:\n- {}\nFix them with `wsp check {} --set --why \"…\" -`.", calls.join("\n- "), member.id);
            text = format!("{}\n\n{why}", text.trim());
            owes = Some(why);
            println!("{}", Paint::new().red("■ a check line calls the row's own check, so the hold is recorded as a block"));
        }
    }
    if state == State::Holds && crate::repair::ready(&member).is_some() {
        match crate::cmd_checkout::land_verdict(store, &member.id) {
            crate::cmd_checkout::Verdict::Nothing => {}
            crate::cmd_checkout::Verdict::Landed { landed, reran: r } => {
                landing = Some(landed);
                reran = r;
            }
            // Not a block of the work (`wsp-605`): the verdict holds and the
            // land is owed, retried once the tree is clean.
            // `wsp-350`: the row lands by merge, so the hold stands and the
            // trunk is the reviewer's to move; the row lands when its PR does.
            crate::cmd_checkout::Verdict::ByMerge(why) => {
                println!("{}", Paint::new().yellow(&format!("■ the hold stands, and nothing is landed: {why}")));
            }
            crate::cmd_checkout::Verdict::Dirty(why) => {
                land_owes = Some(why);
                println!("{}", Paint::new().yellow("■ the hold stands, but the tree is dirty, so its land is owed; the run lands it once the tree is clean"));
            }
            crate::cmd_checkout::Verdict::Blocked(why) => {
                state = State::Blocks;
                caption = "the hold could not land the row".to_string();
                text = format!("{}\n\n{why}", text.trim());
                owes = None;
                println!("{}", Paint::new().red("■ the hold could not land the row, so it is recorded as a block"));
            }
        }
    }
    let recorded = record_checked(store, &member.id, state, &text, me.as_deref(), Some(&caption), checked, owes.as_deref());
    let pass = match recorded {
        Ok(p) => p,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };
    let pass = match &landing {
        Some(l) => note_landing(store, &member.id, l, &reran).unwrap_or(pass),
        None => pass,
    };
    if let Some(why) = &land_owes {
        set_owed(store, &member.id, Some(why), "the verifier's hold");
        let said = format!(
            "{}: the verifier held it, but its land is owed — the tree is dirty, and nothing reached the trunk.\n{why}",
            member.id
        );
        crate::repair::tell_member(store, &member.id, &said);
        tell_governor(store, &member.id, &said);
    }
    if state == State::Holds {
        fold_pieces(store, &member.id);
    }
    if let Some(line) = note.as_deref().filter(|_| state == State::Blocks) {
        store.locked(|| {
            let mut t = store.find_task(&member.id)?;
            crate::log::note(store, &mut t, line).ok()
        });
    }
    store.log_event(
        "task-verified",
        serde_json::json!({
            "id": member.id, "verdict": state.as_str(), "caption": caption, "read": pass.read, "pane": pass.pane,
            "landed": landing.as_ref().map(|l| l.sha.clone()),
            "owed": land_owes,
        }),
    );
    store.git_commit(&format!("wsp: verified {} {} — {}", member.id, state.as_str(), member.title));
    // A row run as legs has no one agent to tell while its rework leg carries the
    // verdict. With no rework leg (the row is at the leg cap) nothing does, so the
    // reason is delivered as for any member sent back.
    if state == State::Blocks {
        let reason = owes.clone().unwrap_or_else(|| owed(&text, &pass.checked));
        deliver_unless_reworked(store, &member.id, &reason);
    }
    crate::cycle::poke_task(store, &member.id, "verified");

    let p = Paint::new();
    let mark = match state {
        State::Holds => p.green("✓"),
        State::CouldNotVerify => p.yellow("■"),
        _ => p.red("■"),
    };
    let read = pass.read.as_deref().map(|r| format!(" at {r}")).unwrap_or_default();
    println!("{mark} {}  {}{read}  {}", p.bold(&member.id), state.as_str(), p.dim(&crate::caption::of_pass(state.as_str(), Some(&caption), "").text));
    match state {
        State::Blocks => println!("  {}", p.dim("sent back to doing with what is owed; its governor is told")),
        State::CouldNotVerify => println!(
            "  {}",
            p.dim(&format!("cause env: no block of the work — {} stays at review, and the run starts a new verifier", member.id))
        ),
        _ => {}
    }
    if pass.checked.is_empty() && state != State::CouldNotVerify {
        println!("  {}", p.dim("no `checked:` lines — one per done-when item you looked at, ending in held or not held, is what `wsp verifications` counts"));
    }
    0
}

/// The newest pass on a row when it is a hold that owes its land (`wsp-605`):
/// the verdict stands, the tree was dirty when it tried, and the row is still
/// ready and unlanded. No verifier is owed for it — only the land.
pub fn land_owed(t: &Task) -> Option<Pass> {
    let p = passes(t).pop()?;
    (p.state == State::Holds && p.owed.is_some() && crate::repair::ready(t).is_some()).then_some(p)
}

/// The newest pass's `owed` clause set or cleared, and the row's log told why.
fn set_owed(store: &Store, member: &str, owed: Option<&str>, by: &str) {
    store.locked(|| {
        let mut t = store.find_task(member)?;
        let mut ps = passes(&t);
        let last = ps.last_mut()?;
        last.owed = owed.map(str::to_string);
        write(&mut t, &ps);
        let line = match owed {
            Some(why) => format!("wsp: the hold's land is owed ({by}): {}", util::truncate(&crate::util::fold(why), 400)),
            None => format!("wsp: the hold's owed land is settled ({by})"),
        };
        crate::log::note(store, &mut t, &line).ok()
    });
}

/// Tell whoever governs the list a row is on; nothing for a row on no list.
pub fn tell_governor(store: &Store, member: &str, text: &str) {
    if let Some(w) = store.find_task(member).and_then(|t| crate::cycle::list_of(store, &t)) {
        crate::cycle::tell(store, &w, text);
    }
}

/// What a retry of an owed land came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Owed {
    /// It reached the trunk at this commit, or `None` when nothing was left to land.
    Landed(Option<String>),
    /// Refused again on a dirty tree: still owed, said on the row and to the governor.
    Still(String),
    /// The row lands by merge (`wsp-350`), so no land is owed by wsp: the owed
    /// mark is cleared and the row waits for its PR. The text names the PR.
    ByMerge(String),
    /// Refused for a reason the member owes work for (a conflict, a red
    /// rerun): recorded as the block it now is, and the row sent back.
    Blocked(String),
}

/// The land a hold owes, tried again with the recorded hold and no new
/// verifier (`wsp-605`) — `by` is what brought it ("reviewed again", "the tree
/// is clean"). It reruns only what a land reruns after a rebase.
///
/// `None` when nothing is owed, or the tree is no longer at the commit the
/// hold read: new commits are work no verifier has read, and their review
/// opens a pass as any landing does.
pub fn retry_owed(store: &Store, member: &str, by: &str) -> Option<Owed> {
    let t = store.find_task(member)?;
    let pass = land_owed(&t)?;
    let ready = crate::repair::ready(&t)?;
    let tree = crate::cmd_checkout::member_tree(store, member)?;
    let head = wsp_base::calls::Command::new("git").arg("-C").arg(&tree).args(["rev-parse", "HEAD"]).output().ok()?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if !same_commit(&head, &ready) {
        return None;
    }
    let out = match crate::cmd_checkout::land_verdict(store, member) {
        crate::cmd_checkout::Verdict::Nothing => {
            set_owed(store, member, None, by);
            Owed::Landed(None)
        }
        crate::cmd_checkout::Verdict::Landed { landed, reran } => {
            set_owed(store, member, None, by);
            note_landing(store, member, &landed, &reran);
            fold_pieces(store, member);
            store.log_event("task-verified", serde_json::json!({ "id": member, "verdict": "holds", "landed": landed.sha, "owed": "landed", "by": by }));
            Owed::Landed(Some(landed.sha))
        }
        crate::cmd_checkout::Verdict::ByMerge(why) => {
            set_owed(store, member, None, by);
            Owed::ByMerge(why)
        }
        crate::cmd_checkout::Verdict::Dirty(why) => {
            set_owed(store, member, Some(&why), &format!("{by}, and refused again"));
            tell_governor(
                store,
                member,
                &format!("{member}: its hold is still owed a land — {by}, and the tree is still dirty, so nothing reached the trunk.\n{why}"),
            );
            Owed::Still(why)
        }
        crate::cmd_checkout::Verdict::Blocked(why) => {
            store.locked(|| {
                let mut t = store.find_task(member)?;
                let mut ps = passes(&t);
                let last = ps.last_mut()?;
                let now = util::now_iso();
                last.state = State::Blocks;
                last.owed = None;
                last.decided = Some(now.clone());
                last.caption = Some("the hold could not land the row".into());
                last.text = format!("{}\n\n{why}", last.text.trim());
                write(&mut t, &ps);
                send_back(&mut t, now, "the hold's owed land was refused", pass.read.as_deref(), &why, None);
                store.save_task(&t).ok()
            });
            deliver_unless_reworked(store, member, &why);
            tell_governor(store, member, &format!("{member}: its owed land was refused ({by}), so the hold is recorded as a block and it is sent back.\n{why}"));
            Owed::Blocked(why)
        }
    };
    store.git_commit(&format!("wsp: {member} owed land — {by}"));
    crate::cycle::poke_task(store, member, "verified");
    Some(out)
}

/// A hold is the end of a row's pieces: `## Pieces` folds into the log, one line
/// a piece, and the section goes (`docs/pieces.md`, *Transience*).
pub fn fold_pieces(store: &Store, member: &str) {
    store.locked(|| {
        let mut t = store.find_task(member)?;
        if !crate::pieces::staged(&t) {
            return None;
        }
        let ran: Vec<crate::pieces::PieceRan> = crate::attempt::attempts_of(&t)
            .into_iter()
            .filter(|a| !a.until.is_empty())
            .filter_map(|a| {
                let n = a.leg.as_deref()?.strip_prefix("piece ")?.parse().ok()?;
                Some(crate::pieces::PieceRan { n, ran: a.ran, turns: a.turns })
            })
            .collect();
        crate::pieces::fold_at_land(&mut t, &ran);
        store.save_task(&t).ok()
    });
}

/// After a hold landed the row: the newest pass gets the `landed` clause when
/// the commit that reached the trunk is not the one it read, and the row's log
/// says what reached the trunk and what was rerun for it.
fn note_landing(store: &Store, member: &str, l: &crate::cmd_checkout::Landed, reran: &[String]) -> Option<Pass> {
    store.locked(|| {
        let mut t = store.find_task(member)?;
        let mut ps = passes(&t);
        let last = ps.last_mut()?;
        if last.own().is_none_or(|r| !same_commit(r, &l.sha)) {
            last.landed = Some(short(&l.sha));
        }
        let pass = last.clone();
        write(&mut t, &ps);
        let again = if reran.is_empty() { String::new() } else { format!(" · reran {} after the trunk moved, green", reran.join(", ")) };
        crate::log::note(store, &mut t, &format!("wsp: the verifier's hold landed {} commit(s) at {}{again}", l.commits.len(), short(&l.sha))).ok()?;
        Some(pass)
    })
}

/// A blocking verdict as what is owed: its prose, and the items it says did not
/// hold, which left the prose when they became `checked` lines.
pub fn owed(text: &str, checked: &[crate::did::Checked]) -> String {
    let not_held: Vec<String> = checked.iter().filter(|c| !c.held).map(|c| format!("- {}", c.item)).collect();
    if not_held.is_empty() {
        return text.trim().to_string();
    }
    format!("{}\n\nNot held:\n{}", text.trim(), not_held.join("\n"))
}

/// A blocking verdict's reason, told to the member's pane unless a rework leg
/// carries it.
pub fn deliver_unless_reworked(store: &Store, member: &str, text: &str) {
    let reworked = store.find_task(member).is_some_and(|t| crate::legs::rework_open(&crate::legs::legs(&t)));
    if !reworked {
        crate::cmd_task::deliver_reason(store, member, text.trim());
    }
    tell_if_capped(store, member, text);
}

/// What a row's log says once whoever decides a cap stall has been told: this,
/// who, and the verdict's first line (`wsp-690`).
pub const STALL_TOLD: &str = "wsp: stalled at the cap, told";

/// The cap a row sent back could not carry, when that is where it stands: at
/// `doing`, nothing of its legs (or pieces) pending or running, and the last
/// send-back on its log refused its rework (`legs::rework_by`, or `pieces`'
/// count) with no leg or piece appended since. `None` once anything moves it —
/// a leg added by hand, the verdict decided, the row split and closed.
///
/// The run starts nothing for such a row and seats nobody on it, so unless
/// somebody is told it waits forever: `compounding-760`, 14 hours on
/// 2026-11-19 behind a governor that believed a rework leg was running.
pub fn stalled_at_cap(t: &Task) -> Option<String> {
    if t.status() != Status::Doing {
        return None;
    }
    let legs = crate::legs::legs(t);
    let rec = crate::pieces::record(t);
    let cap = if !legs.is_empty() {
        if legs.iter().any(|l| matches!(l.state, crate::legs::State::Pending | crate::legs::State::Running)) {
            return None;
        }
        format!("{}-leg cap", crate::legs::MAX_LEGS)
    } else if !rec.pieces.is_empty() {
        if rec.pieces.iter().any(|p| matches!(p.state, crate::pieces::State::Pending | crate::pieces::State::Running)) {
            return None;
        }
        format!("{}-piece cap", crate::pieces::MAX_PIECES)
    } else {
        return None;
    };
    let log = t.section("Log").unwrap_or_default();
    let mut refused = false;
    for l in log.lines() {
        if l.contains("no rework leg was added") || (l.contains("wsp: ") && l.contains("pieces is over the")) {
            refused = true;
        } else if l.contains("wsp: appended leg") || l.contains("wsp: appended piece") {
            refused = false;
        }
    }
    refused.then_some(cap)
}

/// Whether a stall tell is on the log since the row last moved (a leg or
/// piece appended) — for this verdict's first line, or any with `None`.
fn told_since_moved(t: &Task, first: Option<&str>) -> bool {
    let mut told = false;
    for l in t.section("Log").unwrap_or_default().lines() {
        if l.contains("wsp: appended leg") || l.contains("wsp: appended piece") {
            told = false;
        } else if l.contains(STALL_TOLD) && first.is_none_or(|f| l.trim_end().ends_with(&format!("— {f}"))) {
            told = true;
        }
    }
    told
}

/// A row stalled at the cap whose decider has been told so: what the verdict
/// tell at the run's advance stays quiet on, so a governor hears it once.
pub fn stall_told(t: &Task) -> bool {
    stalled_at_cap(t).is_some() && told_since_moved(t, None)
}

/// The first line of a verdict, folded and short: what a stall tell names it by.
fn first_line(verdict: &str) -> String {
    let line = verdict.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("(no verdict text)");
    util::truncate(&crate::util::fold(line), 160)
}

/// A send-back the row could not carry, said to whoever decides it — the
/// list's governor through [`tell_governor`] (the spool, so a busy seat has it
/// on its next turn), or for a row on no list a hand raised with
/// [`crate::cycle::notice_on_row`], which reaches its project's custodian or else
/// the person. Before `wsp-690` the refusal was only logged on the row, and
/// nobody reads a row nothing is running.
///
/// Once per verdict: a second send-back with the same first line, and nothing
/// appended between, tells nobody again. Returns whether it told.
pub fn tell_if_capped(store: &Store, member: &str, verdict: &str) -> bool {
    let Some(t) = store.find_task(member) else { return false };
    let Some(cap) = stalled_at_cap(&t) else { return false };
    let first = first_line(verdict);
    if told_since_moved(&t, Some(&first)) {
        return false;
    }
    let what = if cap.contains("-leg") { "leg" } else { "piece" };
    let text = format!(
        "{member} ({}) was sent back at the {cap}, so NO rework {what} was added and the run will start nothing for it: \
         it waits on you. The verdict's first line: {first}\n\
         `wsp show {member}` has all of it. Your choices: split the row into rows of its own; \
         add a {what} by hand (`wsp leg add {member} <name> --order FILE`); \
         or decide the verdict — `wsp verified {member} --holds --from FILE --caption \"…\"` if the finding is wrong, \
         `wsp decide {member} \"…\"` to answer it.",
        util::truncate(&t.title, 60)
    );
    let whom = match crate::cycle::list_of(store, &t) {
        Some(w) => {
            crate::cycle::tell(store, &w, &text);
            format!("the {} governor", w.id)
        }
        None => {
            if !crate::cycle::notice_on_row(store, member, &text) {
                return false;
            }
            "a hand on the row".to_string()
        }
    };
    store.locked(|| {
        let mut t = store.find_task(member)?;
        t.log(&format!("{STALL_TOLD} {whom} — {first}"));
        store.save_task(&t).ok()
    });
    store.git_commit(&format!("wsp: {member} stalled at the {cap}, {whom} told"));
    true
}

/// The store half of [`verified`]: the pass written, and the member moved when
/// the verdict blocks. Separate so the choice of pass can be tested without a
/// process environment to read a pane out of.
/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["verified"],
        run: cmd_verified,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 160,
                text: r##"  wsp verified <member> --holds|--blocks --from FILE --caption "…"
                                    a verifier's verdict, recorded on the member
                                    under ## Verification. --blocks sends it back
                                    with the verdict as what is owed, and its
                                    governor is told; a governor answering a
                                    block by hand writes --holds in their name.
                                    --caption is required: the reason in under
                                    60, read after the holds/blocks wsp adds"##,
            },
        ],
    },
];

#[cfg(test)]
pub fn record(store: &Store, member: &str, state: State, text: &str, me: Option<&str>, caption: Option<&str>) -> Result<Pass, String> {
    record_checked(store, member, state, text, me, caption, Vec::new(), None)
}

/// [`record`] with the done-when items the verifier checked (`wsp-325`), and on
/// a block the text that is owed when it is not the verdict's own (`wsp-614`:
/// an exit's `owed:` lines are the `sent back:` reason).
#[allow(clippy::too_many_arguments)]
pub fn record_checked(
    store: &Store,
    member: &str,
    state: State,
    text: &str,
    me: Option<&str>,
    caption: Option<&str>,
    checked: Vec<crate::did::Checked>,
    owes: Option<&str>,
) -> Result<Pass, String> {
    store.locked(|| {
        let Some(mut t) = store.find_task(member) else { return Err(format!("no task {member}")) };
        let mut ps = passes(&t);
        let mine = |p: &Pass| me.is_some() && p.pane.as_deref() == me;
        if ps.iter().any(|p| p.state == State::Superseded && mine(p)) && !ps.iter().any(|p| p.state == State::Running && mine(p)) {
            return Err(format!(
                "your pass on {member} was superseded by a newer landing, and a fresh verifier is reading that — nothing recorded"
            ));
        }
        let at = ps
            .iter()
            .rposition(|p| p.state == State::Running && mine(p))
            .or_else(|| me.is_none().then(|| ps.iter().rposition(|p| p.state == State::Running)).flatten());
        let at = match at {
            Some(i) => i,
            None => {
                let mut p = Pass::opened(crate::repair::commit_of(&t), None);
                p.pane = me.map(str::to_string);
                // Nothing to end: this pass was written by somebody already
                // standing somewhere, not by an agent wsp started for it.
                p.ending = Ending::Ended;
                ps.push(p);
                ps.len() - 1
            }
        };
        // One instant for the verdict and for the block's own move of the
        // member: a re-verify of work with no landing asks whether the member
        // moved *after* the verdict, and two clock reads a second apart would
        // count the block itself as the member moving.
        let now = util::now_iso();
        ps[at].state = state;
        ps[at].decided = Some(now.clone());
        ps[at].text = text.trim().to_string();
        ps[at].caption = caption.map(str::to_string);
        ps[at].checked = checked;
        let pass = ps[at].clone();
        write(&mut t, &ps);
        if state == State::Blocks {
            let reason = owes.map(str::to_string).unwrap_or_else(|| owed(text, &pass.checked));
            send_back(&mut t, now, "the verifier blocked", pass.read.as_deref(), &reason, None);
        }
        if state == State::CouldNotVerify {
            t.log(&format!(
                "could not verify{}: the verifier's environment, not the work — {} stays at review and a new verifier is owed",
                pass.read.as_deref().map(|r| format!(" at {r}")).unwrap_or_default(),
                t.id
            ));
        }
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok(pass)
    })
}

/// A member sent back to `doing` with `text` as what is owed: the move, the
/// `sent back:` line, and for a row run as legs the rework leg that carries it.
/// `by` is who sent it ("the verifier blocked", "wsp check failed") and `now`
/// the one instant the caller also wrote its entry with.
pub fn send_back(t: &mut Task, now: String, by: &str, read: Option<&str>, text: &str, rework_head: Option<&str>) {
    t.set_status(Status::Doing);
    t.updated = now;
    t.log(&format!(
        "{} {by}{}: {} — the whole verdict is under ## Verification",
        crate::cycle::SENT_BACK,
        read.map(|r| format!(" at {r}")).unwrap_or_default(),
        util::truncate(&crate::util::fold(text), 300)
    ));
    // A row run as legs is sent back to a leg of its own: one `rework`
    // leg after the last, whose order is the verdict (`docs/legs.md`,
    // *The run*). The step starts it; nobody is told to go back to work.
    let rec = crate::pieces::record(t);
    if !rec.pieces.is_empty() {
        let head = rework_head.unwrap_or("The verifier blocked this row. Its verdict, which is what is owed:\n\n");
        match crate::pieces::rework_by(&rec, &t.id.clone(), text, head, Status::Doing) {
            Ok(Some(after)) => {
                for p in after.pieces.iter().skip(rec.pieces.len()) {
                    t.log(&format!(
                        "wsp: appended piece {} in batch {}{}",
                        p.n,
                        p.batch,
                        if crate::legs::is_default(&p.policy) { String::new() } else { format!(" · on {}", p.policy.label()) },
                    ));
                }
                crate::pieces::write(t, &after);
            }
            Ok(None) => {}
            Err(why) => t.log(&format!("wsp: {why}")),
        }
    }
    let before = crate::legs::legs(t);
    if !before.is_empty() {
        let reworked = match rework_head {
            Some(head) => crate::legs::rework_by(&before, text, head),
            None => crate::legs::rework(&before, text),
        };
        match reworked {
            Ok(Some(after)) => {
                let leg = after.last().expect("a rework leg was appended");
                let of = leg.rework.as_deref().unwrap_or_default();
                t.log(&format!(
                    "wsp: appended leg {} after {}{}{}",
                    leg.name,
                    crate::legs::waits(&after).last().map(|w| w.join(", ")).unwrap_or_default(),
                    if of.is_empty() { String::new() } else { format!(" · the verdict names {}", of.join(", ")) },
                    if crate::legs::is_default(&leg.policy) { String::new() } else { format!(" · on {}", leg.policy.label()) },
                ));
                crate::legs::write(t, &after);
            }
            Ok(None) => {}
            Err(why) => t.log(&format!("wsp: {why}")),
        }
    }
}

/// `wsp migrate --verify-rows [-n]` — every Verify row wsp filed, moved onto
/// its parent's `## Verification` and archived. A one-off: once the store has
/// none, it says so and does nothing.
///
/// **Identified by what wsp wrote, never by title** ([`filed_by_wsp`]). Rows a
/// person made to review something — `compound-291`, `wsp-137` — are listed
/// separately and left exactly as they are.
///
/// **A row still holding a claim is left** until its verifier finishes: moving
/// it would take the record out from under an agent that is about to write to
/// it.
///
/// **Old ids keep resolving.** The row is archived, not deleted, and `wsp show`
/// reads the archive — so a log or a handover that cites `wsp-174` still finds
/// it, and the entry on the parent names it with `was`.
pub fn migrate(store: &Store, args: &Args) -> i32 {
    let dry = args.has("n") || args.has("dry-run");
    let p = Paint::new();
    let plan = plan(store);
    for (parent, rows) in &plan.moves {
        println!("{}  {} pass(es) from {}", p.bold(parent), rows.len(), rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(" "));
    }
    if !plan.running.is_empty() {
        println!("{}", p.yellow(&format!("left running: {} — still holding a claim", plan.running.join(" "))));
    }
    if !plan.orphans.is_empty() {
        println!("{}", p.yellow(&format!("left alone: {} — the parent is not a live row", plan.orphans.join(" "))));
    }
    if !plan.by_hand.is_empty() {
        println!("{}", p.dim(&format!("made by hand, left alone: {}", plan.by_hand.join(" "))));
    }
    let n: usize = plan.moves.values().map(Vec::len).sum();
    println!("\n{n} Verify row(s) onto {} member(s)", plan.moves.len());
    if n == 0 {
        return 0;
    }
    if dry {
        println!("{}", p.dim("nothing written — drop -n to apply"));
        return 0;
    }
    match apply(store, &plan) {
        Ok(moved) => {
            store.git_commit(&format!("wsp: {moved} Verify row(s) moved onto their members' ## Verification"));
            println!("moved {moved}");
            0
        }
        Err(e) => {
            eprintln!("wsp: {e}");
            1
        }
    }
}

/// What [`migrate`] would do.
pub struct Plan {
    /// parent id -> the rows whose passes go onto it, oldest first.
    pub moves: BTreeMap<String, Vec<Task>>,
    pub running: Vec<String>,
    pub orphans: Vec<String>,
    pub by_hand: Vec<String>,
}

pub fn plan(store: &Store) -> Plan {
    let tasks = store.tasks_shared();
    let claims = store.claims();
    let mut plan = Plan { moves: BTreeMap::new(), running: Vec::new(), orphans: Vec::new(), by_hand: Vec::new() };
    for t in tasks.iter() {
        if !filed_by_wsp(t) {
            if made_by_hand(t) {
                plan.by_hand.push(t.id.clone());
            }
            continue;
        }
        if claims.contains_key(&t.id) {
            plan.running.push(t.id.clone());
            continue;
        }
        let parent = t.parent.clone().unwrap_or_default();
        if !tasks.iter().any(|x| x.id == parent) {
            plan.orphans.push(t.id.clone());
            continue;
        }
        plan.moves.entry(parent).or_default().push(t.clone());
    }
    for rows in plan.moves.values_mut() {
        rows.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    }
    plan
}

/// A row somebody wrote to review or verify something, which the migration
/// names so a reader can see it was considered and left. The title's opening
/// words and not a word anywhere in it: a row *about* verification — this one,
/// `wsp-193` — is not a review of anything.
fn made_by_hand(t: &Task) -> bool {
    let title = t.title.to_ascii_lowercase();
    let barrier = title.starts_with("barrier") && (title.contains("review") || title.contains("re-check"));
    t.tags.iter().any(|g| g == VERIFY_TAG) || title.starts_with("verify ") || title.starts_with("re-verify ") || barrier
}

fn apply(store: &Store, plan: &Plan) -> Result<usize, String> {
    let mut moved = 0;
    for (parent, rows) in &plan.moves {
        let wrote = store.locked(|| -> Result<(), String> {
            let mut m = store.find_task(parent).ok_or_else(|| format!("{parent} went away"))?;
            let mut ps = passes(&m);
            for row in rows {
                if ps.iter().any(|p| p.was.as_deref() == Some(row.id.as_str())) {
                    continue;
                }
                ps.push(legacy(row));
            }
            write(&mut m, &ps);
            store.save_task(&m).map_err(|e| format!("{parent}: {e}"))
        });
        wrote?;
        for row in rows {
            store.archive_task(row).map_err(|e| format!("{}: {e}", row.id))?;
            moved += 1;
        }
    }
    Ok(moved)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn scratch(tag: &str) -> (util::Isolated, Store) {
        crate::testkit::isolated_store(&format!("verification-{tag}"))
    }

    /// An entry is read back as it was written — every clause, the ending, and
    /// a verdict that keeps its lines — and an entry with a clause nobody wrote
    /// reads that clause as absent rather than as its neighbour.
    #[test]
    fn a_pass_reads_back_as_it_was_written_whatever_clauses_it_carries() {
        let mut t = Task::new("a member", "m-1");
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-10-06 claimed\n".into();
        let full = Pass {
            at: "2026-10-06T10:00:00Z".into(),
            state: State::Blocks,
            read: Some("55c4839abcde".into()),
            ready: Some("66bb1111cccc".into()),
            from: Some("https://github.com/o/r/pull/7".into()),
            landed: Some("77aa0000bbbb".into()),
            pane: Some("cpd-12".into()),
            on: Some("claude opus/high".into()),
            was: Some("wsp-174".into()),
            ending: Ending::Failed("cpd-12 is still standing".into()),
            decided: Some("2026-10-06T10:20:00Z".into()),
            text: "the second half does not hold\n1. spool first\n2. then deliver".into(),
            // A ` · ` of its own, which must not end the caption at it.
            caption: Some("spool first · then deliver".into()),
            spent: Some(Spent {
                ran: "sonnet-5-5/high".into(),
                turns: 7,
                peak: 70_000,
                spend: crate::agent_kind::Spend { cache: 4_000_000, input: 200_000, output: 10_000, micros: None },
                model_secs: 41,
                tool_secs: 23,
                test_secs: 17,
                first_line: Some("2026-10-06T10:01:07Z".into()),
                load1: Some(108),
            }),
            did: None,
            checked: Vec::new(),
            owed: None,
        };
        let bare = Pass { at: "2026-10-06T11:00:00Z".into(), ..Pass::opened(None, None) };
        write(&mut t, &[bare.clone(), full.clone()]);
        assert_eq!(passes(&t), vec![full, bare], "oldest first, whichever order they were handed in");
        assert!(t.body.find("## Verification").unwrap() < t.body.find("## Log").unwrap(), "and the log stays last");
        assert!(t.body.contains("- 2026-10-06T10:00:00Z blocks · read 55c4839abcde · ready 66bb1111cccc · from https://github.com/o/r/pull/7 · landed 77aa0000bbbb · pane cpd-12"), "{}", t.body);
    }

    /// A verifier's usage lands on its pass (`compounding-104`): the load it
    /// started under is kept through the seat's end, and the seconds come back.
    #[test]
    fn a_passes_load_and_seconds_round_trip_and_the_load_survives_the_end() {
        let mut t = Task::new("t", "t-1");
        let start = Spent { load1: Some(108), ..Spent::default() };
        let ended = Spent::ending(Some(&start), None, Some((41, 23))).unwrap();
        assert_eq!((ended.model_secs, ended.tool_secs, ended.load1), (41, 23, Some(108)), "no session to read still keeps the load");
        assert_eq!(Spent::ending(None, None, None), None, "nothing known writes nothing");
        let kept = Spent::with_session(Some(ended.clone()), Some((17, "2026-10-06T10:01:07Z".into()))).unwrap();
        assert_eq!((kept.test_secs, kept.first_line.as_deref(), kept.load1), (17, Some("2026-10-06T10:01:07Z"), Some(108)), "the transcript's two figures join the reading");
        assert_eq!(Spent::with_session(None, None), None, "an unread transcript adds nothing");
        let p = Pass { at: "2026-11-06T10:00:00Z".into(), pane: Some("cpd-1".into()), spent: Some(ended.clone()), ..Pass::opened(None, None) };
        write(&mut t, &[p]);
        assert!(t.body.contains("model 41s · tools 23s · load1 1.08"), "{}", t.body);
        assert_eq!(passes(&t)[0].spent, Some(ended));
    }

    /// The lines wsp wrote sit under the verdict and come back as fields: the
    /// derived three and the verifier's own, and the verdict's prose is what it
    /// was — including a verdict that ends in a line that looks like one of ours.
    #[test]
    fn the_lines_a_pass_carries_come_back_as_fields_and_the_prose_is_untouched() {
        let mut t = Task::new("a member", "m-1");
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-10-06 claimed\n".into();
        let mut d = crate::did::Did::default();
        d.absorb("ran", "cargo test check:: · failed → passed");
        d.absorb("ran", "wsp check m-1 · passed");
        d.absorb("drove", "ctl · 3 commands");
        d.absorb("read", "src/a.rs:1-40 90-, README.md · diff 9e1c..55c4");
        let checked = vec![
            crate::did::Checked { item: "the gate holds the verifier".into(), held: true },
            crate::did::Checked { item: "compound has a declaration · yet".into(), held: false },
        ];
        for text in ["It holds.\n\nthe prose\nof two paragraphs", "It holds.\n- **ran:** cargo test, I wrote this myself"] {
            let p = Pass {
                at: "2026-10-06T10:00:00Z".into(),
                state: State::Holds,
                text: text.into(),
                did: Some(d.clone()),
                checked: checked.clone(),
                ..Pass::opened(None, None)
            };
            write(&mut t, &[p.clone()]);
            assert_eq!(passes(&t), vec![p], "{}", t.body);
            assert!(t.body.contains("\n  - **ran:** cargo test check:: · failed → passed\n"), "{}", t.body);
        }
        let none = Pass { did: None, checked: checked.clone(), ..Pass::opened(None, None) };
        write(&mut t, &[none.clone()]);
        assert_eq!(passes(&t), vec![none], "an entry never read has no ran line and says nothing about it");
    }

    /// A blocking verdict sent back carries what did not hold, which left its
    /// prose when it became a `checked` line.
    #[test]
    fn what_did_not_hold_is_still_what_is_owed() {
        let held = crate::did::Checked { item: "a".into(), held: true };
        let not = crate::did::Checked { item: "the replay covers 20 rows".into(), held: false };
        assert_eq!(owed("Blocks.", &[held.clone()]), "Blocks.");
        assert_eq!(owed("Blocks.\n", &[held, not]), "Blocks.\n\nNot held:\n- the replay covers 20 rows");
    }

    /// A landing records forty characters and an entry twelve: the same commit
    /// either way round, and never on fewer characters than git's own seven.
    #[test]
    fn a_commit_is_the_same_commit_at_either_length_and_never_on_a_stub() {
        assert!(same_commit("55c4839abcde", "55c4839abcdef0123456789"));
        assert!(same_commit("55c4839abcdef0123456789", "55c4839"));
        assert!(!same_commit("55c4839abcde", "55c4839abcdf"));
        assert!(!same_commit("55c", "55c4839"), "three characters name nothing");
    }

    /// A Verify row filed before `wsp-188`, the way wsp filed them.
    pub fn row(store: &Store, id: &str, parent: &str, created: &str, status: Status, verdict: &str) -> Task {
        let mut t = Task::new(&format!("Verify {parent}: the work"), id);
        t.parent = Some(parent.into());
        t.tags = vec![VERIFY_TAG.into()];
        t.created = created.into();
        t.status_raw = status.as_str().into();
        t.body = format!(
            "## Overview\nVerify **{parent}** (the work), a member of group 4 of the `run` worklist. \
             wsp spawned you when {parent} reached review and landed; nobody else is going to read this work.\n\n\
             ## Log\n- 2026-10-05 wsp: reading cf90cf59556955ce7bc774eb1bbe3c30f0895003\n\
             - 2026-10-05T11:40:00Z claimed by pane cpd-{n} · spawned at opus/high\n\
             - 2026-10-05 wsp: started by this run run group 4\n\
             - 2026-10-05T12:00:00Z {verdict}\n",
            n = &id[id.len() - 3..],
        );
        store.save_task(&t).unwrap();
        t
    }

    /// Ed, 2026-10-06: every Verify row wsp filed moves onto its parent's
    /// `## Verification`, oldest first, and is archived. Shaped on wsp-148,
    /// which had five — four blocks and a holds. Identified by what wsp wrote:
    /// a review somebody made by hand, tagged or titled like one, is named and
    /// left; a verifier still holding a claim is left until it finishes. And
    /// the old ids go on resolving, because logs and handovers cite them.
    #[test]
    fn the_migration_moves_every_row_wsp_filed_onto_its_parent_and_leaves_the_rest() {
        let (_env, store) = scratch("migrate");
        let mut m = Task::new("An empty or dead governor seat is filled", "wsp-148");
        m.status_raw = Status::Review.as_str().into();
        m.body = "## Overview\nthe member\n\n## Log\n- 2026-10-05 claimed\n".into();
        store.save_task(&m).unwrap();
        for (id, at, status, verdict) in [
            ("wsp-174", "2026-10-05T11:38:52Z", Status::Blocked, "blocked: wake::say must spool first"),
            ("wsp-175", "2026-10-05T15:00:00Z", Status::Blocked, "blocked: the reseat races the vacate"),
            ("wsp-178", "2026-10-05T18:00:00Z", Status::Blocked, "blocked: unseated reads as nothing"),
            ("wsp-179", "2026-10-05T21:00:00Z", Status::Blocked, "blocked: one seat filled twice"),
            ("wsp-183", "2026-10-06T07:00:00Z", Status::Review, "review: wsp-148 holds at 55c4839"),
        ] {
            row(&store, id, "wsp-148", at, status, verdict);
        }
        // Made by hand: a re-verify a person filed under the member, untagged,
        // and a barrier re-check tagged `verify` with no parent.
        let mut by_hand = Task::new("Re-verify wsp-148 at 7a0cf7d: the fixes, against the code", "wsp-137");
        by_hand.parent = Some("wsp-148".into());
        store.save_task(&by_hand).unwrap();
        let mut recheck = Task::new("Barrier re-check: round-2 group 3, after the hold", "compound-330");
        recheck.tags = vec![VERIFY_TAG.into()];
        store.save_task(&recheck).unwrap();
        // And a row *about* verification, which reviews nothing.
        store.save_task(&Task::new("A verification is a pass on the member's own row", "wsp-188")).unwrap();
        // Still running: claimed, its verdict not in.
        row(&store, "wsp-200", "wsp-148", "2026-10-06T09:00:00Z", Status::Doing, "note: reading");
        store.set_claim("wsp-200", serde_json::json!({ "workspace": "w" }));

        let planned = plan(&store);
        let named: Vec<&str> = planned.moves["wsp-148"].iter().map(|t| t.id.as_str()).collect();
        assert_eq!(named, ["wsp-174", "wsp-175", "wsp-178", "wsp-179", "wsp-183"], "every row wsp filed, oldest first");
        assert_eq!(planned.moves.len(), 1);
        assert_eq!(planned.running, ["wsp-200"], "the one still holding a claim is left until it finishes");
        let mut hand = planned.by_hand.clone();
        hand.sort();
        assert_eq!(hand, ["compound-330", "wsp-137"], "made by hand: named, and not among the moves");

        // The dry run writes nothing.
        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("verify-rows", "true"), ("n", "true")])), 0);
        assert!(store.find_task("wsp-174").is_some() && passes(&store.find_task("wsp-148").unwrap()).is_empty());

        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("verify-rows", "true")])), 0);
        let ps = passes(&store.find_task("wsp-148").unwrap());
        assert_eq!(
            ps.iter().map(|p| (p.was.as_deref().unwrap(), p.state)).collect::<Vec<_>>(),
            [
                ("wsp-174", State::Blocks),
                ("wsp-175", State::Blocks),
                ("wsp-178", State::Blocks),
                ("wsp-179", State::Blocks),
                ("wsp-183", State::Holds)
            ],
            "all five passes on wsp-148, oldest first"
        );
        assert_eq!(ps[0].read.as_deref(), Some("cf90cf595569"), "the commit it read");
        assert_eq!(ps[0].pane.as_deref(), Some("cpd-174"), "the pane");
        assert_eq!(ps[0].on.as_deref(), Some("opus/high"), "the model");
        assert_eq!(ps[0].text, "wake::say must spool first", "its block text");
        assert_eq!(ps[4].text, "wsp-148 holds at 55c4839", "and its review text");
        let newest = latest(&store.tasks(), "wsp-148").unwrap();
        assert_eq!(
            (newest.was.as_deref(), newest.state),
            (Some("wsp-200"), State::Running),
            "the row still running is still the newest pass, read off the row it is on"
        );

        // Old ids resolve from the archive, and `wsp show` prints them.
        assert!(store.find_task("wsp-174").is_none(), "archived out of the live store");
        assert!(matches!(store.resolve_task("wsp-174"), crate::store::Found::Archived(..)));
        assert_eq!(crate::cmd_task::show(&store, &Args::synth("show", &["wsp-174"], &[])), 0);
        // The rows left alone are where they were.
        for id in ["wsp-137", "compound-330", "wsp-200"] {
            assert!(store.find_task(id).is_some(), "{id} was not wsp's to move");
        }
        // And running it again moves nothing and duplicates nothing.
        store.clear_claim("wsp-200");
        assert_eq!(plan(&store).moves["wsp-148"].len(), 1, "only the one that has since finished");
        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("verify-rows", "true")])), 0);
        assert_eq!(passes(&store.find_task("wsp-148").unwrap()).len(), 6);
    }

    /// Until the migration has run, a Verify row is read as the pass it
    /// recorded — so a binary installed first reads the barrier exactly as the
    /// rows did, and opens nothing because a row looked like no verdict.
    #[test]
    fn an_unmigrated_verify_row_is_read_as_its_pass() {
        let (_env, store) = scratch("legacy");
        let mut m = Task::new("a member", "m-1");
        m.status_raw = Status::Review.as_str().into();
        store.save_task(&m).unwrap();
        row(&store, "v-001", "m-1", "2026-10-05T11:00:00Z", Status::Blocked, "blocked: no");
        assert!(blocked(&store.tasks(), "m-1").is_some_and(|p| p.was.as_deref() == Some("v-001")));
        row(&store, "v-002", "m-1", "2026-10-05T12:00:00Z", Status::Review, "review: yes");
        assert!(verified(&store.tasks(), "m-1"), "the newest row is the verdict");
        // A pass on the row itself, newer than both, is newer than both.
        let mut m = store.find_task("m-1").unwrap();
        let mut p = Pass::opened(None, None);
        p.at = "2026-10-06T00:00:00Z".into();
        write(&mut m, &[p]);
        store.save_task(&m).unwrap();
        assert!(!verified(&store.tasks(), "m-1"), "a pass still reading is not a verdict");
    }

    /// A blocking verdict that names a piece adds one rework piece and moves the
    /// row to doing; a hold folds the pieces into the log.
    #[test]
    fn a_block_adds_a_rework_piece_for_the_piece_it_names_and_a_hold_folds_the_pieces() {
        let (_env, store) = scratch("pieces");
        let mut m = Task::new("a member", "m-1");
        m.set_status(Status::Review);
        let mut rec = crate::pieces::Record::default();
        for (n, label) in [(1, "types"), (2, "board"), (3, "rail")] {
            let mut p = crate::pieces::Piece::pending(n, label, 1);
            p.state = crate::pieces::State::Done;
            p.done_when = format!("{label} works");
            p.caption = Some(format!("{label} read back"));
            rec.pieces.push(p);
        }
        crate::pieces::write(&mut m, &rec);
        store.save_task(&m).unwrap();
        record(&store, "m-1", State::Blocks, "m-1-2 does not hold: the board is empty", None, Some("the board is empty")).unwrap();
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Doing);
        let after = crate::pieces::record(&t);
        assert_eq!(after.pieces.len(), 4, "exactly one rework piece");
        let r = &after.pieces[3];
        assert_eq!((r.title.as_str(), r.batch, r.done_when.as_str()), ("rework of 2", 2, "board works"));
        assert!(r.overview.contains("the board is empty"), "{}", r.overview);

        fold_pieces(&store, "m-1");
        let t = store.find_task("m-1").unwrap();
        assert!(!crate::pieces::staged(&t));
        assert_eq!(t.section("Log").unwrap().matches(" · ").count() >= 4 && (1..=4).all(|n| crate::pieces::folded_line(&t, n).is_some()), true, "one line a piece");
        assert!(crate::pieces::folded_line(&t, 2).unwrap().starts_with("piece 2 · board · done · board read back"));
    }

    /// `wsp verified` from a governor's seat, with no pass of its own open, is
    /// a pass in the governor's name — the way a block is answered by hand.
    #[test]
    fn a_verdict_from_a_seat_with_no_pass_open_is_a_pass_of_its_own() {
        let (_env, store) = scratch("byhand");
        let mut m = Task::new("a member", "m-1");
        m.status_raw = Status::Review.as_str().into();
        store.save_task(&m).unwrap();
        record(&store, "m-1", State::Holds, "the finding was about code nobody shipped", Some("w1:p1"), None).unwrap();
        let ps = passes(&store.find_task("m-1").unwrap());
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].pane.as_deref(), Some("w1:p1"));
        assert_eq!(ps[0].ending, Ending::Ended, "nothing for the tick to end: wsp started no agent for it");
        assert!(seats(&store.tasks()).is_empty(), "and the governor's seat is not named a verifier's");
    }

    /// `wsp-241`: `verified` is refused without a caption, before anything is
    /// read or written; and the parser hands `--caption` to the verb when it
    /// is typed after the member, where the payload's flags otherwise stop.
    #[test]
    fn a_verdict_with_no_caption_is_refused_and_nothing_is_written() {
        let (_env, store) = scratch("no-caption");
        let mut t = Task::new("a member", "m-1");
        t.status_raw = "review".into();
        store.save_task(&t).unwrap();
        let from = store.root.join("verdict.md");
        std::fs::write(&from, "checked 55c4839 on the trunk; it holds").unwrap();
        let from = from.display().to_string();
        let args = Args::synth("verified", &["m-1"], &[("holds", "true"), ("from", &from)]);
        assert_eq!(cmd_verified(&store, &args), 2);
        assert!(passes(&store.find_task("m-1").unwrap()).is_empty(), "no pass opened for a refused verdict");
        let none = Args::synth("verified", &["m-1"], &[("holds", "true"), ("from", &from), ("caption", "none")]);
        assert_eq!(cmd_verified(&store, &none), 2, "`none` takes a caption off; it is not one");

        let line = ["verified", "m-1", "--holds", "--from", "v.md", "--caption", "the wiring is tested now"];
        let parsed = Args::parse(line.iter().map(|s| s.to_string()).collect());
        assert_eq!(parsed.get("caption").as_deref(), Some("the wiring is tested now"));
        // `--from` stays a positional on a stream verb, by design (`Literal::stream`).
        assert_eq!(parsed.rest, vec!["m-1", "--from", "v.md"], "and the caption did not become the verdict");
    }

    /// `wsp-325`: a verdict's `checked:` lines leave its prose and sit on the
    /// entry as items; one that does not say held or not held is refused with
    /// nothing written; and a block sends back what did not hold.
    #[test]
    fn a_verdicts_checked_lines_become_items_and_a_line_nobody_can_count_is_refused() {
        let (_env, store) = scratch("checked");
        let mut t = Task::new("a member", "m-1");
        t.status_raw = "review".into();
        store.save_task(&t).unwrap();
        let from = store.root.join("verdict.md");
        let verdict = |text: &str| {
            std::fs::write(&from, text).unwrap();
            let from = from.display().to_string();
            Args::synth("verified", &["m-1", "--from", &from], &[("blocks", "true"), ("caption", "the replay is short")])
        };

        let refused = verdict("Blocks.\nchecked: the replay was run on 20 rows");
        assert_eq!(cmd_verified(&store, &refused), 2);
        assert!(passes(&store.find_task("m-1").unwrap()).is_empty(), "a refused verdict writes nothing");

        let ok = verdict("Blocks, the replay is short.\n\nchecked: the gate holds the verifier — held\n- **checked:** the replay covers 20 rows — not held\ncause: work — the replay is short\n");
        assert_eq!(cmd_verified(&store, &ok), 0);
        let t = store.find_task("m-1").unwrap();
        let p = passes(&t).pop().unwrap();
        assert_eq!(p.text, "Blocks, the replay is short.\n\ncause: work — the replay is short", "the cause is kept on the pass");
        assert_eq!(
            p.checked,
            vec![
                crate::did::Checked { item: "the gate holds the verifier".into(), held: true },
                crate::did::Checked { item: "the replay covers 20 rows".into(), held: false },
            ]
        );
        assert!(t.body.contains("\n  - **checked:** the replay covers 20 rows · not held"), "{}", t.body);
        let log = t.body.lines().find(|l| l.contains("sent back")).unwrap_or_default();
        assert!(log.contains("Not held: - the replay covers 20 rows"), "what did not hold is what is owed: {log}");
        let v = json(&t);
        assert_eq!(v[0]["checked"][1], serde_json::json!({ "item": "the replay covers 20 rows", "held": false }));
        assert!(v[0]["did"].is_null(), "and nothing derived until the seat ends");
    }

    /// The caption is on the entry's header line and back out of it, and
    /// `show --json` gives every pass with §17.6's caption — a pass written
    /// before captions as its word and its clock, never its first sentence.
    #[test]
    fn a_passes_caption_is_on_its_entry_and_in_show_json_and_an_old_one_reads_as_its_word_and_time() {
        let (_env, store) = scratch("caption-json");
        let mut t = Task::new("a member", "m-1");
        t.status_raw = "review".into();
        let old = "- 2026-10-08T07:10:00Z holds · read 55c4839abcde · decided 2026-10-08T07:16:00Z · ended\n  55c4839 holds: both halves";
        t.body = format!("## Overview\nthe work\n\n## Verification\n{old}\n");
        store.save_task(&t).unwrap();
        record(&store, "m-1", State::Blocks, "def5678 does not hold", None, Some("the wiring between the two has no test")).unwrap();

        let t = store.find_task("m-1").unwrap();
        assert!(t.body.contains(" · caption the wiring between the two has no test\n"), "{}", t.body);
        let v = json(&t);
        let first = &v[0];
        assert_eq!(first["state"], "holds");
        assert_eq!(first["verdict"], "55c4839 holds: both halves");
        assert_eq!(first["read"], "55c4839abcde");
        let clock = util::local_hm(util::epoch_of("2026-10-08T07:16:00Z"));
        assert_eq!(first["caption"]["text"], format!("Holds · {clock}"), "the word and when, not `55c4839 holds`");
        assert_eq!(first["caption"]["from"], "stage");
        let newest = &v[1];
        assert_eq!(newest["caption"]["text"], "Blocks — the wiring between the two has no test");
        assert_eq!(newest["caption"]["word"], "blocks");
        assert_eq!(newest["caption"]["tail"], "the wiring between the two has no test");
        assert_eq!(newest["caption"]["at"], newest["decided"], "a verdict's caption is dated by the verdict");
    }

    // ---- `wsp-614`: a verifier's exit ------------------------------------------

    /// A member `m-1` at review with one pass reading it, no repository behind it.
    pub fn reading(tag: &str) -> (util::Isolated, Store) {
        let (env, store) = scratch(tag);
        let mut t = Task::new("a member", "m-1");
        t.status_raw = "review".into();
        t.body = "## Overview\nthe work\n\n## Log\n\n- 2026-11-12 review: done\n".into();
        write(&mut t, &[Pass::opened(None, None)]);
        store.save_task(&t).unwrap();
        (env, store)
    }

    /// Submit `text` for m-1 from a verifier's seat, from a file.
    pub fn submit_verdict(env: &util::Isolated, store: &Store, text: &str) -> i32 {
        let file = env.path("exit.md");
        std::fs::write(&file, text).unwrap();
        let from = file.display().to_string();
        let args = Args::synth("submit", &["m-1"], &[("from", &from)]);
        crate::cmd_submit::submit_from(store, &args, Some("verifier"), None)
    }

    pub fn newest(store: &Store) -> Pass {
        passes(&store.find_task("m-1").unwrap()).pop().unwrap()
    }

    pub const BLOCKS: &str = "verdict: blocks\nsummary: the second caller has no test\n\nThe wiring is in; the second caller is untested.\n\ndone-when:\n- not held: the wiring is tested — no test of the second caller, src/x.rs:9\n\nchecked:\n- unit (2m): cargo test x:: — pass\n\n";

    #[test]
    fn a_verifier_exit_that_blocks_without_a_cause_is_refused_naming_the_line() {
        let (env, store) = reading("exit-nocause");
        let text = format!("{BLOCKS}owed:\n- a test of the second caller\n");
        let lines = crate::cmd_submit::verifier_exit(crate::exit::Verdict::Blocks, &text).unwrap_err();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(lines[0].starts_with("line 1 (cause): `blocks` and no `cause:`") && lines[0].contains("cause: work | check | tree | env"), "{lines:#?}");
        assert_eq!(submit_verdict(&env, &store, &text), 1);
        assert_eq!(newest(&store).state, State::Running, "nothing recorded");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Review);
        // The alias: `wsp verified --blocks` in prose with no cause is refused the same.
        let file = env.path("prose.md");
        std::fs::write(&file, "the second caller has no test").unwrap();
        let from = file.display().to_string();
        let args = Args::synth("verified", &["m-1", "--from", &from], &[("blocks", "true"), ("caption", "no test")]);
        assert_eq!(verified_as(&store, &args, None), 2);
        assert_eq!(newest(&store).state, State::Running);
        // A block that names nothing not held and no failed check is refused too.
        let empty = "verdict: blocks\nsummary: s\n\nprose\n\ncause: work — it is thin\n\nowed:\n- more\n";
        let lines = crate::cmd_submit::verifier_exit(crate::exit::Verdict::Blocks, empty).unwrap_err();
        assert!(lines.iter().any(|l| l.contains("no item is not held and no check failed")), "{lines:#?}");
    }

    #[test]
    fn a_verifier_exit_that_blocks_on_the_work_sends_the_row_back_with_its_owed_lines_as_the_reason() {
        let (env, store) = reading("exit-work");
        let text = format!("{BLOCKS}cause: work — the second caller is untested\n\nowed:\n- a test of the second caller\n- its error path named\n");
        assert_eq!(submit_verdict(&env, &store, &text), 0);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Doing, "sent back");
        let p = newest(&store);
        assert_eq!(p.state, State::Blocks);
        assert_eq!(p.caption.as_deref(), Some("the second caller has no test"), "the summary is the caption");
        assert_eq!(p.checked, vec![crate::did::Checked { item: "the wiring is tested".into(), held: false }]);
        assert!(p.text.contains("cause: work — the second caller is untested") && p.text.contains("- unit (2m): cargo test x:: — pass"), "{}", p.text);
        let sent = t.section("Log").unwrap().lines().find(|l| l.contains(crate::cycle::SENT_BACK)).unwrap_or_default().to_string();
        assert!(sent.contains("a test of the second caller; its error path named"), "owed: is the reason: {sent}");
        assert!(!sent.contains("The wiring is in"), "the account is not the reason: {sent}");
    }

    #[test]
    fn a_verifier_exit_with_cause_env_leaves_the_row_at_review_and_the_run_owes_a_new_verifier() {
        let (env, store) = reading("exit-env");
        let text = "verdict: blocks\nsummary: could not build: the tree was removed\n\nMy tree went away mid-read; I read nothing of the work.\n\ncause: env — the tree was removed under me\n\nowed: nothing\n";
        assert_eq!(submit_verdict(&env, &store, text), 0);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Review, "a member is never sent back on a verifier's environment");
        assert!(!t.section("Log").unwrap().contains(crate::cycle::SENT_BACK));
        assert!(t.section("Log").unwrap().contains("could not verify"), "{}", t.section("Log").unwrap());
        let p = newest(&store);
        assert_eq!(p.state, State::CouldNotVerify);
        assert!(t.body.contains(" could not verify · "), "the entry says so: {}", t.body);
        let tasks = vec![t.clone()];
        assert!(!verified(&tasks, "m-1") && blocked(&tasks, "m-1").is_none(), "neither a hold nor a block");
        let opened = crate::cycle::open_verifier(&store, &t, &crate::model::Policy::parse("claude").unwrap(), &|_: &str| false, &|_: &str| false, &|| true);
        assert!(opened.is_some(), "the run owes a new verifier");
        let states: Vec<State> = passes(&store.find_task("m-1").unwrap()).iter().map(|p| p.state).collect();
        assert_eq!(states, vec![State::CouldNotVerify, State::Running]);
    }

    #[test]
    fn a_verifier_exit_that_holds_with_a_failed_check_is_refused() {
        let (env, store) = reading("exit-holds-fail");
        let text = "verdict: holds\nsummary: it holds\n\nRead it in the tree.\n\ndone-when:\n- held: the wiring is tested — src/x.rs:30\n\nchecked:\n- unit (2m): cargo test x:: — fail\n\nowed: nothing\n";
        let lines = crate::cmd_submit::verifier_exit(crate::exit::Verdict::Holds, text).unwrap_err();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(lines[0].starts_with("line 10 (checked): `unit` failed, and the verdict is `holds`"), "{lines:#?}");
        assert_eq!(submit_verdict(&env, &store, text), 1);
        assert_eq!(newest(&store).state, State::Running);
        // And a hold with an item not held, through `wsp verified --holds`'s prose.
        let file = env.path("prose.md");
        std::fs::write(&file, "mostly fine\nchecked: the wiring is tested — not held\n").unwrap();
        let from = file.display().to_string();
        let args = Args::synth("verified", &["m-1", "--from", &from], &[("holds", "true"), ("caption", "fine")]);
        assert_eq!(verified_as(&store, &args, None), 2);
        assert_eq!(newest(&store).state, State::Running);
    }

    // ---- `wsp-341`: the hold lands the row ------------------------------------

    pub fn git_t(dir: &std::path::Path, args: &[&str]) -> String {
        let out = wsp_base::calls::Command::new("git")
            .arg("-C").arg(dir).args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A member `m-1` ready in a tree of its own, with a pass reading it, and a
    /// project whose `## Checks` are `checks` (`RAN` in a command is a file the
    /// test reads back). `trunk` is committed to master after the pass opened.
    pub struct Ground {
        _env: util::Isolated,
        store: Store,
        repo: std::path::PathBuf,
        tree: std::path::PathBuf,
        ran: std::path::PathBuf,
        ready: String,
    }

    pub fn ground(tag: &str, checks: &str, member: &[(&str, &str)], trunk: &[(&str, &str)]) -> Ground {
        // The land a hold performs reruns checks through the engine's answers, which `main` installs.
        crate::cmd_checkout::install_engine(crate::cmd_checkout::Engine {
            passed: |_| Default::default(),
            poke_task: |_, _, _| {},
            runs: |_, _| false,
            after_land: crate::check::after_land,
            rerun: crate::check::rerun,
            held: |_| None,
            refused: |store, row| crate::hub_journal::refusal(store, row, "land"),
        });
        let env = util::isolated(&format!("hold-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join("f"), "base\n").unwrap();
        git_t(&repo, &["add", "f"]);
        git_t(&repo, &["commit", "-q", "-m", "first"]);
        let tree = repo.join(".worktrees").join("m-1");
        git_t(&repo, &["worktree", "add", "-q", "-b", "m-1", &tree.display().to_string()]);
        let put = |dir: &std::path::Path, files: &[(&str, &str)], msg: &str| {
            for (path, text) in files {
                let at = dir.join(path);
                std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                std::fs::write(&at, text).unwrap();
                git_t(dir, &["add", path]);
            }
            git_t(dir, &["commit", "-q", "-m", msg]);
        };
        put(&tree, member, "the work");
        let ready = git_t(&tree, &["rev-parse", "HEAD"]);
        let ran = env.path("ran.log");
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        p.body = format!("## Checks\n{}\n", checks.replace("RAN", &ran.display().to_string()).replace("REPO", &repo.display().to_string()));
        store.save_project(&p).unwrap();
        let mut t = Task::new("a member", "m-1");
        t.project = Some("p".into());
        t.status_raw = "review".into();
        t.log(&format!("{} {ready} on m-1", crate::repair::READY));
        write(&mut t, &[Pass::opened(Some(ready.clone()), None)]);
        store.save_task(&t).unwrap();
        if !trunk.is_empty() {
            put(&repo, trunk, "the trunk moved");
        }
        Ground { _env: env, store, repo, tree, ran, ready }
    }

    impl Ground {
        fn verdict(&self, flag: &str) -> i32 {
            let from = self.store.root.join("verdict.md");
            let cause = if flag == "blocks" { "\ncause: work — it does not do what was asked" } else { "" };
            std::fs::write(&from, format!("read it in the tree; that is the verdict{cause}")).unwrap();
            let from = from.display().to_string();
            let args = Args::synth("verified", &["m-1", "--from", &from], &[(flag, "true"), ("caption", "it was read in the tree")]);
            // The seat is said here (none: the member's pass is the newest
            // running one), not read off the process env, which a sibling test
            // that sets the seat without the env lock can change mid-run.
            verified_as(&self.store, &args, None)
        }
        fn trunk(&self) -> String {
            git_t(&self.repo, &["rev-parse", "master"])
        }
        fn member(&self) -> Task {
            self.store.find_task("m-1").unwrap()
        }
        fn reran(&self) -> String {
            std::fs::read_to_string(&self.ran).unwrap_or_default()
        }
    }

    // ---- `wsp-605`: a hold that cannot land on a dirty tree owes its land ------

    /// The tree as `npm run check` left compounding-366's: a tracked file the
    /// checks rewrite, and a new one.
    pub fn dirty_it(g: &Ground) {
        std::fs::write(g.tree.join("f"), "rewritten by the checks\n").unwrap();
        std::fs::write(g.tree.join("shot.png"), "new\n").unwrap();
    }

    pub fn clean_it(g: &Ground) {
        git_t(&g.tree, &["checkout", "--", "f"]);
        std::fs::remove_file(g.tree.join("shot.png")).unwrap();
    }

    /// The run's list, so the governor has somebody to be told.
    pub fn on_a_list(g: &Ground) {
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["m-1".into()], agent: "claude".into(), ..Default::default() }]);
        g.store.save_worklist(&w).unwrap();
    }

    pub fn told() -> Vec<String> {
        crate::cycle::tests::TOLD.with(|t| t.borrow_mut().drain(..).collect())
    }

    /// The journal's side chain for `m-1`, written by this spoke while it was
    /// offline and now behind another user's lease (wsp-469): the state
    /// `hub_journal::refusal` reads.
    pub fn side_chain_it(g: &Ground) {
        let mut sides = crate::hub_journal::Sides::default();
        sides.rows.insert(
            "m-1".into(),
            crate::hub_journal::Side {
                workspace: "ws-1".into(),
                row: "m-1".into(),
                tip: "sha256:0123456789abcdef0123456789abcdef".into(),
                holder: "bo".into(),
                old: "ann".into(),
                mine: true,
                token: Some(1),
                at: "2026-11-23T10:00:00Z".into(),
            },
        );
        sides.write(&g.store).unwrap();
    }

    #[test]
    fn a_row_the_journal_side_chained_refuses_the_hold_and_the_land_and_moves_nothing() {
        let g = ground("side-chained", "", &[("m.txt", "mine\n")], &[]);
        side_chain_it(&g);
        let why = crate::hub_journal::refusal(&g.store, "m-1", "verified").expect("the row is refused");
        let before = g.trunk();
        let log = g.member().body.clone();

        assert_eq!(g.verdict("holds"), 1, "`wsp verified --holds` is refused");
        assert_eq!(g.verdict("blocks"), 1, "and so is a block: the verdict is the old holder's");
        assert_eq!(g.trunk(), before, "no land happened");
        assert_eq!(g.member().body, log, "the verdict wrote nothing");
        assert_eq!(passes(&g.member()).pop().unwrap().state, State::Running, "the pass is still reading");
        assert_eq!(g.member().status(), Status::Review);

        // The land, by the verb and by the run's land of a member.
        let land = |extra: &[(&str, &str)]| crate::cmd_checkout::land(&g.store, &Args::synth("land", &["m-1"], extra));
        crate::cmd_checkout::install_engine(crate::cmd_checkout::Engine {
            passed: |_| Default::default(),
            poke_task: |_, _, _| {},
            runs: |_, _| false,
            after_land: crate::check::after_land,
            rerun: crate::check::rerun,
            held: |_| None,
            refused: |store, row| crate::hub_journal::refusal(store, row, "land"),
        });
        assert_eq!(land(&[]), 1, "`wsp land` is refused");
        assert_eq!(land(&[("for-run", "true")]), 1, "and the run's land of the member");
        match crate::cmd_checkout::land_verdict(&g.store, "m-1") {
            crate::cmd_checkout::Verdict::Blocked(said) => assert_eq!(said, crate::hub_journal::refusal(&g.store, "m-1", "land").unwrap()),
            _ => panic!("the hold's own land is refused with the journal's reason"),
        }
        assert_eq!(g.trunk(), before, "nothing reached the trunk");
        assert!(why.contains("`wsp verified` is refused here") && why.contains("bo"), "{why}");

        // The same row with the side chain gone lands: the refusal was the whole of it.
        std::fs::remove_file(g.store.state.join("hub").join("side.json")).unwrap();
        assert_eq!(land(&[]), 0);
        assert_ne!(g.trunk(), before);
    }

    /// `wsp-350`, EdJ/wsp#6: in a project marked `lands: by-merge`, `wsp land`
    /// (and the run's `--for-run`) refuses with the PR to merge named, the
    /// verifier's hold stands and lands nothing, and nothing reaches the trunk.
    #[test]
    fn a_row_that_lands_by_merge_is_held_without_a_land_and_wsp_land_refuses_naming_the_pr() {
        let g = ground("by-merge", "", &[("m.txt", "mine\n")], &[]);
        let mut p = g.store.project("p").unwrap();
        p.lands = crate::lands::BY_MERGE.into();
        g.store.save_project(&p).unwrap();
        let before = g.trunk();

        let land = |extra: &[(&str, &str)]| crate::cmd_checkout::land(&g.store, &Args::synth("land", &["m-1"], extra));
        assert_eq!(land(&[]), 1, "`wsp land` is refused");
        assert_eq!(land(&[("for-run", "true")]), 1, "and the run's land of the member");
        match crate::cmd_checkout::land_verdict(&g.store, "m-1") {
            crate::cmd_checkout::Verdict::ByMerge(said) => assert_eq!(
                said,
                "wsp: m-1 lands by merge (the project p), so `wsp land` does not put it on the local trunk — merge the PR from branch m-1 \
                 (no open PR was found for it: push the branch and open one); the row is waiting for the PR to merge, and wsp lands it \
                 when the merge is on the remote's trunk",
                "the refusal names the PR's branch, golden"
            ),
            _ => panic!("the hold's own land is by merge"),
        }
        assert_eq!(g.trunk(), before, "nothing reached the trunk");

        assert_eq!(g.verdict("holds"), 0, "the hold is recorded");
        let t = g.member();
        let pass = passes(&t).pop().unwrap();
        assert_eq!(pass.state, State::Holds, "the verdict holds");
        assert!(pass.owed.is_none() && land_owed(&t).is_none(), "and no land is owed by wsp");
        assert_eq!(g.trunk(), before, "still nothing on the trunk");
        assert_eq!(t.status_raw, "review", "not sent back");

        // The field off: the same row lands, as before.
        p.lands.clear();
        g.store.save_project(&p).unwrap();
        assert_eq!(land(&[]), 0);
        assert_ne!(g.trunk(), before);
    }

    /// `wsp-515`: the same refusal with a PR open, as `gh` says it is.
    #[test]
    fn wsp_land_on_a_by_merge_row_with_an_open_pr_refuses_naming_its_url() {
        let g = ground("by-merge-pr", "", &[("m.txt", "mine\n")], &[]);
        let mut p = g.store.project("p").unwrap();
        p.lands = crate::lands::BY_MERGE.into();
        g.store.save_project(&p).unwrap();
        git_t(&g.repo, &["remote", "add", "origin", "https://github.invalid/o/r.git"]);
        let url = "https://github.com/o/r/pull/7";
        let _gh = wsp_testkit::StubGh::open_pr(&g._env.path("bin"), "m-1", url, &g.ready);
        let before = g.trunk();

        assert_eq!(crate::cmd_checkout::land(&g.store, &Args::synth("land", &["m-1"], &[])), 1, "`wsp land` is refused");
        // What it prints on stderr is this same refusal.
        match crate::cmd_checkout::land_verdict(&g.store, "m-1") {
            crate::cmd_checkout::Verdict::ByMerge(said) => assert!(said.contains(&format!("merge its PR, {url};")), "{said}"),
            _ => panic!("the row lands by merge"),
        }
        assert_eq!(g.trunk(), before, "nothing reached the trunk");
    }

    #[test]
    fn a_hold_whose_land_meets_a_dirty_tree_is_owed_its_land_not_a_block() {
        for (tag, trunk) in [("owed-ff", &[][..]), ("owed-moved", &[("t.txt", "theirs\n")][..])] {
            let g = ground(tag, "", &[("m.txt", "mine\n")], trunk);
            let before = g.trunk();
            dirty_it(&g);
            assert_eq!(g.verdict("holds"), 0);
            assert_eq!(g.trunk(), before, "{tag}: nothing reached the trunk");
            let t = g.member();
            let p = passes(&t).pop().unwrap();
            assert_eq!(p.state, State::Holds, "{tag}: the verdict holds");
            let owed = p.owed.clone().expect("the land is owed");
            assert!(owed.contains("written by the checks run in it") && owed.contains(": f"), "{tag}: the tracked file is named as the checks': {owed}");
            assert!(owed.contains("work nobody committed: shot.png"), "{tag}: the new file is named: {owed}");
            assert_eq!(t.status_raw, "review", "{tag}: not sent back");
            assert!(!t.section("Log").unwrap().contains(crate::cycle::SENT_BACK), "{tag}");
            assert!(land_owed(&t).is_some(), "{tag}");
            assert!(crate::cycle::blocked_at_tip(&t).is_none(), "{tag}: no block for wsp-529 to refuse");
            let again = Pass::parse(p.line().lines().next().unwrap()).unwrap();
            assert_eq!(again.owed.as_deref(), Some(crate::util::fold(&owed).replace(" · ", " — ").as_str()), "{tag}: the clause reads back");
        }
    }

    #[test]
    fn a_review_again_at_the_same_commit_with_a_clean_tree_lands_the_owed_hold_with_no_verifier() {
        let g = ground("owed-clean", "", &[("m.txt", "mine\n")], &[("t.txt", "theirs\n")]);
        dirty_it(&g);
        assert_eq!(g.verdict("holds"), 0);
        clean_it(&g);
        let Some(Owed::Landed(Some(sha))) = retry_owed(&g.store, "m-1", "reviewed again") else { panic!("landed") };
        let t = g.member();
        assert_eq!(g.trunk(), sha, "the trunk is at the landed commit");
        assert_eq!(crate::repair::landed(&t).as_deref(), Some(sha.as_str()));
        let ps = passes(&t);
        assert_eq!(ps.len(), 1, "no new pass, so no verifier");
        assert_eq!(ps[0].state, State::Holds);
        assert!(ps[0].owed.is_none(), "the owed land is settled");
        assert!(land_owed(&t).is_none());
        assert_eq!(retry_owed(&g.store, "m-1", "reviewed again"), None, "nothing is owed twice");
    }

    #[test]
    fn the_run_lands_an_owed_hold_once_the_tree_turns_clean() {
        let g = ground("owed-turns", "", &[("m.txt", "mine\n")], &[]);
        dirty_it(&g);
        assert_eq!(g.verdict("holds"), 0);
        let before = g.trunk();
        crate::cycle::retry_owed_land(&g.store, "run", 1, &g.member());
        assert_eq!(g.trunk(), before, "a dirty tree waits");
        assert!(land_owed(&g.member()).is_some());
        clean_it(&g);
        crate::cycle::retry_owed_land(&g.store, "run", 1, &g.member());
        assert_eq!(g.trunk(), g.ready, "landed once clean");
        assert!(land_owed(&g.member()).is_none());
        assert_eq!(passes(&g.member()).len(), 1, "with no verifier");
    }

    #[test]
    fn a_still_dirty_tree_stays_owed_and_tells_the_governor() {
        let g = ground("owed-still", "", &[("m.txt", "mine\n")], &[]);
        on_a_list(&g);
        dirty_it(&g);
        assert_eq!(g.verdict("holds"), 0);
        assert!(told().iter().any(|t| t.contains("its land is owed")), "the governor hears of the owed land");
        let before = g.trunk();
        let Some(Owed::Still(why)) = retry_owed(&g.store, "m-1", "reviewed again") else { panic!("still owed") };
        assert!(why.contains("shot.png"), "{why}");
        assert_eq!(g.trunk(), before);
        let t = g.member();
        assert!(land_owed(&t).is_some(), "still owed");
        assert_eq!(t.status_raw, "review");
        assert!(t.section("Log").unwrap().contains("the hold's land is owed (reviewed again, and refused again)"), "said on the row");
        let heard = told();
        assert!(heard.iter().any(|t| t.contains("m-1: its hold is still owed a land") && t.contains("shot.png")), "{heard:?}");
    }

    #[test]
    fn new_commits_over_an_owed_hold_are_not_landed_unread() {
        let g = ground("owed-moved-on", "", &[("m.txt", "mine\n")], &[]);
        dirty_it(&g);
        assert_eq!(g.verdict("holds"), 0);
        clean_it(&g);
        std::fs::write(g.tree.join("more.txt"), "more\n").unwrap();
        git_t(&g.tree, &["add", "more.txt"]);
        git_t(&g.tree, &["commit", "-q", "-m", "more work"]);
        let before = g.trunk();
        assert_eq!(retry_owed(&g.store, "m-1", "reviewed again"), None);
        assert_eq!(g.trunk(), before, "a commit no verifier read stays off the trunk");
    }

    /// `wsp-529` stands for a real block: at an unchanged commit it is still
    /// one, and nothing is owed to land.
    #[test]
    fn a_real_block_at_an_unchanged_commit_is_still_refused() {
        let g = ground("owed-real-block", "", &[("m.txt", "mine\n")], &[]);
        assert_eq!(g.verdict("blocks"), 0);
        let t = g.member();
        assert!(land_owed(&t).is_none());
        assert_eq!(retry_owed(&g.store, "m-1", "reviewed again"), None);
        let blocked = crate::cycle::blocked_at_tip(&t).expect("the block at the tip");
        assert!(same_commit(blocked.read.as_deref().unwrap(), &g.ready));
        assert!(!crate::cycle::answered_block(&t), "unanswered, so no verifier is owed");
    }

    #[test]
    fn a_hold_lands_the_row_and_records_both_commits() {
        let g = ground("lands", "", &[("m.txt", "mine\n")], &[("t.txt", "theirs\n")]);
        assert_eq!(g.verdict("holds"), 0);
        let t = g.member();
        let landed = crate::repair::landed(&t).expect("a landed line");
        assert_eq!(landed, g.trunk(), "the trunk is at the landed commit");
        assert_ne!(landed, g.ready, "the rebase made a new commit");
        assert!(t.section("Log").unwrap().contains(&format!("{} {landed} on m-1 · since", crate::repair::LANDED)));
        let p = passes(&t).pop().unwrap();
        assert_eq!(p.state, State::Holds);
        assert!(same_commit(p.read.as_deref().unwrap(), &g.ready), "the pass keeps the commit it read");
        assert!(same_commit(p.landed.as_deref().unwrap(), &landed), "and names the one that reached the trunk");
        assert!(crate::repair::ready(&t).is_none(), "landed is not ready");
        assert_eq!(t.status_raw, "review");
        let again = Pass::parse(&p.line().lines().next().unwrap().to_string()).unwrap();
        assert_eq!(again.landed, p.landed, "the clause reads back");
    }

    /// A sibling test sets the seat env without the env lock, so a hold read
    /// as that seat opened a fresh pass at the landed commit and not the one
    /// read. The verdict names its seat itself and does not read the process's.
    #[test]
    fn a_hold_does_not_depend_on_a_seat_set_in_the_process() {
        let g = ground("seat-env", "", &[("m.txt", "mine\n")], &[("t.txt", "theirs\n")]);
        std::env::set_var(crate::base::seat::SEAT_ENV, "cpd-other");
        let code = g.verdict("holds");
        std::env::remove_var(crate::base::seat::SEAT_ENV);
        assert_eq!(code, 0);
        let p = passes(&g.member()).pop().unwrap();
        assert_eq!(passes(&g.member()).len(), 1, "the open pass closed; no second one opened");
        assert!(same_commit(p.read.as_deref().unwrap(), &g.ready), "the pass keeps the commit it read");
    }

    #[test]
    fn a_hold_on_a_row_sent_back_while_it_was_read_lands_nothing_and_is_superseded() {
        let g = ground("sentback", "", &[("m.txt", "mine\n")], &[("t.txt", "theirs\n")]);
        let before = g.trunk();
        let mut t = g.member();
        send_back(&mut t, util::now_iso(), "wsp check failed", Some(&g.ready), "a step failed", None);
        g.store.save_task(&t).unwrap();
        assert_eq!(g.verdict("holds"), 1);
        assert_eq!(g.trunk(), before, "the hold landed nothing");
        let t = g.member();
        assert_eq!(t.status_raw, "doing", "and the send-back stands");
        assert!(crate::repair::landed(&t).is_none());
        assert_eq!(passes(&t).pop().unwrap().state, State::Superseded);
    }

    #[test]
    fn a_hold_on_a_trunk_that_did_not_move_lands_with_no_landed_clause() {
        let g = ground("unmoved", "", &[("m.txt", "mine\n")], &[]);
        assert_eq!(g.verdict("holds"), 0);
        let t = g.member();
        assert_eq!(g.trunk(), g.ready);
        assert!(passes(&t).pop().unwrap().landed.is_none(), "read and landed are the same commit");
        assert_eq!(crate::repair::landed(&t).as_deref(), Some(g.ready.as_str()));
    }

    #[test]
    fn a_block_leaves_the_trunk_alone() {
        let g = ground("blocks", "", &[("m.txt", "mine\n")], &[("t.txt", "theirs\n")]);
        let before = g.trunk();
        assert_eq!(g.verdict("blocks"), 0);
        assert_eq!(g.trunk(), before, "nothing reached the trunk");
        let t = g.member();
        assert_eq!(t.status_raw, "doing");
        assert!(crate::repair::landed(&t).is_none());
        assert_eq!(passes(&t).pop().unwrap().state, State::Blocks);
    }

    #[test]
    fn a_moved_trunk_reruns_only_the_affected_checks() {
        let checks = "- src on src/**: echo src >> RAN\n- docs on docs/**: echo docs >> RAN\n- mine on m.txt: echo mine >> RAN\n- always: echo always >> RAN";
        let g = ground("affected", checks, &[("m.txt", "mine\n")], &[("src/a.txt", "theirs\n")]);
        assert_eq!(g.verdict("holds"), 0);
        assert_eq!(g.reran(), "src\nalways\n", "the rule the rebase touched and the every-landing step, not the rule only the member touched");
        assert!(g.member().section("Log").unwrap().contains("reran src, always after the trunk moved, green"));
        assert_eq!(g.trunk(), crate::repair::landed(&g.member()).unwrap());
    }

    #[test]
    fn a_red_rerun_after_the_rebase_blocks_the_hold() {
        let g = ground("red", "- src on src/**: test ! -f src/a.txt", &[("m.txt", "mine\n")], &[("src/a.txt", "theirs\n")]);
        let before = g.trunk();
        assert_eq!(g.verdict("holds"), 0);
        assert_eq!(g.trunk(), before, "the red rerun kept it off the trunk");
        let t = g.member();
        assert_eq!(t.status_raw, "doing");
        let p = passes(&t).pop().unwrap();
        assert_eq!(p.state, State::Blocks);
        assert!(p.text.contains("read it in the tree") && p.text.contains("`src`"), "the verifier's prose, then the step that failed: {}", p.text);
        assert!(t.section("Log").unwrap().contains(crate::cycle::SENT_BACK));
    }

    #[test]
    fn a_conflict_after_the_trunk_moved_blocks_the_hold() {
        let g = ground("conflict", "", &[("f", "member's line\n")], &[("f", "trunk's line\n")]);
        let before = g.trunk();
        assert_eq!(g.verdict("holds"), 0);
        assert_eq!(g.trunk(), before);
        let t = g.member();
        assert_eq!(passes(&t).pop().unwrap().state, State::Blocks);
        assert!(passes(&t).pop().unwrap().text.contains("does not apply"));
        assert!(!g.tree.join(".git").is_dir() && git_t(&g.tree, &["status", "--porcelain"]).is_empty(), "the tree is not left mid-rebase");
        assert_eq!(git_t(&g.tree, &["rev-parse", "HEAD"]), g.ready, "and is as the member left it");
    }

    /// `wsp-346`: a check target that cannot be resolved after the rebase is a
    /// land that checked nothing, so it is an error with the reason named —
    /// which `land_verdict` turns into a block — and only a plan with nothing
    /// affected is an empty answer.
    #[test]
    fn checks_that_cannot_be_planned_after_the_trunk_moved_are_an_error_not_an_empty_plan() {
        let g = ground("noplan", "- src on src/**: echo src >> RAN", &[("m.txt", "mine\n")], &[("src/a.txt", "theirs\n")]);
        let base = git_t(&g.tree, &["merge-base", "master", "HEAD"]);
        git_t(&g.tree, &["rebase", "master"]);
        let head = git_t(&g.tree, &["rev-parse", "HEAD"]);
        let pulled = vec!["src/a.txt".to_string()];
        let ours = vec!["m.txt".to_string()];
        let rerun = |id: &str| crate::check::rerun_steps(&g.store, id, &head, &base, &pulled, &ours);

        let steps = rerun("m-1").expect("a plannable row");
        assert_eq!(steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["src"]);
        let none = crate::check::rerun_steps(&g.store, "m-1", &head, &base, &["other.txt".to_string()], &ours);
        assert_eq!(none.expect("planned").len(), 0, "none affected is an empty answer");

        let mut t = g.member();
        t.project = Some("gone".into());
        g.store.save_task(&t).unwrap();
        let why = rerun("m-1").unwrap_err();
        assert!(why.contains("no roots"), "the reason is named: {why}");
        assert!(rerun("nobody-1").unwrap_err().contains("not in the store"));
        assert_eq!(g.reran(), "", "and no check ran");
    }

    #[test]
    fn a_trunk_that_keeps_moving_blocks_after_three_rounds() {
        let mover = "- always: n=$(ls REPO | wc -l | tr -d ' '); echo $n > REPO/g$n; git -C REPO add g$n; git -C REPO -c user.name=t -c user.email=t@t commit -qm g$n";
        let g = ground("racing", mover, &[("m.txt", "mine\n")], &[("t.txt", "theirs\n")]);
        let before = g.trunk();
        assert_eq!(g.verdict("holds"), 0);
        let t = g.member();
        let p = passes(&t).pop().unwrap();
        assert_eq!(p.state, State::Blocks);
        assert!(p.text.contains("moved 3 times"), "{}", p.text);
        assert!(crate::repair::landed(&t).is_none(), "nothing landed");
        assert_ne!(g.trunk(), before, "the racing step really moved it");
        let _ = &g.repo;
    }

    /// A row at the leg cap, every leg done, at `review` as a check finds it.
    pub fn at_the_leg_cap(store: &Store, id: &str) {
        let mut t = Task::new(id, id);
        t.status_raw = Status::Review.as_str().into();
        let mut all: Vec<crate::legs::Leg> = (0..crate::legs::MAX_LEGS)
            .map(|n| crate::legs::Leg { state: crate::legs::State::Done, ..crate::legs::Leg::pending(&format!("l{n}"), "x") })
            .collect();
        all[0].after = Some(Vec::new());
        crate::legs::write(&mut t, &all);
        store.save_task(&t).unwrap();
    }

    /// The send-back as `wsp check` makes it, then what follows it outside the lock.
    pub fn sent_back_by_check(store: &Store, id: &str, verdict: &str) {
        let mut t = store.find_task(id).unwrap();
        send_back(&mut t, util::now_iso(), "wsp check failed", None, verdict, Some(crate::check::CHECK_HEAD));
        store.save_task(&t).unwrap();
        deliver_unless_reworked(store, id, verdict);
    }

    /// `wsp-690`: a legs row at MAX_LEGS that is sent back gets no leg, and its
    /// list's governor is told once — naming the row, the cap, the verdict's
    /// first line and the three choices. The same verdict again tells nobody.
    #[test]
    fn a_send_back_at_the_leg_cap_tells_the_governor_once_and_appends_no_leg() {
        let (_env, store) = scratch("cap-told");
        at_the_leg_cap(&store, "m-1");
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["m-1".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        let _ = crate::cycle::tests::TOLD.with(|t| t.borrow_mut().drain(..).count());

        let verdict = "`cargo test` failed: 2 of 40\nthe rest of the verdict";
        sent_back_by_check(&store, "m-1", verdict);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Doing);
        assert_eq!(crate::legs::legs(&t).len(), crate::legs::MAX_LEGS, "no leg appended");
        assert_eq!(stalled_at_cap(&t).as_deref(), Some("8-leg cap"));
        let told: Vec<String> = crate::cycle::tests::TOLD.with(|t| t.borrow_mut().drain(..).collect());
        assert_eq!(told.len(), 1, "exactly one tell: {told:?}");
        let said = &told[0];
        for want in ["m-1", "8-leg cap", "NO rework leg", "`cargo test` failed: 2 of 40", "split the row", "wsp leg add m-1", "--holds"] {
            assert!(said.contains(want), "{want:?} in {said}");
        }
        assert!(!said.contains("the rest of the verdict"), "the first line, not the whole: {said}");
        assert!(stall_told(&t));

        sent_back_by_check(&store, "m-1", verdict);
        assert!(crate::cycle::tests::TOLD.with(|t| t.borrow().is_empty()), "the same verdict is not told again");
        assert_eq!(crate::legs::legs(&store.find_task("m-1").unwrap()).len(), crate::legs::MAX_LEGS);

        // A leg added by hand moves it: no longer stalled.
        let mut t = store.find_task("m-1").unwrap();
        t.set_status(Status::Doing);
        let mut legs = crate::legs::legs(&t);
        legs[crate::legs::MAX_LEGS - 1].state = crate::legs::State::Pending;
        crate::legs::write(&mut t, &legs);
        assert!(stalled_at_cap(&t).is_none(), "a pending leg is the row moving");
    }
}
