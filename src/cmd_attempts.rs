//! `wsp attempts` — which tier ran a piece of work, and how it went.
//!
//! Nothing in wsp recorded which model ran anything, so every claim about
//! routing — including the claim that opus everywhere is right — was
//! unfalsifiable. This is the reading half of the answer; the writing half is
//! two clauses on two log lines that were already being written:
//!
//! ```text
//! - 2026-08-18T12:50:09Z claimed by pane w58:p1 · spawned at opus[1m]/high
//! - 2026-08-18T15:41:02Z released after 2h51m · ran opus-5/high · 96 turns · 242M cache 4.7M in 875k out
//! ```
//!
//! **Nothing else is collected.** Every other column below — reached review,
//! finished, was picked back up, was blocked, how long it took — is read out of
//! the log the store already keeps, because those lines are written by `claim`,
//! `review`, `done`, `block` and `release` whether anyone is calibrating or
//! not. The whole of wsp-060 is one field joined to a workflow that was already
//! recorded.
//!
//! # What it cost, which is `core-029` and the last clause on that line
//!
//! The tier says what served an attempt and the outcome says what came of it;
//! neither says what it consumed, and a routing policy is an argument about
//! spend. So a third clause was added to the same line, in
//! [`crate::agent_commands::Spend`]'s units — read from the cache, read for the
//! first time, produced — plus a price for the kind that publishes one.
//!
//! **Both kinds are read in the same units on purpose.** opencode publishes
//! per-message `cost` and token counts; wsp derives Claude Code's from the
//! transcript and there is no price in it. Left alone that would have made
//! opencode a second, better-looking column that nothing could be set against,
//! which is the failure this row was written to avoid — so what lands is the
//! intersection: the numbers both runtimes report, with the price stated where
//! it is known and absent rather than guessed where it is not.
//!
//! Two things this record is honest about being a floor. A sub-agent's spend
//! belongs to the sub-agent — Claude's sidechains are skipped and an opencode
//! export holds one session's own messages — and an attempt closed before this
//! landed carries no clause at all, which the `unrecorded` line in the summary
//! counts out loud beside a count of how many attempts a total is a total of.
//!
//! # Why the record is on the task and not in the state directory
//!
//! Claims and bindings are written into `WSP_STATE` and are never registered in
//! the store's written set, so git never picks them up — the CORRECTION
//! decision of 2026-08-16. A tier recorded only there is machine-local, dies
//! with the seat, and is invisible to every other machine: fine for *what is
//! this pane running*, useless as evidence. Calibration data has to survive the
//! machine that produced it, so it goes in the task file, which is committed.
//!
//! # Spawned at, ran at, and why both
//!
//! `spawned at` is the intent — what somebody typed, or nothing at all, which
//! is still the ordinary case. `ran at` is the fact, read off the agent's own
//! transcript when the claim ends (`agent_commands::Kind::ran`). They are
//! separate columns because they part company, and every way they do is a way a
//! single field would have lied:
//!
//! - an agent types `/model` mid-session and finishes on a tier no flag names,
//! - a spawn states nothing and is labelled only by an unversioned settings
//!   file,
//! - `--on <machine>` runs that machine's `claude`, at whatever it has
//!   installed.
//!
//! So [`Row::tier`] prints one label when they agree and both when they do not
//! — the fact first, the intent behind it — and those are the interesting rows:
//! a tier that was asked for and not delivered.
//!
//! Either can be missing and the missing one is left blank rather than filled
//! in. A silently wrong label is worse than a missing one, because a router
//! calibrated on it learns the opposite of what happened.

use std::collections::BTreeMap;

use serde_json::json;

use crate::agent_commands::Spend;
use crate::model::Task;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// How an attempt ended, in the vocabulary the store already writes.
///
/// [`Outcome::Reopened`] is the one that is not a log line: it is *reached
/// review or done, and somebody claimed it again anyway*, which is only
/// knowable from the attempt after it. That is the whole reason attempts are
/// parsed as a sequence rather than a task at a time — rework is the signal a
/// routing policy is judged on, and it is invisible from inside the attempt
/// that was reworked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    #[default]
    Open,
    Reaped,
    Dropped,
    Blocked,
    Parked,
    Reopened,
    Review,
    Done,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Open => "open",
            Outcome::Reaped => "reaped",
            Outcome::Dropped => "dropped",
            Outcome::Blocked => "blocked",
            Outcome::Parked => "parked",
            Outcome::Reopened => "reopened",
            Outcome::Review => "review",
            Outcome::Done => "done",
        }
    }
}

/// One agent's turn at one task: from the claim to whatever came next.
///
/// **The window runs claim to claim, not claim to release**, and that is not a
/// detail. `wsp done` lets go of the claim before it marks the task finished —
/// deliberately, so the log reads in the order it happened — so an attempt
/// closed at its own release line would report `dropped` for every task that
/// was actually completed. Nobody else holds the task between one agent putting
/// it down and the next picking it up, so everything in that gap is this
/// attempt's result.
#[derive(Debug, Clone, Default)]
pub struct Attempt {
    pub task: String,
    pub title: String,
    /// When the claim was made.
    pub at: String,
    pub pane: String,
    /// `opus[1m]/high`, or empty where the spawn stated no tier — which is not
    /// a gap in the record so much as the record of an unstated spawn.
    pub spawned: String,
    /// What the transcript says served it: `opus-5/high`, or
    /// `haiku-4-5→opus-5/high` for a session that moved mid-flight.
    pub ran: String,
    /// Requests to the model, which is what a turn is and what a bill is
    /// counted in.
    pub turns: usize,
    /// The largest context any turn of it read (`peak` clause), 0 where the line
    /// carries none — every line written before wsp-290.
    pub peak: u64,
    /// What it consumed, as the release line recorded it — or read live for an
    /// attempt still running. Zero everywhere for an attempt closed before
    /// `core-029`, which is a record that was never written rather than an
    /// attempt that cost nothing; the two are told apart by [`Spend::nothing`].
    pub spend: Spend,
    /// When the agent let go, if it did.
    pub until: String,
    /// When it first reached review — the wall-clock the parent task asked for,
    /// because that is when the work was offered rather than when the agent
    /// happened to stop holding it.
    pub reviewed: String,
    pub outcome: Outcome,
    /// The leg this attempt is, for the start and end lines of a staged row's
    /// leg (`docs/legs.md`, *A leg is a seat*); `None` for a claim's attempt.
    pub leg: Option<String>,
    /// The `why` a leg's start line carries for a tier other than the default.
    pub why: Option<String>,
}

impl Attempt {
    /// Claim to review where there is one, claim to release otherwise, and
    /// claim to now for an attempt still running. Zero when the log is old
    /// enough to carry bare dates on both ends and they are the same day.
    pub fn seconds(&self) -> i64 {
        let end = match (self.reviewed.is_empty(), self.until.is_empty()) {
            (false, _) => util::epoch_of(&self.reviewed),
            (true, false) => util::epoch_of(&self.until),
            (true, true) => util::epoch_secs(),
        };
        (end - util::epoch_of(&self.at)).max(0)
    }
}

/// The tier column: one label when intent and fact agree, both when they do
/// not, and whichever exists when only one does.
///
/// Agreement is checked across two vocabularies rather than by string equality,
/// because they are two vocabularies: `opus[1m]` is what you type and
/// `opus-5` is what served, and no amount of normalising makes them the same
/// token. So the model agrees when what ran begins with the alias that was
/// asked for, and the effort agrees exactly — `default` agreeing with anything,
/// since it is the record of a half that was never stated.
fn agrees(spawned: &str, ran: &str) -> bool {
    // **From the last `/` and not the first**, which matters the moment a kind
    // has model names with slashes in them. opencode spells a model
    // `provider/model` — `opencode/x-preview-f-free` — so `spawned at` writes
    // `opencode/x-preview-f-free/default` and splitting at the first separator
    // reads the *provider* as the model and the rest as an effort. The effort
    // is always the last segment, so this is the correct reading of the format
    // rather than an accommodation of one kind, and it is identical to what
    // splitting at the first did for every label already on record: `opus[1m]`
    // and `opus-5/high` both parse the same either way.
    //
    // The one shape that would still be ambiguous is a [`crate::agent_commands::Ran`]
    // whose model contains a slash and which states no effort — `label` omits
    // the half when there is none. `core-029` is where the kind that would
    // produce one arrived, and what it does instead is write `default`: an
    // effort opencode has not got, stated rather than omitted, so that this
    // split has something to take.
    let split = |s: &str| match s.rsplit_once('/') {
        Some((m, e)) => (m.to_string(), e.to_string()),
        None => (s.to_string(), String::new()),
    };
    // A session that moved never agrees with anything, whichever end of it the
    // flag happened to name. `haiku` asked for and `haiku-4-5→opus-5` served is
    // the escalation this whole field exists to catch, and the first half
    // matching is exactly how it would hide.
    if ran.contains('→') {
        return false;
    }
    let (want_m, want_e) = split(spawned);
    let (got_m, got_e) = split(ran);
    let model = want_m == "default" || got_m.starts_with(want_m.strip_suffix("[1m]").unwrap_or(&want_m));
    let effort = want_e.is_empty() || want_e == "default" || got_e == want_e;
    model && effort
}

