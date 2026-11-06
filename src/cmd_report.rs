//! A worklist hands back outputs (`wsp-375`): a **work report** (what was done)
//! and an **effort report** (what it cost, in time and money), as compound
//! artefacts in `~/.compound/artefacts/<list-slug>/` and never in a project's
//! repo. Ed: "they're not really permanent assets, just reports".
//!
//! - `effort.html` + `effort.json` are written by wsp, no agent. Every figure is
//!   read off the verb that owns it and not recomputed: `cmd_cost::cost_json`
//!   (tokens and USD, embedded whole under `cost`), `timeline::row_figures`
//!   (wall, model time, tool time, the waits and their causes) and
//!   `cmd_attempts::attempts_of` (retries, blocks, reworks).
//! - `work.html` is composed at the last barrier from what the list already
//!   records: per group the members, the commits landed, the files, the verdict
//!   and the follow-ups, then what was left out and what is open. The barrier
//!   check may overwrite it with its own words (`wsp artefact put`).
//! - **The gate**: the last barrier does not pass, and `done` refuses, while a
//!   declared output is missing. `done --force <why>` goes past it, logged.
//!
//! What a list declares is `outputs:` on the list: absent is `work` and
//! `effort`; `off` (what `--none` writes: front matter reads `none` as absent) is nothing; `name:kind:source` adds one and `-name` takes a
//! default off.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::model::{Worklist, WorklistStatus};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

pub const SCHEMA: &str = "wsp-effort/1";

pub const USAGE: &str = "usage:
  wsp worklist output <slug> [add <name> [--from <member>|--barrier|--wsp] [--kind html|md|json|png]]
  wsp worklist output <slug> rm <name>
  wsp worklist output <slug> --none
  wsp worklist report <slug> [--baseline <slug>] [--json]";

/// Who writes an output.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// wsp, no agent: the effort report.
    Wsp,
    /// The last barrier check: the work report.
    Barrier,
    /// A file this member writes into the list's artefacts folder.
    Member(String),
}

