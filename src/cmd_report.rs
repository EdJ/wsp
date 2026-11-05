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

    for (gi, g) in groups.iter().enumerate() {
        let (mut gwall, mut gmodel, mut gtool) = (0i64, 0i64, 0i64);
        let mut gsuites = 0u64;
        let mut members: Vec<Value> = Vec::new();
        for m in &g.members {
            let f = figures(m);
            let suites = crate::throttle::full_suite_runs(&store.state, m);
            gsuites += suites;
            suites_total += suites;
            let tool: i64 = f.tools.values().sum();
            gwall += f.wall;
            gmodel += f.model_secs;
            gtool += tool;
            model += f.model_secs;
            gap += f.gap;
            calls += f.calls;
            idle += (f.wall - f.model_secs - tool).max(0);
            missing_sessions += f.missing;
            if f.end > 0 {
                start = start.min(f.start);
                end = end.max(f.end);
            }
            for (k, v) in &f.waits {
                *waits.entry(k.clone()).or_insert(0) += v;
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
            }));
        }
        let gcost = cost["groups"].get(gi).and_then(|c| c.get("total")).cloned().unwrap_or(Value::Null);
        group_json.push(json!({
            "group": gi + 1, "wall_seconds": gwall, "model_seconds": gmodel, "tool_seconds": gtool,
            "cost": gcost, "full_suite_runs": gsuites, "members": members,
        }));
    }
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
            "time": "per member row, spawn to verdict (wall, gaps); the seat's model and tool time are its own transcript calls inside those phases, legs only where run-leg timings exist",
        },
        "totals": {
            "wall_seconds": wall,
            "model_seconds": model,
            "tool_seconds": tools,
            "tool_seconds_total": tool_total,
            "idle_seconds": idle,
            "gap_seconds": gap,
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
            "r-1" => RowFigures { start: 100, end: 400, wall: 300, gap: 40, model_secs: 120, calls: 5, tokens_in: 10, tokens_out: 4, cache_read: 7, waits: [("a tick".to_string(), 25), ("the cap".to_string(), 15)].into(), tools: [("Bash".to_string(), 50)].into(), ..Default::default() },
            "r-2" => RowFigures { start: 90, end: 300, wall: 210, gap: 30, model_secs: 60, calls: 3, waits: [("a tick".to_string(), 30)].into(), tools: [("Bash".to_string(), 10), ("Read".to_string(), 5)].into(), missing: 1, ..Default::default() },
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
