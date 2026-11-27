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

use crate::model::Worklist;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::base::args::Args;

pub const SCHEMA: &str = "wsp-effort/1";

/// The outputs model at its old paths (wsp-436 moved it to [`crate::outputs`]).
#[allow(unused_imports)]
pub use crate::outputs::{at_last_barrier, declared, dir, gate_done, line, list_outputs, missing, output, Output, Source, USAGE};

/// What the engine reaches the report writers through ([`crate::outputs::Reports`]).
pub const REPORTS: crate::outputs::Reports = crate::outputs::Reports { write_effort, write_work, report, cost: crate::cmd_cost::cost };

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Every file goes in through `artefact::put_bytes` (`wsp-376`), so a report
/// written again keeps the one it replaces beside it as a version and the
/// passes can be compared. wsp is the writer; `note` says when.
pub(crate) fn write(slug: &str, file: &str, text: &str, note: &str) -> Result<PathBuf, String> {
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

/// A total's cost: whole, or `at least` what was read where a seat's is
/// missing, with why; never `$0.00` for what was not read.
fn total_usd(micros: &Value, floor: &Value, why: &Value) -> String {
    if floor != true {
        return usd(micros.as_u64());
    }
    let why = why.as_str().unwrap_or("a seat's cost is missing");
    match micros.as_u64() {
        Some(m) => format!("at least {} ({why})", usd(Some(m))),
        None => format!("unmeasured ({why})"),
    }
}

/// A figure, or "not measured" where it is null: never a 0 for what was not read.
fn or_unread(v: &Value, f: &dyn Fn(u64) -> String) -> String {
    v.as_u64().map_or_else(|| "not measured".to_string(), f)
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

/// A member whose own record is empty (no calls, tokens or tool seconds read:
/// its transcript was not found, or the row has none) but whose seat the cost
/// table measured (`wsp-568`). Its calls and tokens come from the seat, and it
/// reads as measured, so its span is the seat idle and not "working" for the
/// whole of it; the member row says so. `true` where it did.
fn fill_from_seat(f: &mut crate::timeline::RowFigures, total: Option<&Value>) -> bool {
    let empty = f.calls == 0 && f.tokens_in == 0 && f.tokens_out == 0 && f.cache_read == 0 && f.model_secs == 0 && f.tools.is_empty();
    let Some(t) = total.filter(|_| empty && !f.measured) else { return false };
    let (seats, unmeasured) = (n(t, "seats"), n(t, "unmeasured"));
    if seats == 0 || unmeasured >= seats || (n(t, "turns") == 0 && n(t, "output_tokens") == 0) {
        return false;
    }
    f.calls = n(t, "turns");
    f.tokens_in = n(t, "input_tokens");
    f.cache_read = n(t, "cache_tokens");
    f.tokens_out = n(t, "output_tokens");
    f.measured = true;
    true
}

/// A seat whose calls were not timed — its tokens the cost table's, or its
/// transcript's calls without a time on them — has its idle named for that, not
/// "none recorded" (`wsp-313`): a seat that wrote 13k tokens for 28 minutes
/// read as idle with nothing said, and the figure saying so was the wrong one.
fn name_untimed(f: &mut crate::timeline::RowFigures, seat_filled: bool) {
    let why = match (seat_filled, f.untimed > 0) {
        (true, _) => crate::wallbuckets::NO_TRANSCRIPT,
        (false, true) => crate::wallbuckets::NO_TIMESTAMPS,
        _ => return,
    };
    for (_, m) in f.markers.iter_mut().filter(|(_, m)| m == crate::wallbuckets::NONE_RECORDED) {
        *m = why.to_string();
    }
    f.markers.insert(0, (i64::MIN, why.to_string()));
}

/// [`effort`] with the timeline figures of a row handed in, so that a test can
/// hold the sums to what it hands.
pub(crate) fn effort_with(store: &Store, w: &Worklist, baseline: Option<&str>, figures: &dyn Fn(&str) -> crate::timeline::RowFigures) -> Value {
    let es = crate::cmd_cost::entries(store, &w.id).unwrap_or_default();
    let cost = crate::cmd_cost::cost_json(&w.id, &es, w.groups().len());
    effort_over(store, w, baseline, figures, cost)
}

/// [`effort_with`] over a cost block handed in, so that a test can hold a
/// member's seat to what the cost table says of it.
pub(crate) fn effort_over(store: &Store, w: &Worklist, baseline: Option<&str>, figures: &dyn Fn(&str) -> crate::timeline::RowFigures, cost: Value) -> Value {
    let groups = w.groups();

    let mut waits: BTreeMap<String, i64> = BTreeMap::new();
    let mut tools: BTreeMap<String, i64> = BTreeMap::new();
    let (mut model, mut idle, mut gap, mut calls) = (0i64, 0i64, 0i64, 0u64);
    let mut legs_total = 0i64;
    let mut between = 0i64;
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
    let passes = passes_of(store, &w.id);
    // The stretches a person parked the whole list: every second in one is
    // `parked` in the wall buckets and in no other (`compounding-464`).
    let parks: Vec<(i64, i64, String)> = crate::timeline::list_parks(store, &w.id).into_iter().map(|p| (p.start, p.end, p.words())).collect();
    let mut walls: Vec<Option<crate::wallbuckets::GroupWall>> = Vec::new();
    // What each group's wall was read from, so a list still running can have its
    // last started group read again to the cut (`wsp-628`).
    let mut sweeps: Vec<(Vec<crate::wallbuckets::MemberIn>, Option<i64>)> = Vec::new();
    let running = w.status() == crate::model::WorklistStatus::Running;
    let cut = util::epoch_secs();

    for (gi, g) in groups.iter().enumerate() {
        let (mut gmodel, mut gtool) = (0i64, 0i64);
        let mut gsuites = 0u64;
        let mut members: Vec<Value> = Vec::new();
        let mut sweep: Vec<crate::wallbuckets::MemberIn> = Vec::new();
        // The barrier is the group's: one run, however many members. Each
        // member's figures hold it (their own timeline shows it); the sums take
        // it out of the members and put it back once per group (`wsp-390`).
        let mut shared: Option<crate::timeline::RowFigures> = None;
        let (mut gstart, mut glast, mut gend) = (0i64, 0i64, 0i64);
        let open = if gi == 0 { None } else { passes.get(&(gi as u64)).copied() };
        for m in &g.members {
            let mut f = figures(m);
            // A member that started before its window opened is read from the
            // window on, as the buckets read it (`wsp-857`: read-api's wsp-373
            // spawned days ahead, and the list's wall ran 102 h for 9 h).
            if let Some(o) = open.filter(|o| f.end > *o) {
                f.start = f.start.max(o);
            }
            let seat_filled = fill_from_seat(&mut f, cost["groups"].get(gi).and_then(|c| c["members"].as_array()).and_then(|ms| ms.iter().find(|x| x["task"] == m.as_str())).map(|x| &x["total"]));
            name_untimed(&mut f, seat_filled);
            sweep.push(crate::wallbuckets::MemberIn { id: m.clone(), spans: f.spans.clone(), active: f.active.clone(), blocked: f.blocked.clone(), measured: f.measured, markers: f.markers.clone(), shells: f.shells.clone() });
            if f.end > 0 && f.start > 0 && (gstart == 0 || f.start < gstart) {
                gstart = f.start;
            }
            if f.end > 0 {
                glast = glast.max(f.start);
                gend = gend.max(f.end);
            }
            if shared.as_ref().is_none_or(|s| f.shared > s.shared) {
                shared = Some(crate::timeline::RowFigures { shared: f.shared, shared_gap: f.shared_gap, shared_waits: f.shared_waits.clone(), ..Default::default() });
            }
            let suites = crate::throttle::full_suite_runs(&store.state, m);
            gsuites += suites;
            suites_total += suites;
            let tool: i64 = f.tools.values().sum();
            gmodel += f.model_secs;
            gtool += tool;
            model += f.model_secs;
            gap += f.gap - f.shared_gap;
            calls += f.calls;
            // A leg running is the row's work, not its seat idle (`wsp-218`).
            let legs_only = crate::wallbuckets::legs_only_seconds(&f.spans, &f.active);
            legs_total += legs_only;
            // The stretch between a row's attempts is named for what it was
            // (parked, waiting to be restarted, sent back and reopened), not the
            // seat idle (`compounding-217`).
            between += f.between_attempts;
            idle += (f.wall - f.shared - f.model_secs - tool - legs_only - f.between_attempts).max(0);
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
            if let Ok(t) = crate::timeline::read_row(store, m) {
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
                "between_attempts_seconds": f.between_attempts,
                "spawn_to_first_line_seconds": f.spawn_to_first,
                "verifier_spawn_to_first_line_seconds": f.verifier_spawn_to_first,
                "verifier_test_seconds": f.verifier_test_secs,
                "verdict_to_despawn_seconds": f.verdict_to_despawn,
                "verdict_to_despawn_by_seat": f.verdict_to_despawn_seats.iter().map(|(seat, secs)| json!({"seat": seat, "seconds": secs})).collect::<Vec<_>>(),
                "record_empty_seat_measured": seat_filled,
            }));
            fixed_first.extend(f.spawn_to_first);
            fixed_vfirst.extend(f.verifier_spawn_to_first);
            fixed_tests += f.verifier_test_secs;
            fixed_held.extend(f.verdict_to_despawn);
        }
        if let Some(sh) = shared {
            gap += sh.shared_gap;
            idle += sh.shared;
            for (k, v) in &sh.shared_waits {
                *waits.entry(k.clone()).or_insert(0) += v;
            }
        }
        group_starts.push((gstart, glast));
        // The last group has no window after it to take the seconds between
        // its pass and its barrier's last stretch, which the pass line is
        // written a moment before (`compounding-217`: 4 s unattributed).
        let pass = passes.get(&(gi as u64 + 1)).copied();
        let pass = if gi + 1 == groups.len() { pass.map(|p| p.max(gend)) } else { pass };
        let gw = named_wall(&sweep, open, pass, &parks, gi, false, g.agent == "manual");
        sweeps.push((sweep, open));
        walls.push(gw);
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
            "group": gi + 1, "model_seconds": gmodel, "tool_seconds": gtool,
            "cost": gcost, "full_suite_runs": gsuites, "members": members,
            "kind": g.kind, "barrier_scope": note.as_ref().map(|n| n.json()).unwrap_or(Value::Null),
            "wall_buckets": walls.last().and_then(|x| x.as_ref()).map_or(Value::Null, wall_json),
        }));
    }
    // A list still running has no pass for its last started group, so the wall
    // read from the records stops at the last line a seat wrote: a started row
    // with one line reads 15 s of a list that has run for minutes (`wsp-628`).
    // Its last started group is read again to the cut, and the stretch after the
    // last record is named for what it is, not for a pass nobody has given.
    let still_running = running && walls.iter().flatten().next().is_some() && {
        let li = walls.iter().rposition(Option::is_some).unwrap_or(0);
        passes.get(&(li as u64 + 1)).is_none()
    };
    if still_running {
        let li = walls.iter().rposition(Option::is_some).unwrap_or(0);
        let (sweep, open) = &sweeps[li];
        let close = walls[li].as_ref().map_or(0, |g| g.close);
        let gw = named_wall(sweep, *open, Some(cut.max(close)), &parks, li, true, groups[li].agent == "manual");
        group_json[li]["wall_buckets"] = gw.as_ref().map_or(Value::Null, wall_json);
        walls[li] = gw;
    }
    // A group's wall is its window, not its members' walls added up: they run
    // side by side (`wsp-854`). Read after the cut above, so a list still
    // running reads one figure here and in the buckets. Not started: 0.
    for (gj, gw) in group_json.iter_mut().zip(&walls) {
        gj["wall_seconds"] = json!(gw.as_ref().map_or(0, |g| g.close - g.open));
    }
    let wall = if end > start { end - start } else { 0 };
    // The buckets tile the windows from the first group's open to the last
    // pass, which runs past the last member's verdict where a pass is given
    // late (a manual group's, by a person). The list's wall is that stretch:
    // `wall_seconds` here and in the buckets is one figure, first spawn to the
    // last pass; `last_verdict_wall_seconds` is the stretch to the last verdict.
    let reach = {
        let (lo, hi) = walls.iter().flatten().fold((i64::MAX, 0i64), |(lo, hi), g| (lo.min(g.open), hi.max(g.close)));
        if hi > lo { hi - lo } else { 0 }
    };
    let list_wall = wall.max(reach);
    let wall_buckets = wall_buckets_json(&walls, &group_json, list_wall);
    let mut gov = governor_wall(store, &w.id, &group_starts);
    // A manual group's start waits on a person, and the cycle log says nothing
    // of it: the window's `held_by` is the cause the wall buckets name for the
    // same span (`compounding-519`), not null.
    for (gi, g) in gov.iter_mut().enumerate() {
        let Some(g) = g.as_mut().filter(|g| g.held_by.is_none()) else { continue };
        g.held_by = walls.get(gi + 1).and_then(|x| x.as_ref()).and_then(|nw| {
            let cause = format!("group {}: {}", gi + 2, crate::wallbuckets::START_WAIT_MANUAL);
            nw.waits.keys().find(|(_, c)| *c == cause).map(|(_, c)| c.clone())
        });
    }
    for (gi, gj) in group_json.iter_mut().enumerate() {
        gj["governor_wall"] = gov.get(gi).and_then(|x| x.as_ref()).map_or(Value::Null, GovWall::json);
    }
    let govs: Vec<&crate::timeline::GovernorWindow> = gov.iter().flatten().filter_map(|x| x.to_last.as_ref()).collect();
    let gov_windows = gov.iter().flatten().count();
    // Not measured is null, never 0 (`wsp-066`): where no window's governor
    // transcript was read, the sums say nothing of what the governor did.
    let gsum = |f: &dyn Fn(&crate::timeline::GovernorWindow) -> i64| if govs.is_empty() { Value::Null } else { json!(govs.iter().map(|g| f(g)).sum::<i64>()) };
    let seats = standing_seats(store, w, &cost);
    let held: i64 = gov.iter().flatten().map(|x| x.last - x.pass).sum();
    let followups: Vec<Value> = crate::cmd_worklist::followups(w).into_iter().map(|(row, ids)| json!({ "by": row, "rows": ids })).collect();
    let mut wait_list: Vec<(String, i64)> = waits.into_iter().collect();
    wait_list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let tool_total: i64 = tools.values().sum();
    let total = cost["total"].clone();

    let mut v = json!({
        "schema": SCHEMA,
        "list": w.id,
        "title": w.title,
        "generated": util::now_iso(),
        "status": w.status().as_str(),
        "scope": {
            "cost": "whole row: every seat — members, verifiers, barrier checks, governors. A member whose row has a seat unmeasured or unpriced has no cost (cost.groups[].members[].total.usd_micros null, with why); a group's or the list's total is then a floor (floor, why; totals.usd_floor, totals.usd_why), null where nothing was read",
            "full_suite": "full_suite_runs: full-suite `cargo test` runs started in a member's or verifier's seat (the shim's note, wsp-389), by row and group. The barrier's own run is one per group and is not counted here; the aim is zero in seats.",
            "barrier_scope": "groups[].barrier_scope: the barrier's run scoped to what the group touched (wsp-431) — scoped or full and why, the targets it ran, shared paths that widened it, its seconds against the last full run's, and net_red: scoped barriers a red full run stood behind, which is what scoping let through",
            "governor_wall": "groups[].governor_wall: between a group's barrier pass and the next group's first and last member start (wsp-398) — the seconds to each, the cycle-log line saying why a member waited (held_by), and the governor's turns, model and tool seconds and idle in each window, with the turns that only looked (a watch, a listing) and the model seconds they held, which is what a governor that skips a turn when nothing needs judgement would give back. Totals are over the windows to the last start: governor_wall_windows is how many there were and governor_wall_windows_unread how many had no governor transcript read; the governor_* sums are over the windows read, and null (not measured, never 0) where none was. A window is null where no group follows or no pass is found; the governor figures are null where its transcript is not found; its transcript is the one the seat's burn.json names, for each pane `governor-set` seated, clipped to the stretch it held the slot; burn.json holds only a seat's latest session and goes when the seat stops, and then the session ids the ledger (spent.jsonl) banked at each stop are read, so the figures are a floor",
            "standing_seats": "totals.standing_seats (wsp-066): each governor and barrier check seat — what a seat costs while it waits on the run — with its group, seated seconds (a governor's: the stretches it held the slot; a check's: its transcript's first stamp, else the check's opening, to its stop), turns and cost as the cost block reads them, and memory: its resident memory over its seated time from the daemon's samples (seat-memory.json, one per minute): MB-minutes, mean and peak MB, the seconds and number of samples; a check seat opened with no check to go on and ended (wsp-089) is one too, with \"orphaned\": true, its seconds from its opening to its ending and no turns. Any figure not read is null and not 0 — seconds or turns where the seat was never read (counted in unmeasured_seats), memory where the seat was never sampled (it ran before the daemon sampled, or on a machine it does not). governor_seconds, check_seconds and their mb_minutes are the sums over the seats read, null where none was",
            "wall_buckets": "totals.wall_buckets and groups[].wall_buckets (wsp-470): every second of the list's wall in one bucket, by what the run as a whole was doing at that second — working (some member's seat in a model call or a tool, or one of its legs running, or a check or a verifier running; legs_running_seconds is the part of working where only legs were — a member whose legs run has no call of its own then, and wsp-218 stopped calling that its seat idle), waiting (nobody working, some member started and not moving: blocked from the moment it blocks, an unclaimed or slow start, a named gap, a seat idle), serialised (nobody started, a member behind the cap or an `after`), barrier_run (the barrier's runs and the hand-off or daemon tick before a step; a longer stretch with no run and no seat in it is waiting, under what the log says held it or 'nothing the logs name', wsp-852), check_seat (less any stretch the list stood held inside it, from worklist-held, or from the check seat's stop where the seat that wrote the hold stayed open past it, to the go or resume that took it back: that is a person's step, waiting, named 'the barrier waits: held for a person's step — barrier <n> of <list> …' with the hold's reason, wsp-090), between_groups (a pass to the group's first start, where the run starts the group; a manual group's is a person's wait for a hand to start it, so it is waiting, named 'waiting for a hand to start'), parked (a person parked the whole list, worklist-parked to worklist-resumed: every second inside it is parked and in no other bucket, the barrier's wait and its run included, so barrier_run is the run and its own waits and never a park; parked_by names who, when and why; wall_without_parked_seconds is the wall less it, the working wall a retro attributes). A group's window runs from the previous group's pass to its own, so they tile the wall: a member that started before its window opened (a row cut from the previous group and started again) is read from the pass on, and attributed_seconds equals wall_seconds. between_attempts_seconds (totals, and per member) is the stretch between a row's attempts (compounding-217): a row released and started again is two seats, each with its own spawn → release or review span, and what lay between them is named for what it was — the row parked or waiting to be restarted (with the row it waited on when its `after` names one), or reviewed, sent back and reopened (rework; the row's attempt is booked in rework.attempts), or, from where a decision in force on the row says it holds for the person (to the next seat) or a question its seat put to the person stood unanswered (to the answer), a person's step and not rework, named 'the row waits for a person's step — <the decision or the question>' (wsp-311) — and is neither the seat finishing nor the seat idle, nor in idle_seconds. gap_seconds and idle_seconds above are member-seconds read per row, kept beside it: a stall inside a seat is not a gap there. seat_idle_seconds and seat_idle_by_reason (wsp-559), beside waiting_by in each group's wall_buckets and in the totals: the waiting that was a seat idle, split by what the row's log says it waited on from the moment it was written — sent back (the verdict given, the tell not delivered or delivered, the verdict with the governor), reopened, a rebase refused, a hand-off to another row, owed to another (an after), a block, waiting on the governor's or a person's answer to a question it asked (named with the question's id, from the question to its answer), the seat's own shells, the check and the verifier's hand-off, the verdict read — and 'none recorded' where no line precedes the second; where the seat's calls were not timed, 'no call timing: …' says why in its place (no transcript found for the seat, its tokens the cost table's, or a transcript with no timestamps on its calls, wsp-313)",
            "notes": "notes: what the figures say of themselves (wsp-857) — where more than 1 % of the list's wall is in no bucket (totals.wall_buckets.unattributed_seconds), a line saying how much and that the buckets and waits do not add up to the wall. A member's phases are read from its group window's start (the previous group's pass), as the buckets read it, so a row spawned or given legs days before its window books those days to no gap of this one",
            "time": "per member row, spawn to verdict (wall, gaps); the seat's model and tool time are its own transcript calls inside those phases, legs only where run-leg timings exist",
        },
        "totals": {
            "wall_seconds": list_wall,
            "model_seconds": model,
            "tool_seconds": tools,
            "tool_seconds_total": tool_total,
            "idle_seconds": idle,
            "gap_seconds": gap,
            "wall_buckets": wall_buckets,
            "unnamed_wait_seconds": wait_list.iter().filter(|(c, _)| c.starts_with("nothing the logs")).map(|(_, s)| *s).sum::<i64>(),
            "calls": calls,
            "input_tokens": total["input_tokens"],
            "output_tokens": total["output_tokens"],
            "cache_read_tokens": total["cache_tokens"],
            "cache_write_tokens": total["cache_write_tokens"],
            "turns": total["turns"],
            "usd_micros": total["usd_micros"],
            "unmeasured_seats": total["unmeasured"],
            "usd_floor": total["floor"],
            "usd_why": total["why"],
            "transcripts_missing": missing_sessions,
            "full_suite_runs": suites_total,
            "pass_to_last_start_seconds": held,
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
    v["totals"]["last_verdict_wall_seconds"] = json!(wall);
    for (k, x) in [
        ("governor_wall_windows", json!(gov_windows)),
        ("governor_wall_windows_unread", json!(gov_windows - govs.len())),
        ("governor_wall_seconds", gsum(&|g| g.seconds)),
        ("governor_wall_turns", gsum(&|g| g.turns as i64)),
        ("governor_wall_model_seconds", gsum(&|g| g.model_secs)),
        ("governor_wall_idle_seconds", gsum(&|g| g.idle_secs)),
        ("governor_looking_turns", gsum(&|g| g.looking_turns as i64)),
        ("governor_looking_seconds", gsum(&|g| g.looking_secs)),
        ("standing_seats", seats),
    ] {
        v["totals"][k] = x;
    }
    // Said where the wall is partial: no pass yet, the wall run to the cut. A
    // retro row on the list it reads is one of its rows, and counts in it.
    v["totals"]["still_running"] = json!(still_running);
    v["totals"]["cut_at"] = if still_running { json!(util::now_iso()) } else { Value::Null };
    v["totals"]["still_running_note"] = if still_running {
        json!("the list has no pass for its last started group, so its wall runs from the first spawn to the cut (cut_at), not to a last pass; the stretch after the last record is booked as waiting, named 'still running'. A retro row on the list it reads is one of its rows and counts in the wall.")
    } else {
        Value::Null
    };
    v["notes"] = json!(unattributed_note(&v["totals"]["wall_buckets"]).into_iter().collect::<Vec<_>>());
    v["totals"]["legs_running_seconds"] = json!(legs_total);
    v["totals"]["between_attempts_seconds"] = json!(between);
    if let Some(b) = baseline {
        v["baseline"] = against(&v, b);
    }
    v
}

/// The note effort.json carries where more than 1 % of the list's wall is in
/// no bucket (`wsp-857`): the wall is the first spawn to the last pass and the
/// buckets tile the group windows, so a gap between them is a member's span
/// read outside every window, and the figures are not to be read as whole.
fn unattributed_note(wb: &Value) -> Option<String> {
    let (wall, un) = (wb["wall_seconds"].as_i64().unwrap_or(0), wb["unattributed_seconds"].as_i64().unwrap_or(0));
    (wall > 0 && un * 100 > wall).then(|| {
        format!(
            "unattributed: {un} s of the {wall} s wall ({:.1} %) is in no bucket — the wall runs from the first spawn to the last pass and the buckets tile the group windows, so a member's span lies outside every window; the buckets and waits do not add up to the wall",
            un as f64 * 100.0 / wall as f64
        )
    })
}

/// A group's wall with the wait for its pass named by its group (so two
/// groups' waits are two lines) and by whether a barrier ran or the group is
/// manual; or, where the list is still running and the group has no pass, for
/// what it is: the group still running, cut at the pass handed in.
fn named_wall(sweep: &[crate::wallbuckets::MemberIn], open: Option<i64>, pass: Option<i64>, parks: &[(i64, i64, String)], gi: usize, running: bool, manual: bool) -> Option<crate::wallbuckets::GroupWall> {
    let mut gw = crate::wallbuckets::group_wall_for(sweep, open, pass, parks, manual);
    if let Some(g) = gw.as_mut() {
        let waits = std::mem::take(&mut g.waits);
        for ((row, cause), s) in waits {
            let cause = if cause == crate::wallbuckets::PASS_WAIT || cause == crate::wallbuckets::PASS_WAIT_MANUAL {
                if running { format!("group {}: still running — no pass yet, the stretch after the last record to the cut", gi + 1) } else { format!("group {}: {cause}", gi + 1) }
            } else if cause == crate::wallbuckets::START_WAIT_MANUAL {
                format!("group {}: {cause}", gi + 1)
            } else {
                cause
            };
            *g.waits.entry((row, cause)).or_insert(0) += s;
        }
    }
    gw
}

fn wall_json(g: &crate::wallbuckets::GroupWall) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("window_seconds".into(), json!(g.close - g.open));
    for b in crate::wallbuckets::BUCKETS {
        o.insert(format!("{b}_seconds"), json!(g.seconds(b)));
    }
    o.insert("barrier_run_only_seconds".into(), json!(g.barrier_run));
    o.insert("legs_running_seconds".into(), json!(g.legs));
    o.insert("wall_without_parked_seconds".into(), json!(g.close - g.open - g.seconds("parked")));
    o.insert("parked_by".into(), json!(g.parked.iter().map(|(why, s)| json!({ "why": why, "seconds": s })).collect::<Vec<_>>()));
    let mut waits: Vec<(&(String, String), &i64)> = g.waits.iter().collect();
    waits.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    o.insert("waiting_by".into(), json!(waits.iter().map(|((row, cause), s)| json!({ "row": row, "cause": cause, "seconds": s })).collect::<Vec<_>>()));
    insert_seat_idle(&mut o, crate::wallbuckets::seat_idle_reasons(g.waits.iter().map(|((_, c), s)| (c.as_str(), *s))));
    Value::Object(o)
}

/// `seat_idle_seconds` and `seat_idle_by_reason` (`wsp-559`): the seat idle among
/// the waiting causes, split by what the row's log says it waited on.
fn insert_seat_idle(o: &mut serde_json::Map<String, Value>, by: BTreeMap<String, i64>) {
    let mut v: Vec<(String, i64)> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    o.insert("seat_idle_seconds".into(), json!(v.iter().map(|x| x.1).sum::<i64>()));
    o.insert("seat_idle_by_reason".into(), json!(v.iter().map(|(r, s)| json!({ "reason": r, "seconds": s })).collect::<Vec<_>>()));
}

/// The list's wall by bucket: the totals, by cause, and the stretches worth
/// naming — a row's stall and a barrier's run of ten minutes or more.
fn wall_buckets_json(walls: &[Option<crate::wallbuckets::GroupWall>], groups: &[Value], wall: i64) -> Value {
    const NAMED: i64 = 600;
    let mut by: BTreeMap<&str, i64> = BTreeMap::new();
    let mut legs = 0i64;
    let mut causes: BTreeMap<String, i64> = BTreeMap::new();
    let mut named: Vec<(i64, Value)> = Vec::new();
    let mut parked_by: BTreeMap<String, i64> = BTreeMap::new();
    for (gi, g) in walls.iter().enumerate() {
        let Some(g) = g else { continue };
        legs += g.legs;
        for (why, s) in &g.parked {
            *parked_by.entry(why.clone()).or_insert(0) += s;
            named.push((*s, json!({ "group": gi + 1, "bucket": "parked", "what": why, "row": "", "seconds": s })));
        }
        for b in crate::wallbuckets::BUCKETS {
            *by.entry(b).or_insert(0) += g.seconds(b);
        }
        for ((row, cause), s) in &g.waits {
            *causes.entry(cause.clone()).or_insert(0) += s;
            if *s >= NAMED {
                named.push((*s, json!({ "group": gi + 1, "bucket": "waiting", "what": format!("{} — {cause}", if row.is_empty() { "the group" } else { row }), "row": row, "seconds": s })));
            }
        }
        if g.barrier_run >= NAMED {
            let scope = &groups[gi]["barrier_scope"];
            let kind = match scope["scoped"].as_bool() {
                Some(true) => "scoped",
                Some(false) => "full",
                None => "scope not recorded",
            };
            named.push((g.barrier_run, json!({ "group": gi + 1, "bucket": "barrier_run", "what": format!("the barrier's run ({kind})"), "row": "", "seconds": g.barrier_run })));
        }
    }
    named.sort_by(|a, b| b.0.cmp(&a.0));
    let attributed: i64 = by.values().sum();
    let mut o = serde_json::Map::new();
    o.insert("wall_seconds".into(), json!(wall));
    for b in crate::wallbuckets::BUCKETS {
        o.insert(format!("{b}_seconds"), json!(by.get(b).copied().unwrap_or(0)));
    }
    o.insert("legs_running_seconds".into(), json!(legs));
    o.insert("wall_without_parked_seconds".into(), json!(wall - by.get("parked").copied().unwrap_or(0)));
    o.insert("parked_by".into(), json!(parked_by.iter().map(|(why, s)| json!({ "why": why, "seconds": s })).collect::<Vec<_>>()));
    o.insert("attributed_seconds".into(), json!(attributed));
    o.insert("unattributed_seconds".into(), json!(wall - attributed));
    let mut causes: Vec<(String, i64)> = causes.into_iter().collect();
    causes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    o.insert("waiting_by_cause".into(), json!(causes.iter().map(|(c, s)| json!({ "cause": c, "seconds": s })).collect::<Vec<_>>()));
    insert_seat_idle(&mut o, crate::wallbuckets::seat_idle_reasons(causes.iter().map(|(c, s)| (c.as_str(), *s))));
    o.insert("named".into(), json!(named.into_iter().map(|(_, v)| v).collect::<Vec<_>>()));
    Value::Object(o)
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
        // The seat stopped: its `burn.json` went with its directory, and the
        // ledger line `stop()` banked names each session it ran (`wsp-066`:
        // m1-fixes's governor read 0 turns for 113 the cost table counted).
        // A line banked before the ledger carried the path names the session
        // only, and is looked for under `~/.claude/projects`.
        for l in crate::cmd_burn::ledger_lines(&store.state, &s.seat) {
            let Some(sid) = l["session_id"].as_str().filter(|x| !x.is_empty()) else { continue };
            let path = l["transcript"].as_str().filter(|t| t.starts_with('/'));
            match out.iter_mut().find(|o| o.transcript.contains(sid) || Some(o.transcript.as_str()) == path) {
                Some(o) => o.spans.extend(s.spans.iter().copied().filter(|sp| !o.spans.contains(sp)).collect::<Vec<_>>()),
                None => out.push(GovSource { transcript: path.unwrap_or(sid).to_string(), spans: s.spans.clone() }),
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

/// When each group's barrier was passed: the `worklist-go` event that crossed it,
/// by group number.
fn passes_of(store: &Store, list: &str) -> BTreeMap<u64, i64> {
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
    passes
}

fn governor_wall_with(
    store: &Store,
    list: &str,
    group_starts: &[(i64, i64)],
    window: &dyn Fn(&[crate::timeline::GovSource], i64, i64) -> Option<crate::timeline::GovernorWindow>,
) -> Vec<Option<GovWall>> {
    let passes = passes_of(store, list);
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

/// The governor and barrier check seats of the list (`wsp-066`): what a seat
/// costs while it waits on the run. Seconds, turns and cost are the cost
/// block's for the seat; memory is its resident memory over its seated time as
/// the daemon sampled it. A figure not read is null, never 0.
fn standing_seats(store: &Store, w: &Worklist, cost: &Value) -> Value {
    let groups = w.groups();
    // The stretch each seat was seated, for its memory: a governor's tenure,
    // a check's opening to its stop.
    let mut held: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for g in crate::cmd_cost::governors(store, &w.id, groups.len()) {
        let from = g.spans.iter().map(|s| s.0).min().unwrap_or(0);
        let to = if g.spans.iter().any(|s| s.1 == 0) { 0 } else { g.spans.iter().map(|s| s.1).max().unwrap_or(0) };
        held.insert(g.seat, (from, to));
    }
    for c in groups.iter().flat_map(|g| g.checks.iter()) {
        let Some(pane) = c.pane.as_deref() else { continue };
        let ended = crate::cmd_burn::reading_in(&store.state, pane).and_then(|r| r["ended"].as_str().map(util::epoch_of)).unwrap_or(0);
        held.entry(pane.to_string()).or_insert((util::epoch_of(&c.at), ended));
    }
    let mut seats: Vec<Value> = Vec::new();
    for g in cost["groups"].as_array().into_iter().flatten() {
        for (role, key) in [("governor", "governor"), ("check", "barrier")] {
            for e in g[key].as_array().into_iter().flatten().filter(|e| e["shared"] != true) {
                let seat = e["seat"].as_str().unwrap_or("");
                let memory = held.get(seat).and_then(|&(from, to)| crate::memory::seat_memory(&store.state, seat, from, to));
                seats.push(json!({
                    "role": role, "group": g["group"], "seat": seat,
                    "seconds": e["seconds"], "turns": e["turns"], "usd_micros": e["usd_micros"], "measured": e["measured"],
                    "memory": memory.as_ref().map_or(Value::Null, crate::memory::SeatMemory::json),
                }));
            }
        }
    }
    seats.extend(orphaned_checks(&crate::timeline::list_events(store, &["barrier-check-orphaned"]), &w.id));
    let sum = |role: &str, f: &dyn Fn(&Value) -> Option<f64>| {
        let read: Vec<f64> = seats.iter().filter(|s| s["role"] == role).filter_map(f).collect();
        if read.is_empty() { Value::Null } else { json!((read.iter().sum::<f64>() * 10.0).round() / 10.0) }
    };
    let secs = |s: &Value| s["seconds"].as_f64();
    let mbm = |s: &Value| s["memory"]["mb_minutes"].as_f64();
    json!({
        "governor_seconds": sum("governor", &secs),
        "governor_mb_minutes": sum("governor", &mbm),
        "check_seconds": sum("check", &secs),
        "check_mb_minutes": sum("check", &mbm),
        "unmeasured": seats.iter().filter(|s| s["measured"] != true).count(),
        "seats": seats,
    })
}

/// The check seats of `list` opened with no check to go on (`wsp-089`), off
/// the log's `barrier-check-orphaned`: no check entry names them, so the cost
/// block has nothing of them, and one stood thirteen hours unseen. Each is a
/// standing check seat with its seconds, opening to ending, and no turns.
fn orphaned_checks(events: &[(i64, String, Value)], list: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|(_, k, d)| k == "barrier-check-orphaned" && d["id"].as_str() == Some(list))
        .map(|(t, _, d)| {
            let from = d["opened"].as_str().map(util::epoch_of).unwrap_or(*t);
            let to = d["ended"].as_str().map(util::epoch_of).unwrap_or(*t);
            json!({
                "role": "check", "group": d["group"], "seat": d["seat"],
                "seconds": (to - from).max(0) as f64, "turns": Value::Null, "usd_micros": Value::Null, "measured": true,
                "memory": Value::Null, "orphaned": true,
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
    // A cost not read is null and not 0, and a ratio over a floor says so.
    let mut cost = ratio("usd_micros");
    for (k, side) in [("now", now), ("baseline", &then)] {
        if side["totals"]["usd_micros"].is_null() {
            cost[k] = Value::Null;
            cost["ratio"] = Value::Null;
        }
    }
    cost["floor"] = json!(now["totals"]["usd_floor"] == true || then["totals"]["usd_floor"] == true);
    let wait_sum = |v: &Value| -> u64 { v["waits"].as_array().map_or(0, |a| a.iter().map(|w| n(w, "seconds")).sum()) };
    let (wa, wb) = (wait_sum(now), wait_sum(&then));
    json!({
        "list": slug,
        "wall_seconds": ratio("wall_seconds"), "model_seconds": ratio("model_seconds"),
        "idle_seconds": ratio("idle_seconds"), "gap_seconds": ratio("gap_seconds"),
        "input_tokens": ratio("input_tokens"), "output_tokens": ratio("output_tokens"),
        "cache_read_tokens": ratio("cache_read_tokens"), "cache_write_tokens": ratio("cache_write_tokens"),
        "turns": ratio("turns"), "usd_micros": cost, "full_suite_runs": ratio("full_suite_runs"),
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
            vec!["wall (first spawn to last pass)".into(), hms(n(t, "wall_seconds") as i64)],
            vec!["of it, first spawn to last verdict".into(), hms(n(t, "last_verdict_wall_seconds") as i64)],
            vec!["model time".into(), hms(n(t, "model_seconds") as i64)],
            vec!["tool time".into(), format!("{} ({})", hms(n(t, "tool_seconds_total") as i64), if tool_kinds.is_empty() { "none read".into() } else { tool_kinds.join(", ") })],
            vec!["idle (wall under neither model nor tool, per row)".into(), hms(n(t, "idle_seconds") as i64)],
            vec!["tokens in".into(), n(t, "input_tokens").to_string()],
            vec!["tokens out".into(), n(t, "output_tokens").to_string()],
            vec!["cache read".into(), n(t, "cache_read_tokens").to_string()],
            vec!["cache write".into(), n(t, "cache_write_tokens").to_string()],
            vec!["turns".into(), n(t, "turns").to_string()],
            vec!["USD".into(), total_usd(&t["usd_micros"], &t["usd_floor"], &t["usd_why"])],
            vec!["unmeasured seats".into(), n(t, "unmeasured_seats").to_string()],
            vec!["transcripts not found".into(), n(t, "transcripts_missing").to_string()],
            vec!["full suites run in member or verifier seats".into(), n(t, "full_suite_runs").to_string()],
            vec!["each pass to the next group's last start: window / governor turns / governor idle".into(), format!("{} / {} / {}", hms(n(t, "pass_to_last_start_seconds") as i64), or_unread(&t["governor_wall_turns"], &|x| x.to_string()), or_unread(&t["governor_wall_idle_seconds"], &|x| hms(x as i64)))],
            vec!["governor turns that only looked (model time a skipped turn gives back)".into(), format!("{} ({})", or_unread(&t["governor_looking_turns"], &|x| x.to_string()), or_unread(&t["governor_looking_seconds"], &|x| hms(x as i64)))],
            vec!["barriers scoped / full".into(), format!("{} / {}", n(t, "scoped_barriers"), n(t, "full_barriers"))],
            vec!["seconds scoped barriers ran under a full run".into(), n(t, "scoped_seconds_saved").to_string()],
            vec!["safety-net reds scoping let through".into(), n(t, "safety_net_reds").to_string()],
            vec!["seat spawn to its first line (median per row)".into(), hms(n(t, "spawn_to_first_line_median_seconds") as i64)],
            vec!["verifier spawn to its first line (median per row)".into(), hms(n(t, "verifier_spawn_to_first_line_median_seconds") as i64)],
            vec!["verifier test and build time (all passes)".into(), hms(n(t, "verifier_test_seconds") as i64)],
            vec!["verdict to the last seat's despawn (median per row)".into(), hms(n(t, "verdict_to_despawn_median_seconds") as i64)],
        ],
    ));
    let ss = &t["standing_seats"];
    if let Some(seats) = ss["seats"].as_array().filter(|s| !s.is_empty()) {
        b.push_str("<h2>1a · Governor and check seats</h2><p class=\"dim\">What a seat costs while it waits on the run (wsp-066): its seated time, turns and cost, and its resident memory over that time as the daemon sampled it. A figure not read says so, and is never 0.</p>");
        let rows: Vec<Vec<String>> = seats
            .iter()
            .map(|x| {
                let m = &x["memory"];
                vec![
                    format!("{} · group {}", x["role"].as_str().unwrap_or(""), x["group"]),
                    x["seat"].as_str().unwrap_or("").into(),
                    or_unread(&x["seconds"], &|v| hms(v as i64)),
                    or_unread(&x["turns"], &|v| v.to_string()),
                    usd(x["usd_micros"].as_u64()),
                    if m.is_object() { format!("{} MB·min (mean {} MB, peak {} MB)", m["mb_minutes"], m["mean_mb"], m["peak_mb"]) } else { "not sampled".into() },
                ]
            })
            .collect();
        b.push_str(&table(&["seat", "pane", "seated", "turns", "USD", "resident memory"], &rows));
    }
    let wb = &t["wall_buckets"];
    if wb.is_object() {
        let pct = |k: &str| {
            let w = n(wb, "wall_seconds").max(1) as f64;
            format!("{:.0}%", n(wb, k) as f64 / w * 100.0)
        };
        b.push_str("<h2>1b · Where the wall went</h2><p class=\"dim\">Every second of the wall in one bucket, by what the run was doing at that second (wsp-470). The old \"gaps\" figure is each row's own timeline gaps, in member-seconds, and does not see a seat stalled or a start that dragged.</p>");
        let mut rows: Vec<Vec<String>> = vec![vec!["wall".into(), hms(n(wb, "wall_seconds") as i64), "100%".into()]];
        for (k, label) in [
            ("working_seconds", "members working (a model call, a tool, a check or a verifier)"),
            ("waiting_seconds", "members waiting (blocked, unclaimed or slow start, a named gap, a seat idle)"),
            ("serialised_seconds", "serialised (behind the group's cap or an after)"),
            ("barrier_run_seconds", "barrier run"),
            ("check_seat_seconds", "check seat"),
            ("between_groups_seconds", "between groups (a pass to the next first start)"),
            ("parked_seconds", "parked (a person parked the whole list)"),
            ("unattributed_seconds", "not attributed"),
        ] {
            rows.push(vec![label.into(), hms(wb[k].as_i64().unwrap_or(0)), pct(k)]);
        }
        if n(wb, "parked_seconds") > 0 {
            rows.push(vec!["wall without the park".into(), hms(n(wb, "wall_without_parked_seconds") as i64), format!("{:.0}%", n(wb, "wall_without_parked_seconds") as f64 / n(wb, "wall_seconds").max(1) as f64 * 100.0)]);
        }
        rows.push(vec!["old figure: gaps between stages (member-seconds)".into(), hms(n(t, "gap_seconds") as i64), "—".into()]);
        b.push_str(&table(&["bucket", "time", "of the wall"], &rows));
        let named: Vec<Vec<String>> = wb["named"].as_array().into_iter().flatten().map(|x| vec![format!("group {}", x["group"]), x["what"].as_str().unwrap_or("").into(), hms(n(x, "seconds") as i64)]).collect();
        if !named.is_empty() {
            b.push_str("<p>The stretches of ten minutes or more:</p>");
            b.push_str(&table(&["where", "what", "time"], &named));
        }
        let causes: Vec<Vec<String>> = wb["waiting_by_cause"].as_array().into_iter().flatten().map(|x| vec![x["cause"].as_str().unwrap_or("").into(), hms(n(x, "seconds") as i64)]).collect();
        if !causes.is_empty() {
            b.push_str("<p>Members waiting, by cause:</p>");
            b.push_str(&table(&["cause", "time"], &causes));
        }
        let idle: Vec<Vec<String>> = wb["seat_idle_by_reason"].as_array().into_iter().flatten().map(|x| vec![x["reason"].as_str().unwrap_or("").into(), hms(n(x, "seconds") as i64)]).collect();
        if !idle.is_empty() {
            b.push_str(&format!("<p>The seat idle ({}), by what the row's log says it waited on:</p>", hms(n(wb, "seat_idle_seconds") as i64)));
            b.push_str(&table(&["reason", "time"], &idle));
        }
        let rows: Vec<Vec<String>> = v["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|g| g["wall_buckets"].is_object())
            .map(|g| {
                let x = &g["wall_buckets"];
                let mut r = vec![format!("group {}", g["group"]), hms(n(x, "window_seconds") as i64)];
                r.extend(["working", "waiting", "serialised", "barrier_run", "check_seat", "between_groups", "parked"].iter().map(|k| hms(n(x, &format!("{k}_seconds")) as i64)));
                r
            })
            .collect();
        if !rows.is_empty() {
            b.push_str("<p>By group (a group's window runs from the previous pass to its own):</p>");
            b.push_str(&table(&["group", "window", "working", "waiting", "serialised", "barrier run", "check seat", "between", "parked"], &rows));
        }
    }
    b.push_str("<h2>2 · Waits between stages, largest first</h2>");
    let waits: Vec<Vec<String>> = v["waits"].as_array().map(|a| a.iter().map(|w| vec![w["cause"].as_str().unwrap_or("").to_string(), hms(n(w, "seconds") as i64)]).collect()).unwrap_or_default();
    b.push_str(&if waits.is_empty() { "<p>No gaps recorded.</p>".to_string() } else { table(&["waiting for", "total"], &waits) });
    b.push_str("<h2>3 · Per group, then per member</h2>");
    for g in v["groups"].as_array().into_iter().flatten() {
        let c = &g["cost"];
        b.push_str(&format!("<h3>group {} · wall {} · model {} · tools {} · {} · {} turns · {} full suite(s) in seats</h3>", g["group"], hms(n(g, "wall_seconds") as i64), hms(n(g, "model_seconds") as i64), hms(n(g, "tool_seconds") as i64), esc(&total_usd(&c["usd_micros"], &c["floor"], &c["why"])), n(c, "turns"), n(g, "full_suite_runs")));
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
            .map(|k| vec![if bl[*k]["floor"] == true { format!("{k} (a floor)") } else { k.to_string() }, bl[*k]["now"].to_string(), bl[*k]["baseline"].to_string(), bl[*k]["ratio"].as_f64().map_or("—".into(), |r| format!("×{r}"))])
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
    write_effort_of(&w.id, &effort(store, w, baseline), note)
}

/// [`write_effort`] of a reading already taken, so that what is kept and what is
/// quoted from it are one reading (`wsp-852`).
pub(crate) fn write_effort_of(list: &str, v: &Value, note: &str) -> Result<(PathBuf, PathBuf), String> {
    let j = write(list, "effort.json", &serde_json::to_string_pretty(v).unwrap_or_default(), note)?;
    let h = write(list, "effort.html", &effort_html(v), note)?;
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
                Some(l) if l.no_commit => ("no commit".into(), String::new()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Task, WorklistStatus};

    fn list(slug: &str, members: &[&str]) -> (util::Isolated, Store, Worklist) {
        // The writers, as `main` installs them: the engine's test build installed
        // these for the whole crate while it was this one (wsp-440).
        crate::outputs::install(REPORTS);
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let env = util::isolated(&format!("report-{slug}"));
        // The engine's launches (`cycle::launch`) stand down in its own test
        // build; this crate's tests run it as the binary does, and a launch
        // here would run this harness as `wsp` (wsp-440).
        std::env::set_var("WSP_NO_CYCLE", "1");
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

    /// `wsp-066`: m1-fixes's effort.json read its governor (113 turns, seated
    /// 3065 s) and two barrier seats (39 and 12 turns) as 0 s, and held no
    /// memory figure. A governor whose seat stopped is read through the session
    /// the ledger banked; a check seat carries its seated seconds; a seat never
    /// read is null and unmeasured, never 0; and each standing seat carries its
    /// resident memory where the daemon sampled it.
    #[test]
    fn a_stopped_governor_is_read_through_the_ledger_and_a_seat_not_read_is_null_not_zero() {
        use crate::timeline::RowFigures;
        let (env, store, mut w) = list("gl", &["r-1", "r-2"]);
        let t0: i64 = util::epoch_secs() - 10_000;
        let mut gs = w.groups();
        gs.truncate(1);
        gs[0].members = vec!["r-1".into()];
        gs.push(crate::model::Group { members: vec!["r-2".into()], ..Default::default() });
        let mut read = crate::barrier::Check::opened(None, None);
        read.at = util::iso_at(t0 + 50);
        read.pane = Some("cpd-7".into());
        gs[0].checks.push(read);
        let mut unread = crate::barrier::Check::opened(None, None);
        unread.at = util::iso_at(t0 + 250);
        unread.pane = Some("cpd-8".into());
        gs[1].checks.push(unread);
        w.set_groups(&gs);
        store.save_worklist(&w).unwrap();
        let ev = |at: i64, kind: &str, data: Value| json!({ "ts": util::iso_at(at), "kind": kind, "data": data }).to_string();
        let log = [
            ev(t0, "governor-set", json!({ "project": "gl", "workspace": "cpd-1" })),
            ev(t0 + 100, "worklist-go", json!({ "id": "gl", "passed": 1, "started": false })),
            ev(t0 + 400, "governor-vacated", json!({ "project": "gl", "workspace": "cpd-1" })),
        ];
        std::fs::write(store.state_file("events.jsonl"), format!("{}\n", log.join("\n"))).unwrap();
        // Both seats have stopped: no burn.json, only the ledger's lines.
        let projects = env.home().join("transcripts");
        std::fs::create_dir_all(&projects).unwrap();
        let banked = |seat: &str, sid: &str, turns: u64, ended: i64| {
            let path = projects.join(format!("{sid}.jsonl"));
            json!({ "seat": seat, "session_id": sid, "transcript": path.to_string_lossy(), "turns": turns, "input": 1, "output": 10, "cache_read": 0, "cache_write": 0, "cost": 1000, "ended": util::iso_at(ended) }).to_string()
        };
        std::fs::write(store.state_file("spent.jsonl"), format!("{}\n{}\n", banked("cpd-7", "chk-s", 2, t0 + 300), banked("cpd-1", "gov-s", 3, t0 + 400))).unwrap();
        let turn = |at: i64, rid: &str| {
            json!({ "type": "assistant", "requestId": rid, "timestamp": util::iso_at(at),
                "message": { "content": [{ "type": "tool_use", "id": rid, "name": "Bash", "input": { "command": "wsp say x" } }], "usage": { "input_tokens": 1, "output_tokens": 1 } } })
            .to_string()
        };
        let gov = projects.join("gov-s.jsonl");
        std::fs::write(&gov, [turn(t0 + 110, "g1"), turn(t0 + 150, "g2"), turn(t0 + 190, "g3")].join("\n")).unwrap();
        std::fs::write(projects.join("chk-s.jsonl"), [turn(t0 + 120, "c1"), turn(t0 + 130, "c2")].join("\n")).unwrap();
        // The daemon sampled the governor's seat twice: 100 then 200 MB, a minute apart.
        let mem = crate::memory::fold(Value::Null, &[("cpd-1".into(), 100 * 1024)], t0 + 10);
        let mem = crate::memory::fold(mem, &[("cpd-1".into(), 200 * 1024)], t0 + 70);
        std::fs::write(store.state_file(crate::memory::SEAT_MEMORY_FILE), mem.to_string()).unwrap();
        let fig = |id: &str| match id {
            "r-1" => RowFigures { start: t0, end: t0 + 100, wall: 100, measured: true, ..Default::default() },
            _ => RowFigures { start: t0 + 200, end: t0 + 300, wall: 100, measured: true, ..Default::default() },
        };

        let v = effort_with(&store, &w, None, &fig);
        let t = &v["totals"];
        assert_eq!(t["governor_wall_turns"], 3, "the governor's three turns, read through the ledger's session: {t}");
        assert_eq!(t["governor_wall_seconds"], 100, "the pass to group 2's start: {t}");
        assert_eq!((t["governor_wall_windows"].clone(), t["governor_wall_windows_unread"].clone()), (json!(1), json!(0)));
        let seats = t["standing_seats"]["seats"].as_array().unwrap();
        let seat = |s: &str| seats.iter().find(|x| x["seat"] == s).unwrap_or_else(|| panic!("{s}: {seats:?}"));
        let g = seat("cpd-1");
        assert_eq!((g["role"].clone(), g["seconds"].clone(), g["turns"].clone()), (json!("governor"), json!(400), json!(3)), "{g}");
        assert_eq!(g["memory"]["mb_minutes"], 150.0, "(100 + 200) / 2 MB for a minute: {g}");
        assert_eq!(g["memory"]["peak_mb"], 200);
        let c = seat("cpd-7");
        assert_eq!((c["role"].clone(), c["seconds"].clone(), c["turns"].clone()), (json!("check"), json!(180), json!(2)), "its first act to its stop: {c}");
        assert!(c["memory"].is_null(), "never sampled: not measured, not 0 MB: {c}");
        let u = seat("cpd-8");
        assert!(u["seconds"].is_null() && u["turns"].is_null() && u["measured"] == false, "a seat never read is null, not 0: {u}");
        assert_eq!(t["unmeasured_seats"], 1, "{t}");
        assert_eq!(t["standing_seats"]["unmeasured"], 1);
        assert_eq!(t["standing_seats"]["governor_seconds"], 400.0);
        assert_eq!(t["standing_seats"]["check_seconds"], 180.0, "the unread seat is out of the sum, not a 0 in it");
        assert!(t["standing_seats"]["check_mb_minutes"].is_null());
        assert_eq!(v["cost"]["total"]["seconds_unread"], 1);

        // The governor's transcript gone too: its window is not measured, and
        // the totals say so — null, not 0.
        std::fs::remove_file(&gov).unwrap();
        let v = effort_with(&store, &w, None, &fig);
        let t = &v["totals"];
        for k in ["governor_wall_seconds", "governor_wall_turns", "governor_wall_idle_seconds", "governor_wall_model_seconds", "governor_looking_turns", "governor_looking_seconds"] {
            assert!(t[k].is_null(), "{k} is not measured: {t}");
        }
        assert_eq!(t["governor_wall_windows_unread"], 1);
        assert!(v["groups"][0]["governor_wall"]["governor_to_last_start"].is_null());
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
    fn a_list_with_an_unmeasured_seat_says_its_cost_is_a_floor() {
        let (_e, store, w) = list("floor", &["r-1"]);
        let total = |usd: Value| json!({ "usd_micros": usd, "floor": true, "why": "1 of 2 seats unmeasured", "unmeasured": 1 });
        let cost = |usd: Value| json!({ "total": total(usd.clone()), "groups": [{ "group": 1, "total": total(usd), "members": [] }] });
        let v = effort_over(&store, &w, None, &|_| Default::default(), cost(json!(2_500_000)));
        assert_eq!((v["totals"]["usd_floor"].clone(), v["totals"]["usd_why"].clone()), (json!(true), json!("1 of 2 seats unmeasured")));
        let html = effort_html(&v);
        assert_eq!(html.matches("at least $2.50 (1 of 2 seats unmeasured)").count(), 2, "the totals and the group: {html}");
        let v = effort_over(&store, &w, None, &|_| Default::default(), cost(Value::Null));
        let html = effort_html(&v);
        assert!(html.contains("<td>unmeasured (1 of 2 seats unmeasured)</td>") && !html.contains("$0.00 ·"), "{html}");
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

    /// `wsp-089`: a check seat opened with no check to go on is a standing
    /// check seat with its seconds, and only on its own list.
    #[test]
    fn an_orphaned_check_seat_stands_in_standing_seats_with_its_seconds() {
        let ev = vec![
            (100, "barrier-check-orphaned".to_string(), json!({"id": "run", "group": 2, "seat": "w9:p2", "opened": "2026-11-18T01:00:00Z", "ended": "2026-11-18T01:00:07Z"})),
            (100, "barrier-check-orphaned".to_string(), json!({"id": "other", "group": 1, "seat": "w9:p3", "opened": "2026-11-18T01:00:00Z", "ended": "2026-11-18T01:00:09Z"})),
        ];
        let out = orphaned_checks(&ev, "run");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!((out[0]["role"].as_str(), out[0]["group"].as_u64(), out[0]["seat"].as_str()), (Some("check"), Some(2), Some("w9:p2")));
        assert_eq!(out[0]["seconds"], 7.0);
        assert_eq!((out[0]["measured"].as_bool(), out[0]["orphaned"].as_bool()), (Some(true), Some(true)));
        assert!(out[0]["turns"].is_null());
    }

    #[test]
    fn a_member_with_an_empty_record_but_a_measured_seat_is_filled_from_the_seat_and_not_working_its_span() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, w) = list("empty", &["r-1", "r-2"]);
        let spans = |a: i64, b: i64| vec![Span { start: a, end: b, class: Class::Seat }];
        // r-1 read whole; r-2 (the follow-up) has no calls, tokens or tools, and no transcript.
        let fig = |id: &str| match id {
            "r-1" => RowFigures { start: 0, end: 1000, wall: 1000, spans: spans(0, 1000), active: vec![(0, 1000)], measured: true, calls: 4, model_secs: 800, tokens_in: 5, tokens_out: 6, ..Default::default() },
            _ => RowFigures { start: 0, end: 1000, wall: 1000, spans: spans(0, 1000), ..Default::default() },
        };
        let seat = |turns: u64| json!({"seats": 1, "unmeasured": 0, "turns": turns, "input_tokens": 90, "output_tokens": 30, "cache_tokens": 70, "seconds": 900});
        let cost = |turns: u64| json!({"groups": [{"group": 1, "members": [
            {"task": "r-1", "total": seat(4)},
            {"task": "r-2", "total": seat(turns)},
        ]}]});
        let v = effort_over(&store, &w, None, &fig, cost(7));
        let ms = v["groups"][0]["members"].as_array().unwrap();
        let r2 = ms.iter().find(|m| m["task"] == "r-2").unwrap();
        assert_eq!((r2["calls"].as_u64(), r2["tokens_in"].as_u64(), r2["tokens_out"].as_u64()), (Some(7), Some(90), Some(30)), "{r2}");
        assert_eq!(r2["record_empty_seat_measured"], true);
        assert_eq!(ms.iter().find(|m| m["task"] == "r-1").unwrap()["record_empty_seat_measured"], false);
        // r-1 works the whole span; r-2 alone would have been called working for it.
        let alone = effort_over(&store, &w, None, &|id| if id == "r-2" { fig("r-2") } else { RowFigures::default() }, cost(7));
        assert_eq!(alone["groups"][0]["wall_buckets"]["working_seconds"], 0, "{}", alone["groups"][0]["wall_buckets"]);
        // A seat the cost table did not measure fills nothing.
        let un = effort_over(&store, &w, None, &fig, json!({"groups": [{"group": 1, "members": [{"task": "r-2", "total": {"seats": 1, "unmeasured": 1, "turns": 0, "output_tokens": 0}}]}]}));
        let r2 = un["groups"][0]["members"].as_array().unwrap().iter().find(|m| m["task"] == "r-2").unwrap().clone();
        assert_eq!((r2["record_empty_seat_measured"].clone(), r2["calls"].clone()), (json!(false), json!(0)));
    }

    #[test]
    fn a_running_list_with_one_started_row_and_no_pass_reads_its_wall_to_the_cut_and_says_so() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, mut w) = list("running", &["r-1"]);
        w.set_status(crate::model::WorklistStatus::Running);
        let now = util::epoch_secs();
        // Spawned 300 s ago; the one line the row has written is 15 s in.
        let f = RowFigures { start: now - 300, end: now - 285, wall: 15, spans: vec![Span { start: now - 300, end: now - 285, class: Class::Start }], ..Default::default() };
        // The governor's seat alone has run 127 s; the member's 171.
        let cost = json!({"total": {"seconds": 298}, "groups": [{"group": 1, "members": [{"task": "r-1", "total": {"seats": 1, "unmeasured": 0, "turns": 3, "output_tokens": 9, "seconds": 171}}]}]});
        let v = effort_over(&store, &w, None, &|_| f.clone(), cost.clone());
        let t = &v["totals"];
        let wall = t["wall_seconds"].as_i64().unwrap();
        assert!((300..330).contains(&wall), "the wall runs to now, neither the 15 s to the first line nor under the governor's 127: {wall}");
        assert_eq!(t["wall_buckets"]["wall_seconds"], wall);
        assert_eq!(t["wall_buckets"]["unattributed_seconds"], 0, "{}", t["wall_buckets"]);
        assert_eq!((t["still_running"].clone(), t["cut_at"].is_string()), (json!(true), true), "{t}");
        let causes = t["wall_buckets"]["waiting_by_cause"].to_string();
        assert!(causes.contains("still running"), "the stretch after the last record is named: {causes}");
        let fig = crate::retro::Figures::of(&v);
        assert!(fig.wall >= 300 && fig.cut_at.is_some(), "{fig:?}");
        let head = crate::retro::packet_from(&store, &w, &v, &[], "");
        assert!(head.contains("still running, cut at") && head.contains("a list still running") && !head.contains("a cleared list"), "{}", head.lines().take(3).collect::<Vec<_>>().join("\n"));
        // The same record on a list that is not running keeps the wall it had.
        let mut done = w.clone();
        done.set_status(crate::model::WorklistStatus::Done);
        let v = effort_over(&store, &done, None, &|_| f.clone(), cost);
        assert_eq!((v["totals"]["still_running"].clone(), v["totals"]["wall_seconds"].clone()), (json!(false), json!(15)));
    }

    #[test]
    fn the_seat_idle_is_split_by_what_the_log_says_the_row_waited_on() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, w) = list("idle", &["r-1"]);
        let f = RowFigures {
            start: 0,
            end: 1000,
            wall: 1000,
            spans: vec![Span { start: 0, end: 100, class: Class::Seat }, Span { start: 900, end: 1000, class: Class::Verify }],
            active: vec![(0, 100)],
            measured: true,
            markers: vec![(300, "sent back: the verdict given, the seat not yet told or moving".into()), (700, "reopened: the seat is on the send-back".into())],
            ..Default::default()
        };
        let v = effort_with(&store, &w, None, &|_| f.clone());
        let wb = &v["totals"]["wall_buckets"];
        let by: Vec<(&str, i64)> = wb["seat_idle_by_reason"].as_array().unwrap().iter().map(|x| (x["reason"].as_str().unwrap(), x["seconds"].as_i64().unwrap())).collect();
        assert_eq!(by, vec![("sent back: the verdict given, the seat not yet told or moving", 400), ("none recorded", 200), ("reopened: the seat is on the send-back", 200)], "{wb}");
        assert_eq!(wb["seat_idle_seconds"], 800);
        assert_eq!(v["groups"][0]["wall_buckets"]["seat_idle_seconds"], 800);
        assert!(effort_html(&v).contains("sent back: the verdict given"));
    }

    /// `wsp-313`: a seat whose calls were not timed reads its idle under the
    /// reason, not "none recorded", and the wall still tiles.
    #[test]
    fn a_seat_whose_calls_were_not_timed_names_its_idle_and_not_none_recorded() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span, NONE_RECORDED, NO_TIMESTAMPS, NO_TRANSCRIPT};
        let (_e, store, w) = list("untimed", &["r-1"]);
        let f = RowFigures {
            start: 0,
            end: 1000,
            wall: 1000,
            spans: vec![Span { start: 0, end: 1000, class: Class::Seat }],
            markers: vec![(0, NONE_RECORDED.into()), (600, "told: the seat has the sentence".into())],
            ..Default::default()
        };
        let cost = json!({"groups": [{"group": 1, "members": [{"task": "r-1", "total": {"seats": 1, "unmeasured": 0, "turns": 7, "input_tokens": 9, "output_tokens": 13060, "cache_tokens": 0}}]}]});
        let reasons = |v: &Value| -> Vec<(String, i64)> {
            v["totals"]["wall_buckets"]["seat_idle_by_reason"].as_array().unwrap().iter().map(|x| (x["reason"].as_str().unwrap().to_string(), x["seconds"].as_i64().unwrap())).collect()
        };
        let v = effort_over(&store, &w, None, &|_| f.clone(), cost);
        assert_eq!(v["groups"][0]["members"][0]["record_empty_seat_measured"], true);
        assert_eq!(reasons(&v), vec![(NO_TRANSCRIPT.to_string(), 600), ("told: the seat has the sentence".to_string(), 400)]);
        let wb = &v["totals"]["wall_buckets"];
        assert_eq!(wb["attributed_seconds"], wb["wall_seconds"], "{wb}");
        // Read, but its calls carry no time.
        let g = RowFigures { measured: true, sessions: 1, untimed: 1, ..f.clone() };
        let v = effort_over(&store, &w, None, &|_| g.clone(), json!({}));
        assert_eq!(reasons(&v), vec![(NO_TIMESTAMPS.to_string(), 600), ("told: the seat has the sentence".to_string(), 400)]);
        // A seat read and timed keeps "none recorded" for a stretch nothing names.
        let h = RowFigures { measured: true, sessions: 1, ..f.clone() };
        let v = effort_over(&store, &w, None, &|_| h.clone(), json!({}));
        assert_eq!(reasons(&v)[0], (NONE_RECORDED.to_string(), 600));
    }

    #[test]
    fn a_members_legs_running_are_working_and_not_its_idle() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, w) = list("legs", &["r-1"]);
        let f = RowFigures {
            start: 0,
            end: 1000,
            wall: 1000,
            spans: vec![Span { start: 0, end: 100, class: Class::Seat }, Span { start: 100, end: 900, class: Class::Leg }, Span { start: 900, end: 1000, class: Class::Verify }],
            active: vec![(0, 100), (500, 520)],
            measured: true,
            model_secs: 120,
            ..Default::default()
        };
        let v = effort_with(&store, &w, None, &|_| f.clone());
        let wb = &v["totals"]["wall_buckets"];
        assert_eq!(wb["seat_idle_seconds"], 0, "{wb}");
        assert_eq!(wb["legs_running_seconds"], 780, "the legs' 800 less the seat's own 20 inside them");
        assert_eq!(wb["working_seconds"], 1000);
        assert_eq!(v["groups"][0]["wall_buckets"]["legs_running_seconds"], 780);
        assert_eq!(v["totals"]["legs_running_seconds"], 780);
        assert_eq!(v["totals"]["idle_seconds"], 100, "wall 1000 less model 120 and the legs' 780: the check's 100 is left, as before, and no leg second: {}", v["totals"]);
        assert!(wb["named"].as_array().unwrap().is_empty(), "no leg run in the biggest stalls: {wb}");
    }

    #[test]
    fn the_stretch_between_a_rows_attempts_is_named_and_in_neither_the_seat_idle_nor_idle_seconds() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, w) = list("attempts", &["r-1"]);
        let why = format!("{} \u{2014} parked, waiting on wsp-552", crate::timeline::PARKED_CAUSE);
        let f = RowFigures {
            start: 0,
            end: 1000,
            wall: 1000,
            gap: 800,
            waits: [(why.clone(), 800)].into(),
            between_attempts: 800,
            spans: vec![
                Span { start: 0, end: 100, class: Class::Seat },
                Span { start: 100, end: 900, class: Class::Gap(why.clone()) },
                Span { start: 900, end: 1000, class: Class::Seat },
            ],
            active: vec![(0, 100), (900, 1000)],
            measured: true,
            model_secs: 200,
            ..Default::default()
        };
        let v = effort_with(&store, &w, None, &|_| f.clone());
        let wb = &v["totals"]["wall_buckets"];
        assert_eq!(wb["seat_idle_seconds"], 0, "{wb}");
        assert_eq!(v["totals"]["idle_seconds"], 0, "{}", v["totals"]);
        assert_eq!(v["totals"]["between_attempts_seconds"], 800);
        assert_eq!(v["groups"][0]["members"][0]["between_attempts_seconds"], 800);
        assert_eq!(wb["waiting_seconds"], 800);
        assert_eq!(wb["unattributed_seconds"], 0, "{wb}");
        assert!(wb["waiting_by_cause"].as_array().unwrap().iter().any(|c| c["cause"].as_str().unwrap().contains("parked, waiting on wsp-552")), "{wb}");
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
        // Each member's own line still holds the phases it saw.
        assert_eq!(v["groups"][0]["members"][0]["gap_seconds"], 50);
    }

    /// `wsp-854`: three members run side by side (turnaround's group 1), so the
    /// group's wall is its window, not the 8864 s their walls add to; with one
    /// group that is the list's wall.
    #[test]
    fn a_groups_wall_is_its_window_and_not_its_members_walls_added_up() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, w) = list("window", &["r-1", "r-2", "r-3"]);
        let fig = |id: &str| {
            let wall = if id == "r-3" { 1064 } else { 3900 };
            RowFigures { start: 100, end: 100 + wall, wall, spans: vec![Span { start: 100, end: 100 + wall, class: Class::Seat }], measured: true, ..Default::default() }
        };
        let v = effort_with(&store, &w, None, &fig);
        let g = &v["groups"][0];
        assert_eq!(g["wall_seconds"], 3900, "{g}");
        assert_eq!(g["wall_seconds"], g["wall_buckets"]["window_seconds"]);
        assert_eq!(g["wall_seconds"], v["totals"]["wall_seconds"]);
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

    #[test]
    fn the_wall_buckets_name_a_blocked_row_and_the_barrier_run_and_tile_the_wall() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, w) = list("buckets", &["r-1", "r-2"]);
        let sp = |a, b, class| Span { start: a, end: b, class };
        let fig = |id: &str| {
            let blocked = id == "r-2";
            let mut spans = vec![sp(0, 10, Class::Start), sp(10, if blocked { 2000 } else { 100 }, Class::Seat)];
            spans.push(sp(2000, 2900, Class::Barrier));
            spans.push(sp(2900, 3000, Class::Check));
            RowFigures {
                start: 0,
                end: 3000,
                wall: 3000,
                spans,
                active: if blocked { vec![(10, 50), (1900, 2000)] } else { vec![(10, 100)] },
                blocked: if blocked { vec![(50, 1900)] } else { vec![] },
                measured: true,
                ..Default::default()
            }
        };
        let v = effort_with(&store, &w, None, &fig);
        let b = &v["totals"]["wall_buckets"];
        assert_eq!(b["wall_seconds"], 3000, "{v}");
        assert_eq!(b["attributed_seconds"], 3000, "every second of the wall is in a bucket: {b}");
        assert_eq!(b["unattributed_seconds"], 0);
        assert_eq!(b["working_seconds"], 90 + 100);
        assert_eq!(b["waiting_seconds"], 10 + 1800);
        assert_eq!(b["barrier_run_seconds"], 900);
        assert_eq!(b["check_seat_seconds"], 100);
        let named = b["named"].as_array().unwrap();
        assert!(named.iter().any(|x| x["row"] == "r-2" && x["seconds"] == 1800 && x["what"].as_str().unwrap().contains("blocked")), "{b}");
        assert!(named.iter().any(|x| x["bucket"] == "barrier_run" && x["seconds"] == 900), "{b}");
        let html = effort_html(&v);
        assert!(html.contains("Where the wall went") && html.contains("r-2"), "the report prints the buckets and names the stall");
    }

    /// react-chat (`compounding-464`): a person parked the whole list while a row
    /// stood blocked and the rest was released. The park is its own bucket, who
    /// and why are named, the row it fell on keeps none of it, and the wall is given
    /// without it.
    #[test]
    fn a_list_parked_by_a_person_is_its_own_bucket_named_for_who_and_off_the_row_it_fell_on() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, w) = list("parkedlist", &["r-1", "r-2"]);
        let t0: i64 = 1_000_000_000;
        let ev = |at: i64, kind: &str, by: &str| json!({ "ts": util::iso_at(t0 + at), "kind": kind, "data": { "id": "parkedlist", "why": "stop everything, he needs the machine", "by": by } }).to_string();
        std::fs::write(store.state_file("events.jsonl"), format!("{}\n{}\n", ev(200, "worklist-parked", "Ed"), ev(1900, "worklist-resumed", "Ed"))).unwrap();
        let sp = |a: i64, b: i64, class| Span { start: t0 + a, end: t0 + b, class };
        let fig = |id: &str| {
            let blocked = id == "r-2";
            let mut spans = vec![sp(0, 10, Class::Start), sp(10, if blocked { 2000 } else { 100 }, Class::Seat)];
            spans.push(sp(2000, 2900, Class::Barrier));
            spans.push(sp(2900, 3000, Class::Check));
            RowFigures {
                start: t0,
                end: t0 + 3000,
                wall: 3000,
                spans,
                active: if blocked { vec![(t0 + 10, t0 + 50), (t0 + 1900, t0 + 2000)] } else { vec![(t0 + 10, t0 + 100)] },
                blocked: if blocked { vec![(t0 + 50, t0 + 1900)] } else { vec![] },
                measured: true,
                ..Default::default()
            }
        };
        let v = effort_with(&store, &w, None, &fig);
        let b = &v["totals"]["wall_buckets"];
        assert_eq!(b["wall_seconds"], 3000, "{v}");
        assert_eq!(b["parked_seconds"], 1700, "{b}");
        assert_eq!(b["wall_without_parked_seconds"], 1300, "{b}");
        assert_eq!(b["attributed_seconds"], 3000, "the park tiles the wall with the rest: {b}");
        assert_eq!(b["waiting_seconds"], 10 + 100, "the blocked row's wait stops at the park: {b}");
        assert_eq!(b["barrier_run_seconds"], 900);
        let by = b["parked_by"].as_array().unwrap();
        assert!(by.len() == 1 && by[0]["seconds"] == 1700 && by[0]["why"].as_str().unwrap().contains("parked by Ed") && by[0]["why"].as_str().unwrap().contains("he needs the machine"), "{b}");
        let named = b["named"].as_array().unwrap();
        assert!(named.iter().any(|x| x["bucket"] == "parked" && x["seconds"] == 1700 && x["row"] == ""), "{b}");
        assert!(!b["waiting_by_cause"].as_array().unwrap().iter().any(|c| c["cause"].as_str().unwrap().contains("parked")), "no row's wait is the park: {b}");
        assert_eq!(v["groups"][0]["wall_buckets"]["parked_seconds"], 1700);
        let html = effort_html(&v);
        assert!(html.contains("wall without the park"), "{html}");
    }

    /// A cause past its cap reads the same wherever effort.json carries it, and
    /// is cut between words (`wsp-856`).
    #[test]
    fn a_long_wait_cause_is_cut_between_words_and_reads_the_same_everywhere() {
        use crate::timeline::{Inputs, SeatReport};
        let (_e, store, w) = list("longcause", &["r-1"]);
        let t0: i64 = 1_000_000_000;
        let why = "the list stops here for a person. ".repeat(20);
        let i = Inputs {
            id: "r-1".into(),
            group: Some(("longcause".into(), 1, vec!["r-1".into()])),
            cycle: vec![
                (t0 + 1000, "longcause group 1: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 60s".into()),
                (t0 + 1010, "spawn longcause --agent --check 1 --kind claude".into()),
                (t0 + 9000, "tell longcause: Barrier 1 of longcause passed".into()),
            ],
            events: [(110, "task-claimed"), (305, "task-review"), (380, "task-verified")].iter().map(|(t, k)| (t0 + t, k.to_string(), json!({"id": "r-1"}))).collect(),
            holds: vec![(t0 + 2000, t0 + 8000, why.clone())],
            ..Default::default()
        };
        let phases = crate::timeline::build(&i);
        let v = effort_with(&store, &w, None, &|_| crate::timeline::figures_of(&phases, &SeatReport::default()));
        let held = |a: &Value, k: &str| -> String {
            a.as_array().into_iter().flatten().filter_map(|x| x[k].as_str()).find(|c| c.contains(crate::timeline::HELD_FOR_STEP)).unwrap_or_else(|| panic!("{v}")).to_string()
        };
        let cause = held(&v["waits"], "cause");
        let (_, quoted) = cause.split_once(": the list stops").unwrap_or_else(|| panic!("{cause}"));
        let kept = format!("the list stops{}", quoted.strip_suffix('…').unwrap_or_else(|| panic!("{cause}")));
        assert!(why.starts_with(&format!("{kept} ")), "cut between words: {cause}");
        assert_eq!(held(&v["groups"][0]["wall_buckets"]["waiting_by"], "cause"), cause);
        assert_eq!(held(&v["totals"]["wall_buckets"]["waiting_by_cause"], "cause"), cause);
        assert_eq!(held(&v["totals"]["wall_buckets"]["named"], "what"), format!("the group — {cause}"));
    }

    #[test]
    fn a_manual_groups_wait_for_its_pass_is_named_and_the_list_has_one_wall_to_that_pass() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, _w) = list("passwait", &["r-1"]);
        store.save_task(&Task::new("r-2", "r-2")).unwrap();
        assert_eq!(run(&store, &["add", "passwait", "r-2"], &[]), 0);
        assert_eq!(run(&store, &["group", "passwait", "2"], &[("agent", "manual")]), 0);
        let w = store.worklist("passwait").unwrap();
        assert_eq!(w.groups().len(), 2);
        let t0: i64 = 1_000_000_000;
        // Group 1 passes at 110; the last member (group 2, manual) lands at 300
        // and the person gives the pass 37 minutes later, at 2520.
        let ev = |at: i64, n: u64| json!({ "ts": util::iso_at(t0 + at), "kind": "worklist-go", "data": { "id": "passwait", "passed": n, "started": false } }).to_string();
        std::fs::write(store.state_file("events.jsonl"), format!("{}\n{}\n", ev(110, 1), ev(2520, 2))).unwrap();
        let sp = |a: i64, b: i64, class| Span { start: t0 + a, end: t0 + b, class };
        let fig = |id: &str| {
            if id == "r-1" {
                RowFigures {
                    start: t0,
                    end: t0 + 110,
                    wall: 110,
                    spans: vec![sp(0, 10, Class::Start), sp(10, 100, Class::Seat), sp(100, 107, Class::Barrier), sp(107, 110, Class::Check)],
                    active: vec![(t0 + 10, t0 + 100)],
                    measured: true,
                    ..Default::default()
                }
            } else {
                RowFigures {
                    start: t0 + 120,
                    end: t0 + 300,
                    wall: 180,
                    spans: vec![sp(120, 130, Class::Start), sp(130, 300, Class::Seat)],
                    active: vec![(t0 + 130, t0 + 300)],
                    measured: true,
                    ..Default::default()
                }
            }
        };
        let v = effort_with(&store, &w, None, &fig);
        let t = &v["totals"];
        let b = &t["wall_buckets"];
        assert_eq!(t["wall_seconds"], 2520, "first spawn to the last pass: {t}");
        assert_eq!(t["last_verdict_wall_seconds"], 300);
        assert_eq!(b["wall_seconds"], t["wall_seconds"], "one wall for the list");
        let walls: Vec<i64> = v["groups"].as_array().unwrap().iter().map(|g| g["wall_seconds"].as_i64().unwrap()).collect();
        assert_eq!(walls, vec![110, 2410], "each group's window, pass to pass");
        assert_eq!(walls.iter().sum::<i64>(), t["wall_seconds"].as_i64().unwrap());
        assert_eq!(b["attributed_seconds"], b["wall_seconds"], "{b}");
        assert_eq!(b["unattributed_seconds"], 0);
        assert_eq!(v["groups"][1]["wall_buckets"]["barrier_run_seconds"], 0, "a manual group ran no barrier");
        assert_eq!(b["barrier_run_seconds"], 7, "group 1's one barrier run");
        let cause = "group 2: waiting for the pass (manual group: no barrier ran, the pass not yet given)";
        let wait = b["waiting_by_cause"].as_array().unwrap().iter().find(|x| x["cause"] == cause).unwrap_or_else(|| panic!("{b}"));
        assert_eq!(wait["seconds"], 2220);
        assert!(b["named"].as_array().unwrap().iter().any(|x| x["group"] == 2 && x["seconds"] == 2220 && x["what"].as_str().unwrap().contains("manual group")), "{b}");
        let html = effort_html(&v);
        assert!(html.contains("first spawn to last pass") && html.contains("42m"), "the report prints the one wall");
    }

    #[test]
    fn a_manual_groups_wait_for_a_hand_to_start_is_named_in_the_wall_buckets_and_the_governor_wall() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, _w) = list("handwait", &["r-1"]);
        store.save_task(&Task::new("r-2", "r-2")).unwrap();
        assert_eq!(run(&store, &["add", "handwait", "r-2"], &[]), 0);
        assert_eq!(run(&store, &["group", "handwait", "2"], &[("agent", "manual")]), 0);
        let w = store.worklist("handwait").unwrap();
        let t0: i64 = 1_000_000_000;
        // Group 1 passes at 110; nobody starts the manual group 2 for 9 h 53 m
        // (35 580 s), its member runs 180 s, and its pass is given at 36 000.
        // The cycle log says nothing of the list in that time.
        let start2 = 110 + 35_580;
        let ev = |at: i64, n: u64| json!({ "ts": util::iso_at(t0 + at), "kind": "worklist-go", "data": { "id": "handwait", "passed": n, "started": false } }).to_string();
        std::fs::write(store.state_file("events.jsonl"), format!("{}\n{}\n", ev(110, 1), ev(36_000, 2))).unwrap();
        let sp = |a: i64, b: i64, class| Span { start: t0 + a, end: t0 + b, class };
        let fig = |id: &str| {
            if id == "r-1" {
                RowFigures {
                    start: t0,
                    end: t0 + 110,
                    wall: 110,
                    spans: vec![sp(0, 10, Class::Start), sp(10, 100, Class::Seat), sp(100, 107, Class::Barrier), sp(107, 110, Class::Check)],
                    active: vec![(t0 + 10, t0 + 100)],
                    measured: true,
                    ..Default::default()
                }
            } else {
                RowFigures {
                    start: t0 + start2,
                    end: t0 + start2 + 180,
                    wall: 180,
                    spans: vec![sp(start2, start2 + 10, Class::Start), sp(start2 + 10, start2 + 180, Class::Seat)],
                    active: vec![(t0 + start2 + 10, t0 + start2 + 180)],
                    measured: true,
                    ..Default::default()
                }
            }
        };
        let v = effort_with(&store, &w, None, &fig);
        let b = &v["totals"]["wall_buckets"];
        assert_eq!(b["attributed_seconds"], b["wall_seconds"], "{b}");
        assert_eq!(b["unattributed_seconds"], 0, "{b}");
        let g2 = &v["groups"][1]["wall_buckets"];
        assert_eq!(g2["between_groups_seconds"], 0, "a manual group's wait for a start is not between_groups: {g2}");
        let cause = format!("group 2: {}", crate::wallbuckets::START_WAIT_MANUAL);
        assert!(cause.contains("waiting for a hand to start"), "{cause}");
        let by = g2["waiting_by"].as_array().unwrap();
        let wait = by.iter().find(|x| x["cause"] == cause.as_str()).unwrap_or_else(|| panic!("{g2}"));
        assert_eq!(wait["seconds"], 35_580 - 0, "{g2}");
        let in_totals = b["waiting_by_cause"].as_array().unwrap().iter().find(|x| x["cause"] == cause.as_str()).unwrap_or_else(|| panic!("{b}"));
        assert_eq!(in_totals["seconds"], 35_580);
        assert!(b["named"].as_array().unwrap().iter().any(|x| x["group"] == 2 && x["seconds"] == 35_580 && x["what"].as_str().unwrap().contains("waiting for a hand to start")), "the stalls list names it: {b}");
        let gw = &v["groups"][0]["governor_wall"];
        assert_eq!(gw["seconds_to_first_start"], 35_580, "{gw}");
        assert_eq!(gw["held_by"], cause.as_str(), "the cycle log is silent, the cause is the wall buckets': {gw}");
        assert!(v["groups"][1]["governor_wall"].is_null(), "the last group has no next start");
    }

    /// A two-group list for `wsp-857`: r-1 runs group 1 from `t0 + 399_000`
    /// to its pass at `w`, r-2 is group 2's, read by the timeline from
    /// `shape`, and group 2 passes at `w + 1200`.
    fn early_member(slug: &str, shape: fn(&str, i64, Option<i64>) -> crate::timeline::Inputs) -> Value {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, _w) = list(slug, &["r-1"]);
        store.save_task(&Task::new("r-2", "r-2")).unwrap();
        assert_eq!(run(&store, &["add", slug, "r-2"], &[]), 0);
        let w = store.worklist(slug).unwrap();
        assert_eq!(w.groups().len(), 2);
        let (t0, at): (i64, i64) = (1_000_000_000, 1_000_400_000);
        let ev = |t: i64, n: u64| json!({ "ts": util::iso_at(t), "kind": "worklist-go", "data": { "id": slug, "passed": n, "started": false } }).to_string();
        std::fs::write(store.state_file("events.jsonl"), format!("{}\n{}\n", ev(at, 1), ev(at + 1200, 2))).unwrap();
        // The window r-2's timeline reads is the one gather finds: group 1's pass.
        assert_eq!(crate::timeline::gather(&store, "r-2").window, Some(at));
        assert_eq!(crate::timeline::gather(&store, "r-1").window, None, "the first group's window opens at its first start");
        let r2 = crate::timeline::shapes::figures(&shape("r-2", t0, Some(at)));
        let fig = |id: &str| {
            if id == "r-1" {
                let sp = |a: i64, b: i64, class| Span { start: t0 + a, end: t0 + b, class };
                RowFigures {
                    start: t0 + 399_000,
                    end: at,
                    wall: 1000,
                    spans: vec![sp(399_000, 399_010, Class::Start), sp(399_010, 399_900, Class::Seat), sp(399_900, 400_000, Class::Barrier)],
                    active: vec![(t0 + 399_010, t0 + 399_900)],
                    measured: true,
                    ..Default::default()
                }
            } else {
                r2.clone()
            }
        };
        effort_with(&store, &w, None, &fig)
    }

    /// What `wsp-857` holds of an effort report with a member read from before
    /// its window: the wall is the windows, every second of it in a bucket,
    /// and the waits agree with the buckets' causes.
    fn holds_to_its_windows(v: &Value) {
        let t = &v["totals"];
        let b = &t["wall_buckets"];
        let walls: Vec<i64> = v["groups"].as_array().unwrap().iter().map(|g| g["wall_seconds"].as_i64().unwrap()).collect();
        assert_eq!(walls, vec![1000, 1200], "each group's window: {v}");
        assert_eq!(t["wall_seconds"], walls.iter().sum::<i64>(), "the wall is the sum of the group windows: {t}");
        assert_eq!(b["attributed_seconds"], t["wall_seconds"], "{b}");
        assert_eq!(b["unattributed_seconds"], 0, "{b}");
        assert_eq!(v["notes"], json!([]), "nothing to note: {v}");
        let by_cause: Vec<(String, i64)> = b["waiting_by_cause"].as_array().unwrap().iter().map(|c| (c["cause"].as_str().unwrap().to_string(), c["seconds"].as_i64().unwrap())).collect();
        for w in v["waits"].as_array().unwrap() {
            let cause = w["cause"].as_str().unwrap();
            let got: i64 = by_cause.iter().filter(|(c, _)| c.ends_with(cause)).map(|(_, s)| *s).sum();
            assert_eq!(Some(got), w["seconds"].as_i64(), "waits[] {cause:?} against waiting_by_cause: {b}");
        }
        let unnamed: i64 = by_cause.iter().filter(|(c, _)| c.contains("nothing the logs")).map(|(_, s)| *s).sum();
        assert_eq!(t["unnamed_wait_seconds"], unnamed, "{v}");
    }

    /// `wsp-857`, read-api's wsp-373: a member spawned days before its group
    /// window. Its gap is read from the window's start, and the list's wall is
    /// its two windows, not 102 h from the early spawn with 92 % unattributed.
    #[test]
    fn a_member_spawned_days_before_its_window_leaves_the_wall_its_windows_and_nothing_unattributed() {
        let v = early_member("ahead", crate::timeline::shapes::spawned_ahead);
        holds_to_its_windows(&v);
        let gap = v["waits"].as_array().unwrap().iter().find(|w| w["cause"].as_str().unwrap().starts_with(crate::timeline::PARKED_CAUSE)).unwrap_or_else(|| panic!("{v}"));
        assert_eq!(gap["seconds"], 90, "from the window's start to the next spawn: {v}");
        assert_eq!(v["totals"]["between_attempts_seconds"], 90);
    }

    /// `wsp-857`, turnaround's wsp-802: a member never seated whose legs were
    /// added days before its group.
    #[test]
    fn a_never_started_member_with_legs_added_days_ahead_waits_from_its_window() {
        let v = early_member("legsahead", crate::timeline::shapes::legs_added_ahead);
        holds_to_its_windows(&v);
        assert_eq!(v["totals"]["unnamed_wait_seconds"], 100, "the window's start to leg a: {v}");
    }

    /// `wsp-857`: effort.json says so where more than 1 % of the wall is in no
    /// bucket, and is silent at 1 % or less.
    /// `wsp-901`: crate-split's report, rebuilt after members were archived,
    /// read 0 legs and half the reopens. The member here ran two legs, was
    /// reopened, and its rework took the pane id of an earlier barrier check,
    /// whose banked line is the rework's and not the check's.
    #[test]
    fn a_finished_lists_effort_reads_the_same_once_its_members_are_archived() {
        let (env, store, mut w) = list("arch", &["r-1"]);
        let at = |hm: &str| format!("2026-10-18T{hm}:00Z");
        let mut t = store.find_task("r-1").unwrap();
        t.body = format!(
            "## Legs\n- a · done\n- b · done\n\n## Log\n\
- {} claimed by pane cpd-1 · spawned at sonnet/high\n\
- {} review: first\n\
- {} released after 30m · ran sonnet-5-5/high · 9 turns · 1M cache 1k in 1k out\n\
- {} claimed by pane cpd-9 · spawned at sonnet/high\n\
- {} leg a started by pane cpd-2 · spawned at sonnet/high\n\
- {} leg a ended after 10m · ran sonnet-5-5/high · 3 turns · 1M cache 1k in 1k out\n\
- {} leg b started by pane cpd-3 · spawned at sonnet/high\n\
- {} leg b ended after 10m · ran sonnet-5-5/high · 3 turns · 1M cache 1k in 1k out\n\
- {} review: again\n",
            at("09:00"), at("09:30"), at("09:31"), at("10:00"), at("10:05"), at("10:15"), at("10:20"), at("10:30"), at("10:40")
        );
        t.status_raw = crate::model::Status::Done.as_str().into();
        store.save_task(&t).unwrap();
        let mut gs = w.groups();
        let mut check = crate::barrier::Check::opened(None, None);
        check.at = at("08:00");
        check.pane = Some("cpd-9".into());
        gs[0].checks.push(check);
        w.set_groups(&gs);
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        let ev = |hm: &str, kind: &str, data: Value| json!({ "ts": at(hm), "kind": kind, "data": data }).to_string();
        let events = [
            ev("09:00", "task-claimed", json!({ "id": "r-1", "pane": "cpd-1" })),
            ev("09:30", "task-review", json!({ "id": "r-1" })),
            ev("09:31", "task-released", json!({ "id": "r-1" })),
            ev("10:00", "task-claimed", json!({ "id": "r-1", "pane": "cpd-9" })),
            ev("10:05", "leg-added", json!({ "id": "r-1", "leg": "a" })),
            ev("10:15", "leg-done", json!({ "id": "r-1", "leg": "a" })),
            ev("10:30", "leg-done", json!({ "id": "r-1", "leg": "b" })),
            ev("10:40", "task-review", json!({ "id": "r-1" })),
            ev("10:50", "task-verified", json!({ "id": "r-1", "verdict": "holds" })),
            ev("10:51", "worklist-go", json!({ "id": "arch", "passed": 1, "started": false })),
        ];
        std::fs::write(store.state_file("events.jsonl"), format!("{}\n", events.join("\n"))).unwrap();
        let ledger: String = ["cpd-1", "cpd-2", "cpd-3", "cpd-9"]
            .iter()
            .map(|p| format!("{}\n", json!({ "seat": p, "ended": at("10:45"), "session_id": p, "turns": 3, "input": 10, "output": 100, "cache_read": 5, "cache_write": 2, "cost": 1000 })))
            .collect();
        std::fs::write(env.state().join(crate::place_super::SPENT_FILE), ledger).unwrap();

        let read = || {
            let mut v = effort(&store, &store.worklist("arch").unwrap(), None);
            v.as_object_mut().unwrap().remove("generated");
            v
        };
        let legs = || crate::cmd_cost::entries(&store, "arch").unwrap().iter().filter(|e| e.role == "leg").count();
        let before = read();
        assert_eq!(before["rework"]["reopened"], 1, "{}", before["rework"]);
        assert_eq!(legs(), 2);
        assert!(before["totals"]["wall_seconds"].as_i64().unwrap() > 0);
        let live = store.find_task("r-1").unwrap();
        store.archive_task(&live).unwrap();
        assert!(store.find_task("r-1").is_none(), "archived");
        let after = read();
        assert_eq!(legs(), 2);
        // Key by key first, so a difference is named and not the whole report.
        for (k, x) in before.as_object().unwrap() {
            assert_eq!(&after[k], x, "{k}");
        }
        assert_eq!(after, before);
    }

    #[test]
    fn an_unattributed_share_over_one_percent_of_the_wall_is_noted() {
        let note = unattributed_note(&json!({ "wall_seconds": 367_253, "unattributed_seconds": 333_032 })).unwrap();
        assert!(note.contains("333032 s of the 367253 s wall (90.7 %)"), "{note}");
        assert_eq!(unattributed_note(&json!({ "wall_seconds": 1000, "unattributed_seconds": 10 })), None);
        assert!(unattributed_note(&json!({ "wall_seconds": 1000, "unattributed_seconds": 11 })).is_some());
        assert_eq!(unattributed_note(&json!({ "wall_seconds": 0, "unattributed_seconds": 0 })), None);
    }

    /// An agent group's start is the run's: a cycle-log line still names it, and
    /// with none the window says nothing, and its seconds stay `between_groups`.
    #[test]
    fn a_run_started_groups_wait_keeps_between_groups_and_no_held_by() {
        use crate::timeline::RowFigures;
        use crate::wallbuckets::{Class, Span};
        let (_e, store, _w) = list("runstart", &["r-1"]);
        store.save_task(&Task::new("r-2", "r-2")).unwrap();
        assert_eq!(run(&store, &["add", "runstart", "r-2"], &[]), 0);
        let w = store.worklist("runstart").unwrap();
        assert_eq!(w.groups().len(), 2);
        let t0: i64 = 1_000_000_000;
        let ev = |at: i64, n: u64| json!({ "ts": util::iso_at(t0 + at), "kind": "worklist-go", "data": { "id": "runstart", "passed": n, "started": false } }).to_string();
        std::fs::write(store.state_file("events.jsonl"), format!("{}\n{}\n", ev(110, 1), ev(400, 2))).unwrap();
        let sp = |a: i64, b: i64, class| Span { start: t0 + a, end: t0 + b, class };
        let fig = |id: &str| {
            if id == "r-1" {
                RowFigures { start: t0, end: t0 + 110, wall: 110, spans: vec![sp(0, 10, Class::Start), sp(10, 100, Class::Seat), sp(100, 107, Class::Barrier), sp(107, 110, Class::Check)], active: vec![(t0 + 10, t0 + 100)], measured: true, ..Default::default() }
            } else {
                RowFigures { start: t0 + 150, end: t0 + 390, wall: 240, spans: vec![sp(150, 160, Class::Start), sp(160, 390, Class::Seat)], active: vec![(t0 + 160, t0 + 390)], measured: true, ..Default::default() }
            }
        };
        let v = effort_with(&store, &w, None, &fig);
        let g2 = &v["groups"][1]["wall_buckets"];
        assert_eq!(g2["between_groups_seconds"], 40, "{g2}");
        assert!(!g2["waiting_by"].to_string().contains("waiting for a hand to start"), "{g2}");
        assert!(v["groups"][0]["governor_wall"]["held_by"].is_null(), "{}", v["groups"][0]["governor_wall"]);
        let b = &v["totals"]["wall_buckets"];
        assert_eq!(b["attributed_seconds"], b["wall_seconds"], "{b}");
    }
}