/// One line of the report, and the shape `--json` hands out.
pub struct Row(pub Attempt);

impl Row {
    pub fn tier(&self) -> String {
        let a = &self.0;
        match (a.spawned.is_empty(), a.ran.is_empty()) {
            (true, true) => String::new(),
            (false, true) => a.spawned.clone(),
            (true, false) => a.ran.clone(),
            (false, false) if agrees(&a.spawned, &a.ran) => a.ran.clone(),
            // The fact first and the intent behind it, in the log's own clause
            // shape. Not `asked → ran`, which is what this said first: `→` is
            // already how a session that moved mid-flight is written, and
            // `haiku/medium → haiku-4-5→opus-5/medium→high` is two arrows
            // meaning two different things in one column.
            (false, false) => format!("{} · asked {}", a.ran, a.spawned),
        }
    }
}

/// What a task's attempts recorded, in total: tokens read and produced, and
/// the price **only where every recording attempt's runtime stated one**.
///
/// `None` when nothing was recorded — no attempt ever wrote a spend clause —
/// which is the ordinary case for a member nobody has spawned yet. The price is
/// the part that cannot be filled in: `core-045` says a turn that died
/// upstream records as one that ran without a clause at all, so a sum over the
/// clauses is a floor on what was spent and never the bill. A stated zero
/// survives — [`crate::agent_commands::Spend::micros`] holds `Some(0)` for a
/// free model, and that is a fact the runtime stated rather than one computed
/// here — but a mix of priced and unpriced attempts answers `None` rather than
/// a number that quietly means *the priced ones only*.
///
/// This is the reading the worklists section draws at page width, which is why
/// it lives here beside the parser and not at the call site: a format with its
/// summariser in another file drifts.
pub fn recorded(t: &Task) -> Option<(u64, Option<u64>)> {
    let mut tokens = 0u64;
    let mut micros = 0u64;
    let mut any = false;
    let mut all_priced = true;
    for a in attempts_of(t) {
        let s = a.spend;
        if s.nothing() {
            continue;
        }
        any = true;
        tokens += s.cache + s.input + s.output;
        match s.micros {
            Some(m) => micros += m,
            None => all_priced = false,
        }
    }
    any.then_some((tokens, all_priced.then_some(micros)))
}

/// Every attempt in one task's log, oldest first.
///
/// Reads the two clauses this task added and the five lines the store was
/// already writing, and nothing else. A log line it does not recognise is a log
/// line: notes, renames, moves and hand-written direction all fall through, and
/// have to, because the log is prose that people write in.
pub fn attempts_of(t: &Task) -> Vec<Attempt> {
    let Some(log) = t.section("Log") else { return Vec::new() };
    let mut out: Vec<Attempt> = Vec::new();
    for line in log.lines() {
        let Some((stamp, rest)) = dated(line) else { continue };
        // Clauses are `·`-separated and the sentence is the first of them, so
        // the two records this task writes are read off the end without the
        // sentence in front of them having to be parsed for them.
        let mut parts = rest.split(" · ");
        let head = parts.next().unwrap_or("").trim();
        // A leg's two lines, which carry the claim and release lines' clauses
        // under the leg's name (`leg_seat`). **Paired by that name** and not
        // with the last open claim: two siblings are open at once and their
        // lines interleave, so the last one open is as likely the other's.
        if let Some((name, said)) = head.strip_prefix("leg ").and_then(|h| h.split_once(' ')) {
            if let Some(who) = said.strip_prefix("started by pane ") {
                let mut a = Attempt {
                    task: t.id.clone(),
                    title: t.title.clone(),
                    at: stamp.to_string(),
                    pane: who.trim().to_string(),
                    outcome: Outcome::Open,
                    leg: Some(name.to_string()),
                    ..Attempt::default()
                };
                for clause in parts {
                    let clause = clause.trim();
                    if let Some(tier) = clause.strip_prefix("spawned at ") {
                        a.spawned = tier.trim().to_string();
                    } else if let Some(why) = clause.strip_prefix("why ") {
                        a.why = Some(why.trim().to_string());
                    }
                }
                out.push(a);
                continue;
            }
            if said.starts_with("ended") {
                let open = out.iter_mut().rev().find(|a| a.leg.as_deref() == Some(name) && a.until.is_empty());
                if let Some(a) = open {
                    a.until = stamp.to_string();
                    a.outcome = Outcome::Done;
                    ran_clauses(a, parts);
                }
                continue;
            }
        }
        if let Some(who) = head.strip_prefix("claimed by pane ") {
            let mut a = Attempt {
                task: t.id.clone(),
                title: t.title.clone(),
                at: stamp.to_string(),
                pane: who.split(',').next().unwrap_or(who).trim().to_string(),
                outcome: Outcome::Open,
                ..Attempt::default()
            };
            for clause in parts {
                if let Some(tier) = clause.trim().strip_prefix("spawned at ") {
                    a.spawned = tier.trim().to_string();
                }
            }
            out.push(a);
            continue;
        }
        let Some(a) = out.iter_mut().rev().find(|a| a.leg.is_none()) else { continue };
        if head.starts_with("released") || head.starts_with("handed off to") {
            a.until = stamp.to_string();
            ran_clauses(a, parts);
        // Both spellings. `review` takes an account and logs `review: …`
        // beside `blocked:` and `parked:`; every row logged before that says
        // `→ review`, and an attempt record that stopped reading them would
        // lose the outcome of every attempt in the store's history.
        } else if head == "→ review" || head.starts_with("review:") {
            a.reviewed = stamp.to_string();
            a.outcome = Outcome::Review;
        } else if head == "→ done" {
            a.outcome = Outcome::Done;
        } else if head.starts_with("blocked:") || head == "blocked" {
            a.outcome = Outcome::Blocked;
        } else if head.starts_with("parked:") {
            a.outcome = Outcome::Parked;
        }
    }
    // An attempt that got somewhere and was then claimed again did not finish
    // the work, whatever its own last line said. This is the rework signal, and
    // it can only be written from here. Claims only: a leg that is done is done,
    // and the leg a blocking verdict appends is a leg of its own.
    let claims: Vec<usize> = (0..out.len()).filter(|&i| out[i].leg.is_none()).collect();
    for w in claims.windows(2) {
        if matches!(out[w[0]].outcome, Outcome::Review | Outcome::Done) {
            out[w[0]].outcome = Outcome::Reopened;
        }
    }
    // Everything that let go without reaching anything. Left until last so the
    // sequence rule above still sees what each attempt actually reached.
    for a in out.iter_mut().filter(|a| a.leg.is_none()) {
        if a.outcome == Outcome::Open && !a.until.is_empty() {
            a.outcome = Outcome::Dropped;
        }
    }
    out
}

/// The clauses a release line and a leg's end line share: the tier that ran, the
/// turns, the peak context, the spend, and the workspace closing under it.
fn ran_clauses<'a>(a: &mut Attempt, clauses: impl Iterator<Item = &'a str>) {
    for clause in clauses {
        let clause = clause.trim();
        if let Some(tier) = clause.strip_prefix("ran ") {
            a.ran = tier.to_string();
        } else if let Some(n) = clause.strip_suffix(" turns") {
            a.turns = n.parse().unwrap_or(0);
        } else if let Some(n) = clause.strip_prefix("peak ") {
            a.peak = util::count_of(n).unwrap_or(0);
        } else if let Some(spend) = Spend::of_clause(clause) {
            a.spend = spend;
        } else if clause == "workspace closed" {
            // Not `dropped`. The agent did not put this down and may
            // never have known it had stopped holding it, so counting
            // it against the tier that ran it is the one systematic
            // mislabel this whole record exists to avoid.
            a.outcome = Outcome::Reaped;
        }
    }
}

