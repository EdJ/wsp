//! `wsp timeline <row> [--json]` — where a row's wall time goes, from the
//! spawn that was asked for to the barrier that passed it.
//!
//! In-session wall has always been measured. What a person waits for is the
//! whole of it: the seat starting, the warm-up, each leg and the gap before
//! it, the review, the verifier, the verdict, the barrier. This reads that off
//! what wsp already writes — `events.jsonl`, `cycle.log`, the row's legs and
//! the first prompt in a seat's transcript — and prints one line per phase.
//!
//! **A gap is a phase where nothing was running.** It is named for what was
//! being waited on: a hand-off (a few seconds, the poke), the daemon's tick,
//! the seat's cap, or nothing the logs can name. `wsp-366`.

use serde_json::{json, Value};

use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// The daemon's tick, which a step nobody poked waits for.
const TICK: i64 = 20;
/// Longest a poke takes to show as a start: a process, a lock, a pane.
const HAND_OFF: i64 = 8;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Phase {
    pub label: String,
    pub start: i64,
    pub end: i64,
    /// Nothing was running.
    pub gap: bool,
    /// For a gap: what was being waited for.
    pub waiting_for: Option<String>,
    /// The group's, not the member's: a barrier phase is one run for every
    /// member of the group, and a sum over members counts it once per group.
    pub shared: bool,
}

impl Phase {
    fn work(label: impl Into<String>, start: i64, end: i64) -> Phase {
        Phase { label: label.into(), start, end: end.max(start), gap: false, waiting_for: None, shared: false }
    }
    fn gap(label: impl Into<String>, start: i64, end: i64, why: String) -> Phase {
        Phase { label: label.into(), start, end: end.max(start), gap: true, waiting_for: Some(why), shared: false }
    }
    fn shared(mut self) -> Phase {
        self.shared = true;
        self
    }
    pub(crate) fn seconds(&self) -> i64 {
        self.end - self.start
    }
}

/// One leg, as the row records it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LegIn {
    pub name: String,
    /// The legs it waits on.
    pub after: Vec<String>,
    pub skipped: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Inputs {
    pub id: String,
    /// `(epoch, kind, data)`, for the row and for the members of its group.
    pub events: Vec<(i64, String, Value)>,
    /// `(epoch, line)` of `cycle.log`, for the row and its list.
    pub cycle: Vec<(i64, String)>,
    pub legs: Vec<LegIn>,
    /// The first prompt of the row's own seat, when the transcript says.
    pub first_request: Option<i64>,
    /// The list and group the row runs in, and the group's members.
    pub group: Option<(String, usize, Vec<String>)>,
    /// `(epoch, kind, row)` of the events that end a seat, for the rows a cap
    /// line named as holders (`wsp-394`).
    pub holder_events: Vec<(i64, String, String)>,
    /// The first line of the row's own seat's transcript, whatever it is.
    pub seat_first: Option<i64>,
    /// The verifier seats the row's sessions name, read off their transcripts.
    pub verifiers: Vec<VerifierSeat>,
}

/// One verifier seat as its own transcript has it (`wsp-429`). wsp's
/// `started <row> · verifying` line is written when the step that spawned the
/// seat finishes, after every `wsp spawn` it ran has returned and each of those
/// waits for its agent: it lags the seat's real start by that long (100 s or
/// more on 9 of 17 verifiers in react-pass-1). The transcript's first line is
/// the start.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct VerifierSeat {
    /// When wsp learned the session.
    pub learned: i64,
    /// The first line of its transcript.
    pub first: i64,
    /// Seconds its test and build commands ran ([`is_test_command`]).
    pub test_secs: i64,
}

impl Inputs {
    fn kind(&self, kind: &str, id: &str) -> Vec<(i64, &Value)> {
        self.events
            .iter()
            .filter(|(_, k, d)| k == kind && d.get("id").and_then(|v| v.as_str()) == Some(id))
            .map(|(t, _, d)| (*t, d))
            .collect()
    }
    fn first_line(&self, from: i64, pred: impl Fn(&str) -> bool) -> Option<i64> {
        self.cycle.iter().find(|(t, l)| *t >= from && pred(l)).map(|(t, _)| *t)
    }
    fn first_learned_after(&self, at: i64) -> Option<i64> {
        self.kind("session-learned", &self.id).into_iter().map(|(t, _)| t).find(|t| *t >= at)
    }
}

/// Why nothing was running between `from` and `to`, for `who`: the row's id, or
/// `<list> · barrier <n>` for the barrier. A cap line names the one that
/// waits before ` waits —`; whoever it lists after the colon holds a place.
fn wait_name(i: &Inputs, from: i64, to: i64, who: &str) -> String {
    let span = to - from;
    let cap = i.cycle.iter().find_map(|(t, l)| {
        if *t < from || *t > to {
            return None;
        }
        let (before, after) = l.split_once(" waits — the seat is at its cap")?;
        let what = before.rsplit_once(": ").map_or(before, |(_, w)| w);
        names(what, who).then(|| after.to_string())
    });
    if let Some(after) = cap {
        let held = cap_holders(i, &after, to);
        if held.is_empty() { "the seat's cap (a place to come free)".into() } else { format!("the seat's cap (a place to come free) — held by {held}") }
    } else if span <= HAND_OFF {
        "the hand-off (a poke and a spawn)".into()
    } else if span <= TICK + HAND_OFF {
        format!("a daemon tick (every {TICK}s) — nothing poked the step")
    } else {
        "nothing the logs name — the step did not run for longer than a tick".into()
    }
}

/// One place a cap line lists after its colon (`x in cpd-1, y starting, 2
/// more`), and what kind of seat it is.
enum Held {
    /// A row's work seat.
    Work(String),
    /// A row's step seat (`r · verifying`).
    Step(String),
    /// A list's barrier check seat (`list · barrier 2`): the label and the list.
    Check(String, String, usize),
    /// `N more`: places nobody named, so nothing to say of them.
    More,
}

fn held_in(after: &str) -> Vec<(String, Held)> {
    let list = after.split_once(": ").map_or("", |(_, l)| l);
    list.split(", ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            let what = item.strip_suffix(" starting").or_else(|| item.rsplit_once(" in ").map(|(w, _)| w)).unwrap_or(item);
            let held = if item.strip_suffix(" more").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
                Held::More
            } else if let Some((list, at)) = crate::barrier::parse_label(what) {
                Held::Check(what.trim().to_string(), list.to_string(), at)
            } else if let Some((row, _)) = what.split_once(" · ") {
                Held::Step(row.to_string())
            } else {
                Held::Work(what.to_string())
            };
            (item.to_string(), held)
        })
        .collect()
}

/// The holders a cap line lists, each with whether its seat had finished by
/// `to`: a work seat finishes at its row's review or release, a step's seat
/// (`r · verifying`) at the row's verdict, a barrier check seat when its
/// barrier passed.
fn cap_holders(i: &Inputs, after: &str, to: i64) -> String {
    let mut out = Vec::new();
    for (item, held) in held_in(after) {
        let ended = |pred: &dyn Fn(&str, &str) -> bool| {
            i.holder_events.iter().filter(|(t, k, r)| *t <= to && pred(k, r)).map(|(t, _, _)| *t).min()
        };
        let ended = match &held {
            Held::More => {
                out.push(item);
                continue;
            }
            Held::Work(row) => ended(&|k, r| r == row && (k == "task-review" || k == "task-released")),
            Held::Step(row) => ended(&|k, r| r == row && k == "task-verified"),
            Held::Check(label, _, _) => ended(&|k, r| r == label && k == "barrier-passed"),
        };
        out.push(match ended {
            Some(t) => format!("{item} (finished {})", clock(t)),
            None => format!("{item} (running)"),
        });
    }
    out.join(", ")
}

/// When the row's own seat was asked for.
fn spawn_line(i: &Inputs) -> Option<i64> {
    let id = i.id.as_str();
    i.first_line(0, |l| {
        (l.contains(&format!("starting {id} on ")) || (l.starts_with(&format!("spawn {id} --agent")) && !l.contains("--leg") && !l.contains("--verify")))
            || l.contains(&format!(": starting {id} on "))
    })
}

const VERIFIER_START: &str = "verifier seat start (spawn → its first transcript line)";
const VERIFIER_START_LINE: &str = "verifier seat start (spawn → wsp's started line; the seat's first line unread, so this lags it)";
const VERIFIER_HELD: &str = "verdict → verifier seat ended (clean-up, alongside what follows)";

