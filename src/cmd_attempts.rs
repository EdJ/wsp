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
        let Some(a) = out.last_mut() else { continue };
        if head.starts_with("released") || head.starts_with("handed off to") {
            a.until = stamp.to_string();
            for clause in parts {
                let clause = clause.trim();
                if let Some(tier) = clause.strip_prefix("ran ") {
                    a.ran = tier.to_string();
                } else if let Some(n) = clause.strip_suffix(" turns") {
                    a.turns = n.parse().unwrap_or(0);
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
        } else if head == "→ review" {
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
    // it can only be written from here.
    for i in 0..out.len().saturating_sub(1) {
        if matches!(out[i].outcome, Outcome::Review | Outcome::Done) {
            out[i].outcome = Outcome::Reopened;
        }
    }
    // Everything that let go without reaching anything. Left until last so the
    // sequence rule above still sees what each attempt actually reached.
    for a in out.iter_mut() {
        if a.outcome == Outcome::Open && !a.until.is_empty() {
            a.outcome = Outcome::Dropped;
        }
    }
    out
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
        a.ran = ran.label();
        a.spend = ran.spend;
    }
}

pub fn attempts(store: &Store, args: &Args) -> i32 {
    let tasks = match wanted(store, args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    let mut rows: Vec<Attempt> = tasks.iter().flat_map(|t| attempts_of(t)).collect();
    rows.sort_by(|a, b| a.at.cmp(&b.at).then(a.task.cmp(&b.task)));
    // Every attempt that has not *ended*, which is not the same set as the ones
    // still `open`: an agent that has put its work up for review and is holding
    // the claim while somebody reads it has reached an outcome and written
    // nothing down, because the clause is written when the claim ends. There
    // are usually a few of those, and they are the most recent evidence there
    // is.
    for a in rows.iter_mut().filter(|a| a.until.is_empty()) {
        live(store, a);
    }

    if args.json() {
        println!(
            "{}",
            json!({
                "attempts": rows.iter().map(|a| json!({
                    "task": a.task,
                    "title": a.title,
                    "at": a.at,
                    "pane": a.pane,
                    "spawned_at": a.spawned,
                    "ran_at": a.ran,
                    "turns": a.turns,
                    "cache_tokens": a.spend.cache,
                    "input_tokens": a.spend.input,
                    "output_tokens": a.spend.output,
                    "usd_micros": a.spend.micros,
                    "released_at": a.until,
                    "reviewed_at": a.reviewed,
                    "seconds": a.seconds(),
                    "outcome": a.outcome.as_str(),
                })).collect::<Vec<_>>(),
            })
        );
        return 0;
    }

    let p = Paint::new();
    if rows.is_empty() {
        println!("no attempts recorded here yet");
        return 0;
    }

    let w_task = rows.iter().map(|a| a.task.len()).max().unwrap_or(4);
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
            p.bold(&util::pad(&a.task, w_task)),
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
}