/// `- <stamp> <rest>`, which is the one shape `Task::log` writes. Anything else
/// on a line is somebody's prose and is stepped over, the same rule
/// `cmd_agent::blocked_question` reads by.
fn dated(line: &str) -> Option<(&str, &str)> {
    let l = line.trim().strip_prefix("- ")?;
    let (stamp, rest) = l.split_once(' ')?;
    util::is_stamp(stamp).then_some((stamp, rest))
}

/// What the report is about: one task, one project's subtree, or the project
/// the caller is standing in.
///
/// Done and finished work is *included*, and that is the point rather than an
/// oversight — a finished task is the only kind that has an outcome worth
/// calibrating against. `--all` is the escape hatch for the whole store.
fn wanted(store: &Store, args: &Args) -> Result<Vec<Task>, String> {
    let tasks = store.tasks();
    if args.has("all") {
        return Ok(tasks);
    }
    let index = Index::new(store.projects());
    if let Some(needle) = args.rest.first() {
        if let Some(t) = store.find_task(needle) {
            return Ok(vec![t]);
        }
        let Some(p) = index.find(needle) else {
            return Err(format!("no task or project matching `{needle}`"));
        };
        let under = index.subtree(&p.id);
        return Ok(tasks
            .into_iter()
            .filter(|t| t.project.as_ref().is_some_and(|p| under.contains(p)))
            .collect());
    }
    let here = match crate::cmd_agent::current_project(store, args, &index) {
        Ok(Some(p)) => p,
        _ => return Err("no project here — name one, or pass --all".into()),
    };
    let under = index.subtree(&here);
    Ok(tasks
        .into_iter()
        .filter(|t| t.project.as_ref().is_some_and(|p| under.contains(p)))
        .collect())
}

/// What one tier produced, over every attempt served at it.
///
/// Keyed on what **ran** rather than on what was asked for, because the
/// question a routing policy is calibrated against is what a tier delivers, and
/// an attempt that was asked for haiku and served by opus is evidence about
/// opus. Attempts with no reading at all are counted under `unrecorded`, out
/// loud, because a summary that quietly dropped them would make the record look
/// more complete than it is.
#[derive(Default)]
struct Tally {
    n: usize,
    by: BTreeMap<Outcome, usize>,
    clocks: Vec<i64>,
    /// Everything this tier read and produced, added up.
    ///
    /// A **sum** where the clock is a median, and the two answer different
    /// questions on purpose: the middle of the distribution is what says how
    /// long a tier takes, and the total is what says what it cost — which is
    /// the number `core-012` is accounting for and the one a capacity argument
    /// is made of. A median bill would answer neither.
    ///
    /// Attempts closed before `core-029` contribute nothing, so a total is a
    /// floor rather than a figure, and the `unrecorded` row beside it is how
    /// far from the ceiling it might be.
    spend: Spend,
    /// Whether anything in this tier published a price at all. Without it a
    /// tier of Claude attempts would total `$0` and read as free.
    priced: bool,
    /// How many of the attempts the spend was actually read for.
    ///
    /// Printed whenever it is fewer than all of them, because a total under a
    /// count of 126 attempts reads as the whole of them and until `core-029`
    /// none of them recorded anything. The number says how much of the tier
    /// the total is a total of.
    measured: usize,
}

impl Tally {
    fn add(&mut self, a: &Attempt) {
        self.n += 1;
        *self.by.entry(a.outcome).or_default() += 1;
        let secs = a.seconds();
        if secs > 0 && a.outcome != Outcome::Open {
            self.clocks.push(secs);
        }
        if !a.spend.nothing() {
            self.measured += 1;
        }
        self.spend.cache += a.spend.cache;
        self.spend.input += a.spend.input;
        self.spend.output += a.spend.output;
        if let Some(m) = a.spend.micros {
            self.priced = true;
            self.spend.micros = Some(self.spend.micros.unwrap_or(0) + m);
        }
    }

    /// The median rather than the mean: one attempt that sat overnight waiting
    /// for a person moves a mean by hours and says nothing about the tier.
    fn median(&mut self) -> Option<i64> {
        if self.clocks.is_empty() {
            return None;
        }
        self.clocks.sort_unstable();
        Some(self.clocks[self.clocks.len() / 2])
    }
}

/// The tier of an attempt whose claim has not ended, read off the transcript
/// now.
///
/// The one reading here that is not out of the log, and it is not an exception
/// to the rule that evidence lives on the task — it is the same rule. An
/// unfinished attempt *has* no closing line, because the agent is still in the
/// seat, so there is nothing on the task to read and no possibility of there
/// being any. When it ends, `hand_off` writes what this is reading and the row
/// stops being live.
///
/// Which makes it the question a person actually asks about a running agent —
/// *what is that pane running* — answered from the record rather than by
/// walking over to look, and it is answerable for the same reason `wsp resume`
/// works: the binding carries the session.
///
/// Skipped for a claim held on another machine. A transcript is a fact about
/// one machine's disk, so the honest answer from here is silence rather than a
/// scan that could only ever come back empty.
fn live(store: &Store, a: &mut Attempt) {
    if !a.ran.is_empty() {
        return;
    }
    let here = util::hostname();
    let host = store
        .claims()
        .get(&a.task)
        .and_then(|c| c.get("host"))
        .and_then(|h| h.as_str())
        .unwrap_or_default()
        .to_string();
    if !host.is_empty() && host != here {
        return;
    }
    let Some(thread) = crate::cmd_resume::thread_for_task(store, &a.task) else { return };
    let read = crate::agent_commands::of(&thread.kind).ran(
        &thread.session,
        &thread.cwd,
        util::epoch_of(&a.at),
    );
    if let Some(ran) = read {
        a.turns = ran.turns;
        a.peak = ran.peak;
        a.ran = ran.label();
        a.spend = ran.spend;
    }
}

/// The `attempts --json` objects. The token and dollar keys are exact, from
/// `cmd_burn::reading(pane)` (live burn, else the stop ledger), when the pane
/// served only one attempt among `rows`: then `input_tokens` is fresh input
/// alone, `cache_write_tokens` is kept apart, and `exact` is true. A pane that
/// served two attempts, or has no reading, keeps the log clause's rounded
/// figures (input there includes cache writes) and says `exact: false`.
fn attempts_json(rows: &[Attempt]) -> Vec<serde_json::Value> {
    attempts_json_with(rows, &|pane| crate::cmd_burn::reading(pane))
}

fn attempts_json_with(
    rows: &[Attempt],
    read: &dyn Fn(&str) -> Option<serde_json::Value>,
) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|a| {
            let alone = !a.pane.is_empty() && rows.iter().filter(|b| b.pane == a.pane).count() == 1;
            let exact = if alone { read(&a.pane) } else { None };
            let num = |k: &str| exact.as_ref().and_then(|r| r.get(k)).and_then(|v| v.as_u64());
            let (input, output, cache, write, micros) = match &exact {
                Some(_) => (
                    json!(num("input")),
                    json!(num("output")),
                    json!(num("cache_read")),
                    json!(num("cache_write")),
                    json!(num("cost")),
                ),
                None => (
                    json!(a.spend.input),
                    json!(a.spend.output),
                    json!(a.spend.cache),
                    json!(null),
                    json!(a.spend.micros),
                ),
            };
            json!({
                "task": a.task,
                "role": "member",
                "leg": a.leg,
                "title": a.title,
                "at": a.at,
                "pane": a.pane,
                "spawned_at": a.spawned,
                "ran_at": a.ran,
                "turns": match num("turns") { Some(t) => t as usize, None => a.turns },
                "peak_tokens": a.peak,
                "cache_tokens": cache,
                "input_tokens": input,
                "output_tokens": output,
                "cache_write_tokens": write,
                "usd_micros": micros,
                "exact": exact.is_some(),
                "released_at": a.until,
                "reviewed_at": a.reviewed,
                "seconds": a.seconds(),
                "outcome": a.outcome.as_str(),
            })
        })
        .collect()
}