/// The phases of a row, in time order.
pub(crate) fn build(i: &Inputs) -> Vec<Phase> {
    let id = i.id.as_str();
    let mut out: Vec<Phase> = Vec::new();
    let claimed = i.kind("task-claimed", id).first().map(|(t, _)| *t);
    let reviews: Vec<i64> = i.kind("task-review", id).into_iter().map(|(t, _)| t).collect();
    // A release before the first review is a seat handing its row to legs. One
    // after it is the seat's end, long after its verdict: it is not where the
    // seat's work stopped (`wsp-429`: compounding-114 and 117 read as one seat
    // phase to the release, with the verifier and the barrier inside it).
    let released = i.kind("task-released", id).first().map(|(t, _)| *t).filter(|t| reviews.first().is_none_or(|r| t <= r));
    let released_last = i.kind("task-released", id).last().map(|(t, _)| *t);
    let learned = i.kind("session-learned", id).first().map(|(t, _)| *t);

    // Spawn → the seat is up → its first request.
    let spawn = spawn_line(i);
    let begin = spawn.or(claimed);
    if let (Some(s), Some(c)) = (spawn, claimed) {
        out.push(Phase::work("spawn requested → seat claimed", s, c));
    }
    if let (Some(c), Some(l)) = (claimed, learned) {
        out.push(Phase::work("seat up (session started)", c, l));
    }
    let work_from = match (learned, i.first_request) {
        (Some(l), Some(r)) if r >= l => {
            out.push(Phase::work("warm-up → first request", l, r));
            Some(r)
        }
        (l, _) => l.or(claimed),
    };
    let seat_end = released.or(reviews.first().copied());
    if let (Some(a), Some(b)) = (work_from, seat_end) {
        out.push(Phase::work(if released.is_some() { "the seat's work until release" } else { "the seat's work until review" }, a, b));
    }

    // Legs.
    let mut done: Vec<(String, i64)> = Vec::new();
    let mut last_end = released;
    for (n, leg) in i.legs.iter().enumerate() {
        if leg.skipped {
            continue;
        }
        let added = i.events.iter().find(|(_, k, d)| k == "leg-added" && d["id"] == id && d["leg"] == leg.name.as_str()).map(|(t, _, _)| *t);
        let waits: Vec<String> = if leg.after.is_empty() && n > 0 && !i.legs[n - 1].skipped { vec![i.legs[n - 1].name.clone()] } else { leg.after.clone() };
        let started = i.first_line(added.unwrap_or(0), |l| l.contains(&format!("starting leg {} of {id}", leg.name)));
        let Some(s) = started else { continue };
        // The waits as the row has them now: one that finished after this leg
        // had started was edited since (`wsp leg after`), and is not what held
        // it back.
        let mut ready = added.unwrap_or(0).max(released.unwrap_or(0));
        for w in &waits {
            if let Some((_, t)) = done.iter().find(|(d, t)| d == w && *t <= s) {
                ready = ready.max(*t);
            }
        }
        // The leg that ends the row ends with the review, not a `leg done`.
        let done_event = i.events.iter().find(|(t, k, d)| k == "leg-done" && d["id"] == id && d["leg"] == leg.name.as_str() && *t >= s);
        // A close that came late (`wsp-372`) carries when the baton was written:
        // the leg's work ended then, and the rest is the stall, not work.
        let baton_at = done_event.and_then(|(t, _, d)| d["baton_at"].as_i64().filter(|b| *b >= s && b < t));
        let finished = baton_at
            .or_else(|| done_event.map(|(t, _, _)| *t))
            .or_else(|| reviews.iter().copied().find(|r| *r >= s));
        if ready > 0 && s > ready {
            out.push(Phase::gap(format!("gap before leg {}", leg.name), ready, s, wait_name(i, ready, s, id)));
        }
        let up = i.first_learned_after(s);
        let label = match up {
            Some(u) => format!("leg {} (seat up after {}s)", leg.name, u - s),
            None => format!("leg {}", leg.name),
        };
        if let Some(f) = finished {
            out.push(Phase::work(label, s, f));
            done.push((leg.name.clone(), f));
            last_end = Some(last_end.map_or(f, |e| e.max(f)));
            if let (Some(b), Some((closed, _, d))) = (baton_at, done_event) {
                let by = d["closed_by"].as_str().map(str::to_string).unwrap_or_else(|| "the leg's own seat".into());
                out.push(Phase::gap(
                    format!(
                        "closed late: leg {} done at {}, closed at {}",
                        leg.name,
                        util::iso_at(b).get(11..16).unwrap_or_default(),
                        util::iso_at(*closed).get(11..16).unwrap_or_default()
                    ),
                    b,
                    *closed,
                    format!("the close — {by} (not work)"),
                ));
            }
        }
    }

    // Review → verifier → verdict.
    // The last review that a verdict followed: a row typed `wsp review` again
    // after it had landed (to say what it did for the barrier, say) has a later
    // review nothing verified.
    let verdicts: Vec<i64> = i.kind("task-verified", id).into_iter().map(|(t, _)| t).collect();
    let review = reviews.iter().copied().rev().find(|r| verdicts.iter().any(|v| v >= r)).or_else(|| reviews.last().copied());
    if let (Some(r), Some(e)) = (review, last_end) {
        if r > e {
            out.push(Phase::gap("after the last leg → review typed", e, r, "the row's own seat finishing (not wsp)".into()));
        }
    }
    let verdict = verdicts.iter().copied().find(|t| review.is_none_or(|r| *t >= r));
    if let Some(r) = review {
        let spawned = i.first_line(r.saturating_sub(1), |l| l.starts_with(&format!("spawn {id} --agent --verify")));
        let up = spawned.and_then(|s| i.first_line(s, |l| l.contains(&format!("started {id} · verifying"))));
        if let Some(s) = spawned {
            // `wsp review` runs the row's own check before it spawns the verifier;
            // the last `task-checked` between the two ends it.
            let checked = i.kind("task-checked", id).into_iter().map(|(t, _)| t).filter(|t| *t >= r && *t <= s).max();
            match checked {
                Some(c) => {
                    out.push(Phase::work("the row's own check (wsp check, run by review)", r, c));
                    if s > c {
                        out.push(Phase::gap("row's check ended → verifier spawn requested", c, s, wait_name(i, c, s, id)));
                    }
                }
                None => out.push(Phase::gap("review typed → verifier spawn requested", r, s, wait_name(i, r, s, id))),
            }
            // The seat's own first line, when its transcript was read; wsp's line
            // is the fallback, and says it lags.
            let first = i.verifiers.iter().map(|v| v.first).find(|f| *f >= s && verdict.is_none_or(|v| *f <= v));
            let started = first.map(|f| (f, VERIFIER_START)).or(up.map(|u| (u, VERIFIER_START_LINE)));
            if let Some((u, label)) = started {
                out.push(Phase::work(label, s, u));
                if let Some(v) = verdict {
                    out.push(Phase::work("verifier reads → verdict recorded (landed on a hold)", u, v));
                }
            }
        }
    }
    if let Some(v) = verdict {
        if let Some(e) = i.first_line(v, |l| l.starts_with("ended ") && l.contains(&format!("which verified {id}"))) {
            out.push(Phase::work(VERIFIER_HELD, v, e));
        }
    }

    // The seat released after its verdict: it stayed up, and was asked for the
    // barrier's fix or let go late. Alongside the barrier, which it overlaps.
    if let (Some(v), Some(rel)) = (verdict, released_last) {
        if rel > v && released != Some(rel) {
            let again = reviews.iter().copied().find(|r| *r > v);
            let label = match again {
                Some(r) => format!("the row's own seat after the verdict → released (reviewed again at {}; alongside what follows)", util::iso_at(r).get(11..16).unwrap_or_default()),
                None => "the row's own seat after the verdict → released (alongside what follows)".to_string(),
            };
            out.push(Phase::work(label, v, rel));
        }
    }

    // The barrier, for a list member.
    if let (Some((list, at, members)), Some(_)) = (&i.group, verdict) {
        let last = members
            .iter()
            .filter_map(|m| i.kind("task-verified", m).into_iter().map(|(t, _)| t).max())
            .max()
            .unwrap_or(0);
        let from = last.max(verdict.unwrap_or(0));
        let req = i.first_line(from.saturating_sub(1), |l| l.starts_with(&format!("spawn {list} --agent --check {at}")) || l.contains(&format!("barrier-run {at}")));
        let seat_up = i.first_line(from, |l| l.contains(&format!("started {list} · barrier {at}")));
        // The run's own line is written when it ENDS, with its elapsed seconds
        // (lock wait included): it began that long before.
        // Copies of a run share one line of the log, or follow one another; the
        // longest of those that end together began first, and is the run.
        let who = format!("{list} · barrier {at}");
        let head = format!("{list} group {at}: barrier run — ");
        let mut runs: Vec<(i64, &str)> = Vec::new();
        for (t, l) in i.cycle.iter().filter(|(t, l)| *t >= from && l.contains(&head)) {
            runs.extend(l.split(&head).skip(1).map(|seg| (*t, seg)));
        }
        let first_end = runs.first().map(|(t, _)| *t);
        let run = runs
            .iter()
            .filter(|(t, _)| first_end.is_some_and(|f| *t <= f + 120))
            .max_by_key(|(_, seg)| run_seconds(seg).unwrap_or(0))
            .copied();
        let pass = i.first_line(from, |l| l.starts_with(&format!("tell {list}: Barrier {at} of {list}")));
        let others = if last > verdict.unwrap_or(0) { " (other members' verdicts came later)" } else { "" };
        if let Some((end, line)) = run {
            let secs = run_seconds(line).unwrap_or(0);
            let start = (end - secs).max(from);
            if start > from {
                out.push(Phase::gap(format!("last verdict → barrier {at} run started{others}"), from, start, wait_name(i, from, start, &who)).shared());
            }
            out.push(Phase::work(run_label(*at, line), start, end).shared());
            // A check seat after the run: its own wait, then its work.
            let seat = req.or(seat_up).filter(|b| *b >= end);
            if let Some(b) = seat {
                if b > end {
                    out.push(Phase::gap(format!("barrier {at} run ended → check seat started"), end, b, wait_name(i, end, b, &who)).shared());
                }
                if let Some(p) = pass.filter(|p| *p >= b) {
                    out.push(Phase::work(format!("barrier {at} check seat → passed"), b, p).shared());
                }
            } else if let Some(p) = pass.filter(|p| *p >= end) {
                out.push(Phase::work(format!("barrier {at} run ended → passed"), end, p).shared());
            }
        } else if let Some(b) = req.or(seat_up) {
            out.push(Phase::gap(format!("last verdict → barrier {at} started{others}"), from, b, wait_name(i, from, b, &who)).shared());
            if let Some(p) = pass.filter(|p| *p >= b) {
                out.push(Phase::work(format!("barrier {at} runs → passed"), b, p).shared());
            }
        }
    }
    let _ = begin;
    out.sort_by_key(|p| (p.start, p.end));
    out
}

/// The seconds a `barrier run —` segment reports: the run's elapsed time.
fn run_seconds(seg: &str) -> Option<i64> {
    let after = seg.split(" failed, ").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if after[digits.len()..].starts_with('s') { digits.parse().ok() } else { None }
}

/// The run's label: its counts, and the seconds of each step that ran.
fn run_label(at: usize, seg: &str) -> String {
    let counts = match (seg.split(" failed, ").next(), run_seconds(seg)) {
        (Some(head), Some(secs)) => format!("{head} failed, {secs}s"),
        _ => seg.split(" · ").next().unwrap_or("").to_string(),
    };
    let steps = seg.split(" · per step: ").nth(1).map(|s| s.split(" · ").next().unwrap_or(s).trim_end()).map(|s| format!(" — per step: {s}")).unwrap_or_default();
    format!("barrier {at} run ({counts}){steps}")
}

fn clock(t: i64) -> String {
    util::iso_at(t).get(11..19).unwrap_or("").to_string()
}

