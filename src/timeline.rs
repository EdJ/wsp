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
}

impl Phase {
    fn work(label: impl Into<String>, start: i64, end: i64) -> Phase {
        Phase { label: label.into(), start, end: end.max(start), gap: false, waiting_for: None }
    }
    fn gap(label: impl Into<String>, start: i64, end: i64, why: String) -> Phase {
        Phase { label: label.into(), start, end: end.max(start), gap: true, waiting_for: Some(why) }
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

/// Why nothing was running between `from` and `to`.
fn wait_name(i: &Inputs, from: i64, to: i64) -> String {
    let span = to - from;
    let cap = i.cycle.iter().any(|(t, l)| *t >= from && *t <= to && l.contains(&i.id) && l.contains("waits — the seat is at its cap"));
    if cap {
        "the seat's cap (a place to come free)".into()
    } else if span <= HAND_OFF {
        "the hand-off (a poke and a spawn)".into()
    } else if span <= TICK + HAND_OFF {
        format!("a daemon tick (every {TICK}s) — nothing poked the step")
    } else {
        "nothing the logs name — the step did not run for longer than a tick".into()
    }
}

/// The phases of a row, in time order.
pub(crate) fn build(i: &Inputs) -> Vec<Phase> {
    let id = i.id.as_str();
    let mut out: Vec<Phase> = Vec::new();
    let claimed = i.kind("task-claimed", id).first().map(|(t, _)| *t);
    let released = i.kind("task-released", id).first().map(|(t, _)| *t);
    let reviews: Vec<i64> = i.kind("task-review", id).into_iter().map(|(t, _)| t).collect();
    let learned = i.kind("session-learned", id).first().map(|(t, _)| *t);

    // Spawn → the seat is up → its first request.
    let spawn = i.first_line(0, |l| {
        (l.contains(&format!("starting {id} on ")) || (l.starts_with(&format!("spawn {id} --agent")) && !l.contains("--leg") && !l.contains("--verify")))
            || l.contains(&format!(": starting {id} on "))
    });
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
        let finished = i
            .events
            .iter()
            .find(|(t, k, d)| k == "leg-done" && d["id"] == id && d["leg"] == leg.name.as_str() && *t >= s)
            .map(|(t, _, _)| *t)
            .or_else(|| reviews.iter().copied().find(|r| *r >= s));
        if ready > 0 && s > ready {
            out.push(Phase::gap(format!("gap before leg {}", leg.name), ready, s, wait_name(i, ready, s)));
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
        }
    }

    // Review → verifier → verdict.
    let review = reviews.last().copied();
    if let (Some(r), Some(e)) = (review, last_end) {
        if r > e {
            out.push(Phase::gap("after the last leg → review typed", e, r, "the row's own seat finishing (not wsp)".into()));
        }
    }
    let verdict = i.kind("task-verified", id).into_iter().map(|(t, _)| t).find(|t| review.is_none_or(|r| *t >= r));
    if let Some(r) = review {
        let spawned = i.first_line(r.saturating_sub(1), |l| l.starts_with(&format!("spawn {id} --agent --verify")));
        let up = spawned.and_then(|s| i.first_line(s, |l| l.contains(&format!("started {id} · verifying"))));
        if let Some(s) = spawned {
            out.push(Phase::gap("review typed → verifier spawn requested", r, s, wait_name(i, r, s)));
            if let Some(u) = up {
                out.push(Phase::work("verifier seat start", s, u));
                if let Some(v) = verdict {
                    out.push(Phase::work("verifier reads → verdict recorded (landed on a hold)", u, v));
                }
            }
        }
    }
    if let Some(v) = verdict {
        if let Some(e) = i.first_line(v, |l| l.starts_with("ended ") && l.contains(&format!("which verified {id}"))) {
            out.push(Phase::work("verdict → verifier seat ended (clean-up, alongside what follows)", v, e));
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
        let up = i.first_line(from, |l| l.contains(&format!("started {list} · barrier {at}")) || l.contains(&format!("{list} group {at}: barrier run")));
        let pass = i.first_line(from, |l| l.starts_with(&format!("tell {list}: Barrier {at} of {list}")));
        if let Some(b) = req.or(up) {
            let others = if last > verdict.unwrap_or(0) { " (other members' verdicts came later)" } else { "" };
            out.push(Phase::gap(format!("last verdict → barrier {at} started{others}"), from, b, wait_name(i, from, b)));
            if let Some(p) = pass.filter(|p| *p >= b) {
                out.push(Phase::work(format!("barrier {at} runs → passed"), b, p));
            }
        }
    }
    let _ = begin;
    out.sort_by_key(|p| (p.start, p.end));
    out
}

fn clock(t: i64) -> String {
    util::iso_at(t).get(11..19).unwrap_or("").to_string()
}

fn render(i: &Inputs, phases: &[Phase], p: &Paint) -> String {
    let mut s = String::new();
    let (Some(a), Some(b)) = (phases.iter().map(|x| x.start).min(), phases.iter().map(|x| x.end).max()) else {
        return format!("{}: nothing recorded to time\n", i.id);
    };
    s.push_str(&format!("{} · {} start to end\n", p.bold(&i.id), util::duration_human(b - a)));
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
        })).collect::<Vec<_>>(),
        "gap_seconds": phases.iter().filter(|x| x.gap).map(|x| x.seconds()).sum::<i64>(),
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

/// The first prompt a session was given, from its Claude Code transcript.
fn first_request(session: &str) -> Option<i64> {
    let home = std::env::var("HOME").ok()?;
    let root = std::path::Path::new(&home).join(".claude/projects");
    for dir in std::fs::read_dir(root).ok()?.flatten() {
        let file = dir.path().join(format!("{session}.jsonl"));
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for line in text.lines() {
            if !line.contains("\"type\":\"user\"") {
                continue;
            }
            let v: Value = serde_json::from_str(line).ok()?;
            if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()) {
                return Some(util::epoch_of(ts)).filter(|t| *t > 0);
            }
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
    i.first_request = i.kind("session-learned", id).first().and_then(|(_, d)| d["session"].as_str().map(String::from)).and_then(|s| first_request(&s));
    i
}

const USAGE: &str = "wsp timeline <row> [--json]";

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
    if args.json() {
        println!("{}", to_json(&i, &phases));
    } else {
        print!("{}", render(&i, &phases, &Paint::new()));
    }
    0
}

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
        }
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
    fn a_cap_line_in_the_gap_names_the_cap() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-1 waits — the seat is at its cap of 4: x".into()));
        let ph = build(&i);
        let gap = ph.iter().find(|p| p.label.starts_with("gap before leg a")).unwrap();
        assert!(gap.waiting_for.as_deref().unwrap().contains("cap"), "{gap:?}");
    }

    #[test]
    fn an_id_is_matched_whole() {
        assert!(names("starting wsp-366 on x", "wsp-366"));
        assert!(!names("starting wsp-3660 on x", "wsp-366"));
        assert!(!names("starting xwsp-366", "wsp-366"));
    }
}