pub fn attempts(store: &Store, args: &Args) -> i32 {
    if args.has("cost") && args.get("list").is_some_and(|l| l != "true") {
        return cost_report(store, args, Vec::new());
    }
    let tasks = match wanted(store, args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    if args.has("cost") {
        return cost_report(store, args, tasks);
    }
    let mut rows: Vec<Attempt> = tasks.iter().flat_map(|t| attempts_of(t)).collect();
    rows.sort_by(|a, b| a.at.cmp(&b.at).then(a.task.cmp(&b.task)));
    // Every attempt that has not *ended*, which is not the same set as the ones
    // still `open`: an agent that has put its work up for review and is holding
    // the claim while somebody reads it has reached an outcome and written
    // nothing down, because the clause is written when the claim ends. There
    // are usually a few of those, and they are the most recent evidence there
    // is.
    // A leg's seat is its own session, found by the seat; `live` reads the task's
    // and would price the wrong one.
    for a in rows.iter_mut().filter(|a| a.until.is_empty() && a.leg.is_none()) {
        live(store, a);
    }

    if args.json() {
        let mut out = attempts_json(&rows);
        for t in &tasks {
            out.extend(crate::cmd_cost::verifier_rows(t));
        }
        println!("{}", json!({ "attempts": out }));
        return 0;
    }

    let p = Paint::new();
    if rows.is_empty() {
        println!("no attempts recorded here yet");
        return 0;
    }

    // A leg is the row's, named `<task>.<leg>` as its tree is (`leg_tree`).
    let named = |a: &Attempt| match &a.leg {
        Some(l) => format!("{}.{l}", a.task),
        None => a.task.clone(),
    };
    let w_task = rows.iter().map(|a| named(a).len()).max().unwrap_or(4);
    let w_tier = rows
        .iter()
        .map(|a| Row(a.clone()).tier().chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    // The price column is there only when something published a price. Claude
    // Code publishes none, so on a store of Claude attempts an empty `cost`
    // would be a column of blanks implying wsp failed to read something.
    let priced = rows.iter().any(|a| a.spend.micros.is_some());
    // A header, which this table did not have and now needs: `692  21M  3.5M`
    // is three numbers a reader has to be told apart, and the alternative is
    // spelling the unit into every cell of every row.
    println!(
        "{}",
        p.dim(&format!(
            "{}  {}  {}  {}  {}  {}{}  {}",
            util::pad("date", 10),
            util::pad("task", w_task),
            util::pad("tier", w_tier),
            // `turns` and not `reqs`: one word for one thing, the same one the
            // log line and [`crate::agent_commands::Ran`] use. What it counts
            // is API requests — see there — and a report with a second name
            // for it would be two vocabularies for one number.
            util::pad("turns", 5),
            util::pad("read", 6),
            util::pad("out", 6),
            match priced {
                true => format!("  {}", util::pad("cost", 8)),
                false => String::new(),
            },
            util::pad("clock", 6),
        ))
    );
    for a in &rows {
        let tier = Row(a.clone()).tier();
        let turns = match a.turns {
            0 => String::new(),
            n => n.to_string(),
        };
        // What the model read, cache and all, in one number: the split between
        // the cheap half and the dear one is what the record keeps, and what a
        // person scanning a report wants is how much context this attempt got
        // through.
        let read = match a.spend.cache + a.spend.input {
            0 => String::new(),
            n => util::count_human(n),
        };
        let out = match a.spend.output {
            0 => String::new(),
            n => util::count_human(n),
        };
        let cost = match (priced, a.spend.micros) {
            (false, _) => String::new(),
            (true, None) => format!("  {}", util::pad("", 8)),
            (true, Some(m)) => format!("  {}", util::pad(&util::money_human(m), 8)),
        };
        println!(
            "{}  {}  {}  {}  {}  {}{}  {}  {}",
            p.dim(&util::local_ymd(&a.at)),
            p.bold(&util::pad(&named(a), w_task)),
            util::pad(&tier, w_tier),
            p.dim(&util::pad(&turns, 5)),
            p.dim(&util::pad(&read, 6)),
            p.dim(&util::pad(&out, 6)),
            p.dim(&cost),
            p.dim(&util::pad(&util::duration_human(a.seconds()), 6)),
            outcome_colour(&p, a.outcome),
        );
    }

    // The summary is the whole reason the rows are gathered across tasks: one
    // attempt says nothing about a tier, and a routing policy is an argument
    // about the middle of a distribution.
    let mut by_tier: BTreeMap<String, Tally> = BTreeMap::new();
    for a in &rows {
        let key = match a.ran.is_empty() {
            true => "unrecorded".to_string(),
            false => a.ran.clone(),
        };
        by_tier.entry(key).or_default().add(a);
    }
    println!();
    let w_key = by_tier.keys().map(|k| k.chars().count()).max().unwrap_or(4);
    for (tier, tally) in by_tier.iter_mut() {
        let mut parts: Vec<String> = vec![format!("{} attempt{}", tally.n, plural(tally.n))];
        for (outcome, n) in &tally.by {
            parts.push(format!("{n} {}", outcome.as_str()));
        }
        if let Some(m) = tally.median() {
            parts.push(format!("median {}", util::duration_human(m)));
        }
        if tally.measured > 0 && tally.measured < tally.n {
            parts.push(format!("{} measured", tally.measured));
        }
        let read = tally.spend.cache + tally.spend.input;
        if read > 0 {
            parts.push(format!("{} read", util::count_human(read)));
        }
        if tally.spend.output > 0 {
            parts.push(format!("{} out", util::count_human(tally.spend.output)));
        }
        if let Some(m) = tally.spend.micros.filter(|_| tally.priced) {
            parts.push(util::money_human(m));
        }
        println!("{}  {}", p.bold(&util::pad(tier, w_key)), p.dim(&parts.join(" · ")));
    }
    0
}

// ---- what a row cost: `wsp attempts <row> --cost` ---------------------------

/// What a recorded spend cost in dollars, in micro-dollars.
///
/// The runtime's own price where it states one (opencode). Otherwise the tokens
/// the record kept, priced at [`crate::cmd_burn::PRICES`] on the model that ran:
/// the log keeps only a total, so a session that moved tier mid-flight
/// (`haiku-4-5→opus-5`) is priced at the **dearest** it named, which cannot make
/// the attempt look cheap. **A floor, too:** the record's `in` is input plus cache
/// writes, and the table cannot tell them apart, so a write is priced at the
/// input rate rather than a quarter above it. `None` where nothing was recorded,
/// and where the model is not one the table knows (an opencode model that
/// published no price is not Fable).
pub fn priced(ran: &str, s: &Spend) -> Option<u64> {
    if let Some(m) = s.micros {
        return Some(m);
    }
    if s.nothing() {
        return None;
    }
    let models = ran.rsplit_once('/').map_or(ran, |(m, _)| m);
    let mut dearest: Option<u64> = None;
    for m in models.split('→') {
        let id = format!("claude-{m}");
        if !crate::cmd_burn::known(&id) {
            return None;
        }
        let c = crate::cmd_burn::cost(&id, s.input, s.output, s.cache, 0);
        dearest = Some(dearest.map_or(c, |d| d.max(c)));
    }
    dearest
}

/// One line of a row's cost: a claim's attempt, a leg, or a verifier's pass.
#[derive(Debug, Clone, PartialEq)]
pub struct CostLine {
    /// `worker`, `rework`, `leg` or `verifier`.
    pub kind: &'static str,
    pub leg: Option<String>,
    /// A leg that waits on more than one, whose turns are the merge's as much as
    /// the work's.
    pub join: bool,
    pub at: String,
    pub tier: String,
    pub turns: usize,
    pub peak: u64,
    pub micros: Option<u64>,
    pub seconds: i64,
}

impl CostLine {
    fn what(&self) -> String {
        let name = match &self.leg {
            Some(l) => format!("leg {l}"),
            None => self.kind.to_string(),
        };
        match self.join {
            true => format!("{name} (join)"),
            false => name,
        }
    }
}

/// A row's cost, line by line, with the two clocks a staged row has.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowCost {
    pub task: String,
    pub title: String,
    pub lines: Vec<CostLine>,
    /// First leg start to last leg end, and `None` for a row with no legs.
    pub wall: Option<i64>,
    /// The legs' own times added: what the row would have taken one after
    /// another. Beside `wall`, the difference is what overlap bought.
    pub legs_sum: i64,
    /// Legs not on the default tier: leg, the tier that ran it or was asked for,
    /// and the `why:` its entry carries.
    pub off_default: Vec<(String, String, String)>,
}

impl RowCost {
    pub fn total(&self) -> Option<u64> {
        self.lines.iter().filter_map(|l| l.micros).reduce(|a, b| a + b)
    }
    pub fn unmeasured(&self) -> usize {
        self.lines.iter().filter(|l| l.micros.is_none()).count()
    }
    pub fn turns(&self) -> usize {
        self.lines.iter().map(|l| l.turns).sum()
    }
    pub fn join_turns(&self) -> usize {
        self.lines.iter().filter(|l| l.join).map(|l| l.turns).sum()
    }
    pub fn peak(&self) -> u64 {
        self.lines.iter().map(|l| l.peak).max().unwrap_or(0)
    }
}

/// A row's attempts, its legs' and its verifiers' passes, as a cost.
///
/// Everything is read off the row: the log's claim, release and leg lines, the
/// `## Legs` record for what waits on what and which tier needed a reason, and
/// the `## Verification` passes' spend clauses. A pass whose seat has not been
/// ended carries none and has no line.
pub fn row_cost(t: &Task) -> RowCost {
    let legs = crate::legs::legs(t);
    let waits = crate::legs::waits(&legs);
    let mut out = RowCost { task: t.id.clone(), title: t.title.clone(), ..RowCost::default() };
    let mut claims = 0;
    for a in attempts_of(t) {
        let (kind, join) = match &a.leg {
            Some(name) => {
                let i = legs.iter().position(|l| &l.name == name);
                let rework = i.is_some_and(|i| legs[i].rework.is_some());
                (if rework { "rework" } else { "leg" }, i.is_some_and(|i| waits[i].len() > 1))
            }
            None => {
                claims += 1;
                (if claims == 1 { "worker" } else { "rework" }, false)
            }
        };
        out.lines.push(CostLine {
            kind,
            leg: a.leg.clone(),
            join,
            at: a.at.clone(),
            tier: Row(a.clone()).tier(),
            turns: a.turns,
            peak: a.peak,
            micros: priced(&a.ran, &a.spend),
            seconds: a.seconds(),
        });
    }
    for p in crate::verification::passes(t) {
        let Some(s) = p.spent else { continue };
        let seconds = p.decided.as_deref().map_or(0, |d| (util::epoch_of(d) - util::epoch_of(&p.at)).max(0));
        out.lines.push(CostLine {
            kind: "verifier",
            leg: None,
            join: false,
            at: p.at.clone(),
            micros: priced(&s.ran, &s.spend),
            tier: s.ran,
            turns: s.turns,
            peak: s.peak,
            seconds,
        });
    }
    out.lines.sort_by(|a, b| a.at.cmp(&b.at));
    let ran: Vec<Attempt> = attempts_of(t).into_iter().filter(|a| a.leg.is_some() && !a.until.is_empty()).collect();
    out.legs_sum = ran.iter().map(Attempt::seconds).sum();
    let first = ran.iter().map(|a| util::epoch_of(&a.at)).min();
    let last = ran.iter().map(|a| util::epoch_of(&a.until)).max();
    out.wall = first.zip(last).map(|(f, l)| (l - f).max(0));
    for l in legs.iter().filter(|l| !crate::legs::is_default(&l.policy)) {
        let asked = [&l.policy.model, &l.policy.effort].into_iter().flatten().cloned().collect::<Vec<_>>().join("/");
        let tier = out.lines.iter().find(|c| c.leg.as_deref() == Some(&l.name)).map(|c| c.tier.clone()).filter(|t| !t.is_empty());
        out.off_default.push((l.name.clone(), tier.unwrap_or(asked), l.why.clone().unwrap_or_default()));
    }
    out
}

fn cost_json(rc: &RowCost) -> serde_json::Value {
    json!({
        "task": rc.task,
        "title": rc.title,
        "lines": rc.lines.iter().map(|l| json!({
            "kind": l.kind,
            "leg": l.leg,
            "join": l.join,
            "at": l.at,
            "tier": l.tier,
            "turns": l.turns,
            "peak_tokens": l.peak,
            "usd_micros": l.micros,
            "seconds": l.seconds,
        })).collect::<Vec<_>>(),
        "total": {
            "usd_micros": rc.total(),
            "turns": rc.turns(),
            "peak_tokens": rc.peak(),
            "join_turns": rc.join_turns(),
            "unmeasured": rc.unmeasured(),
        },
        "wall_seconds": rc.wall,
        "legs_seconds": rc.legs_sum,
        "off_default": rc.off_default.iter().map(|(leg, tier, why)| json!({ "leg": leg, "tier": tier, "why": why })).collect::<Vec<_>>(),
    })
}

fn money(m: Option<u64>) -> String {
    m.map(util::money_human).unwrap_or_default()
}

/// A row's table, ready to print.
fn cost_table(p: &Paint, rc: &RowCost) -> String {
    let mut out = vec![format!("{}  {}", p.bold(&rc.task), p.dim(&rc.title))];
    let w_what = rc.lines.iter().map(|l| l.what().chars().count()).max().unwrap_or(4).max(5);
    let w_tier = rc.lines.iter().map(|l| l.tier.chars().count()).max().unwrap_or(4).max(4);
    out.push(p.dim(&format!(
        "  {}  {}  {}  {}  {}  {}",
        util::pad("what", w_what),
        util::pad("tier", w_tier),
        util::pad("turns", 5),
        util::pad("peak", 6),
        util::pad("cost", 8),
        util::pad("clock", 6)
    )));
    let num = |n: u64| if n == 0 { String::new() } else { util::count_human(n) };
    for l in &rc.lines {
        let turns = if l.turns == 0 { String::new() } else { l.turns.to_string() };
        out.push(format!(
            "  {}  {}  {}  {}  {}  {}",
            util::pad(&l.what(), w_what),
            util::pad(&l.tier, w_tier),
            p.dim(&util::pad(&turns, 5)),
            p.dim(&util::pad(&num(l.peak), 6)),
            util::pad(&money(l.micros), 8),
            p.dim(&util::pad(&util::duration_human(l.seconds), 6)),
        ));
    }
    let mut tail = vec![format!("{} turns", rc.turns())];
    if rc.peak() > 0 {
        tail.push(format!("peak {}", util::count_human(rc.peak())));
    }
    if rc.unmeasured() > 0 {
        tail.push(format!("{} of {} unmeasured", rc.unmeasured(), rc.lines.len()));
    }
    out.push(format!("  {}  {}  {}", p.bold(&util::pad("total", w_what + 2 + w_tier + 2 + 5 + 2 + 6)), p.bold(&util::pad(&money(rc.total()), 8)), p.dim(&tail.join(" · "))));
    if let Some(wall) = rc.wall {
        let mut line = format!("  wall {} beside legs summed {}", util::duration_human(wall), util::duration_human(rc.legs_sum));
        if rc.join_turns() > 0 {
            line.push_str(&format!(" · join legs {} of {} turns", rc.join_turns(), rc.turns()));
        }
        out.push(p.dim(&line));
    }
    for (leg, tier, why) in &rc.off_default {
        let why = if why.is_empty() { "no why: recorded".to_string() } else { crate::cmd_task::fold(why) };
        out.push(p.dim(&format!("  leg {leg} on {tier} — {why}")));
    }
    out.join("\n")
}

/// `wsp attempts <row> --cost` and `--cost --list <worklist>`: one table a row.
fn cost_report(store: &Store, args: &Args, tasks: Vec<Task>) -> i32 {
    let tasks = match args.get("list").filter(|l| l != "true") {
        None => tasks,
        Some(name) => {
            let Some(w) = store.worklist(&name) else {
                eprintln!("wsp: no worklist {name}");
                return 2;
            };
            let ids: Vec<String> = w.groups().into_iter().flat_map(|g| g.members).collect();
            ids.iter().filter_map(|id| store.find_task(id)).collect()
        }
    };
    let rows: Vec<RowCost> = tasks.iter().map(row_cost).filter(|r| !r.lines.is_empty()).collect();
    let total = rows.iter().filter_map(RowCost::total).reduce(|a, b| a + b);
    if args.json() {
        println!("{}", json!({ "rows": rows.iter().map(cost_json).collect::<Vec<_>>(), "usd_micros": total }));
        return 0;
    }
    if rows.is_empty() {
        println!("nothing recorded to cost here yet");
        return 0;
    }
    let p = Paint::new();
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}", cost_table(&p, r));
    }
    if rows.len() > 1 {
        println!();
        println!("{}  {}", p.bold(&format!("{} rows", rows.len())), p.bold(&money(total)));
    }
    println!();
    println!("{}", p.dim(&format!("priced at Anthropic list prices read {}; a cache write is counted at the input rate, so a total is a floor", crate::cmd_burn::PRICES_READ)));
    0
}