fn render(i: &Inputs, phases: &[Phase], p: &Paint) -> String {
    let mut s = String::new();
    let (Some(a), Some(b)) = (phases.iter().map(|x| x.start).min(), phases.iter().map(|x| x.end).max()) else {
        return format!("{}: nothing recorded to time\n", i.id);
    };
    s.push_str(&format!("{} · {} start to end · times UTC\n", p.bold(&i.id), util::duration_human(b - a)));
    for x in phases {
        let line = format!("{}  {}  {:>6}  {}", clock(x.start), clock(x.end), format!("{}s", x.seconds()), x.label);
        if x.gap {
            s.push_str(&p.yellow(&format!("{line}  ← waiting for {}", x.waiting_for.as_deref().unwrap_or("?"))));
        } else {
            s.push_str(&line);
        }
        s.push('\n');
    }
    let gaps: i64 = phases.iter().filter(|x| x.gap).map(|x| x.seconds()).sum();
    s.push_str(&format!("gaps total {}s of {}s\n", gaps, b - a));
    let (member, verifier, tests, held) = fixed_of(i, phases);
    let secs = |v: Option<i64>| v.map_or("not read".to_string(), |v| format!("{v}s"));
    s.push_str(&format!(
        "fixed parts: seat spawn → first line {} · verifier spawn → first line {} · verifier test and build {} · verdict → despawn {}\n",
        secs(member),
        secs(verifier),
        if i.verifiers.is_empty() { "not read".to_string() } else { format!("{tests}s") },
        secs(held)
    ));
    s
}

fn to_json(i: &Inputs, phases: &[Phase]) -> Value {
    json!({
        "id": i.id,
        "phases": phases.iter().map(|x| json!({
            "label": x.label,
            "start": util::iso_at(x.start),
            "end": util::iso_at(x.end),
            "seconds": x.seconds(),
            "gap": x.gap,
            "waiting_for": x.waiting_for,
            "shared": x.shared,
        })).collect::<Vec<_>>(),
        "gap_seconds": phases.iter().filter(|x| x.gap).map(|x| x.seconds()).sum::<i64>(),
        "fixed": fixed_json(i, phases),
    })
}

fn fixed_json(i: &Inputs, phases: &[Phase]) -> Value {
    let (member, verifier, tests, held) = fixed_of(i, phases);
    json!({
        "spawn_to_first_line_seconds": member,
        "verifier_spawn_to_first_line_seconds": verifier,
        "verifier_test_seconds": tests,
        "verdict_to_despawn_seconds": held,
    })
}

/// Whether `line` names `id` as a whole token (`wsp-36` is not `wsp-366`).
fn names(line: &str, id: &str) -> bool {
    let mut from = 0;
    while let Some(p) = line[from..].find(id) {
        let (a, b) = (from + p, from + p + id.len());
        let before = line[..a].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '-'));
        let after = line[b..].chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '-'));
        if before && after {
            return true;
        }
        from = b;
    }
    false
}

/// A session's Claude Code transcript, whole.
fn transcript(session: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    transcript_under(&std::path::Path::new(&home).join(".claude/projects"), session)
}

fn transcript_under(root: &std::path::Path, session: &str) -> Option<String> {
    for dir in std::fs::read_dir(root).ok()?.flatten() {
        if let Ok(text) = std::fs::read_to_string(dir.path().join(format!("{session}.jsonl"))) {
            return Some(text);
        }
    }
    None
}

/// The first line of a transcript that carries a time: the seat's first act.
fn first_stamp(text: &str) -> Option<i64> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| v.get("timestamp").and_then(|t| t.as_str()).map(util::epoch_of).filter(|t| *t > 0))
}

/// A command a verifier runs to test or build: what `scripts/row-cost/measure.py`
/// counts as test and build seconds (compounding-122), so that the two agree.
pub(crate) fn is_test_command(cmd: &str) -> bool {
    let c = cmd.trim_start();
    if c.starts_with("echo") || c.starts_with("cat >") {
        return false;
    }
    ["check:box", "vitest", "cargo test", "cargo build", "playwright"].iter().any(|k| c.contains(k))
}