impl Source {
    fn word(&self) -> String {
        match self {
            Source::Wsp => "wsp".into(),
            Source::Barrier => "barrier".into(),
            Source::Member(m) => format!("member={m}"),
        }
    }
    fn parse(s: &str) -> Source {
        match s {
            "wsp" => Source::Wsp,
            "barrier" => Source::Barrier,
            m => Source::Member(m.strip_prefix("member=").unwrap_or(m).to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub name: String,
    pub kind: String,
    pub source: Source,
}

impl Output {
    pub fn file(&self) -> String {
        format!("{}.{}", self.name, self.kind)
    }
}

/// `~/.compound/artefacts/<slug>/`.
pub fn dir(slug: &str) -> PathBuf {
    crate::artefact::root().join(slug)
}

fn defaults() -> Vec<Output> {
    vec![
        Output { name: "work".into(), kind: "html".into(), source: Source::Barrier },
        Output { name: "effort".into(), kind: "html".into(), source: Source::Wsp },
    ]
}

/// What the list declares: the defaults unless `none`, less any `-name`, plus
/// every `name:kind:source` it adds.
pub fn declared(w: &Worklist) -> Vec<Output> {
    let raw = w.outputs.clone().unwrap_or_default();
    let items: Vec<&str> = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if items.contains(&"off") {
        return Vec::new();
    }
    let mut out = defaults();
    for i in &items {
        if let Some(name) = i.strip_prefix('-') {
            out.retain(|o| o.name != name);
            continue;
        }
        let mut p = i.splitn(3, ':');
        let (Some(name), kind, source) = (p.next(), p.next().unwrap_or("html"), p.next().unwrap_or("barrier")) else { continue };
        out.retain(|o| o.name != name);
        out.push(Output { name: name.into(), kind: kind.into(), source: Source::parse(source) });
    }
    out
}

fn encode(outs: &[Output], none: bool) -> Option<String> {
    if none {
        return Some("off".into());
    }
    let mut items: Vec<String> = Vec::new();
    for d in defaults() {
        if !outs.iter().any(|o| o.name == d.name) {
            items.push(format!("-{}", d.name));
        }
    }
    for o in outs {
        let is_default = defaults().iter().any(|d| *d == *o);
        if !is_default {
            items.push(format!("{}:{}:{}", o.name, o.kind, o.source.word()));
        }
    }
    (!items.is_empty()).then(|| items.join(","))
}

/// The files a declared output is still missing, by name.
pub fn missing(w: &Worklist) -> Vec<String> {
    let d = dir(&w.id);
    declared(w).iter().filter(|o| !d.join(o.file()).is_file()).map(|o| o.file()).collect()
}

/// One line for `wsp worklist show`.
pub fn line(w: &Worklist) -> String {
    let outs = declared(w);
    if outs.is_empty() {
        return "none declared".into();
    }
    let miss = missing(w);
    let names: Vec<String> = outs.iter().map(|o| if miss.contains(&o.file()) { format!("{} (not yet)", o.file()) } else { o.file() }).collect();
    format!("{} · {}", names.join(", "), dir(&w.id).display())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Every file goes in through `artefact::put_bytes` (`wsp-376`), so a report
/// written again keeps the one it replaces beside it as a version and the
/// passes can be compared. wsp is the writer; `note` says when.
fn write(slug: &str, file: &str, text: &str, note: &str) -> Result<PathBuf, String> {
    let who = crate::artefact::Who { row: Some(slug.to_string()), seat: Some("wsp".to_string()) };
    crate::artefact::put_bytes(&crate::artefact::root(), slug, file, text.as_bytes(), note, &who, false).map(|p| p.path)
}

// ---- the effort report -------------------------------------------------

fn hms(s: i64) -> String {
    let s = s.max(0);
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

fn usd(micros: Option<u64>) -> String {
    micros.map_or("unmeasured".into(), |m| format!("${:.2}", m as f64 / 1_000_000.0))
}

fn n(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// The effort report as the stable `effort.json` object.
pub fn effort(store: &Store, w: &Worklist, baseline: Option<&str>) -> Value {
    effort_with(store, w, baseline, &|id| crate::timeline::row_figures(store, id))
}

/// The median of the figures read, `0` for none (the row count is not hidden:
/// each member's own figure is beside it).
fn median(v: &mut [i64]) -> i64 {
    v.sort_unstable();
    match v.len() {
        0 => 0,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2,
    }
}

/// [`effort`] with the timeline figures of a row handed in, so that a test can
/// hold the sums to what it hands.
pub(crate) fn effort_with(store: &Store, w: &Worklist, baseline: Option<&str>, figures: &dyn Fn(&str) -> crate::timeline::RowFigures) -> Value {
    let groups = w.groups();
    let es = crate::cmd_cost::entries(store, &w.id).unwrap_or_default();
    let cost = crate::cmd_cost::cost_json(&w.id, &es, groups.len());

    let mut waits: BTreeMap<String, i64> = BTreeMap::new();
    let mut tools: BTreeMap<String, i64> = BTreeMap::new();
    let (mut model, mut idle, mut gap, mut calls) = (0i64, 0i64, 0i64, 0u64);
    let (mut start, mut end) = (i64::MAX, 0i64);
    let mut missing_sessions = 0usize;
    let mut group_json: Vec<Value> = Vec::new();
    let mut rework_rows: Vec<Value> = Vec::new();
    let (mut blocked, mut reopened, mut dropped, mut rework_usd) = (0u64, 0u64, 0u64, 0u64);
    let mut suites_total = 0u64;
    let (mut scoped_n, mut full_n, mut saved_secs, mut net_reds) = (0u64, 0u64, 0u64, 0u64);
    let mut net_red_rows: Vec<Value> = Vec::new();
    let (mut fixed_first, mut fixed_vfirst, mut fixed_held): (Vec<i64>, Vec<i64>, Vec<i64>) = Default::default();
    let mut fixed_tests = 0i64;
    let mut group_starts: Vec<(i64, i64)> = Vec::new();

    for (gi, g) in groups.iter().enumerate() {
        let (mut gwall, mut gmodel, mut gtool) = (0i64, 0i64, 0i64);
        let mut gsuites = 0u64;
        let mut members: Vec<Value> = Vec::new();
        // The barrier is the group's: one run, however many members. Each
        // member's figures hold it (their own timeline shows it); the sums take
        // it out of the members and put it back once per group (`wsp-390`).
        let mut shared: Option<crate::timeline::RowFigures> = None;
        let (mut gstart, mut glast) = (0i64, 0i64);
        for m in &g.members {
            let f = figures(m);
            if f.end > 0 && f.start > 0 && (gstart == 0 || f.start < gstart) {
                gstart = f.start;
            }
            if f.end > 0 {
                glast = glast.max(f.start);
            }
            if shared.as_ref().is_none_or(|s| f.shared > s.shared) {
                shared = Some(crate::timeline::RowFigures { shared: f.shared, shared_gap: f.shared_gap, shared_waits: f.shared_waits.clone(), ..Default::default() });
            }
            let suites = crate::throttle::full_suite_runs(&store.state, m);
            gsuites += suites;
            suites_total += suites;
            let tool: i64 = f.tools.values().sum();
            gwall += f.wall - f.shared;
            gmodel += f.model_secs;
            gtool += tool;
            model += f.model_secs;
            gap += f.gap - f.shared_gap;
            calls += f.calls;
            idle += (f.wall - f.shared - f.model_secs - tool).max(0);
            missing_sessions += f.missing;
            if f.end > 0 {
                start = start.min(f.start);
                end = end.max(f.end);
            }
            for (k, v) in &f.waits {
                let own = v - f.shared_waits.get(k).copied().unwrap_or(0);
                if own > 0 {
                    *waits.entry(k.clone()).or_insert(0) += own;
                }
            }
            for (k, v) in &f.tools {
                *tools.entry(k.clone()).or_insert(0) += v;
            }
            if let Some(t) = store.task(m) {
                for a in crate::cmd_attempts::attempts_of(&t) {
                    use crate::cmd_attempts::Outcome;
                    let counts = match a.outcome {
                        Outcome::Blocked => {
                            blocked += 1;
                            true
                        }
                        Outcome::Reopened => {
                            reopened += 1;
                            true
                        }
                        Outcome::Dropped => {
                            dropped += 1;
                            true
                        }
                        _ => false,
                    };
                    if counts {
                        let u = crate::cmd_attempts::priced(&a.ran, &a.spend);
                        rework_usd += u.unwrap_or(0);
                        rework_rows.push(json!({
                            "group": gi + 1, "task": m, "outcome": a.outcome.as_str(),
                            "seconds": a.seconds(), "turns": a.turns, "usd_micros": u,
                        }));
                    }
                }
            }
            members.push(json!({
                "task": m, "wall_seconds": f.wall, "model_seconds": f.model_secs,
                "tool_seconds": f.tools, "gap_seconds": f.gap, "calls": f.calls,
                "tokens_in": f.tokens_in, "tokens_cache_read": f.cache_read, "tokens_out": f.tokens_out,
                "transcripts_missing": f.missing, "full_suite_runs": suites,
                "spawn_to_first_line_seconds": f.spawn_to_first,
                "verifier_spawn_to_first_line_seconds": f.verifier_spawn_to_first,
                "verifier_test_seconds": f.verifier_test_secs,
                "verdict_to_despawn_seconds": f.verdict_to_despawn,
            }));
            fixed_first.extend(f.spawn_to_first);
            fixed_vfirst.extend(f.verifier_spawn_to_first);
            fixed_tests += f.verifier_test_secs;
            fixed_held.extend(f.verdict_to_despawn);
        }
        if let Some(sh) = shared {
            gwall += sh.shared;
            gap += sh.shared_gap;
            idle += sh.shared;
            for (k, v) in &sh.shared_waits {
                *waits.entry(k.clone()).or_insert(0) += v;
            }
        }
        group_starts.push((gstart, glast));
        let gcost = cost["groups"].get(gi).and_then(|c| c.get("total")).cloned().unwrap_or(Value::Null);
        // How the group's barrier was scoped, against a full run (`wsp-431`).
        let note = crate::scope::Note::read(store, &w.id, gi + 1);
        if let Some(n) = &note {
            if n.scoped {
                scoped_n += 1;
                saved_secs += n.saved();
            } else {
                full_n += 1;
            }
            net_reds += u64::from(!n.net_red.is_empty());
            if !n.net_red.is_empty() {
                net_red_rows.push(json!({ "group": gi + 1, "behind": n.net_red }));
            }
        }
        group_json.push(json!({
            "group": gi + 1, "wall_seconds": gwall, "model_seconds": gmodel, "tool_seconds": gtool,
            "cost": gcost, "full_suite_runs": gsuites, "members": members,
            "kind": g.kind, "barrier_scope": note.as_ref().map(|n| n.json()).unwrap_or(Value::Null),
        }));
    }
    let gov = governor_wall(store, &w.id, &group_starts);
    for (gi, gj) in group_json.iter_mut().enumerate() {
        gj["governor_wall"] = gov.get(gi).and_then(|x| x.as_ref()).map_or(Value::Null, GovWall::json);
    }
    let govs: Vec<&crate::timeline::GovernorWindow> = gov.iter().flatten().filter_map(|x| x.to_last.as_ref()).collect();
    let held: i64 = gov.iter().flatten().map(|x| x.last - x.pass).sum();
    let followups: Vec<Value> = crate::cmd_worklist::followups(w).into_iter().map(|(row, ids)| json!({ "by": row, "rows": ids })).collect();
    let mut wait_list: Vec<(String, i64)> = waits.into_iter().collect();
    wait_list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let tool_total: i64 = tools.values().sum();
    let total = cost["total"].clone();
    let wall = if end > start { end - start } else { 0 };

    let mut v = json!({
        "schema": SCHEMA,
        "list": w.id,
        "title": w.title,
        "generated": util::now_iso(),
        "status": w.status().as_str(),
        "scope": {
            "cost": "whole row: every seat — members, verifiers, barrier checks, governors",
            "full_suite": "full_suite_runs: full-suite `cargo test` runs started in a member's or verifier's seat (the shim's note, wsp-389), by row and group. The barrier's own run is one per group and is not counted here; the aim is zero in seats.",
            "barrier_scope": "groups[].barrier_scope: the barrier's run scoped to what the group touched (wsp-431) — scoped or full and why, the targets it ran, shared paths that widened it, its seconds against the last full run's, and net_red: scoped barriers a red full run stood behind, which is what scoping let through",
            "governor_wall": "groups[].governor_wall: between a group's barrier pass and the next group's first and last member start (wsp-398) — the seconds to each, the cycle-log line saying why a member waited (held_by), and the governor's turns, model and tool seconds and idle in each window, with the turns that only looked (a watch, a listing) and the model seconds they held, which is what a governor that skips a turn when nothing needs judgement would give back. Totals are over the windows to the last start. null where no group follows or no pass is found; the governor figures are null where its transcript is not found; its transcript is the one the seat's burn.json names, for each pane `governor-set` seated, clipped to the stretch it held the slot; burn.json holds only a seat's latest session and goes when the seat stops, so the figures are a floor",
            "time": "per member row, spawn to verdict (wall, gaps); the seat's model and tool time are its own transcript calls inside those phases, legs only where run-leg timings exist",
        },
        "totals": {
            "wall_seconds": wall,
            "model_seconds": model,
            "tool_seconds": tools,
            "tool_seconds_total": tool_total,
            "idle_seconds": idle,
            "gap_seconds": gap,
            "unnamed_wait_seconds": wait_list.iter().filter(|(c, _)| c.starts_with("nothing the logs")).map(|(_, s)| *s).sum::<i64>(),
            "calls": calls,
            "input_tokens": total["input_tokens"],
            "output_tokens": total["output_tokens"],
            "cache_read_tokens": total["cache_tokens"],
            "cache_write_tokens": total["cache_write_tokens"],
            "turns": total["turns"],
            "usd_micros": total["usd_micros"],
            "unmeasured_seats": total["unmeasured"],
            "transcripts_missing": missing_sessions,
            "full_suite_runs": suites_total,
            "pass_to_last_start_seconds": held,
            "governor_wall_seconds": govs.iter().map(|g| g.seconds).sum::<i64>(),
            "governor_wall_turns": govs.iter().map(|g| g.turns).sum::<u64>(),
            "governor_wall_model_seconds": govs.iter().map(|g| g.model_secs).sum::<i64>(),
            "governor_wall_idle_seconds": govs.iter().map(|g| g.idle_secs).sum::<i64>(),
            "governor_looking_turns": govs.iter().map(|g| g.looking_turns).sum::<u64>(),
            "governor_looking_seconds": govs.iter().map(|g| g.looking_secs).sum::<i64>(),
            "scoped_barriers": scoped_n,
            "full_barriers": full_n,
            "scoped_seconds_saved": saved_secs,
            "safety_net_reds": net_reds,
            "safety_net_misses": net_red_rows,
            "spawn_to_first_line_median_seconds": median(&mut fixed_first),
            "verifier_spawn_to_first_line_median_seconds": median(&mut fixed_vfirst),
            "verifier_test_seconds": fixed_tests,
            "verdict_to_despawn_median_seconds": median(&mut fixed_held),
        },
        "waits": wait_list.iter().map(|(c, s)| json!({ "cause": c, "seconds": s })).collect::<Vec<_>>(),
        "groups": group_json,
        "rework": {
            "blocked": blocked, "reopened": reopened, "dropped": dropped,
            "usd_micros": rework_usd, "attempts": rework_rows, "followups": followups,
        },
        "baseline": Value::Null,
        "conditions": { "load": "missing: the record carries no load figure", "thermals": "missing: the record carries no thermal figure" },
        "cost": cost,
    });
    if let Some(b) = baseline {
        v["baseline"] = against(&v, b);
    }
    v
}

/// What the run did between a group's barrier pass and the next group's
/// starts (`wsp-398`).
struct GovWall {
    pass: i64,
    /// The next group's first member start, and its last (all started).
    first: i64,
    last: i64,
    /// The governor's turns to each; `None` where its transcript is not found.
    to_first: Option<crate::timeline::GovernorWindow>,
    to_last: Option<crate::timeline::GovernorWindow>,
    /// The first line the cycle log wrote in the window saying why a member of
    /// the next group was waiting.
    held_by: Option<String>,
}

impl GovWall {
    fn json(&self) -> Value {
        let win = |w: &Option<crate::timeline::GovernorWindow>| {
            w.as_ref().map_or(Value::Null, |w| {
                json!({
                    "turns": w.turns, "model_seconds": w.model_secs, "tool_seconds": w.tool_secs, "idle_seconds": w.idle_secs,
                    "looking_turns": w.looking_turns, "looking_model_seconds": w.looking_secs,
                })
            })
        };
        json!({
            "pass_at": util::iso_at(self.pass),
            "first_start_at": util::iso_at(self.first), "last_start_at": util::iso_at(self.last),
            "seconds_to_first_start": self.first - self.pass, "seconds_to_last_start": self.last - self.pass,
            "governor_to_first_start": win(&self.to_first), "governor_to_last_start": win(&self.to_last),
            "held_by": self.held_by,
        })
    }
}

/// For each group, the window from its barrier pass (the `worklist-go` that
/// crossed it) to the next group's first and last member start
/// (`group_starts[i + 1]`), with the governor's turns in each: `None` for the
/// last group, a group not yet started, or a pass not found (`wsp-398`).
/// Group 3 → 4 of pre-prep: the group started at the pass (0 s) and its second
/// member waited 14.5 min — the seat's cap of 4 — which the last start shows.
fn governor_wall(store: &Store, list: &str, group_starts: &[(i64, i64)]) -> Vec<Option<GovWall>> {
    governor_wall_with(store, list, group_starts, &crate::timeline::governor_window)
}

/// The transcripts of the list's governor, each with the stretches its pane
/// held the slot. A governor pane never gets a `session-learned` event; its
/// session is the one its seat's `burn.json` names (`transcript`), for each
/// pane [`crate::cmd_cost::governors`] reads from `governor-set`/`-vacated`.
/// `burn.json` holds only the seat's latest session and goes with the seat's
/// directory when it stops, so a pane reseated within a tenure, or a seat long
/// stopped, gives nothing for the earlier session: the figures are a floor,
/// never a record made up. Older `session-learned` events that name the
/// project, or a pane that held the slot, are kept.
fn governor_sources(store: &Store, list: &str, groups: usize) -> Vec<crate::timeline::GovSource> {
    use crate::timeline::GovSource;
    let seats = crate::cmd_cost::governors(store, list, groups);
    let mut out: Vec<GovSource> = Vec::new();
    for s in &seats {
        for root in ["compound-seats", "seats"] {
            let rec = crate::place_super::read_json(&store.state.join(root).join(&s.seat).join("burn.json"));
            if let Some(t) = rec["transcript"].as_str().filter(|t| !t.is_empty()) {
                match out.iter_mut().find(|o| o.transcript == t) {
                    Some(o) => o.spans.extend(s.spans.iter().copied()),
                    None => out.push(GovSource { transcript: t.to_string(), spans: s.spans.clone() }),
                }
                break;
            }
        }
    }
    let text = std::fs::read_to_string(store.state_file("events.jsonl")).unwrap_or_default();
    let now = util::epoch_secs();
    for v in text.lines().filter(|l| l.contains("session-learned")).filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if v["kind"] != "session-learned" {
            continue;
        }
        let d = &v["data"];
        let at = util::epoch_of(v["ts"].as_str().unwrap_or(""));
        let held = d["pane"].as_str().is_some_and(|p| {
            seats.iter().any(|s| s.seat == p && s.spans.iter().any(|&(from, to)| at >= from && at <= if to == 0 { now } else { to }))
        });
        if !(held || d["project"] == list) {
            continue;
        }
        if let Some(s) = d["session"].as_str() {
            if !out.iter().any(|o| o.transcript.contains(s)) {
                out.push(GovSource { transcript: s.to_string(), spans: Vec::new() });
            }
        }
    }
    out
}

fn governor_wall_with(
    store: &Store,
    list: &str,
    group_starts: &[(i64, i64)],
    window: &dyn Fn(&[crate::timeline::GovSource], i64, i64) -> Option<crate::timeline::GovernorWindow>,
) -> Vec<Option<GovWall>> {
    let text = std::fs::read_to_string(store.state_file("events.jsonl")).unwrap_or_default();
    let mut passes: BTreeMap<u64, i64> = BTreeMap::new();
    for v in text.lines().filter(|l| l.contains("worklist-go")).filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let d = &v["data"];
        if v["kind"] == "worklist-go" && d["id"] == list {
            if let Some(p) = d["passed"].as_u64() {
                passes.entry(p).or_insert_with(|| util::epoch_of(v["ts"].as_str().unwrap_or("")));
            }
        }
    }
    let sessions = governor_sources(store, list, group_starts.len());
    let cycle = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
    (0..group_starts.len())
        .map(|gi| {
            let pass = *passes.get(&(gi as u64 + 1))?;
            let (first, last) = group_starts.get(gi + 1).copied().filter(|(f, _)| *f > 0)?;
            let (first, last) = (first.max(pass), last.max(first).max(pass));
            let head = format!("{list} group {}: ", gi + 2);
            let held_by = cycle
                .lines()
                .filter(|l| l.len() > 21 && l.is_char_boundary(20) && l.is_char_boundary(21))
                .filter(|l| {
                    let t = util::epoch_of(&l[..20]);
                    t >= pass && t <= last
                })
                .map(|l| l[20..].trim_start())
                .find(|r| r.starts_with(&head) && r.contains(" waits — "))
                .map(|r| r[head.len()..].to_string());
            Some(GovWall {
                pass,
                first,
                last,
                to_first: window(&sessions, pass, first),
                to_last: window(&sessions, pass, last),
                held_by,
            })
        })
        .collect()
}

/// The previous pass beside this one, each ratio shown (this ÷ baseline).
fn against(now: &Value, slug: &str) -> Value {
    let path = dir(slug).join("effort.json");
    let Some(then) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return json!({ "list": slug, "missing": format!("{} has no effort.json — `wsp worklist report {slug}` writes one", path.display()) });
    };
    let ratio = |k: &str| -> Value {
        let (a, b) = (n(&now["totals"], k), n(&then["totals"], k));
        json!({ "now": a, "baseline": b, "ratio": (b > 0).then(|| (a as f64 / b as f64 * 100.0).round() / 100.0) })
    };
    let wait_sum = |v: &Value| -> u64 { v["waits"].as_array().map_or(0, |a| a.iter().map(|w| n(w, "seconds")).sum()) };
    let (wa, wb) = (wait_sum(now), wait_sum(&then));
    json!({
        "list": slug,
        "wall_seconds": ratio("wall_seconds"), "model_seconds": ratio("model_seconds"),
        "idle_seconds": ratio("idle_seconds"), "gap_seconds": ratio("gap_seconds"),
        "input_tokens": ratio("input_tokens"), "output_tokens": ratio("output_tokens"),
        "cache_read_tokens": ratio("cache_read_tokens"), "cache_write_tokens": ratio("cache_write_tokens"),
        "turns": ratio("turns"), "usd_micros": ratio("usd_micros"), "full_suite_runs": ratio("full_suite_runs"),
        "spawn_to_first_line_median_seconds": ratio("spawn_to_first_line_median_seconds"),
        "verifier_spawn_to_first_line_median_seconds": ratio("verifier_spawn_to_first_line_median_seconds"),
        "verifier_test_seconds": ratio("verifier_test_seconds"),
        "verdict_to_despawn_median_seconds": ratio("verdict_to_despawn_median_seconds"),
        "waits_seconds": { "now": wa, "baseline": wb, "ratio": (wb > 0).then(|| (wa as f64 / wb as f64 * 100.0).round() / 100.0) },
    })
}

fn table(head: &[&str], rows: &[Vec<String>]) -> String {
    let mut s = String::from("<table><thead><tr>");
    for h in head {
        s.push_str(&format!("<th>{}</th>", esc(h)));
    }
    s.push_str("</tr></thead><tbody>");
    for r in rows {
        s.push_str("<tr>");
        for c in r {
            s.push_str(&format!("<td>{}</td>", esc(c)));
        }
        s.push_str("</tr>");
    }
    s.push_str("</tbody></table>");
    s
}

const STYLE: &str = "body{font:14px/1.5 -apple-system,system-ui,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem;color:#222}table{border-collapse:collapse;margin:.5rem 0 1.5rem}th,td{border:1px solid #ccc;padding:.25rem .6rem;text-align:left}th{background:#f4f4f4}h2{margin-top:2rem}.dim{color:#777}";

fn page(title: &str, body: &str) -> String {
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{STYLE}</style></head><body>{body}</body></html>\n", esc(title))
}

pub fn effort_html(v: &Value) -> String {
    let t = &v["totals"];
    let mut b = format!("<h1>Effort · {}</h1><p class=\"dim\">{} · status {} · generated {}</p>", esc(v["list"].as_str().unwrap_or("")), esc(v["title"].as_str().unwrap_or("")), esc(v["status"].as_str().unwrap_or("")), esc(v["generated"].as_str().unwrap_or("")));
    b.push_str(&format!("<p class=\"dim\">Cost: {}<br>Time: {}</p>", esc(v["scope"]["cost"].as_str().unwrap_or("")), esc(v["scope"]["time"].as_str().unwrap_or(""))));
    b.push_str("<h2>1 · Totals</h2>");
    let tool_kinds: Vec<String> = t["tool_seconds"].as_object().map(|o| o.iter().map(|(k, s)| format!("{k} {}", hms(s.as_i64().unwrap_or(0)))).collect()).unwrap_or_default();
    b.push_str(&table(
        &["figure", "value"],
        &[
            vec!["wall (first spawn to last verdict)".into(), hms(n(t, "wall_seconds") as i64)],
            vec!["model time".into(), hms(n(t, "model_seconds") as i64)],
            vec!["tool time".into(), format!("{} ({})", hms(n(t, "tool_seconds_total") as i64), if tool_kinds.is_empty() { "none read".into() } else { tool_kinds.join(", ") })],
            vec!["idle (wall under neither model nor tool, per row)".into(), hms(n(t, "idle_seconds") as i64)],
            vec!["tokens in".into(), n(t, "input_tokens").to_string()],
            vec!["tokens out".into(), n(t, "output_tokens").to_string()],
            vec!["cache read".into(), n(t, "cache_read_tokens").to_string()],
            vec!["cache write".into(), n(t, "cache_write_tokens").to_string()],
            vec!["turns".into(), n(t, "turns").to_string()],
            vec!["USD".into(), usd(t["usd_micros"].as_u64())],
            vec!["unmeasured seats".into(), n(t, "unmeasured_seats").to_string()],
            vec!["transcripts not found".into(), n(t, "transcripts_missing").to_string()],
            vec!["full suites run in member or verifier seats".into(), n(t, "full_suite_runs").to_string()],
            vec!["each pass to the next group's last start: window / governor turns / governor idle".into(), format!("{} / {} / {}", hms(n(t, "pass_to_last_start_seconds") as i64), n(t, "governor_wall_turns"), hms(n(t, "governor_wall_idle_seconds") as i64))],
            vec!["governor turns that only looked (model time a skipped turn gives back)".into(), format!("{} ({})", n(t, "governor_looking_turns"), hms(n(t, "governor_looking_seconds") as i64))],
            vec!["barriers scoped / full".into(), format!("{} / {}", n(t, "scoped_barriers"), n(t, "full_barriers"))],
            vec!["seconds scoped barriers ran under a full run".into(), n(t, "scoped_seconds_saved").to_string()],
            vec!["safety-net reds scoping let through".into(), n(t, "safety_net_reds").to_string()],
            vec!["seat spawn to its first line (median per row)".into(), hms(n(t, "spawn_to_first_line_median_seconds") as i64)],
            vec!["verifier spawn to its first line (median per row)".into(), hms(n(t, "verifier_spawn_to_first_line_median_seconds") as i64)],
            vec!["verifier test and build time (all passes)".into(), hms(n(t, "verifier_test_seconds") as i64)],
            vec!["verdict to the verifier's despawn (median per row)".into(), hms(n(t, "verdict_to_despawn_median_seconds") as i64)],
        ],
    ));
    b.push_str("<h2>2 · Waits between stages, largest first</h2>");
    let waits: Vec<Vec<String>> = v["waits"].as_array().map(|a| a.iter().map(|w| vec![w["cause"].as_str().unwrap_or("").to_string(), hms(n(w, "seconds") as i64)]).collect()).unwrap_or_default();
    b.push_str(&if waits.is_empty() { "<p>No gaps recorded.</p>".to_string() } else { table(&["waiting for", "total"], &waits) });
    b.push_str("<h2>3 · Per group, then per member</h2>");
    for g in v["groups"].as_array().into_iter().flatten() {
        let c = &g["cost"];
        b.push_str(&format!("<h3>group {} · wall {} · model {} · tools {} · {} · {} turns · {} full suite(s) in seats</h3>", g["group"], hms(n(g, "wall_seconds") as i64), hms(n(g, "model_seconds") as i64), hms(n(g, "tool_seconds") as i64), usd(c["usd_micros"].as_u64()), n(c, "turns"), n(g, "full_suite_runs")));
        let rows: Vec<Vec<String>> = g["members"].as_array().into_iter().flatten().map(|m| vec![m["task"].as_str().unwrap_or("").into(), hms(n(m, "wall_seconds") as i64), hms(n(m, "model_seconds") as i64), hms(n(m, "gap_seconds") as i64), n(m, "tokens_in").to_string(), n(m, "tokens_out").to_string(), n(m, "full_suite_runs").to_string()]).collect();
        b.push_str(&table(&["member", "wall", "model", "gaps", "tokens in", "tokens out", "full suites"], &rows));
        let gw = &g["governor_wall"];
        if gw.is_object() {
            let w = &gw["governor_to_last_start"];
            let gov = if w.is_object() {
                format!("governor {} turns · model {} · tools {} · idle {} · {} looked only ({})", n(w, "turns"), hms(n(w, "model_seconds") as i64), hms(n(w, "tool_seconds") as i64), hms(n(w, "idle_seconds") as i64), n(w, "looking_turns"), hms(n(w, "looking_model_seconds") as i64))
            } else {
                "governor transcript not found".to_string()
            };
            b.push_str(&format!(
                "<p>next group: first start {} after the pass, last start {} after it · {}{}</p>",
                hms(n(gw, "seconds_to_first_start") as i64), hms(n(gw, "seconds_to_last_start") as i64), esc(&gov),
                gw["held_by"].as_str().map_or(String::new(), |h| format!(" · held: {}", esc(h)))
            ));
        }
    }
    let r = &v["rework"];
    b.push_str(&format!("<h2>4 · Rework</h2><p>{} blocked · {} reopened · {} dropped · {}</p>", n(r, "blocked"), n(r, "reopened"), n(r, "dropped"), usd(r["usd_micros"].as_u64())));
    let rows: Vec<Vec<String>> = r["attempts"].as_array().into_iter().flatten().map(|a| vec![format!("group {}", a["group"]), a["task"].as_str().unwrap_or("").into(), a["outcome"].as_str().unwrap_or("").into(), hms(n(a, "seconds") as i64), usd(a["usd_micros"].as_u64())]).collect();
    if !rows.is_empty() {
        b.push_str(&table(&["group", "row", "outcome", "time", "cost"], &rows));
    }
    for f in r["followups"].as_array().into_iter().flatten() {
        let ids: Vec<&str> = f["rows"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        b.push_str(&format!("<p>follow-up by {}: {}</p>", esc(f["by"].as_str().unwrap_or("")), esc(&ids.join(" "))));
    }
    b.push_str("<h2>5 · Against a baseline</h2>");
    let bl = &v["baseline"];
    if bl.is_null() {
        b.push_str("<p>No baseline given (<code>--baseline &lt;slug&gt;</code>).</p>");
    } else if let Some(m) = bl["missing"].as_str() {
        b.push_str(&format!("<p>{}</p>", esc(m)));
    } else {
        let rows: Vec<Vec<String>> = ["wall_seconds", "model_seconds", "idle_seconds", "gap_seconds", "waits_seconds", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "turns", "usd_micros", "full_suite_runs"]
            .iter()
            .map(|k| vec![k.to_string(), bl[*k]["now"].to_string(), bl[*k]["baseline"].to_string(), bl[*k]["ratio"].as_f64().map_or("—".into(), |r| format!("×{r}"))])
            .collect();
        b.push_str(&format!("<p>against {}</p>", esc(bl["list"].as_str().unwrap_or(""))));
        b.push_str(&table(&["figure", "this pass", "baseline", "ratio"], &rows));
    }
    b.push_str(&format!("<h2>6 · Conditions</h2><p>load: {}<br>thermals: {}</p>", esc(v["conditions"]["load"].as_str().unwrap_or("")), esc(v["conditions"]["thermals"].as_str().unwrap_or(""))));
    page(&format!("Effort · {}", v["list"].as_str().unwrap_or("")), &b)
}

/// Write `effort.json` and `effort.html`; the paths. `note` is the version's
/// note: "effort report, at group 2 of 5".
pub fn write_effort(store: &Store, w: &Worklist, baseline: Option<&str>, note: &str) -> Result<(PathBuf, PathBuf), String> {
    let v = effort(store, w, baseline);
    let j = write(&w.id, "effort.json", &serde_json::to_string_pretty(&v).unwrap_or_default(), note)?;
    let h = write(&w.id, "effort.html", &effort_html(&v), note)?;
    Ok((h, j))
}

fn note_now(store: &Store, w: &Worklist) -> String {
    let pos = crate::worklist::position(store, w, crate::worklist::Reading::Settled);
    match pos.at {
        Some(n) => format!("effort report, at group {n} of {}", pos.of),
        None => format!("effort report, after group {}", pos.of),
    }
}

// ---- the work report ---------------------------------------------------

/// `work.html`, composed from the list's record. `last` is the sentence the
/// last barrier is being passed with, which is not on the record yet.
pub fn work_html(store: &Store, w: &Worklist, last: Option<&str>) -> String {
    let groups = w.groups();
    let mut b = format!("<h1>Work · {}</h1><p>{}</p>", esc(&w.id), esc(&w.title));
    b.push_str(&format!("<p class=\"dim\">Cost and time are in <a href=\"effort.html\">effort.html</a>.</p>"));
    let overview = w.section("Overview").unwrap_or_default();
    if !overview.trim().is_empty() {
        b.push_str(&format!("<h2>Goal and stop lines</h2><pre>{}</pre>", esc(overview.trim())));
    }
    let followups = crate::cmd_worklist::followups(w);
    let mut left_out: Vec<String> = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        b.push_str(&format!("<h2>Group {} — {}</h2>", i + 1, esc(&if g.caption.trim().is_empty() { g.members.join(" ") } else { g.caption.clone() })));
        if !g.stop.trim().is_empty() {
            b.push_str(&format!("<p class=\"dim\">stop: {}</p>", esc(g.stop.trim())));
        }
        let mut rows: Vec<Vec<String>> = Vec::new();
        for m in &g.members {
            let status = store.task(m).map_or("missing".to_string(), |t| t.status().as_str().to_string());
            if status != "done" {
                left_out.push(format!("{m} (group {}) is {status}", i + 1));
            }
            let (commit, files) = match g.landed.iter().find(|l| l.member == *m) {
                Some(l) => (l.commit.clone().map(|c| c.chars().take(10).collect::<String>()).unwrap_or_else(|| "unplaced".into()), l.files.join(", ")),
                None if g.landed.is_empty() && g.verdict.trim().is_empty() => ("pending (the barrier has not been passed)".into(), String::new()),
                None => ("unplaced".into(), String::new()),
            };
            let title = store.task(m).map(|t| t.title.clone()).unwrap_or_default();
            rows.push(vec![m.clone(), title, status, commit, files]);
        }
        b.push_str(&table(&["row", "title", "status", "commit", "files"], &rows));
        let verdict = if g.verdict.trim().is_empty() && i + 1 == groups.len() { last.unwrap_or("").trim().to_string() } else { g.verdict.trim().to_string() };
        if !verdict.is_empty() {
            b.push_str(&format!("<p><b>verdict:</b> {}</p>", esc(&verdict)));
        }
        for (row, ids) in followups.iter().filter(|(_, ids)| ids.iter().any(|x| g.members.contains(x))) {
            b.push_str(&format!("<p>follow-ups added by {}: {}</p>", esc(row), esc(&ids.join(" "))));
        }
    }
    b.push_str("<h2>Not done, and what is open</h2>");
    if left_out.is_empty() {
        b.push_str("<p>Every member is done.</p>");
    } else {
        b.push_str(&format!("<ul>{}</ul>", left_out.iter().map(|l| format!("<li>{}</li>", esc(l))).collect::<String>()));
    }
    page(&format!("Work · {}", w.id), &b)
}

/// Write `work.html` unless it is there already (the check may have put its own).
pub fn write_work(store: &Store, w: &Worklist, last: Option<&str>) -> Result<Option<PathBuf>, String> {
    let p = dir(&w.id).join("work.html");
    if p.is_file() {
        return Ok(None);
    }
    write(&w.id, "work.html", &work_html(store, w, last), "work report, at the last barrier")?;
    Ok(Some(p))
}

/// At the last barrier: effort first, then the work report, then whatever
/// declared output is still missing. `Err` names them and holds the barrier.
pub fn at_last_barrier(store: &Store, w: &Worklist, said: &str, dry: bool) -> Result<(), Vec<String>> {
    let outs = declared(w);
    if dry || outs.is_empty() {
        return Ok(());
    }
    if outs.iter().any(|o| o.name == "effort") {
        let _ = write_effort(store, w, None, "effort report, at the last barrier");
    }
    if outs.iter().any(|o| o.name == "work") {
        let _ = write_work(store, w, Some(said));
    }
    let miss = missing(w);
    if miss.is_empty() {
        Ok(())
    } else {
        Err(miss)
    }
}

// ---- the verbs -----------------------------------------------------------

pub fn report(store: &Store, args: &Args) -> i32 {
    let Some(slug) = args.rest.get(1) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let Some(w) = store.worklist(slug) else {
        eprintln!("wsp: no worklist {slug}");
        return 1;
    };
    let baseline = args.get("baseline");
    match write_effort(store, &w, baseline.as_deref(), &note_now(store, &w)) {
        Ok((h, j)) => {
            if args.json() {
                println!("{}", json!({ "list": w.id, "effort_html": h, "effort_json": j }));
            } else {
                let p = Paint::new();
                println!("{} {}", p.bold(&w.id), p.dim("effort report written"));
                println!("  {}\n  {}", h.display(), j.display());
            }
            0
        }
        Err(e) => {
            eprintln!("wsp: {e}");
            1
        }
    }
}

pub fn output(store: &Store, args: &Args) -> i32 {
    let Some(slug) = args.rest.get(1) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let Some(mut w) = store.worklist(slug) else {
        eprintln!("wsp: no worklist {slug}");
        return 1;
    };
    let mut outs = declared(&w);
    let mut none = declared(&w).is_empty() && w.outputs.as_deref().is_some_and(|o| o.split(',').any(|x| x.trim() == "off"));
    let did = if args.has("none") {
        none = true;
        "no outputs".to_string()
    } else {
        match args.rest.get(2).map(String::as_str) {
            None | Some("ls") => {
                if args.json() {
                    println!("{}", json!({ "list": w.id, "dir": dir(&w.id), "outputs": outs.iter().map(|o| json!({ "name": o.name, "kind": o.kind, "source": o.source.word(), "file": o.file(), "present": dir(&w.id).join(o.file()).is_file() })).collect::<Vec<_>>() }));
                } else {
                    println!("{}", line(&w));
                }
                return 0;
            }
            Some("add") => {
                let Some(name) = args.rest.get(3) else {
                    eprintln!("{USAGE}");
                    return 2;
                };
                if name.is_empty() || name.contains(['/', ':', ',']) || name.starts_with(['.', '-']) {
                    eprintln!("wsp: `{name}` is not a usable output name");
                    return 2;
                }
                let kind = args.get("kind").unwrap_or_else(|| "html".into());
                if !["html", "md", "json", "png"].contains(&kind.as_str()) {
                    eprintln!("wsp: kind is html, md, json or png, not `{kind}`");
                    return 2;
                }
                let source = if let Some(m) = args.get("from") {
                    Source::Member(m)
                } else if args.has("wsp") {
                    Source::Wsp
                } else {
                    Source::Barrier
                };
                none = false;
                outs.retain(|o| o.name != *name);
                outs.push(Output { name: name.clone(), kind, source });
                format!("added {name}")
            }
            Some("rm" | "remove") => {
                let Some(name) = args.rest.get(3) else {
                    eprintln!("{USAGE}");
                    return 2;
                };
                if !outs.iter().any(|o| o.name == *name) {
                    eprintln!("wsp: {} declares no output `{name}`", w.id);
                    return 1;
                }
                outs.retain(|o| o.name != *name);
                format!("removed {name}")
            }
            Some(other) => {
                eprintln!("wsp: worklist output: `{other}` is not add, rm or ls\n{USAGE}");
                return 2;
            }
        }
    };
    w.outputs = encode(&outs, none);
    w.log(&format!("outputs — {did} · by {}", crate::cmd_message::caller(store)));
    if let Err(e) = store.save_worklist(&w) {
        eprintln!("wsp: {e}");
        return 1;
    }
    store.git_commit(&format!("wsp: worklist {} outputs: {did}", w.id));
    println!("{} {}", w.id, line(&w));
    0
}

/// `done` over a missing output: refuse unless forced with a why.
pub fn gate_done(w: &Worklist, force: Option<&str>) -> Result<Option<String>, Vec<String>> {
    if w.status() == WorklistStatus::Done {
        return Ok(None);
    }
    let miss = missing(w);
    if miss.is_empty() {
        return Ok(None);
    }
    match force {
        Some(why) if !why.trim().is_empty() => Ok(Some(format!("forced past missing output(s) {} — {}", miss.join(", "), why.trim()))),
        _ => Err(miss),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;

    fn list(slug: &str, members: &[&str]) -> (util::Isolated, Store, Worklist) {
        let env = util::isolated(&format!("report-{slug}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let w = more(&store, slug, members);
        (env, store, w)
    }

    /// Another list in the same store: the isolation lock is held by the first.
    fn more(store: &Store, slug: &str, members: &[&str]) -> Worklist {
        for m in members {
            store.save_task(&Task::new(m, m)).unwrap();
        }
        let d = |args: &Args| crate::cmd_worklist::dispatch(store, args);
        assert_eq!(d(&Args::synth("worklist", &["new", slug, "A list"], &[])), 0);
        let mut add = vec!["add", slug];
        add.extend(members);
        assert_eq!(d(&Args::synth("worklist", &add, &[])), 0);
        store.worklist(slug).unwrap()
    }

    fn run(store: &Store, argv: &[&str], flags: &[(&str, &str)]) -> i32 {
        crate::cmd_worklist::dispatch(store, &Args::synth("worklist", argv, flags))
    }

    #[test]
    fn every_list_declares_work_and_effort_unless_it_says_none() {
        let (_e, store, w) = list("decl", &["r-1"]);
        let names: Vec<String> = declared(&w).iter().map(Output::file).collect();
        assert_eq!(names, ["work.html", "effort.html"]);
        assert_eq!(run(&store, &["output", "decl", "add", "notes"], &[("kind", "md"), ("from", "r-1")]), 0);
        let w = store.worklist("decl").unwrap();
        let o = declared(&w).into_iter().find(|o| o.name == "notes").unwrap();
        assert_eq!((o.file().as_str(), o.source.clone()), ("notes.md", Source::Member("r-1".into())));
        assert_eq!(run(&store, &["output", "decl", "rm", "work"], &[]), 0);
        let w = store.worklist("decl").unwrap();
        assert_eq!(declared(&w).iter().map(Output::file).collect::<Vec<_>>(), ["effort.html", "notes.md"]);
        assert_eq!(run(&store, &["output", "decl"], &[("none", "true")]), 0);
        assert!(declared(&store.worklist("decl").unwrap()).is_empty());
        assert_eq!(run(&store, &["output", "decl", "add", "work"], &[]), 0);
        assert_eq!(declared(&store.worklist("decl").unwrap()).iter().map(Output::file).collect::<Vec<_>>(), ["work.html"]);
    }

    #[test]
    fn show_names_the_outputs_and_what_is_not_there_yet() {
        let (_e, _s, w) = list("shown", &["r-1"]);
        let l = line(&w);
        assert!(l.contains("work.html (not yet)") && l.contains("effort.html (not yet)") && l.contains("shown"), "{l}");
    }

    #[test]
    fn a_pass_to_the_next_groups_last_start_names_the_wait_that_held_it() {
        let (_e, store, _w) = list("gw", &["r-1", "r-2"]);
        store.log_event("worklist-go", json!({ "id": "gw", "passed": 1, "started": false }));
        let go = std::fs::read_to_string(store.state_file("events.jsonl")).unwrap();
        // The pass is the one the event log stamped; the starts and the log are placed after it.
        let pass = util::epoch_of(go.lines().last().and_then(|l| serde_json::from_str::<Value>(l).ok()).unwrap()["ts"].as_str().unwrap());
        std::fs::write(
            store.state_file("cycle.log"),
            format!("{} gw group 2: r-2 waits — the seat is at its cap of 4: a, b, c, d\n", util::iso_at(pass + 5)),
        )
        .unwrap();
        let w = governor_wall(&store, "gw", &[(pass - 100, pass - 50), (pass + 1, pass + 870)]);
        let g = w[0].as_ref().expect("a window after group 1");
        assert_eq!((g.first - g.pass, g.last - g.pass), (1, 870));
        assert_eq!(g.held_by.as_deref(), Some("r-2 waits — the seat is at its cap of 4: a, b, c, d"));
        assert!(g.to_last.is_none(), "no governor transcript here: named as null, not zero");
        assert!(w[1].is_none(), "the last group has no next start");
        assert_eq!(g.json()["seconds_to_last_start"], 870);
    }

    /// `wsp-398`, found by the verifier: a governor's pane never gets a
    /// `session-learned` event. Its session is the transcript its seat's
    /// `burn.json` names, so that file is what the fixture writes. A member's
    /// session on another pane (a `session-learned` event), and a turn of the
    /// governor's transcript after the slot was vacated, are not counted.
    #[test]
    fn the_governors_turns_come_from_the_transcript_its_seats_burn_json_names() {
        let (env, store, _w) = list("gv", &["r-1", "r-2"]);
        let t0: i64 = util::epoch_secs() - 10_000;
        let ev = |at: i64, kind: &str, data: Value| json!({ "ts": util::iso_at(at), "kind": kind, "data": data }).to_string();
        let log = [
            ev(t0, "governor-set", json!({ "project": "gv", "workspace": "cpd-1" })),
            ev(t0 + 6, "session-learned", json!({ "id": "r-1", "kind": "claude", "pane": "cpd-7", "session": "member-s" })),
            ev(t0 + 100, "worklist-go", json!({ "id": "gv", "passed": 1, "started": false })),
            ev(t0 + 180, "governor-vacated", json!({ "project": "gv", "workspace": "cpd-1" })),
        ]
        .join("\n");
        std::fs::write(store.state_file("events.jsonl"), format!("{log}\n")).unwrap();
        let root = env.home().join("transcripts");
        std::fs::create_dir_all(&root).unwrap();
        let turn = |at: i64, rid: &str, cmd: &str| {
            json!({ "type": "assistant", "requestId": rid, "timestamp": util::iso_at(at),
                "message": { "content": [{ "type": "tool_use", "id": rid, "name": "Bash", "input": { "command": cmd } }], "usage": { "input_tokens": 1, "output_tokens": 1 } } })
            .to_string()
        };
        let put = |name: &str, lines: Vec<String>| {
            let p = root.join(format!("{name}.jsonl"));
            std::fs::write(&p, lines.join("\n")).unwrap();
            p
        };
        // The third governor turn is after the slot was vacated (t0 + 180).
        let gov = put("gov-s", vec![turn(t0 + 110, "g1", "wsp say x"), turn(t0 + 150, "g2", "wsp watch --once"), turn(t0 + 190, "g3", "wsp say y")]);
        put("member-s", vec![turn(t0 + 120, "m1", "cargo test"), turn(t0 + 130, "m2", "cargo test"), turn(t0 + 140, "m3", "cargo test")]);
        let seat = store.state.join("compound-seats/cpd-1");
        std::fs::create_dir_all(&seat).unwrap();
        std::fs::write(seat.join("burn.json"), json!({ "turns": 3, "session_id": "gov-s", "transcript": gov.to_string_lossy() }).to_string()).unwrap();
        let sources = governor_sources(&store, "gv", 2);
        assert_eq!(sources.len(), 1, "the governor's transcript only: {sources:?}");
        assert_eq!(sources[0].transcript, gov.to_string_lossy());
        let w = governor_wall_with(&store, "gv", &[(t0, t0 + 50), (t0 + 200, t0 + 250)], &crate::timeline::governor_window);
        let g = w[0].as_ref().expect("a window after group 1");
        let last = g.to_last.as_ref().expect("the governor's turns are found through burn.json");
        assert_eq!((last.turns, last.looking_turns), (2, 1), "the governor's two turns inside its tenure, not the member's three: {last:?}");
        assert_eq!(g.json()["governor_to_last_start"]["turns"], 2);
    }

    #[test]
    fn the_effort_report_agrees_with_cost_and_keeps_a_stable_shape() {
        let (_e, store, w) = list("eff", &["r-1", "r-2"]);
        assert_eq!(run(&store, &["report", "eff"], &[]), 0);
        let d = dir("eff");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(d.join("effort.json")).unwrap()).unwrap();
        assert_eq!(v["schema"], SCHEMA);
        for k in ["totals", "waits", "groups", "rework", "baseline", "conditions", "cost", "scope"] {
            assert!(v.get(k).is_some(), "effort.json lacks {k}");
        }
        let es = crate::cmd_cost::entries(&store, "eff").unwrap_or_default();
        let cost = crate::cmd_cost::cost_json("eff", &es, w.groups().len());
        assert_eq!(v["cost"], cost, "the cost block is `wsp worklist cost --json`, whole");
        assert_eq!(v["totals"]["usd_micros"], cost["total"]["usd_micros"]);
        assert_eq!(v["totals"]["turns"], cost["total"]["turns"]);
        assert_eq!(v["totals"]["input_tokens"], cost["total"]["input_tokens"]);
        assert_eq!(v["groups"][0]["members"].as_array().unwrap().len(), 2);
        let html = std::fs::read_to_string(d.join("effort.html")).unwrap();
        for h in ["1 · Totals", "2 · Waits", "3 · Per group", "4 · Rework", "5 · Against a baseline", "6 · Conditions", "missing: the record carries no load"] {
            assert!(html.contains(h), "effort.html lacks {h}");
        }
    }

    #[test]
    fn a_baseline_sits_beside_the_pass_with_each_ratio() {
        let (_e, store, w) = list("pass2", &["r-1"]);
        let mut then = effort(&store, &w, None);
        then["totals"]["usd_micros"] = json!(2_000_000);
        then["totals"]["turns"] = json!(10);
        write("pass1", "effort.json", &then.to_string(), "t").unwrap();
        let mut now = effort(&store, &w, None);
        now["totals"]["usd_micros"] = json!(3_000_000);
        now["totals"]["turns"] = json!(5);
        let b = against(&now, "pass1");
        assert_eq!(b["usd_micros"]["ratio"], json!(1.5));
        assert_eq!(b["turns"]["ratio"], json!(0.5));
        assert!(against(&now, "nope")["missing"].is_string());
    }

    #[test]
    fn done_waits_for_the_declared_outputs_and_force_with_a_why_is_logged() {
        let (_e, store, _w) = list("fin", &["r-1"]);
        assert_eq!(run(&store, &["done", "fin"], &[]), 1, "no reports yet");
        assert_ne!(store.worklist("fin").unwrap().status(), WorklistStatus::Done);
        assert_eq!(run(&store, &["done", "fin"], &[("force", "true")]), 1, "force without a why");
        assert_eq!(run(&store, &["done", "fin", "scratch", "list"], &[("force", "true")]), 0);
        let w = store.worklist("fin").unwrap();
        assert_eq!(w.status(), WorklistStatus::Done);
        assert!(w.section("Log").unwrap().contains("forced past missing output(s) work.html, effort.html — scratch list"));
    }

    #[test]
    fn done_passes_once_the_outputs_exist_and_on_a_list_with_none() {
        let (_e, store, w) = list("ok", &["r-1"]);
        write_effort(&store, &w, None, "t").unwrap();
        assert_eq!(run(&store, &["done", "ok"], &[]), 1, "work.html is still missing");
        write_work(&store, &w, None).unwrap();
        assert_eq!(run(&store, &["done", "ok"], &[]), 0);
        more(&store, "bare", &["r-2"]);
        assert_eq!(run(&store, &["output", "bare"], &[("none", "true")]), 0);
        assert_eq!(run(&store, &["done", "bare"], &[]), 0);
    }

    #[test]
    fn the_last_barrier_writes_effort_then_work_and_holds_on_what_a_member_owes() {
        let (_e, store, w) = list("last", &["r-1"]);
        assert_eq!(run(&store, &["output", "last", "add", "notes"], &[("kind", "md"), ("from", "r-1")]), 0);
        let w2 = store.worklist("last").unwrap();
        let held = at_last_barrier(&store, &w2, "fine", false).unwrap_err();
        assert_eq!(held, ["notes.md"], "named, and the two defaults were written");
        assert!(dir("last").join("effort.html").is_file() && dir("last").join("work.html").is_file());
        assert!(std::fs::read_to_string(dir("last").join("work.html")).unwrap().contains("effort.html"));
        write("last", "notes.md", "x", "t").unwrap();
        assert!(at_last_barrier(&store, &w2, "fine", false).is_ok());
        // a dry run writes nothing
        let w3 = more(&store, "dry", &["r-2"]);
        assert!(at_last_barrier(&store, &w3, "x", true).is_ok());
        assert!(!dir("dry").join("effort.html").exists());
        let _ = w;
    }

    #[test]
    fn two_reports_leave_two_versions_and_a_check_seats_own_work_html_survives() {
        let (_e, store, w) = list("vers", &["r-1"]);
        assert_eq!(run(&store, &["report", "vers"], &[]), 0);
        assert_eq!(run(&store, &["report", "vers"], &[("baseline", "pass0")]), 0);
        let root = crate::artefact::root();
        for f in ["effort.html", "effort.json"] {
            let v = crate::artefact::versions(&root, "vers", f);
            assert_eq!(v.len(), 2, "{f}: the first report is kept beside the second");
            assert!(v.iter().all(|x| !x.edited), "{f}: written through put, so nothing is 'changed outside'");
        }
        // the check seat puts its own work report first: wsp keeps it
        let who = crate::artefact::Who { row: None, seat: Some("cpd-1".into()) };
        crate::artefact::put_bytes(&root, "vers", "work.html", b"its own", "n", &who, false).unwrap();
        assert!(write_work(&store, &w, None).unwrap().is_none());
        assert_eq!(std::fs::read_to_string(dir("vers").join("work.html")).unwrap(), "its own");
        let (_, _) = write_effort(&store, &w, None, "again").unwrap();
    }

    #[test]
    fn the_totals_are_the_sum_of_the_members_figures_the_timeline_gives() {
        use crate::timeline::RowFigures;
        let (_e, store, w) = list("sums", &["r-1", "r-2"]);
        store.save_task(&Task::new("r-3", "r-3")).unwrap();
        assert_eq!(run(&store, &["add", "sums", "r-3"], &[]), 0);
        let w = store.worklist("sums").unwrap_or(w);
        let fig = |id: &str| match id {
            "r-1" => RowFigures { start: 100, end: 400, wall: 300, gap: 40, model_secs: 120, calls: 5, tokens_in: 10, tokens_out: 4, cache_read: 7, waits: [("a tick".to_string(), 25), ("the cap".to_string(), 15)].into(), tools: [("Bash".to_string(), 50)].into(), spawn_to_first: Some(20), verifier_spawn_to_first: Some(7), verifier_test_secs: 40, verdict_to_despawn: Some(180), ..Default::default() },
            "r-2" => RowFigures { start: 90, end: 300, wall: 210, gap: 30, model_secs: 60, calls: 3, waits: [("a tick".to_string(), 30)].into(), tools: [("Bash".to_string(), 10), ("Read".to_string(), 5)].into(), missing: 1, spawn_to_first: Some(10), verifier_spawn_to_first: Some(9), verifier_test_secs: 5, verdict_to_despawn: Some(60), ..Default::default() },
            _ => RowFigures { start: 500, end: 600, wall: 100, gap: 0, model_secs: 20, calls: 1, ..Default::default() },
        };
        let v = effort_with(&store, &w, None, &fig);
        let t = &v["totals"];
        assert_eq!(t["model_seconds"], 200);
        assert_eq!(t["gap_seconds"], 70);
        assert_eq!(t["calls"], 9);
        assert_eq!(t["wall_seconds"], 510, "first spawn to last verdict: 90 to 600");
        assert_eq!(t["tool_seconds"]["Bash"], 60);
        assert_eq!(t["tool_seconds_total"], 65);
        assert_eq!(t["transcripts_missing"], 1);
        // The fixed parts: each row's own, a median over the rows that read one, and the tests summed.
        assert_eq!(t["spawn_to_first_line_median_seconds"], 15);
        assert_eq!(t["verifier_spawn_to_first_line_median_seconds"], 8);
        assert_eq!(t["verifier_test_seconds"], 45);
        assert_eq!(t["verdict_to_despawn_median_seconds"], 120);
        let first = v["groups"][0]["members"].as_array().unwrap().iter().find(|m| m["task"] == "r-1").unwrap();
        assert_eq!((first["spawn_to_first_line_seconds"].as_i64(), first["verifier_test_seconds"].as_i64(), first["verdict_to_despawn_seconds"].as_i64()), (Some(20), Some(40), Some(180)));
        assert_eq!(t["idle_seconds"], (300 - 120 - 50) + (210 - 60 - 15) + (100 - 20));
        let w0 = v["waits"].as_array().unwrap();
        assert_eq!((w0[0]["cause"].as_str().unwrap(), w0[0]["seconds"].as_i64().unwrap()), ("a tick", 55), "summed per cause, largest first");
        assert_eq!(w0[1]["seconds"], 15);
        let members: Vec<&Value> = v["groups"].as_array().unwrap().iter().flat_map(|g| g["members"].as_array().unwrap()).collect();
        assert_eq!(members.iter().map(|m| m["model_seconds"].as_i64().unwrap()).sum::<i64>(), 200);
        assert_eq!(members.iter().map(|m| m["gap_seconds"].as_i64().unwrap()).sum::<i64>(), t["gap_seconds"].as_i64().unwrap());
        assert_eq!(members[0]["wall_seconds"], 300);
    }

    #[test]
    fn a_groups_barrier_is_counted_once_however_many_members_hold_it() {
        use crate::timeline::RowFigures;
        let (_e, store, w) = list("once", &["r-1", "r-2"]);
        let barrier = |wall: i64, own_gap: i64| RowFigures {
            start: 100,
            end: 100 + wall,
            wall,
            gap: own_gap + 40,
            model_secs: 50,
            waits: [("a tick".to_string(), own_gap), ("nothing the logs name — x".to_string(), 40)].into(),
            // The barrier: a 40 s unnamed wait and a 300 s run, in both members' timelines.
            shared: 340,
            shared_gap: 40,
            shared_waits: [("nothing the logs name — x".to_string(), 40)].into(),
            ..Default::default()
        };
        let fig = |id: &str| if id == "r-1" { barrier(500, 10) } else { barrier(480, 20) };
        let v = effort_with(&store, &w, None, &fig);
        let t = &v["totals"];
        assert_eq!(t["gap_seconds"], 10 + 20 + 40, "the members' own gaps and the barrier's wait once, not twice");
        assert_eq!(t["unnamed_wait_seconds"], 40, "{v}");
        let waits = v["waits"].as_array().unwrap();
        assert_eq!(waits.iter().map(|w| w["seconds"].as_i64().unwrap()).sum::<i64>(), 70);
        assert_eq!(t["idle_seconds"], (500 - 340 - 50) + (480 - 340 - 50) + 340, "the run is idle for the seats once");
        assert_eq!(v["groups"][0]["wall_seconds"], (500 - 340) + (480 - 340) + 340);
        // Each member's own line still holds the phases it saw.
        assert_eq!(v["groups"][0]["members"][0]["gap_seconds"], 50);
    }

    #[test]
    fn full_suite_runs_in_seats_are_counted_per_row_per_group_and_in_total() {
        let (_e, store, w) = list("suites", &["r-1", "r-2"]);
        let d = store.state.join("full-suite");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("r-1.json"), r#"{"task":"r-1","runs":3,"seats":{"w1:p1":3}}"#).unwrap();
        let v = effort_with(&store, &w, None, &|_| Default::default());
        assert_eq!(v["totals"]["full_suite_runs"], 3);
        let members: Vec<&Value> = v["groups"].as_array().unwrap().iter().flat_map(|g| g["members"].as_array().unwrap()).collect();
        let runs = |id: &str| members.iter().find(|m| m["task"] == id).unwrap()["full_suite_runs"].as_u64().unwrap();
        assert_eq!((runs("r-1"), runs("r-2")), (3, 0));
        let groups: u64 = v["groups"].as_array().unwrap().iter().map(|g| g["full_suite_runs"].as_u64().unwrap()).sum();
        assert_eq!(groups, 3);
        assert!(effort_html(&v).contains("full suites"));
    }
}