fn plural(n: usize) -> &'static str {
    match n {
        1 => "",
        _ => "s",
    }
}

/// Green for work that stuck, yellow for work that came back, red for work that
/// stopped. The colours are the summary a person reads before the numbers.
fn outcome_colour(p: &Paint, o: Outcome) -> String {
    match o {
        Outcome::Done => p.green(o.as_str()),
        Outcome::Review => p.cyan(o.as_str()),
        Outcome::Reopened => p.yellow(o.as_str()),
        Outcome::Blocked => p.red(o.as_str()),
        _ => p.dim(o.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(log: &[&str]) -> Task {
        let mut t = Task::new("a task", "t-1");
        t.body = format!("## Log\n{}\n", log.join("\n"));
        t
    }

    /// wsp-264: a pane with a reading gives exact fields, cache write apart; a
    /// pane two attempts shared, or with no reading, says `exact: false`.
    #[test]
    fn an_attempt_with_a_reading_is_exact_and_a_shared_pane_is_not() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane cpd-1 · spawned at sonnet/high",
            "- 2026-08-18T10:00:00Z released after 1h · ran sonnet-5-5/high · 9 turns · 5.00M cache 1.50M in 60k out",
            "- 2026-08-18T11:00:00Z claimed by pane cpd-2",
            "- 2026-08-18T12:00:00Z released after 1h · ran sonnet-5-5/high · 4 turns",
            "- 2026-08-18T13:00:00Z claimed by pane cpd-2",
            "- 2026-08-18T14:00:00Z released after 1h · ran sonnet-5-5/high · 4 turns",
        ]);
        let rows = attempts_of(&t);
        let burn = json!({ "turns": 9, "input": 1_234_567, "output": 59_999, "cache_read": 4_999_999,
            "cache_write": 265_433, "cost": 777_001 });
        let out = attempts_json_with(&rows, &|pane| (pane == "cpd-1" || pane == "cpd-2").then(|| burn.clone()));
        assert_eq!(out.len(), 3);
        let a = &out[0];
        assert_eq!(a["exact"], true);
        assert_eq!(
            (a["input_tokens"].as_u64(), a["output_tokens"].as_u64(), a["cache_tokens"].as_u64(),
             a["cache_write_tokens"].as_u64(), a["usd_micros"].as_u64()),
            (Some(1_234_567), Some(59_999), Some(4_999_999), Some(265_433), Some(777_001))
        );
        for shared in &out[1..] {
            assert_eq!(shared["exact"], false, "two attempts, one pane");
            assert!(shared["cache_write_tokens"].is_null());
        }
        let none = attempts_json_with(&rows[..1], &|_| None);
        assert_eq!(none[0]["exact"], false);
    }

    /// The two clauses this task added, read back off the lines they were
    /// written onto — and the fact that they are clauses on existing lines and
    /// not lines of their own, which is what keeps them out of `wsp brief`'s
    /// four-line budget.
    #[test]
    fn an_attempt_is_the_tier_it_was_asked_for_beside_the_tier_that_served_it() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1 · spawned at haiku/high",
            "- 2026-08-18T10:00:00Z → review",
            "- 2026-08-18T10:01:00Z released after 1h1m · ran opus-5/high · 96 turns",
            "- 2026-08-18T10:02:00Z → done",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0].spawned, "haiku/high");
        assert_eq!(a[0].ran, "opus-5/high");
        assert_eq!(a[0].turns, 96);
        assert_eq!(a[0].outcome, Outcome::Done);
        assert_eq!(
            Row(a[0].clone()).tier(),
            "opus-5/high · asked haiku/high",
            "a tier asked for and not delivered is the row worth seeing"
        );
    }

    /// `review` grew an account and logs `review: …` for it. Every attempt in
    /// the store's history was logged with `→ review`, and a record that read
    /// only the new spelling would report those as having reached nothing —
    /// the outcome column is what `wsp attempts` is for.
    #[test]
    fn an_attempt_reaches_review_by_either_spelling_the_verb_has_had() {
        for line in ["→ review", "review: the gate is on the account, not on what landed"] {
            let t = task(&[
                "- 2026-08-25T09:00:00Z claimed by pane w1:p1 · spawned at opus/high",
                &format!("- 2026-08-25T10:00:00Z {line}"),
            ]);
            let a = attempts_of(&t);
            assert_eq!(a[0].outcome, Outcome::Review, "{line}");
            assert_eq!(a[0].reviewed, "2026-08-25T10:00:00Z", "{line}");
        }
    }

    /// A model name with a separator in it is not a corner case: it is how the
    /// second kind wsp knows spells every model it has.
    #[test]
    fn a_model_named_provider_slash_model_is_not_read_as_a_provider_at_default_effort() {
        let t = task(&[
            "- 2026-08-21T09:00:00Z claimed by pane w1:p1 · spawned at opencode/x-preview-f-free/default",
            "- 2026-08-21T09:30:00Z released after 30m",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(
            Row(a[0].clone()).tier(),
            "opencode/x-preview-f-free/default",
            "the whole tier is printed back, whatever is inside the model half"
        );
        // The reason this test exists: the effort is the *last* segment, and
        // reading it as the first made `x-preview-f-free/default` an effort.
        assert!(
            agrees("opencode/x-preview-f-free/default", "opencode/x-preview-f-free/default"),
            "a tier agrees with itself, which splitting at the first separator broke"
        );
    }

    /// What the attempt cost, off the line that closed it. `core-029`: the
    /// third clause on a line that was already being written, and the only
    /// durable copy there is — the transcript it was read from is one
    /// machine's disk and is cleaned up on its own schedule.
    #[test]
    fn an_attempt_carries_what_it_read_and_what_it_produced() {
        let t = task(&[
            "- 2026-08-22T09:00:00Z claimed by pane w1:p1 · spawned at opus[1m]/high",
            "- 2026-08-22T11:00:00Z released after 2h · ran opus-5/high · 96 turns · 242M cache 4.7M in 875k out",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a[0].turns, 96, "requests, which is what a bill is counted in");
        assert_eq!(a[0].spend.cache, 242_000_000);
        assert_eq!(a[0].spend.input, 4_700_000);
        assert_eq!(a[0].spend.output, 875_000);
        assert_eq!(a[0].spend.micros, None, "Claude Code publishes no price");
    }

    /// The other kind, whose runtime publishes what it charged. Both columns
    /// are read off the same clause in the same units, which is the whole
    /// point of `core-029` — an opencode attempt that nobody can set against a
    /// Claude one is a better-looking number and no more.
    #[test]
    fn an_opencode_attempt_is_read_in_the_same_units_and_carries_its_price() {
        let t = task(&[
            "- 2026-08-22T09:00:00Z claimed by pane w1:p1 · spawned at opencode/x-preview-f-free/default",
            "- 2026-08-22T09:30:00Z released after 30m · ran opencode/x-preview-f-free/default · 16 turns · 295k cache 76k in 9.8k out $0",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a[0].spend.cache, 295_000);
        assert_eq!(a[0].spend.micros, Some(0), "free is a reading, not a gap");
        assert_eq!(
            Row(a[0].clone()).tier(),
            "opencode/x-preview-f-free/default",
            "and the tier it was asked for is the tier it served"
        );
    }

    /// An attempt closed before `core-029` has no such clause, and what that
    /// records is a measurement nobody took rather than an attempt that cost
    /// nothing.
    #[test]
    fn an_attempt_from_before_the_record_existed_reads_as_unmeasured() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1",
            "- 2026-08-18T10:00:00Z released after 1h · ran opus-5/high · 96 turns",
        ]);
        let a = attempts_of(&t);
        assert!(a[0].spend.nothing(), "no clause is no reading: {:?}", a[0].spend);
    }

    /// `wsp done` releases the claim *before* it marks the task done, so an
    /// attempt closed at its own release line would report `dropped` for every
    /// task anyone ever finished.
    #[test]
    fn an_attempt_owns_the_log_until_the_next_claim_and_not_until_its_own_release() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1",
            "- 2026-08-18T10:00:00Z released after 1h",
            "- 2026-08-18T10:00:01Z → done",
        ]);
        assert_eq!(attempts_of(&t)[0].outcome, Outcome::Done);
    }

    /// The reading the worklists section draws its cost zone from. The three
    /// arms between them are the whole honesty of that zone: a stated zero is
    /// a fact and survives; a mix of priced and unpriced attempts refuses to
    /// be priced, because a sum of the priced ones only is a number that
    /// quietly means something else; and no clauses at all is nothing.
    #[test]
    fn recorded_totals_are_what_the_runtimes_stated_and_nothing_computed() {
        // Stated zero survives — the free runtime said $0 and that is news.
        let free = task(&[
            "- 2026-08-22T09:00:00Z claimed by pane w1:p1",
            "- 2026-08-22T09:30:00Z released after 30m · ran opus-5/high · 16 turns · 295k cache 76k in 9.8k out $0",
        ]);
        assert_eq!(recorded(&free), Some((295_000 + 76_000 + 9_800, Some(0))));

        // Unpriced beside priced: tokens still total, the price does not.
        let mixed = task(&[
            "- 2026-08-22T09:00:00Z claimed by pane w1:p1",
            "- 2026-08-22T09:10:00Z released after 10m · ran opus-5/high · 4 turns · 5M cache 100k in 2k out $0.50",
            "- 2026-08-22T09:20:00Z claimed by pane w1:p2",
            "- 2026-08-22T09:40:00Z released after 20m · ran opus-5/high · 8 turns · 242M cache 4.7M in 875k out",
        ]);
        assert_eq!(
            recorded(&mixed),
            Some((5_000_000 + 100_000 + 2_000 + 242_000_000 + 4_700_000 + 875_000, None)),
            "a turn nobody measured must not read as one that was free"
        );

        // Nothing recorded at all — the ordinary state of a member not yet
        // spawned, and of every turn that died upstream (`core-045`).
        let silent = task(&["- 2026-08-22T09:00:00Z claimed by pane w1:p1"]);
        assert_eq!(recorded(&silent), None);
    }

    /// Rework is invisible from inside the attempt that was reworked: its own
    /// last line says `review`, and only the claim after it says the work came
    /// back.
    #[test]
    fn an_attempt_that_reached_review_and_was_claimed_again_is_rework() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1 · spawned at haiku/default",
            "- 2026-08-18T10:00:00Z → review",
            "- 2026-08-18T10:01:00Z released after 1h1m · ran haiku-4-5 · 12 turns",
            "- 2026-08-18T11:00:00Z claimed by pane w2:p1 · spawned at opus/high",
            "- 2026-08-18T12:00:00Z → review",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a.len(), 2, "{a:?}");
        assert_eq!(a[0].outcome, Outcome::Reopened);
        assert_eq!(a[1].outcome, Outcome::Review);
    }

    /// An agent whose workspace closed under it did not put the work down, and
    /// may never have known it had stopped holding it. Counting that against
    /// the tier that ran it is the one systematic mislabel a record like this
    /// can acquire without anybody noticing.
    #[test]
    fn a_reaped_attempt_is_not_an_attempt_that_gave_up() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1 · spawned at haiku/high",
            "- 2026-08-18T10:00:00Z released after 1h · workspace closed · ran haiku-4-5/high · 4 turns",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a[0].outcome, Outcome::Reaped);
        assert_eq!(a[0].ran, "haiku-4-5/high", "the reason clause does not eat the tier one");
        assert_eq!(a[0].turns, 4);
    }

    /// A session that changed model under itself is both names, in order. This
    /// is the case that made a single spawned-at field a lie rather than an
    /// approximation.
    #[test]
    fn a_session_that_escalated_mid_flight_keeps_both_tiers() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1 · spawned at haiku/default",
            "- 2026-08-18T10:00:00Z released after 1h · ran haiku-4-5→opus-5/high · 41 turns",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a[0].ran, "haiku-4-5→opus-5/high");
        assert_eq!(
            Row(a[0].clone()).tier(),
            "haiku-4-5→opus-5/high · asked haiku/default",
            "the fact first, and only one kind of arrow in it"
        );
    }

    /// The ordinary spawn states no tier at all, and the record of that is an
    /// empty column rather than a guess at what the settings file said.
    #[test]
    fn a_claim_with_nothing_stated_records_nothing_stated() {
        let t = task(&["- 2026-08-18T09:00:00Z claimed by pane w1:p1"]);
        let a = attempts_of(&t);
        assert_eq!(a[0].spawned, "");
        assert_eq!(a[0].outcome, Outcome::Open);
        assert_eq!(Row(a[0].clone()).tier(), "");
    }

    /// Two vocabularies, and agreement is a question about them rather than
    /// about strings: `opus[1m]` is what you type and `opus-5` is what serves.
    #[test]
    fn the_alias_that_was_typed_and_the_model_that_served_are_the_same_tier() {
        assert!(agrees("opus[1m]/high", "opus-5/high"));
        assert!(agrees("default/high", "opus-5/high"), "an unstated model agrees with any");
        assert!(!agrees("haiku/high", "opus-5/high"));
        assert!(!agrees("opus/low", "opus-5/high"), "the cheaper knob is half the tier");
        assert!(
            !agrees("haiku/default", "haiku-4-5→opus-5/high"),
            "a session that moved agrees with neither end of itself"
        );
    }

    /// Prose in the log is prose. People write notes into it, and a note that
    /// happens to begin with a word this parser knows must not become an
    /// attempt.
    #[test]
    fn a_log_line_that_is_not_one_of_the_stores_own_is_stepped_over() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1",
            "  claimed by pane w9:p9 is a thing somebody wrote in a note",
            "- 2026-08-18T09:30:00Z priority normal → high",
            "- 2026-08-18T10:00:00Z → review",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0].outcome, Outcome::Review, "a priority line is not a status line");
    }

    /// The wall-clock the parent asked for is claim to review, because that is
    /// when the work was offered — not when the agent happened to let go of it,
    /// which on a finished task is a minute later and on an abandoned one is
    /// never.
    #[test]
    fn the_clock_runs_to_review_where_there_is_one() {
        let t = task(&[
            "- 2026-08-18T09:00:00Z claimed by pane w1:p1",
            "- 2026-08-18T10:00:00Z → review",
            "- 2026-08-18T13:00:00Z released after 4h",
        ]);
        assert_eq!(attempts_of(&t)[0].seconds(), 3600);
    }

    /// wsp-290: two siblings' lines interleaved, the way a staged row writes them
    /// — b starts after a and ends before it. Each end belongs to the start of
    /// its own name, and neither is read as the other's.
    #[test]
    fn two_siblings_interleaved_lines_pair_each_end_with_its_own_start() {
        let t = task(&[
            "- 2026-10-18T09:00:00Z leg scout started by pane cpd-1 · spawned at sonnet/high",
            "- 2026-10-18T09:10:00Z leg scout ended after 10m · ran sonnet-5-5/high · 12 turns · peak 80k · 20M cache 1M in 50k out",
            "- 2026-10-18T09:11:00Z leg build-a started by pane cpd-2 · spawned at sonnet/high",
            "- 2026-10-18T09:12:00Z leg build-b started by pane cpd-3 · spawned at opus/high · why the parser is subtle",
            "- 2026-10-18T09:30:00Z leg build-b ended after 18m · ran opus-5-5/high · 40 turns · peak 150k · 90M cache 4M in 200k out",
            "- 2026-10-18T09:50:00Z leg build-a ended after 39m · ran sonnet-5-5/high · 63 turns · peak 120k · 60M cache 3M in 100k out",
        ]);
        let a = attempts_of(&t);
        let by = |leg: &str| a.iter().find(|x| x.leg.as_deref() == Some(leg)).unwrap();
        assert_eq!(a.len(), 3, "{a:?}");
        assert_eq!((by("build-a").pane.as_str(), by("build-a").turns, by("build-a").peak), ("cpd-2", 63, 120_000));
        assert_eq!((by("build-b").pane.as_str(), by("build-b").turns, by("build-b").peak), ("cpd-3", 40, 150_000));
        assert_eq!(by("build-b").ran, "opus-5-5/high");
        assert_eq!(by("build-b").why.as_deref(), Some("the parser is subtle"));
        assert_eq!(by("build-a").until, "2026-10-18T09:50:00Z");
        assert_eq!(by("build-b").until, "2026-10-18T09:30:00Z");
        assert_eq!(by("scout").outcome, Outcome::Done);
        assert_eq!(by("build-a").seconds(), 39 * 60);
    }

    /// An end line with no start of its name opens nothing and closes nothing: it
    /// must not be given to whichever leg happens to be open.
    #[test]
    fn a_leg_end_with_no_start_of_its_name_closes_nothing() {
        let t = task(&[
            "- 2026-10-18T09:00:00Z leg a started by pane cpd-1",
            "- 2026-10-18T09:10:00Z leg b ended after 10m · ran sonnet-5-5/high · 3 turns",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a.len(), 1);
        assert!(a[0].until.is_empty() && a[0].turns == 0, "{a:?}");
    }

    /// A claim's lines are untouched by a leg's in between: the sequence rule that
    /// marks an attempt reopened reads claims and not legs.
    #[test]
    fn legs_between_claims_do_not_change_what_a_claim_came_to() {
        let t = task(&[
            "- 2026-10-18T08:00:00Z claimed by pane w1:p1 · spawned at opus/high",
            "- 2026-10-18T08:30:00Z → review",
            "- 2026-10-18T08:31:00Z leg x started by pane cpd-1",
            "- 2026-10-18T08:40:00Z leg x ended after 9m",
            "- 2026-10-18T09:00:00Z claimed by pane w1:p2",
        ]);
        let a = attempts_of(&t);
        assert_eq!(a[0].outcome, Outcome::Reopened);
        assert_eq!(a[1].outcome, Outcome::Done, "the leg's own word");
        assert_eq!(a[2].outcome, Outcome::Open);
    }

    /// wsp-290: an `opus-5-5` attempt is priced at the Opus 5.5 rate — $4/$20 and
    /// cache reads at 0.05x — and the same tokens on Opus 5 at its own.
    #[test]
    fn an_opus_five_five_attempt_is_priced_at_its_listed_rate() {
        let spend = Spend { cache: 100_000_000, input: 1_000_000, output: 500_000, micros: None };
        // 100M * $0.20 + 1M * $4 + 0.5M * $20
        assert_eq!(priced("opus-5-5/high", &spend), Some(20_000_000 + 4_000_000 + 10_000_000));
        // 100M * $0.50 + 1M * $5 + 0.5M * $25
        assert_eq!(priced("opus-5/high", &spend), Some(50_000_000 + 5_000_000 + 12_500_000));
        assert_eq!(priced("sonnet-5-5/high", &spend), Some(10_000_000 + 2_000_000 + 5_000_000));
    }

    #[test]
    fn what_cannot_be_priced_is_left_blank_and_a_stated_price_is_kept() {
        let tokens = Spend { cache: 1_000_000, input: 0, output: 0, micros: None };
        assert_eq!(priced("opus-5-5/high", &Spend::default()), None, "nothing recorded");
        assert_eq!(priced("opencode/x-free/default", &tokens), None, "a model the table does not know");
        let stated = Spend { micros: Some(0), ..tokens };
        assert_eq!(priced("opencode/x-free/default", &stated), Some(0), "a stated zero is a fact");
        // A session that moved is priced at the dearest tier it named.
        let moved = Spend { cache: 0, input: 1_000_000, output: 0, micros: None };
        assert_eq!(priced("haiku-4-5→opus-5/medium→high", &moved), Some(5_000_000));
    }

    /// The cost of a staged row, end to end from captured lines: a worker, a leg
    /// pair with a join, a rework, and a verifier pass that kept its spend.
    #[test]
    fn a_rows_cost_names_the_worker_the_legs_the_rework_and_the_verifier() {
        let mut t = task(&[
            "- 2026-10-18T09:00:00Z leg scout started by pane cpd-1 · spawned at sonnet/high",
            "- 2026-10-18T09:10:00Z leg scout ended after 10m · ran sonnet-5-5/high · 12 turns · peak 80k · 20M cache 1M in 50k out",
            "- 2026-10-18T09:11:00Z leg build-a started by pane cpd-2 · spawned at sonnet/high",
            "- 2026-10-18T09:12:00Z leg build-b started by pane cpd-3 · spawned at opus/high · why the parser is subtle",
            "- 2026-10-18T09:30:00Z leg build-b ended after 18m · ran opus-5-5/high · 40 turns · peak 150k · 90M cache 4M in 200k out",
            "- 2026-10-18T09:50:00Z leg build-a ended after 39m · ran sonnet-5-5/high · 63 turns · peak 120k · 60M cache 3M in 100k out",
            "- 2026-10-18T09:51:00Z leg join started by pane cpd-4 · spawned at sonnet/high",
            "- 2026-10-18T10:00:00Z leg join ended after 9m · ran sonnet-5-5/high · 9 turns · peak 60k · 5M cache 1M in 20k out",
        ]);
        let mut legs: Vec<crate::legs::Leg> = ["scout", "build-a", "build-b", "join"].iter().map(|n| crate::legs::Leg::pending(n, "")).collect();
        legs[1].after = Some(vec!["scout".into()]);
        legs[2].after = Some(vec!["scout".into()]);
        legs[2].policy = crate::model::Policy::parse("claude opus high").unwrap();
        legs[2].why = Some("the parser is subtle".into());
        legs[3].after = Some(vec!["build-a".into(), "build-b".into()]);
        crate::legs::write(&mut t, &legs);
        let mut pass = crate::verification::Pass::opened(None, Some("claude".into()));
        pass.at = "2026-10-18T10:05:00Z".into();
        pass.decided = Some("2026-10-18T10:15:00Z".into());
        pass.state = crate::verification::State::Holds;
        pass.spent = Some(crate::verification::Spent { ran: "sonnet-5-5/high".into(), turns: 7, peak: 70_000, spend: Spend { cache: 4_000_000, input: 200_000, output: 10_000, micros: None } });
        crate::verification::write(&mut t, &[pass]);

        let rc = row_cost(&t);
        let what: Vec<String> = rc.lines.iter().map(CostLine::what).collect();
        assert_eq!(what, ["leg scout", "leg build-a", "leg build-b", "leg join (join)", "verifier"]);
        assert_eq!(rc.turns(), 12 + 63 + 40 + 9 + 7);
        assert_eq!(rc.join_turns(), 9);
        assert_eq!(rc.peak(), 150_000);
        assert_eq!(rc.wall, Some(60 * 60), "09:00 to 10:00");
        assert_eq!(rc.legs_sum, (10 + 39 + 18 + 9) * 60 - 0, "the legs' own times: {:?}", rc.lines);
        assert_eq!(rc.off_default, [("build-b".to_string(), "opus-5-5/high".to_string(), "the parser is subtle".to_string())]);
        let b = rc.lines.iter().find(|l| l.leg.as_deref() == Some("build-b")).unwrap();
        assert_eq!(b.micros, Some(18_000_000 + 16_000_000 + 4_000_000), "90M at $0.20, 4M at $4, 200k at $20");
        let v = rc.lines.last().unwrap();
        assert_eq!((v.kind, v.seconds), ("verifier", 600));
        assert!(rc.total().is_some() && rc.unmeasured() == 0);
        let shown = cost_table(&Paint::new(), &rc);
        for want in ["leg build-b", "verifier", "wall 1h beside legs summed 1h16m", "join legs 9 of 131 turns", "leg build-b on opus-5-5/high — the parser is subtle"] {
            assert!(shown.contains(want), "{want} missing from:\n{shown}");
        }
    }

    /// A row nobody staged: the claim's attempts are the worker and then the
    /// rework, and a row with no legs has no wall time to speak of.
    #[test]
    fn an_unstaged_row_costs_a_worker_then_its_rework() {
        let t = task(&[
            "- 2026-10-18T08:00:00Z claimed by pane w1:p1 · spawned at opus/high",
            "- 2026-10-18T08:30:00Z → review",
            "- 2026-10-18T08:31:00Z released after 31m · ran opus-5-5/high · 30 turns · peak 90k · 50M cache 2M in 100k out",
            "- 2026-10-18T09:00:00Z claimed by pane w1:p2 · spawned at opus/high",
            "- 2026-10-18T09:20:00Z released after 20m · ran opus-5-5/high · 10 turns · peak 95k · 20M cache 1M in 40k out",
        ]);
        let rc = row_cost(&t);
        assert_eq!(rc.lines.iter().map(|l| l.kind).collect::<Vec<_>>(), ["worker", "rework"]);
        assert_eq!(rc.wall, None);
        assert_eq!(rc.lines[0].micros, Some(10_000_000 + 8_000_000 + 2_000_000));
    }

    /// A pass's spend rides on its entry and reads back.
    #[test]
    fn a_passes_spend_is_a_clause_of_its_entry_and_reads_back() {
        let mut t = Task::new("m", "m-1");
        t.body = "## Overview\nx\n".into();
        let mut p = crate::verification::Pass::opened(None, None);
        p.pane = Some("cpd-9".into());
        p.spent = Some(crate::verification::Spent { ran: "sonnet-5-5/high".into(), turns: 7, peak: 70_000, spend: Spend { cache: 4_000_000, input: 200_000, output: 10_000, micros: None } });
        crate::verification::write(&mut t, &[p.clone()]);
        let back = crate::verification::passes(&t);
        assert_eq!(back[0].spent, p.spent);
        assert_eq!(back[0].pane, p.pane, "and the clauses before it are still its own");
    }
}