/// Seconds the Bash calls in a transcript that [`is_test_command`] names took,
/// from a call to its result.
pub(crate) fn test_secs(text: &str) -> i64 {
    let mut pending: std::collections::HashMap<String, (i64, bool)> = Default::default();
    let mut total = 0;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(t) = v["timestamp"].as_str().map(util::epoch_of).filter(|t| *t > 0) else { continue };
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            match (v["type"].as_str(), b["type"].as_str()) {
                (Some("assistant"), Some("tool_use")) if b["name"] == "Bash" => {
                    if let Some(id) = b["id"].as_str() {
                        pending.entry(id.to_string()).or_insert((t, is_test_command(b["input"]["command"].as_str().unwrap_or(""))));
                    }
                }
                (Some("user"), Some("tool_result")) => {
                    if let Some((at, test)) = b["tool_use_id"].as_str().and_then(|id| pending.remove(id)) {
                        if test {
                            total += (t - at).max(0);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    total
}

/// The first prompt a session was given, from its Claude Code transcript.
fn first_request(session: &str) -> Option<i64> {
    let text = transcript(session)?;
    for line in text.lines() {
        if !line.contains("\"type\":\"user\"") {
            continue;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()) {
            return Some(util::epoch_of(ts)).filter(|t| *t > 0);
        }
    }
    None
}

pub(crate) fn gather(store: &Store, id: &str) -> Inputs {
    let mut i = Inputs { id: id.to_string(), ..Default::default() };
    let group = store.worklists().into_iter().find_map(|w| {
        w.groups().iter().enumerate().find(|(_, g)| g.members.iter().any(|m| m == id)).map(|(n, g)| (w.id.clone(), n + 1, g.members.clone()))
    });
    let mut ids = vec![id.to_string()];
    if let Some((_, _, m)) = &group {
        ids.extend(m.iter().cloned());
    }
    if let Ok(text) = std::fs::read_to_string(store.state_file("events.jsonl")) {
        for line in text.lines() {
            if !ids.iter().any(|x| line.contains(x.as_str())) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let (Some(ts), Some(kind)) = (v["ts"].as_str(), v["kind"].as_str()) else { continue };
            let data = v["data"].clone();
            let mine = data["id"].as_str().is_some_and(|d| ids.iter().any(|x| x == d));
            let keep = if data["id"].as_str() == Some(id) { true } else { mine && kind == "task-verified" };
            if keep {
                i.events.push((util::epoch_of(ts), kind.to_string(), data));
            }
        }
    }
    let list = group.as_ref().map(|(l, _, _)| l.clone());
    if let Ok(text) = std::fs::read_to_string(store.state_file("cycle.log")) {
        for line in text.lines() {
            if line.len() < 21 || line.as_bytes()[19] != b'Z' || !line.is_char_boundary(20) {
                continue;
            }
            let (ts, rest) = line.split_at(20);
            let rest = rest.trim_start();
            if names(rest, id) || list.as_deref().is_some_and(|l| names(rest, l) && (rest.contains("barrier") || rest.contains("Barrier") || rest.contains("--check"))) {
                i.cycle.push((util::epoch_of(ts), rest.to_string()));
            }
        }
    }
    if let Some(t) = store.find_task(id) {
        i.legs = crate::legs::legs(&t)
            .into_iter()
            .map(|l| LegIn { skipped: l.skipped.is_some(), after: l.after.clone().unwrap_or_default(), name: l.name })
            .collect();
    }
    i.group = group;
    // The seats a cap line named as holders, and when they ended.
    let mut holders: Vec<String> = Vec::new();
    let mut checks: Vec<(String, String, usize)> = Vec::new();
    for (_, l) in &i.cycle {
        if let Some((_, after)) = l.split_once(" waits — the seat is at its cap") {
            for (_, held) in held_in(after) {
                match held {
                    Held::Work(r) | Held::Step(r) if !r.is_empty() && !holders.contains(&r) => holders.push(r),
                    Held::Check(label, list, at) if !checks.iter().any(|c| c.0 == label) => checks.push((label, list, at)),
                    _ => {}
                }
            }
        }
    }
    if !holders.is_empty() {
        if let Ok(text) = std::fs::read_to_string(store.state_file("events.jsonl")) {
            for line in text.lines() {
                if !holders.iter().any(|h| line.contains(h.as_str())) {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                let (Some(ts), Some(kind)) = (v["ts"].as_str(), v["kind"].as_str()) else { continue };
                if !matches!(kind, "task-review" | "task-released" | "task-verified") {
                    continue;
                }
                if let Some(row) = v["data"]["id"].as_str().filter(|r| holders.iter().any(|h| h == r)) {
                    i.holder_events.push((util::epoch_of(ts), kind.to_string(), row.to_string()));
                }
            }
        }
    }
    if !checks.is_empty() {
        // A check seat ends with its barrier: the line `tell <list>: Barrier <n> of <list> passed`.
        if let Ok(text) = std::fs::read_to_string(store.state_file("cycle.log")) {
            for line in text.lines() {
                if line.len() < 21 || line.as_bytes()[19] != b'Z' || !line.is_char_boundary(20) {
                    continue;
                }
                let (ts, rest) = line.split_at(20);
                let rest = rest.trim_start();
                for (label, list, at) in &checks {
                    if rest.starts_with(&format!("tell {list}: Barrier {at} of {list}")) && rest.contains("passed") {
                        i.holder_events.push((util::epoch_of(ts), "barrier-passed".to_string(), label.clone()));
                    }
                }
            }
        }
    }
    // The seat's own first line, and the verifier seats': the sessions learned
    // for the row after its first review that are not the seat's.
    let first_review = i.kind("task-review", id).first().map(|(t, _)| *t);
    let learned: Vec<(i64, String)> = i
        .kind("session-learned", id)
        .into_iter()
        .filter(|(_, d)| d.get("leg").is_none())
        .filter_map(|(t, d)| d["session"].as_str().map(|s| (t, s.to_string())))
        .collect();
    let mine: Vec<&String> = learned.iter().filter(|(t, _)| first_review.is_none_or(|r| *t <= r)).map(|(_, s)| s).collect();
    i.seat_first = mine.first().and_then(|s| transcript(s)).as_deref().and_then(first_stamp);
    // The verifier seats' own first lines and test seconds, which wsp wrote on
    // each pass when it ended the seat (the binding that names the session is
    // gone after the despawn).
    if let Some(t) = store.find_task(id) {
        for p in crate::verification::passes(&t) {
            let Some(sp) = p.spent.as_ref() else { continue };
            let Some(first) = sp.first_line.as_deref().map(util::epoch_of).filter(|t| *t > 0) else { continue };
            i.verifiers.push(VerifierSeat { learned: util::epoch_of(&p.at), first, test_secs: sp.test_secs });
        }
    }
    i.first_request = i.kind("session-learned", id).first().and_then(|(_, d)| d["session"].as_str().map(String::from)).and_then(|s| first_request(&s));
    i
}

// ---- the seat's own model time and tokens ------------------------------------

/// What the seat spent in one phase of the row.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SeatPhase {
    pub label: String,
    pub start: i64,
    pub end: i64,
    pub calls: u64,
    pub model_secs: i64,
    /// Fresh input plus cache writes.
    pub tokens_in: u64,
    pub cache_read: u64,
    pub tokens_out: u64,
    /// Seconds from a tool call to its result, by tool.
    pub tools: std::collections::BTreeMap<String, i64>,
}

impl SeatPhase {
    fn holds(&self, t: i64, last: bool) -> bool {
        t >= self.start && (t < self.end || (last && t == self.end))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SeatReport {
    pub sessions: Vec<String>,
    /// Sessions whose transcript could not be found.
    pub missing: Vec<String>,
    pub phases: Vec<SeatPhase>,
    /// Where the leg boundaries came from.
    pub source: String,
    /// When the seat was in a model call or a tool, from the transcripts read
    /// (not merged): what the wall-time buckets call a member working.
    pub active: Vec<(i64, i64)>,
}

/// One model call, once per requestId.
#[derive(Debug, Default)]
struct Call {
    at: i64,
    start: i64,
    end: i64,
    tin: u64,
    cache_read: u64,
    out: u64,
    /// What its tool uses ran: a Bash call's command, another tool's name.
    cmds: Vec<String>,
}

const TOOL_KINDS: [&str; 4] = ["Bash", "Read", "Edit", "Write"];

/// The model calls and tool times in one transcript. A call is a requestId:
/// one transcript line per content block repeats its usage, so the figures are
/// kept once (the largest seen). A call's model seconds run from the line
/// before it to its last line; a tool's from its call to its result.
fn read_transcript(text: &str) -> (Vec<Call>, Vec<(i64, String, i64)>) {
    let mut calls: Vec<Call> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = Default::default();
    let mut pending: std::collections::HashMap<String, (i64, String)> = Default::default();
    let mut tools: Vec<(i64, String, i64)> = Vec::new();
    let mut prev: Option<i64> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(t) = v["timestamp"].as_str().map(util::epoch_of).filter(|t| *t > 0) else { continue };
        match v["type"].as_str() {
            Some("assistant") => {
                let m = &v["message"];
                let key = v["requestId"].as_str().or_else(|| m["id"].as_str()).map(String::from);
                let n = match key.as_ref().and_then(|k| index.get(k)) {
                    Some(n) => *n,
                    None => {
                        calls.push(Call { at: t, start: prev.unwrap_or(t), end: t, ..Default::default() });
                        if let Some(k) = key {
                            index.insert(k, calls.len() - 1);
                        }
                        calls.len() - 1
                    }
                };
                let c = &mut calls[n];
                c.end = c.end.max(t);
                let u = &m["usage"];
                let num = |k: &str| u[k].as_u64().unwrap_or(0);
                c.tin = c.tin.max(num("input_tokens") + num("cache_creation_input_tokens"));
                c.cache_read = c.cache_read.max(num("cache_read_input_tokens"));
                c.out = c.out.max(num("output_tokens"));
                for b in m["content"].as_array().into_iter().flatten() {
                    if b["type"] == "tool_use" {
                        let ran = b["input"]["command"].as_str().or_else(|| b["name"].as_str()).unwrap_or("");
                        c.cmds.push(ran.trim().to_string());
                        if let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) {
                            pending.entry(id.to_string()).or_insert((t, name.to_string()));
                        }
                    }
                }
            }
            Some("user") => {
                for b in v["message"]["content"].as_array().into_iter().flatten() {
                    if b["type"] == "tool_result" {
                        if let Some((at, name)) = b["tool_use_id"].as_str().and_then(|id| pending.remove(id)) {
                            tools.push((at, name, (t - at).max(0)));
                        }
                    }
                }
            }
            _ => {}
        }
        prev = Some(t);
    }
    (calls, tools)
}

/// A session's test and build seconds and the time of its first line, as
/// `util::iso_at` writes it: what a verifier's pass keeps (`wsp-429`).
pub(crate) fn session_fixed(session: &str) -> Option<(i64, String)> {
    let text = transcript(session)?;
    Some((test_secs(&text), util::iso_at(first_stamp(&text)?)))
}

/// A session's model seconds and tool seconds from `since` on, read off its
/// Claude Code transcript (`compounding-104`: a verifier's usage lands on its
/// pass). `None` for a session with no transcript to read.
pub(crate) fn session_secs(session: &str, since: i64) -> Option<(i64, i64)> {
    let (calls, tools) = read_transcript(&transcript(session)?);
    let model = calls.iter().filter(|c| c.at >= since).map(|c| (c.end - c.start).max(0)).sum();
    let tool = tools.iter().filter(|(at, _, _)| *at >= since).map(|(_, _, s)| *s).sum();
    Some((model, tool))
}

/// The governor's turns inside one window of the run: from a group's barrier
/// pass to the next group's first start (`wsp-398`).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct GovernorWindow {
    /// The window's length, seconds.
    pub seconds: i64,
    /// Model calls the governor began inside it.
    pub turns: u64,
    pub model_secs: i64,
    pub tool_secs: i64,
    /// The window under neither the governor's model nor its tools: the run
    /// waiting on something that is not the governor.
    pub idle_secs: i64,
    /// Turns that only looked (a watch, a listing, a bare reply) and made no
    /// change: what a governor that skips a turn when nothing needs judgement
    /// would not take, and the model seconds they held.
    pub looking_turns: u64,
    pub looking_secs: i64,
}

/// A command that only reads the run: no choice was made by running it.
fn only_looks(cmd: &str) -> bool {
    let c = cmd.trim_start();
    ["wsp watch", "wsp ls", "wsp status", "wsp wip", "wsp worklist next", "wsp worklist show", "wsp timeline", "sleep "]
        .iter()
        .any(|p| c.starts_with(p))
}

/// One transcript of the governor, and the stretches of the run its pane held
/// the slot: a turn outside them is not the governor's (`wsp-398`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GovSource {
    /// An absolute path to the transcript (a seat's `burn.json` names one), or a
    /// bare session id looked for under `~/.claude/projects` (older events).
    pub transcript: String,
    /// `(from, to)` epoch seconds, `to == 0` for still held; empty = unclipped.
    pub spans: Vec<(i64, i64)>,
}

fn read_source(src: &GovSource) -> Option<String> {
    if src.transcript.starts_with('/') {
        std::fs::read_to_string(&src.transcript).ok()
    } else {
        transcript(&src.transcript)
    }
}

/// The governor's figures over `[from, to)` out of the transcripts of its
/// sessions. `None` when none could be read.
pub(crate) fn governor_window(sources: &[GovSource], from: i64, to: i64) -> Option<GovernorWindow> {
    let texts: Vec<(String, Vec<(i64, i64)>)> = sources.iter().filter_map(|s| read_source(s).map(|t| (t, s.spans.clone()))).collect();
    governor_window_of(&texts, from, to)
}

fn governor_window_of(texts: &[(String, Vec<(i64, i64)>)], from: i64, to: i64) -> Option<GovernorWindow> {
    if texts.is_empty() || to < from {
        return None;
    }
    let mut w = GovernorWindow { seconds: to - from, ..Default::default() };
    let now = util::epoch_secs();
    let held = |spans: &[(i64, i64)], at: i64| spans.is_empty() || spans.iter().any(|&(a, b)| at >= a && at <= if b == 0 { now } else { b });
    for (t, spans) in texts {
        let (calls, tools) = read_transcript(t);
        for c in calls.iter().filter(|c| c.at >= from && c.at < to && held(spans, c.at)) {
            let secs = (c.end - c.start).max(0);
            w.turns += 1;
            w.model_secs += secs;
            if c.cmds.iter().all(|x| only_looks(x)) {
                w.looking_turns += 1;
                w.looking_secs += secs;
            }
        }
        w.tool_secs += tools.iter().filter(|(at, _, _)| *at >= from && *at < to && held(spans, *at)).map(|(_, _, s)| *s).sum::<i64>();
    }
    w.idle_secs = (w.seconds - w.model_secs - w.tool_secs).max(0);
    Some(w)
}

/// The seat's own sessions: those learned for the row outside its legs' windows
/// and before it was first reviewed (a leg's seat and the verifier's are not it).
fn seat_sessions(i: &Inputs, windows: &[(i64, i64)]) -> Vec<String> {
    let review = i.kind("task-review", &i.id).first().map(|(t, _)| *t);
    let mut out: Vec<String> = Vec::new();
    for (t, d) in i.kind("session-learned", &i.id) {
        let Some(s) = d["session"].as_str() else { continue };
        if windows.iter().any(|(a, b)| t >= *a && t <= *b) || review.is_some_and(|r| t > r) || d.get("leg").is_some() {
            continue;
        }
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    }
    out
}

/// One leg's span: `(name, start, end)`.
pub(crate) type Span = (String, i64, i64);

/// The legs `run-leg.sh` timed: every `legs/*/timeline.json` under `dirs`,
/// whose `start` and `end` events fall inside `[from, to]` (the row's own
/// time, so another row's legs in the same directory are left out).
pub(crate) fn run_legs(dirs: &[std::path::PathBuf], from: i64, to: i64) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("timeline.json")) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            let at = |ev: &str| v["events"].as_array().into_iter().flatten().find(|x| x["ev"] == ev).and_then(|x| x["at"].as_f64());
            let (Some(a), Some(b)) = (at("start"), at("end")) else { continue };
            let (a, b) = (a.floor() as i64, b.ceil() as i64);
            if a < from.saturating_sub(1) || b > to + 1 {
                continue;
            }
            let name = match (v["component"].as_str(), v["leg"].as_str()) {
                (Some(c), Some(l)) => format!("{c}-{l}"),
                _ => e.file_name().to_string_lossy().to_string(),
            };
            out.push((name, a, b));
        }
    }
    out.sort_by_key(|(_, a, _)| *a);
    out
}

