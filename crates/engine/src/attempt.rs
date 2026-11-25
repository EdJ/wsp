//! One agent's turn at one task, read off the task's log: the claim (or a leg's
//! or piece's start) to whatever came next, with the tier that ran it and what
//! it spent (wsp-436: moved down from `cmd_attempts`, which the run reads it
//! from, so the engine and the seats name no timeline module for it).
//!
//! `cmd_attempts` keeps the report and re-exports these at their old paths; the
//! reasons for the record's shape are in that module's header.

use crate::agent_kind::Spend;
use crate::model::Task;
use crate::util;

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
    /// Ran as a packet: the claim line carries the `packet` clause
    /// (`wsp packet`), so cost by kind can tell a short start from the usual.
    pub packet: bool,
    /// The bytes of the packet the agent was handed, when the claim line says.
    pub packet_bytes: Option<u64>,
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

    /// A leg start that never took (`compounding-222`): closed as dropped with no
    /// turn and no tier that ran. It is a try, not a seat; the cost table and the
    /// effort census do not count it as one.
    pub fn never_took(&self) -> bool {
        self.leg.is_some() && self.outcome == Outcome::Dropped && self.turns == 0 && self.ran.is_empty()
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
pub fn agrees(spawned: &str, ran: &str) -> bool {
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
    // The one shape that would still be ambiguous is a [`crate::agent_kind::Ran`]
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
/// survives — [`crate::agent_kind::Spend::micros`] holds `Some(0)` for a
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
        // A piece's two lines are a leg's under the name `piece N` (`piece_seat`),
        // paired by that number for the same reason.
        let seat_line = head
            .strip_prefix("leg ")
            .and_then(|h| h.split_once(' '))
            .map(|(name, said)| (name.to_string(), said))
            .or_else(|| {
                head.strip_prefix("piece ")
                    .and_then(|h| h.split_once(' '))
                    .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    .map(|(n, said)| (format!("piece {n}"), said))
            });
        if let Some((name, said)) = seat_line.as_ref().map(|(n, s)| (n.as_str(), *s)) {
            if let Some(who) = said.strip_prefix("started by pane ") {
                // A start that never took left no end line, and the leg's next
                // start finds it still open: it ended where the next began.
                close_unfinished(&mut out, name, stamp);
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
        // `leg_start_failed`'s note: the seat the start named was ended and the
        // leg put back to pending, with no end line. The attempt ran until here.
        if let Some(name) = head
            .strip_prefix("wsp could not spawn leg ")
            .and_then(|h| h.rsplit_once(" (try "))
            .map(|(n, _)| n)
        {
            close_unfinished(&mut out, name, stamp);
            continue;
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
                let clause = clause.trim();
                if let Some(tier) = clause.strip_prefix("spawned at ") {
                    a.spawned = tier.trim().to_string();
                } else if let Some(bytes) = crate::packet::clause_bytes(clause) {
                    a.packet = true;
                    a.packet_bytes = bytes;
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

/// Close the leg's open attempt at `stamp` as dropped: a start that never took
/// is not a seat that is still running, and must not be read to the clock's now
/// (`compounding-222`: a window of 3053 s read 24737 s).
fn close_unfinished(out: &mut [Attempt], name: &str, stamp: &str) {
    if let Some(a) = out.iter_mut().rev().find(|a| a.leg.as_deref() == Some(name) && a.until.is_empty()) {
        a.until = stamp.to_string();
        a.outcome = Outcome::Dropped;
    }
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

/// What a recorded spend cost in dollars, in micro-dollars.
///
/// The runtime's own price where it states one (opencode). Otherwise the tokens
/// the record kept, priced at [`crate::place_super::PRICES`] on the model that ran:
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
        if !crate::place_super::known(&id) {
            return None;
        }
        let c = crate::place_super::cost(&id, s.input, s.output, s.cache, 0);
        dearest = Some(dearest.map_or(c, |d| d.max(c)));
    }
    dearest
}
