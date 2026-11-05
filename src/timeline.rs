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
        let done_event = i.events.iter().find(|(t, k, d)| k == "leg-done" && d["id"] == id && d["leg"] == leg.name.as_str() && *t >= s);
        // A close that came late (`wsp-372`) carries when the baton was written:
        // the leg's work ended then, and the rest is the stall, not work.
        let baton_at = done_event.and_then(|(t, _, d)| d["baton_at"].as_i64().filter(|b| *b >= s && b < t));
        let finished = baton_at
            .or_else(|| done_event.map(|(t, _, _)| *t))
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

/// A session's Claude Code transcript, whole.
fn transcript(session: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let root = std::path::Path::new(&home).join(".claude/projects");
    for dir in std::fs::read_dir(root).ok()?.flatten() {
        if let Ok(text) = std::fs::read_to_string(dir.path().join(format!("{session}.jsonl"))) {
            return Some(text);
        }
    }
    None
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
}

pub(crate) fn row_figures(store: &Store, id: &str) -> RowFigures {
    let i = gather(store, id);
    let phases = build(&i);
    let run = run_legs_for(&i, &phases, None);
    let seat = seat_report(&i, &phases, &run, &transcript);
    figures_of(&phases, &seat)
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
    for p in phases.iter().filter(|p| p.gap) {
        f.gap += p.seconds();
        *f.waits.entry(p.waiting_for.clone().unwrap_or_else(|| "unnamed".into())).or_insert(0) += p.seconds();
    }
    f.sessions = seat.sessions.len();
    f.missing = seat.missing.len();
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