/// Where run-leg timelines may be: `legs` directories one to three levels under
/// the row's tree (`spikes/react-ports/legs`), never inside a dependency or
/// another worktree.
pub(crate) fn legs_dirs(tree: &std::path::Path) -> Vec<std::path::PathBuf> {
    let skip = |n: &str| n.starts_with('.') || n == "node_modules" || n == "target";
    let mut out = Vec::new();
    let mut level = vec![tree.to_path_buf()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for d in &level {
            if d.join("legs").is_dir() {
                out.push(d.join("legs"));
            }
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                if e.path().is_dir() && !skip(&e.file_name().to_string_lossy()) && e.file_name() != "legs" {
                    next.push(e.path());
                }
            }
        }
        level = next;
    }
    out
}

/// The `cwd` the first line of a transcript records: the tree the seat worked in.
fn cwd_of(text: &str) -> Option<std::path::PathBuf> {
    text.lines().take(50).find_map(|l| serde_json::from_str::<Value>(l).ok()?["cwd"].as_str().map(std::path::PathBuf::from))
}

/// The seat's phases: before the first leg, between each pair, after the last
/// up to review typed; one phase for a row with no legs. Boundaries are the
/// given legs, which run-leg's timelines or wsp's own leg entries supply.
fn seat_bounds(i: &Inputs, phases: &[Phase], legs: &[Span]) -> Vec<(String, i64, i64)> {
    let first_review = |from: i64| {
        let r: Vec<i64> = i.kind("task-review", &i.id).into_iter().map(|(t, _)| t).collect();
        r.iter().copied().find(|t| *t >= from).or_else(|| r.first().copied())
    };
    let last_leg = legs.iter().map(|l| l.2).max();
    let begin = phases.iter().map(|p| p.start).chain(legs.iter().map(|l| l.1)).min();
    let all_end = phases.iter().map(|p| p.end).chain(legs.iter().map(|l| l.2)).max();
    let end = first_review(last_leg.unwrap_or(0)).or(all_end);
    let (Some(begin), Some(end)) = (begin, end) else { return Vec::new() };
    let end = end.max(last_leg.unwrap_or(0));
    if legs.is_empty() {
        return vec![("the seat, spawn to review".into(), begin, end)];
    }
    let mut out = vec![(format!("before leg {}", legs[0].0), begin.min(legs[0].1), legs[0].1)];
    for w in legs.windows(2) {
        out.push((format!("between leg {} and leg {}", w[0].0, w[1].0), w[0].2, w[1].1.max(w[0].2)));
    }
    let l = &legs[legs.len() - 1];
    out.push((format!("after leg {} → review typed", l.0), l.2, end));
    out
}

/// The seat's tokens and model time per phase. `load` reads a session's
/// transcript; `None` is a transcript that cannot be found. `run_legs` are the
/// legs `run-leg.sh` timed; with none, the legs are wsp's own.
pub(crate) fn seat_report(i: &Inputs, phases: &[Phase], run_legs: &[Span], load: &dyn Fn(&str) -> Option<String>) -> SeatReport {
    let (legs, source): (Vec<Span>, &str) = if run_legs.is_empty() {
        let l = phases
            .iter()
            .filter(|p| !p.gap && p.label.starts_with("leg "))
            .map(|p| (p.label["leg ".len()..].split(" (").next().unwrap_or("").to_string(), p.start, p.end))
            .collect();
        (l, SEAT_SOURCE_WSP)
    } else {
        (run_legs.to_vec(), SEAT_SOURCE_RUN)
    };
    let windows: Vec<(i64, i64)> = legs.iter().map(|(_, a, b)| (*a, *b)).collect();
    let mut r = SeatReport { sessions: seat_sessions(i, &windows), source: source.to_string(), ..Default::default() };
    r.phases = seat_bounds(i, phases, &legs).into_iter().map(|(label, start, end)| SeatPhase { label, start, end, ..Default::default() }).collect();
    let last = r.phases.len().saturating_sub(1);
    let mut during = SeatPhase {
        label: "while a leg ran (the seat's own calls inside a leg; its wait on the leg is not counted)".into(),
        start: windows.iter().map(|w| w.0).min().unwrap_or(0),
        end: windows.iter().map(|w| w.1).max().unwrap_or(0),
        ..Default::default()
    };
    for s in r.sessions.clone() {
        let Some(text) = load(&s) else {
            r.missing.push(s);
            continue;
        };
        let (calls, tools) = read_transcript(&text);
        r.active.extend(calls.iter().filter(|c| c.end > c.start).map(|c| (c.start, c.end)));
        r.active.extend(tools.iter().filter(|(_, _, secs)| *secs > 0).map(|(at, _, secs)| (*at, at + secs)));
        for c in calls {
            let inside = windows.iter().any(|(a, b)| c.at >= *a && c.at < *b);
            let found = r.phases.iter_mut().enumerate().find(|(n, p)| p.holds(c.at, *n == last)).map(|(_, p)| p);
            if let Some(p) = found.or(if inside { Some(&mut during) } else { None }) {
                p.calls += 1;
                p.model_secs += (c.end - c.start).max(0);
                p.tokens_in += c.tin;
                p.cache_read += c.cache_read;
                p.tokens_out += c.out;
            }
        }
        // A tool's time is the part of its call-to-result span inside a phase:
        // the call that starts a leg and waits for it counts nothing of the leg.
        for (at, name, secs) in tools {
            let kind = if TOOL_KINDS.contains(&name.as_str()) { name } else { "other".to_string() };
            for p in r.phases.iter_mut() {
                let o = (at + secs).min(p.end) - at.max(p.start);
                if o > 0 {
                    *p.tools.entry(kind.clone()).or_insert(0) += o;
                }
            }
        }
    }
    if during.calls > 0 {
        r.phases.push(during);
    }
    r
}

const SEAT_SOURCE_WSP: &str = "phase boundaries from wsp's leg entries (cycle.log 'starting leg', leg-done events, the review)";
const SEAT_SOURCE_RUN: &str = "phase boundaries from run-leg.sh's timeline.json (start and end per leg), the end at the review";

fn render_seat(r: &SeatReport) -> String {
    let mut s = String::new();
    if r.sessions.is_empty() {
        return "the seat's own work: no session recorded for this row\n".into();
    }
    if r.missing.len() == r.sessions.len() {
        return format!("the seat's own work: transcript not found for session {}\n", r.missing.join(", "));
    }
    s.push_str(&format!("the seat's own work (times UTC; {}); tokens: in = fresh + cache write\n", r.source));
    for p in &r.phases {
        let tools: Vec<String> = p.tools.iter().map(|(k, v)| format!("{k} {v}s")).collect();
        s.push_str(&format!(
            "{}  {}  {:>6}  {}  {} calls · model {}s · in {} (cache read {}) out {} · tools {}\n",
            clock(p.start),
            clock(p.end),
            format!("{}s", p.end - p.start),
            p.label,
            p.calls,
            p.model_secs,
            p.tokens_in,
            p.cache_read,
            p.tokens_out,
            if tools.is_empty() { "none".to_string() } else { tools.join(", ") }
        ));
    }
    for m in &r.missing {
        s.push_str(&format!("transcript not found for session {m}; its calls are not counted\n"));
    }
    s
}

fn seat_json(r: &SeatReport) -> Value {
    let all_missing = r.sessions.is_empty() || r.missing.len() == r.sessions.len();
    json!({
        "source": r.source,
        "utc": true,
        "sessions": r.sessions,
        "missing": r.missing,
        "note": if r.sessions.is_empty() { json!("no session recorded for this row") } else if all_missing { json!(format!("transcript not found for session {}", r.missing.join(", "))) } else { Value::Null },
        "phases": if all_missing { Vec::new() } else { r.phases.iter().map(|p| {
            let mut tools = serde_json::Map::new();
            for k in TOOL_KINDS.iter().chain(["other"].iter()) {
                tools.insert((*k).to_string(), json!(p.tools.get(*k).copied().unwrap_or(0)));
            }
            json!({
                "label": p.label,
                "start": util::iso_at(p.start),
                "end": util::iso_at(p.end),
                "wall_seconds": p.end - p.start,
                "calls": p.calls,
                "model_seconds": p.model_secs,
                "tokens_in": p.tokens_in,
                "tokens_cache_read": p.cache_read,
                "tokens_out": p.tokens_out,
                "tool_seconds": tools,
            })
        }).collect::<Vec<_>>() },
    })
}

/// The legs `run-leg.sh` timed for this row: in `--legs DIR`, or else in the
/// `legs` directories of the tree the seat's transcript says it worked in.
fn run_legs_for(i: &Inputs, phases: &[Phase], flag: Option<String>) -> Vec<Span> {
    let from = phases.iter().map(|p| p.start).min().or_else(|| i.kind("task-claimed", &i.id).first().map(|(t, _)| *t)).unwrap_or(0);
    let to = i.kind("task-review", &i.id).last().map(|(t, _)| *t).or_else(|| phases.iter().map(|p| p.end).max()).unwrap_or(i64::MAX - 2);
    let dirs = match flag {
        Some(d) => vec![util::expand(&d)],
        None => {
            let tree = i.kind("session-learned", &i.id).into_iter().find_map(|(_, d)| d["session"].as_str().and_then(transcript)).as_deref().and_then(cwd_of);
            tree.map(|t| legs_dirs(&t)).unwrap_or_default()
        }
    };
    run_legs(&dirs, from, to)
}

/// One row's timeline figures, read by the same functions `wsp timeline` prints
/// from, for the effort report (`wsp-375`): nothing here is computed a second way.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct RowFigures {
    /// First phase start to last phase end, seconds.
    pub wall: i64,
    pub start: i64,
    pub end: i64,
    /// Sum of the gap phases.
    pub gap: i64,
    /// Gap seconds by what was being waited for.
    pub waits: std::collections::BTreeMap<String, i64>,
    /// The seat's own calls, model seconds, tokens, and tool seconds by kind.
    pub calls: u64,
    pub model_secs: i64,
    pub tokens_in: u64,
    pub cache_read: u64,
    pub tokens_out: u64,
    pub tools: std::collections::BTreeMap<String, i64>,
    /// Sessions whose transcript was not found: their calls are not counted.
    pub missing: usize,
    pub sessions: usize,
    /// The barrier phases (the group's, one run for all its members): their
    /// seconds, the gap among them, and that gap by what it waited for. They
    /// are inside `wall`, `gap` and `waits` too, for the row's own view.
    pub shared: i64,
    pub shared_gap: i64,
    pub shared_waits: std::collections::BTreeMap<String, i64>,
    /// The fixed parts the phases above hide (`wsp-429`): the row's seat from
    /// the spawn to its first transcript line; the verifier's from its spawn to
    /// its first line; the seconds its test and build commands ran, summed over
    /// its passes; and the seat's hold from the verdict to its despawn. `None`
    /// is not read (no transcript, no verifier, no despawn line yet).
    pub spawn_to_first: Option<i64>,
    pub verifier_spawn_to_first: Option<i64>,
    pub verifier_test_secs: i64,
    pub verdict_to_despawn: Option<i64>,
    /// What the wall-time buckets read (`wsp-470`): the row's own phases by what
    /// kind of time they are, when its seat was in a model call or a tool
    /// (merged), and when it stood blocked.
    pub spans: Vec<crate::wallbuckets::Span>,
    pub active: Vec<(i64, i64)>,
    pub blocked: Vec<(i64, i64)>,
    /// Every transcript the seat's calls come from was found: where one was
    /// not, its seat time cannot be told from idle and is not called waiting.
    pub measured: bool,
}

/// When the row stood blocked: from each `task-blocked` to the next event that
/// moves its status (a release, a todo, a reopen, a review, a park), or to the
/// row's last phase end.
pub(crate) fn blocked_spans(i: &Inputs, end: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut from: Option<i64> = None;
    let mut evs: Vec<&(i64, String, Value)> = i.events.iter().filter(|(_, _, d)| d["id"] == i.id.as_str()).collect();
    evs.sort_by_key(|e| e.0);
    for (t, k, _) in evs {
        match k.as_str() {
            "task-blocked" => {
                from.get_or_insert(*t);
            }
            "task-todo" | "task-doing" | "task-reopen" | "task-review" | "task-released" | "task-parked" | "task-done" | "task-claimed" => {
                if let Some(f) = from.take() {
                    out.push((f, (*t).max(f)));
                }
            }
            _ => {}
        }
    }
    if let Some(f) = from {
        out.push((f, end.max(f)));
    }
    out
}

/// The fixed parts of a row, from what [`gather`] read and the phases [`build`] made.
pub(crate) fn fixed_of(i: &Inputs, phases: &[Phase]) -> (Option<i64>, Option<i64>, i64, Option<i64>) {
    let spawn = spawn_line(i).or_else(|| phases.iter().map(|p| p.start).min());
    let member = match (spawn, i.seat_first) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    };
    // Only a phase that ends at the seat's own first line is that figure.
    let verifier = phases.iter().find(|p| p.label == VERIFIER_START).map(|p| p.seconds());
    let held = phases.iter().find(|p| p.label == VERIFIER_HELD).map(|p| p.seconds());
    (member, verifier, i.verifiers.iter().map(|v| v.test_secs).sum(), held)
}

pub(crate) fn row_figures(store: &Store, id: &str) -> RowFigures {
    let i = gather(store, id);
    let phases = build(&i);
    let run = run_legs_for(&i, &phases, None);
    let seat = seat_report(&i, &phases, &run, &transcript);
    let mut f = figures_of(&phases, &seat);
    f.blocked = blocked_spans(&i, f.end);
    (f.spawn_to_first, f.verifier_spawn_to_first, f.verifier_test_secs, f.verdict_to_despawn) = fixed_of(&i, &phases);
    f
}

/// The figures from the phases and the seat report, which `wsp timeline` prints
/// from too: [`to_json`] and [`seat_json`] are what a test holds this against.
pub(crate) fn figures_of(phases: &[Phase], seat: &SeatReport) -> RowFigures {
    let mut f = RowFigures::default();
    let a = phases.iter().map(|p| p.start).min().unwrap_or(0);
    let b = phases.iter().map(|p| p.end).max().unwrap_or(0);
    f.wall = (b - a).max(0);
    f.start = a;
    f.end = b;
    for p in phases.iter().filter(|p| p.shared) {
        f.shared += p.seconds();
    }
    for p in phases.iter().filter(|p| p.gap) {
        f.gap += p.seconds();
        let why = p.waiting_for.clone().unwrap_or_else(|| "unnamed".into());
        if p.shared {
            f.shared_gap += p.seconds();
            *f.shared_waits.entry(why.clone()).or_insert(0) += p.seconds();
        }
        *f.waits.entry(why).or_insert(0) += p.seconds();
    }
    f.sessions = seat.sessions.len();
    f.missing = seat.missing.len();
    f.measured = f.missing == 0 && f.sessions > 0;
    f.active = crate::wallbuckets::merge(&seat.active);
    f.spans = phases.iter().map(crate::wallbuckets::Span::of).collect();
    for p in &seat.phases {
        f.calls += p.calls;
        f.model_secs += p.model_secs;
        f.tokens_in += p.tokens_in;
        f.cache_read += p.cache_read;
        f.tokens_out += p.tokens_out;
        for (k, v) in &p.tools {
            *f.tools.entry(k.clone()).or_insert(0) += v;
        }
    }
    f
}

const USAGE: &str = "wsp timeline <row> [--json] [--legs DIR]";

pub fn timeline(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let t = match store.task_or_why(needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let i = gather(store, &t.id);
    let phases = build(&i);
    let seat_run_legs = run_legs_for(&i, &phases, args.get("legs"));
    let seat = seat_report(&i, &phases, &seat_run_legs, &transcript);
    if args.json() {
        let mut v = to_json(&i, &phases);
        v["utc"] = json!(true);
        v["seat"] = seat_json(&seat);
        println!("{v}");
    } else {
        print!("{}", render(&i, &phases, &Paint::new()));
        print!("{}", render_seat(&seat));
    }
    0
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["timeline"],
        run: timeline,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 620,
                text: r##"  wsp timeline <row> [--json] [--legs DIR]
                                    where a row's wall time goes, spawn to barrier:
                                    one line per phase, and the gaps where nothing
                                    ran named for what they waited on
                                    then the seat's own model time and tokens per
                                    phase, split at the row's legs (times UTC);
                                    legs from run-leg.sh's timeline.json under the
                                    row's tree, or --legs DIR, else wsp's own"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: i64, kind: &str, d: Value) -> (i64, String, Value) {
        (t, kind.into(), d)
    }

    fn row() -> Inputs {
        let id = "r-1";
        Inputs {
            id: id.into(),
            events: vec![
                ev(110, "task-claimed", json!({"id": id})),
                ev(112, "session-learned", json!({"id": id, "session": "s"})),
                ev(160, "leg-added", json!({"id": id, "leg": "a"})),
                ev(165, "task-released", json!({"id": id})),
                ev(200, "session-learned", json!({"id": id})),
                ev(300, "leg-done", json!({"id": id, "leg": "a"})),
                ev(305, "task-review", json!({"id": id})),
                ev(380, "task-verified", json!({"id": id})),
            ],
            cycle: vec![
                (100, "starting r-1 on claude (new)".into()),
                (190, "list group 1: starting leg a of r-1".into()),
                (306, "spawn r-1 --agent --verify --kind claude".into()),
                (310, "list group 1: started r-1 · verifying".into()),
            ],
            legs: vec![LegIn { name: "a".into(), ..Default::default() }],
            first_request: Some(118),
            group: None,
            holder_events: Vec::new(),
            ..Default::default()
        }
    }

    /// The verifier's seat is up long before wsp says `started` (`wsp-429`): the
    /// line is written when the step that spawned it ends.
    #[test]
    fn the_verifier_phase_starts_at_the_seats_first_line_not_wsps_started_line() {
        let mut i = row();
        i.verifiers = vec![VerifierSeat { learned: 306, first: 313, test_secs: 21 }];
        i.cycle[3].0 = 430;
        let ph = build(&i);
        let start = ph.iter().find(|p| p.label == VERIFIER_START).expect("from the first line");
        assert_eq!((start.start, start.end), (306, 313));
        let reads = ph.iter().find(|p| p.label.starts_with("verifier reads")).unwrap();
        assert_eq!(reads.start, 313, "the reading starts at the first line");
        assert!(!ph.iter().any(|p| p.label == VERIFIER_START_LINE));
        let (member, verifier, tests, _) = fixed_of(&i, &ph);
        assert_eq!((verifier, tests), (Some(7), 21));
        assert_eq!(member, None, "no transcript of the row's own seat was read");
    }

    #[test]
    fn without_the_first_line_wsps_started_line_is_the_start_and_says_it_lags() {
        let ph = build(&row());
        let start = ph.iter().find(|p| p.label == VERIFIER_START_LINE).expect("the fallback");
        assert_eq!((start.start, start.end), (306, 310));
        assert_eq!(fixed_of(&row(), &ph).1, None, "that is not the seat's first line, so it is not read");
    }

    #[test]
    fn the_seat_s_spawn_to_first_line_and_the_hold_after_the_verdict_are_read() {
        let mut i = row();
        i.seat_first = Some(104);
        i.cycle.push((385, "ended cpd-9, which verified r-1 (holds)".into()));
        let ph = build(&i);
        let (member, _, _, held) = fixed_of(&i, &ph);
        assert_eq!((member, held), (Some(4), Some(5)));
    }

    /// compounding-114 and 117: reviewed, verified, then reviewed again (a fix
    /// for the barrier) and released. The release is the seat's end, not where its
    /// work stopped, and the last review is not the one the verdict followed.
    #[test]
    fn a_row_reviewed_again_after_its_verdict_keeps_its_verifier_and_barrier_phases() {
        let mut i = row();
        i.events.retain(|(_, k, _)| k != "task-released" && k != "leg-added");
        i.legs.clear();
        i.events.push(ev(500, "task-review", json!({"id": "r-1"})));
        i.events.push(ev(900, "task-released", json!({"id": "r-1"})));
        let ph = build(&i);
        assert!(ph.iter().any(|p| p.label.starts_with("verifier reads") && p.end == 380), "{ph:?}");
        assert!(ph.iter().any(|p| p.label.starts_with("the seat's work until review") && p.end == 305), "{ph:?}");
        let after = ph.iter().find(|p| p.label.starts_with("the row's own seat after the verdict → released")).expect("named");
        assert_eq!((after.start, after.end), (380, 900));
    }

    #[test]
    fn test_and_build_commands_are_what_measure_py_counts_and_their_seconds_are_summed() {
        assert!(is_test_command("cd app && cargo test cmd_spawn::"));
        assert!(is_test_command("npm run check:box"));
        assert!(!is_test_command("echo cargo test"));
        assert!(!is_test_command("cat > t.sh <<EOF\ncargo test\nEOF"));
        assert!(!is_test_command("git status"));
        let text = [
            line(10, json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "cargo test x"}}]}})),
            line(40, json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "a"}]}})),
            line(41, json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "b", "name": "Bash", "input": {"command": "git diff"}}]}})),
            line(50, json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "b"}]}})),
        ]
        .join("\n");
        assert_eq!(test_secs(&text), 30);
        assert_eq!(first_stamp(&text), Some(10));
    }

    #[test]
    fn a_row_reads_as_phases_and_the_wait_after_a_release_is_named_a_gap() {
        let ph = build(&row());
        let gap = ph.iter().find(|p| p.label.starts_with("gap before leg a")).expect("a gap before the leg");
        assert_eq!((gap.start, gap.end, gap.seconds()), (165, 190, 25));
        assert!(gap.gap && gap.waiting_for.as_deref().unwrap().contains("tick"), "{gap:?}");
        assert!(ph.iter().any(|p| p.label.starts_with("leg a (seat up after 10s)") && p.seconds() == 110), "{ph:?}");
        assert!(ph.windows(2).all(|w| w[0].start <= w[1].start), "in time order");
        let ver = ph.iter().find(|p| p.label.starts_with("review typed → verifier")).unwrap();
        assert_eq!(ver.seconds(), 1);
        assert!(ver.waiting_for.as_deref().unwrap().contains("hand-off"));
    }

    #[test]
    fn a_leg_closed_late_is_timed_to_its_baton_and_the_stall_is_a_gap_not_work() {
        let mut i = row();
        for e in i.events.iter_mut().filter(|(_, k, d)| k == "leg-done" && d["leg"] == "a") {
            e.2["baton_at"] = json!(e.0 - 60);
            e.2["closed_by"] = json!("wsp: seat idle 9m");
        }
        let ph = build(&i);
        let leg = ph.iter().find(|p| p.label.starts_with("leg a (")).expect("the leg is a phase");
        let late = ph.iter().find(|p| p.label.starts_with("closed late: leg a done at")).expect("the stall is named");
        assert_eq!(leg.end, late.start, "work ends at the baton");
        assert_eq!(late.seconds(), 60);
        assert!(late.gap && late.waiting_for.as_deref().unwrap().contains("wsp: seat idle 9m"), "{late:?}");
    }

    #[test]
    fn a_cap_line_in_the_gap_names_the_cap() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-1 waits — the seat is at its cap of 4: x".into()));
        let ph = build(&i);
        let gap = ph.iter().find(|p| p.label.starts_with("gap before leg a")).unwrap();
        assert!(gap.waiting_for.as_deref().unwrap().contains("cap"), "{gap:?}");
    }

    #[test]
    fn a_cap_wait_names_its_holders_and_says_which_seat_had_finished() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-1 waits — the seat is at its cap of 4: r-9 · verifying in cpd-1, r-8 in cpd-2, r-7 starting".into()));
        i.holder_events.push((100, "task-verified".into(), "r-9".into()));
        i.holder_events.push((120, "task-review".into(), "r-8".into()));
        i.holder_events.push((900, "task-review".into(), "r-7".into()));
        let ph = build(&i);
        let why = ph.iter().filter_map(|p| p.waiting_for.clone()).find(|w| w.contains("cap")).expect("a cap wait");
        assert!(why.contains("r-9 · verifying in cpd-1 (finished "), "{why}");
        assert!(why.contains("r-8 in cpd-2 (finished "), "{why}");
        assert!(why.contains("r-7 starting (running)"), "{why}");
    }

    #[test]
    fn a_barrier_check_holder_that_passed_reads_finished_and_a_more_item_is_left_unlabelled() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-1 waits — the seat is at its cap of 4: other · barrier 2 in cpd-3, list · barrier 3 in cpd-4, 2 more".into()));
        i.holder_events.push((100, "barrier-passed".into(), "other · barrier 2".into()));
        i.holder_events.push((900, "barrier-passed".into(), "list · barrier 3".into()));
        let ph = build(&i);
        let why = ph.iter().filter_map(|p| p.waiting_for.clone()).find(|w| w.contains("cap")).expect("a cap wait");
        assert!(why.contains("other · barrier 2 in cpd-3 (finished "), "{why}");
        assert!(why.contains("list · barrier 3 in cpd-4 (running)"), "{why}");
        assert!(why.ends_with("2 more"), "{why}");
        assert!(!why.contains("2 more ("), "{why}");
    }

    #[test]
    fn a_cap_line_naming_the_row_as_a_holder_is_not_its_wait() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-2 waits — the seat is at its cap of 4: r-1 starting".into()));
        let ph = build(&i);
        let gap = ph.iter().find(|p| p.label.starts_with("gap before leg a")).unwrap();
        assert!(!gap.waiting_for.as_deref().unwrap().contains("cap"), "{gap:?}");
    }

    #[test]
    fn the_row_s_own_check_runs_from_review_to_the_task_checked_end() {
        let mut i = row();
        i.events.push(ev(405, "task-checked", json!({"id": "r-1", "took": 95})));
        i.cycle.retain(|(_, l)| !l.starts_with("spawn r-1 --agent --verify"));
        i.cycle.push((410, "spawn r-1 --agent --verify --kind claude".into()));
        i.cycle.push((415, "list group 1: started r-1 · verifying".into()));
        i.events.retain(|(_, k, _)| k != "task-verified");
        i.events.push(ev(500, "task-verified", json!({"id": "r-1"})));
        let ph = build(&i);
        let check = ph.iter().find(|p| p.label.starts_with("the row's own check")).expect("named");
        assert_eq!((check.start, check.end, check.gap), (305, 405, false));
        let after = ph.iter().find(|p| p.label.starts_with("row's check ended")).unwrap();
        assert_eq!(after.seconds(), 5);
        assert!(after.waiting_for.as_deref().unwrap().contains("hand-off"));
    }

    #[test]
    fn the_barrier_run_is_a_phase_from_its_start_with_per_step_seconds() {
        let mut i = row();
        i.group = Some(("list".into(), 1, vec!["r-1".into()]));
        i.cycle.push((1000, "list group 1: barrier run — 2 step(s), 0 reused, 2 ran, 0 failed, 600s · per step: build 100s; test 480s".into()));
        i.cycle.push((1010, "tell list: Barrier 1 of list passed".into()));
        let ph = build(&i);
        let run = ph.iter().find(|p| p.label.starts_with("barrier 1 run (")).expect("the run is a phase");
        assert_eq!((run.start, run.end, run.gap), (400, 1000, false));
        assert!(run.label.contains("per step: build 100s; test 480s"), "{}", run.label);
        let wait = ph.iter().find(|p| p.label.starts_with("last verdict → barrier 1 run started")).unwrap();
        assert_eq!((wait.start, wait.end), (380, 400));
        assert!(ph.iter().any(|p| p.label.starts_with("barrier 1 run ended → passed") && p.seconds() == 10), "{ph:?}");
        let unnamed: i64 = ph.iter().filter(|p| p.gap && p.waiting_for.as_deref().is_some_and(|w| w.starts_with("nothing the logs"))).map(|p| p.seconds()).sum();
        assert_eq!(unnamed, 0);
    }

    #[test]
    fn a_verifier_spawn_waiting_at_the_cap_is_the_cap_and_a_garbled_copied_run_line_reads_as_the_longest_run() {
        let mut i = row();
        // The cap line names the waiting step ("r-1 · verifying") before ` waits`.
        i.events.push(ev(405, "task-checked", json!({"id": "r-1", "took": 95})));
        i.cycle.retain(|(_, l)| !l.starts_with("spawn r-1 --agent --verify"));
        i.cycle.push((450, "list group 1: r-1 · verifying waits — the seat is at its cap of 4: x in cpd-1".into()));
        i.cycle.push((600, "spawn r-1 --agent --verify --kind claude".into()));
        i.cycle.push((605, "list group 1: started r-1 · verifying".into()));
        i.events.retain(|(_, k, _)| k != "task-verified");
        i.events.push(ev(700, "task-verified", json!({"id": "r-1"})));
        i.group = Some(("list".into(), 1, vec!["r-1".into()]));
        // Two copies wrote one line; the longer began first.
        i.cycle.push((1000, "list group 1: barrier run — 1 step(s), 1 reused, 0 ran, 0 failed, 35slist group 1: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 300s".into()));
        let ph = build(&i);
        let w = ph.iter().find(|p| p.label.starts_with("row's check ended")).unwrap();
        assert!(w.waiting_for.as_deref().unwrap().contains("cap"), "{w:?}");
        let run = ph.iter().find(|p| p.label.starts_with("barrier 1 run (")).unwrap();
        assert_eq!((run.start, run.end), (700, 1000));
        assert!(run.label.contains("1 ran, 0 failed, 300s)"), "{}", run.label);
    }

    #[test]
    fn an_id_is_matched_whole() {
        assert!(names("starting wsp-366 on x", "wsp-366"));
        assert!(!names("starting wsp-3660 on x", "wsp-366"));
        assert!(!names("starting xwsp-366", "wsp-366"));
    }

    fn line(t: i64, v: Value) -> String {
        let mut v = v;
        v["timestamp"] = json!(util::iso_at(t));
        v.to_string()
    }
    fn asst(t: i64, rid: &str, usage: (u64, u64, u64), tool: Option<(&str, &str)>) -> String {
        let content = match tool {
            Some((id, name)) => json!([{"type": "tool_use", "id": id, "name": name}]),
            None => json!([{"type": "text", "text": "x"}]),
        };
        line(t, json!({"type": "assistant", "requestId": rid, "message": {"content": content,
            "usage": {"input_tokens": usage.0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": usage.1, "output_tokens": usage.2}}}))
    }
    fn bash(t: i64, rid: &str, id: &str, cmd: &str) -> String {
        line(t, json!({"type": "assistant", "requestId": rid, "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": cmd}}],
            "usage": {"input_tokens": 1, "output_tokens": 1}}}))
    }

    #[test]
    fn the_governors_window_counts_its_turns_idle_and_the_ones_that_only_looked() {
        let text = [
            bash(1000, "r0", "t0", "wsp say x"),
            bash(1010, "r1", "t1", "wsp watch --once"),
            result(1012, "t1"),
            bash(1030, "r2", "t2", "wsp worklist go y"),
            result(1040, "t2"),
            bash(1200, "r3", "t3", "wsp say z"),
        ]
        .join("\n");
        let w = governor_window_of(&[(text, vec![])], 1005, 1100).unwrap();
        assert_eq!((w.seconds, w.turns, w.looking_turns), (95, 2, 1), "{w:?}");
        assert_eq!(w.tool_secs, 2 + 10);
        assert_eq!(w.looking_secs, 10);
        assert_eq!((w.model_secs, w.idle_secs), (28, 55), "{w:?}");
        assert!(governor_window_of(&[], 1005, 1100).is_none());
    }

    fn result(t: i64, id: &str) -> String {
        line(t, json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id}]}}))
    }

    fn two_legs() -> Inputs {
        let id = "r-1";
        Inputs {
            id: id.into(),
            events: vec![
                ev(110, "task-claimed", json!({"id": id})),
                ev(112, "session-learned", json!({"id": id, "session": "s"})),
                ev(200, "session-learned", json!({"id": id, "session": "l1"})),
                ev(300, "leg-done", json!({"id": id, "leg": "a"})),
                ev(410, "session-learned", json!({"id": id, "session": "l2"})),
                ev(450, "leg-done", json!({"id": id, "leg": "b"})),
                ev(460, "task-review", json!({"id": id})),
                ev(470, "session-learned", json!({"id": id, "session": "v"})),
            ],
            cycle: vec![
                (100, "starting r-1 on claude (new)".into()),
                (190, "list group 1: starting leg a of r-1".into()),
                (400, "list group 1: starting leg b of r-1".into()),
            ],
            legs: vec![LegIn { name: "a".into(), ..Default::default() }, LegIn { name: "b".into(), ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn the_seats_tokens_are_summed_once_per_request_and_split_at_the_legs() {
        let i = two_legs();
        let ph = build(&i);
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(120, "q1", (10, 100, 5), None),
            asst(121, "q1", (10, 100, 5), Some(("t1", "Bash"))),
            result(126, "t1"),
            asst(133, "q2", (20, 200, 7), Some(("t2", "Read"))),
            result(134, "t2"),
            asst(250, "q4", (99, 99, 99), None),
            line(305, json!({"type": "user", "message": {"content": "next"}})),
            asst(310, "q3", (1, 2, 3), Some(("t3", "Edit"))),
            result(315, "t3"),
            asst(455, "q5", (4, 0, 8), Some(("t5", "Write"))),
            result(459, "t5"),
        ]
        .join("\n");
        let r = seat_report(&i, &ph, &[], &|s| (s == "s").then(|| text.clone()));
        assert_eq!(r.sessions, vec!["s".to_string()], "a leg's and the verifier's sessions are not the seat's");
        assert!(r.missing.is_empty());
        let labels: Vec<&str> = r.phases.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels[..3], ["before leg a", "between leg a and leg b", "after leg b → review typed"]);
        assert!(labels[3].starts_with("while a leg ran"), "the seat's call inside a leg is kept: {labels:?}");
        assert_eq!((r.phases[3].calls, r.phases[3].tokens_out), (1, 99));
        let p = &r.phases[0];
        assert_eq!((p.calls, p.model_secs, p.tokens_in, p.cache_read, p.tokens_out), (2, 3 + 7, 30, 300, 12));
        assert_eq!((p.tools["Bash"], p.tools["Read"]), (5, 1));
        let p = &r.phases[1];
        assert_eq!((p.calls, p.model_secs, p.tokens_in, p.cache_read, p.tokens_out), (1, 5, 1, 2, 3));
        assert_eq!(p.tools["Edit"], 5);
        let p = &r.phases[2];
        assert_eq!((p.calls, p.tokens_in, p.tokens_out, p.tools["Write"]), (1, 4, 8, 4));
    }

    #[test]
    fn the_row_figures_the_effort_report_reads_are_what_timeline_json_prints() {
        let i = two_legs();
        let ph = build(&i);
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(120, "q1", (10, 100, 5), None),
            asst(121, "q1", (10, 100, 5), Some(("t1", "Bash"))),
            result(126, "t1"),
            asst(133, "q2", (20, 200, 7), Some(("t2", "Read"))),
            result(134, "t2"),
            line(305, json!({"type": "user", "message": {"content": "next"}})),
            asst(310, "q3", (1, 2, 3), Some(("t3", "Edit"))),
            result(315, "t3"),
            asst(455, "q5", (4, 0, 8), Some(("t5", "Write"))),
            result(459, "t5"),
        ]
        .join("\n");
        let seat = seat_report(&i, &ph, &[], &|s| (s == "s").then(|| text.clone()));
        let f = figures_of(&ph, &seat);
        let tl = to_json(&i, &ph);
        let sj = seat_json(&seat);
        assert!(f.gap > 0, "the row has a wait in it: {ph:?}");
        assert_eq!(f.gap, tl["gap_seconds"].as_i64().unwrap(), "gap seconds are the timeline's");
        assert_eq!(f.waits.values().sum::<i64>(), f.gap, "every wait is under a cause");
        let first = tl["phases"].as_array().unwrap().iter().map(|p| p["start"].as_str().unwrap().to_string()).min().unwrap();
        assert_eq!(util::iso_at(f.start), first);
        let sum = |k: &str| sj["phases"].as_array().unwrap().iter().map(|p| p[k].as_u64().unwrap()).sum::<u64>();
        assert_eq!(f.model_secs as u64, sum("model_seconds"));
        assert_eq!(f.calls, sum("calls"));
        assert_eq!((f.tokens_in, f.cache_read, f.tokens_out), (sum("tokens_in"), sum("tokens_cache_read"), sum("tokens_out")));
        for k in TOOL_KINDS.iter().chain(["other"].iter()) {
            let want: i64 = sj["phases"].as_array().unwrap().iter().map(|p| p["tool_seconds"][*k].as_i64().unwrap()).sum();
            assert_eq!(f.tools.get(*k).copied().unwrap_or(0), want, "tool kind {k}");
        }
        assert!(f.model_secs > 0 && f.tools.values().sum::<i64>() > 0, "the transcript was read: {f:?}");
    }

    #[test]
    fn a_missing_transcript_is_named_not_zero() {
        let i = two_legs();
        let ph = build(&i);
        let r = seat_report(&i, &ph, &[], &|_| None);
        assert_eq!(r.missing, vec!["s".to_string()]);
        let out = render_seat(&r);
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("transcript not found for session s"), "{out}");
        let j = seat_json(&r);
        assert!(j["phases"].as_array().unwrap().is_empty());
        assert!(j["note"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn a_row_with_no_legs_has_one_seat_phase() {
        let i = row();
        let ph = build(&i);
        let r = seat_report(&i, &ph, &[], &|_| Some(String::new()));
        let legless = Inputs { legs: vec![], ..row() };
        let r2 = seat_report(&legless, &build(&legless), &[], &|_| Some(String::new()));
        assert_eq!(r.phases.len(), 2);
        assert_eq!(r2.phases.len(), 1);
    }

    #[test]
    fn the_last_phase_ends_at_review_typed_not_at_the_verifier_or_the_barrier() {
        let mut i = two_legs();
        i.events.push(ev(520, "task-verified", json!({"id": "r-1"})));
        i.cycle.push((461, "spawn r-1 --agent --verify --kind claude".into()));
        i.cycle.push((470, "list group 1: started r-1 · verifying".into()));
        i.cycle.push((540, "list group 1: barrier-run 1".into()));
        i.cycle.push((600, "tell list: Barrier 1 of list passed".into()));
        i.group = Some(("list".into(), 1, vec!["r-1".into()]));
        let ph = build(&i);
        assert!(ph.iter().any(|p| p.end >= 600), "the timeline runs to the barrier: {ph:?}");
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(455, "q5", (4, 0, 8), None),
            asst(480, "q6", (50, 50, 50), None),
        ]
        .join("\n");
        let r = seat_report(&i, &ph, &[], &|s| (s == "s").then(|| text.clone()));
        let last = r.phases.last().unwrap();
        assert_eq!((last.start, last.end), (450, 460), "{last:?}");
        assert_eq!((last.calls, last.tokens_out), (1, 8), "a call after review typed is not the seat's work");
    }

    #[test]
    fn legs_run_leg_timed_split_the_seat_and_another_rows_legs_are_left_out() {
        let dir = std::env::temp_dir().join(format!("wsp-371-legs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let put = |name: &str, a: f64, b: f64| {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            let v = json!({"component": "board", "leg": name, "events": [{"ev": "start", "at": a}, {"ev": "end", "at": b}]});
            std::fs::write(dir.join(name).join("timeline.json"), v.to_string()).unwrap();
        };
        put("a", 150.4, 280.2);
        put("b", 300.0, 400.0);
        put("other", 9000.0, 9100.0);
        let legs = run_legs(&[dir.clone()], 100, 460);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(legs, vec![("board-a".to_string(), 150, 281), ("board-b".to_string(), 300, 400)]);
        let mut i = two_legs();
        i.cycle.clear();
        i.events.retain(|(_, k, _)| k != "leg-done");
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(149, "q1", (1, 1, 1), Some(("t1", "Bash"))),
            result(281, "t1"),
            asst(283, "q2", (2, 2, 2), None),
            asst(420, "q3", (3, 3, 3), None),
        ]
        .join("\n");
        let ph = build(&i);
        let r = seat_report(&i, &ph, &legs, &|s| (s == "s").then(|| text.clone()));
        assert!(r.source.contains("run-leg.sh"), "{}", r.source);
        let labels: Vec<&str> = r.phases.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["before leg board-a", "between leg board-a and leg board-b", "after leg board-b → review typed"]);
        assert_eq!((r.phases[0].start, r.phases[0].end), (110, 150));
        assert_eq!(r.phases[0].tools["Bash"], 1, "only the second before the leg; the wait inside it is the leg's");
        assert_eq!(r.phases[1].tools.get("Bash"), None, "the leg's wait is in no phase");
        assert_eq!(r.phases[1].calls, 1);
        assert_eq!((r.phases[2].start, r.phases[2].end, r.phases[2].calls), (400, 460, 1));
    }
}
