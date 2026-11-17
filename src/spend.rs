//! `wsp spend [--month YYYY-MM] [--json]` — the month's spend, for compound's
//! governance view (wsp-661; compounding-744 draws it, -745 joins it).
//!
//! **The money is the stop ledger** (`<state>/spent.jsonl`): one line per ended
//! seat, its `cost` in micro-dollars at list prices ([`crate::cmd_burn`]), dated
//! by `ended`. A seat still up is in no month yet; `wsp burn` has it.
//!
//! **Each line is placed by the seat that spent it**, by the join
//! `wsp attempts --cost` and `wsp worklist cost` make ([`crate::cmd_cost`]): a
//! list's governor and barrier-check seats, a row's verifier passes, then a
//! row's attempts (claims and legs), first one wins. A row is in the first list
//! that holds it; its project is its own, else its list's home.
//!
//! - **forecast** — the month's pace so far carried to its end: spent ÷ days
//!   elapsed (today counted whole) × days in the month. A month past is its
//!   spend; a month to come is 0.
//! - **landed** — rows with a verifier's `holds` decided in the month; the
//!   month's spend over them, beside last month's the same way.
//! - **waste**, each seat counted once, in this order:
//!   `rework` the whole seat of an attempt that reached review and was sent back
//!   (reopened); `other` the whole seat of an attempt dropped or reaped;
//!   `killed_checks` the share of an attempt's seat that its row's own checks
//!   spent timed out under load and killed (`task-checked` state `waiting`, the
//!   effort report's kind, wsp-645) — their seconds over the attempt's;
//!   `idle` the whole seat of an ended seat that held no row, leg, verifier
//!   pass, barrier check or governorship (a person's own seat counts here).
//! - **act on** — the largest few: a list or row whose attempts looped (sent
//!   back), the dearest idle seats, a project over its budget's pace.
//!
//! Out of v1: teams (projects stand in for them, `wsp budget`), risk and
//! compliance controls, and reconciling against an invoice — the figures are
//! list prices, not a bill.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{json, Value};

use crate::cmd_attempts::{attempts_of, Outcome};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// A calendar month.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Month {
    pub year: i64,
    pub month: i64,
}

impl Month {
    pub fn parse(s: &str) -> Option<Month> {
        let (y, m) = s.split_once('-')?;
        let (year, month) = (y.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
        (y.len() == 4 && m.len() == 2 && (1..=12).contains(&month)).then_some(Month { year, month })
    }
    pub fn of(secs: i64) -> Month {
        Month::parse(&util::iso_at(secs)[..7]).expect("iso_at writes YYYY-MM")
    }
    pub fn now() -> Month {
        Month::of(util::epoch_secs())
    }
    pub fn label(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
    pub fn start(self) -> i64 {
        util::epoch_of(&format!("{}-01T00:00:00Z", self.label()))
    }
    pub fn next(self) -> Month {
        if self.month == 12 { Month { year: self.year + 1, month: 1 } } else { Month { year: self.year, month: self.month + 1 } }
    }
    pub fn prev(self) -> Month {
        if self.month == 1 { Month { year: self.year - 1, month: 12 } } else { Month { year: self.year, month: self.month - 1 } }
    }
    pub fn days(self) -> i64 {
        (self.next().start() - self.start()) / 86_400
    }
}

/// One ledger line: an ended seat and what it spent.
#[derive(Clone, Debug)]
pub struct Line {
    pub seat: String,
    pub ended: i64,
    pub model: String,
    pub micros: u64,
}

pub fn ledger(state: &Path) -> Vec<Line> {
    let text = std::fs::read_to_string(state.join(crate::place_super::SPENT_FILE)).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            Some(Line {
                seat: v["seat"].as_str()?.to_string(),
                ended: util::epoch_of(v["ended"].as_str()?),
                model: v["model"].as_str().unwrap_or("").to_string(),
                micros: v["cost"].as_u64().unwrap_or(0),
            })
        })
        .filter(|l| l.ended > 0)
        .collect()
}

/// The tier family a model id is billed as.
pub fn family(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    ["opus", "sonnet", "haiku"].into_iter().find(|f| m.contains(f)).unwrap_or("other")
}

/// What a seat held.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Place {
    pub task: Option<String>,
    pub list: Option<String>,
    pub project: Option<String>,
    /// An attempt that reached review and was sent back.
    pub sent_back: bool,
    /// An attempt dropped or reaped.
    pub dropped: bool,
    /// The share of the attempt its row's timed-out checks took, 0..=1.
    pub check_share: f64,
}

/// Every seat the store can place, by seat id (see the module docs for the
/// order).
pub fn places(store: &Store) -> HashMap<String, Place> {
    let events: Vec<Value> = std::fs::read_to_string(store.state_file("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    // Timed-out checks by row: (when it ended, seconds it ran).
    let mut timed_out: HashMap<String, Vec<(i64, i64)>> = HashMap::new();
    for e in events.iter().filter(|e| e["kind"] == "task-checked" && e["data"]["state"] == "waiting") {
        if let Some(id) = e["data"]["id"].as_str() {
            let at = util::epoch_of(e["ts"].as_str().unwrap_or(""));
            timed_out.entry(id.to_string()).or_default().push((at, e["data"]["took"].as_i64().unwrap_or(0).max(0)));
        }
    }

    let lists = store.worklists();
    let tasks = store.tasks();
    // Every list's home in one read, as `wip` takes them (`worklist::homes`).
    let homes = crate::worklist::homes(&lists, &tasks, &crate::resolve::Index::new(store.projects()));
    let home: HashMap<String, Option<String>> = lists.iter().map(|w| (w.id.clone(), homes.get(&w.id).cloned())).collect();
    let at_list = |id: &str| Place { list: Some(id.to_string()), project: home.get(id).cloned().flatten(), ..Place::default() };
    let mut list_of: HashMap<String, String> = HashMap::new();
    let mut out: HashMap<String, Place> = HashMap::new();
    // A governor's seat, from the one read of the log: a list's when the scope
    // is a list (the seats `cmd_cost::governors` counts for it, which reads the
    // log once a list — 174 lists of a 14 MB log is minutes), else its
    // project's own, which no list holds.
    for e in events.iter().filter(|e| e["kind"] == "governor-set") {
        let (Some(p), Some(ws)) = (e["data"]["project"].as_str(), e["data"]["workspace"].as_str()) else { continue };
        let place = if home.contains_key(p) { at_list(p) } else { Place { project: Some(p.to_string()), ..Place::default() } };
        out.entry(ws.to_string()).or_insert(place);
    }
    for w in &lists {
        let groups = w.groups();
        for m in groups.iter().flat_map(|g| g.members.iter()) {
            list_of.entry(m.clone()).or_insert_with(|| w.id.clone());
        }
        for pane in groups.iter().flat_map(|g| g.checks.iter()).filter_map(|c| c.pane.clone()) {
            out.entry(pane).or_insert_with(|| at_list(&w.id));
        }
    }

    let row = |t: &crate::model::Task| {
        let list = list_of.get(&t.id).cloned();
        let project = t.project.clone().or_else(|| list.as_ref().and_then(|l| home.get(l).cloned().flatten()));
        Place { task: Some(t.id.clone()), list, project, ..Place::default() }
    };
    for t in &tasks {
        for pane in crate::verification::passes(t).into_iter().filter_map(|p| p.pane) {
            out.entry(pane).or_insert_with(|| row(t));
        }
    }
    for t in &tasks {
        for a in attempts_of(t).into_iter().filter(|a| !a.never_took() && !a.pane.is_empty()) {
            if out.contains_key(&a.pane) {
                continue;
            }
            let from = util::epoch_of(&a.at);
            let to = from + a.seconds();
            let checks: i64 = timed_out.get(&t.id).map_or(0, |cs| cs.iter().filter(|(at, _)| *at > from && *at <= to).map(|(_, took)| took).sum());
            let mut p = row(t);
            p.sent_back = a.outcome == Outcome::Reopened;
            p.dropped = matches!(a.outcome, Outcome::Dropped | Outcome::Reaped);
            p.check_share = if a.seconds() > 0 { (checks as f64 / a.seconds() as f64).min(1.0) } else { 0.0 };
            out.insert(a.pane.clone(), p);
        }
    }
    out
}

fn usd(micros: u64) -> f64 {
    (micros as f64 / 10_000.0).round() / 100.0
}

fn usd_f(dollars: f64) -> f64 {
    (dollars * 100.0).round() / 100.0
}

/// Rows with a `holds` decided in `m`.
fn landed_in(tasks: &[crate::model::Task], m: Month) -> usize {
    let (from, to) = (m.start(), m.next().start());
    tasks
        .iter()
        .filter(|t| {
            crate::verification::passes(t).iter().any(|p| {
                p.state == crate::verification::State::Holds
                    && p.decided.as_deref().map(util::epoch_of).is_some_and(|d| d >= from && d < to)
            })
        })
        .count()
}

/// The report for month `m` as of `now`, in the shape compound draws.
pub fn report(store: &Store, m: Month, now: i64) -> Value {
    let lines = ledger(&store.state);
    let places = places(store);
    let tasks = store.tasks();
    let budgets = crate::budget::for_month(&store.state, &m.label());
    // A list still under way, which `wsp worklist park` can stop; a finished
    // one's money is spent and there is nothing to park.
    let live: std::collections::HashSet<String> = store
        .worklists()
        .into_iter()
        .filter(|w| matches!(w.status(), crate::model::WorklistStatus::Running | crate::model::WorklistStatus::Held))
        .map(|w| w.id)
        .collect();
    let (from, to) = (m.start(), m.next().start());
    let days = m.days();
    // The day the month stands at: today in it, all of it once past, none before.
    let day = if now >= to { days } else if now < from { 0 } else { (now - from) / 86_400 + 1 };
    let pace = |spent: f64| if day == 0 { 0.0 } else if day >= days { spent } else { spent / day as f64 * days as f64 };

    let in_month: Vec<&Line> = lines.iter().filter(|l| l.ended >= from && l.ended < to).collect();
    let spent_micros: u64 = in_month.iter().map(|l| l.micros).sum();
    let spent = usd(spent_micros);

    // Daily: the 30 days to the month's day (today, or its last).
    let end_day = if day == 0 { from - 86_400 } else { from + (day - 1) * 86_400 };
    let mut daily = Vec::new();
    for k in (0..30).rev() {
        let d0 = end_day - k * 86_400;
        let mut by: BTreeMap<&str, u64> = ["opus", "sonnet", "haiku", "other"].into_iter().map(|f| (f, 0)).collect();
        for l in lines.iter().filter(|l| l.ended >= d0 && l.ended < d0 + 86_400) {
            *by.entry(family(&l.model)).or_default() += l.micros;
        }
        let total: u64 = by.values().sum();
        daily.push(json!({
            "date": &util::iso_at(d0)[..10],
            "total": usd(total),
            "by_model": by.iter().map(|(k, v)| (k.to_string(), json!(usd(*v)))).collect::<serde_json::Map<_, _>>(),
        }));
    }

    let mut by_project: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_list: BTreeMap<String, (Option<String>, u64, u64)> = BTreeMap::new();
    let mut by_row_rework: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    let (mut rework, mut killed, mut idle, mut other) = (0u64, 0f64, 0u64, 0u64);
    let mut idle_seats: Vec<&Line> = Vec::new();
    for l in &in_month {
        let Some(p) = places.get(&l.seat) else {
            idle += l.micros;
            idle_seats.push(l);
            *by_project.entry("unplaced".into()).or_default() += l.micros;
            continue;
        };
        *by_project.entry(p.project.clone().unwrap_or_else(|| "unplaced".into())).or_default() += l.micros;
        if let Some(list) = &p.list {
            let e = by_list.entry(list.clone()).or_insert((p.project.clone(), 0, 0));
            e.1 += l.micros;
            if p.sent_back {
                e.2 += l.micros;
            }
        }
        if p.sent_back {
            rework += l.micros;
            if let Some(t) = &p.task {
                let e = by_row_rework.entry(t.clone()).or_default();
                e.0 += l.micros;
                e.1 += 1;
            }
        } else if p.dropped {
            other += l.micros;
        } else {
            killed += l.micros as f64 * p.check_share;
        }
    }
    for b in budgets.keys() {
        by_project.entry(b.clone()).or_default();
    }

    let mut act: Vec<(f64, Value)> = Vec::new();
    let severity = |amount: f64| if spent > 0.0 && amount >= spent * 0.25 { "critical" } else { "serious" };
    let projects: Vec<Value> = by_project
        .iter()
        .map(|(id, micros)| {
            let s = usd(*micros);
            let f = usd_f(pace(s));
            let b = budgets.get(id).copied();
            let note = match (id.as_str(), b) {
                ("unplaced", _) => Some("seats the join could not place on a row, list or project".to_string()),
                (_, Some(b)) if s > b => Some(format!("over its budget by ${:.2}", s - b)),
                (_, Some(b)) if f > b => Some(format!("on pace to pass its budget by ${:.2}", f - b)),
                _ => None,
            };
            if let Some(b) = b.filter(|b| f > *b) {
                let list = by_list.iter().filter(|(k, v)| v.0.as_deref() == Some(id) && live.contains(*k)).max_by_key(|(_, v)| v.1).map(|(k, _)| k.clone());
                let action = match &list {
                    Some(l) => format!("wsp worklist park {l} \"over {id}'s budget pace\""),
                    None => format!("wsp budget set {id} {:.0}", f.ceil()),
                };
                act.push((f - b, json!({
                    "severity": if s > b { "critical" } else { severity(f - b) },
                    "title": format!("{id} is on pace for ${f:.2} against a ${b:.2} budget"),
                    "why": format!("${s:.2} spent by day {day} of {days}; the pace carried to month end passes the budget by ${:.2}", f - b),
                    "amount": usd_f(f - b),
                    "action": action,
                    "target": id,
                })));
            }
            json!({ "id": id, "spent": s, "forecast": f, "budget": b, "note": note })
        })
        .collect();
    let mut lists: Vec<(&String, &(Option<String>, u64, u64))> = by_list.iter().collect();
    lists.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    for (slug, (_, total, rw)) in &lists {
        if *rw > 0 && live.contains(*slug) {
            act.push((usd(*rw), json!({
                "severity": if *rw * 2 >= *total { "critical" } else { severity(usd(*rw)) },
                "title": format!("{slug} looped: ${:.2} on attempts sent back", usd(*rw)),
                "why": format!("{:.0}% of the list's ${:.2} went to attempts that reached review and were reopened", *rw as f64 * 100.0 / (*total).max(1) as f64, usd(*total)),
                "amount": usd(*rw),
                "action": format!("wsp worklist park {slug} \"looping: rework is {:.0}% of its spend\"", *rw as f64 * 100.0 / (*total).max(1) as f64),
                "target": slug,
            })));
        }
    }
    let in_list: std::collections::HashSet<&str> = places.values().filter(|p| p.list.is_some()).filter_map(|p| p.task.as_deref()).collect();
    for (row, (rw, n)) in &by_row_rework {
        if *n >= 2 && !in_list.contains(row.as_str()) {
            act.push((usd(*rw), json!({
                "severity": severity(usd(*rw)),
                "title": format!("{row} was sent back {n} times"),
                "why": format!("${:.2} on attempts that reached review and were reopened", usd(*rw)),
                "amount": usd(*rw),
                "action": format!("wsp park {row} \"sent back {n} times; rethink before another attempt\""),
                "target": row,
            })));
        }
    }
    idle_seats.sort_by(|a, b| b.micros.cmp(&a.micros));
    for l in idle_seats.iter().take(2).filter(|l| l.micros > 0) {
        act.push((usd(l.micros), json!({
            "severity": if family(&l.model) == "opus" { "critical" } else { severity(usd(l.micros)) },
            "title": format!("seat {} spent ${:.2} on {} holding nothing", l.seat, usd(l.micros), if l.model.is_empty() { "an unknown model" } else { &l.model }),
            "why": "an ended seat that held no row, leg, verifier pass, barrier check or governorship".to_string(),
            "amount": usd(l.micros),
            "action": "wsp burn".to_string(),
            "target": l.seat,
        })));
    }
    act.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let act_on: Vec<Value> = act.into_iter().filter(|(a, _)| *a > 0.0).take(5).map(|(_, v)| v).collect();

    let rows = landed_in(&tasks, m);
    let prev = m.prev();
    let prev_spent: u64 = lines.iter().filter(|l| l.ended >= prev.start() && l.ended < from).map(|l| l.micros).sum();
    let prev_rows = landed_in(&tasks, prev);
    let per = |micros: u64, rows: usize| (rows > 0).then(|| usd(micros) / rows as f64).map(usd_f);

    let budget: Option<f64> = (!budgets.is_empty()).then(|| usd_f(budgets.values().sum()));
    let waste = usd(rework) + usd_f(killed / 1e6) + usd(idle) + usd(other);
    json!({
        "month": m.label(), "day": day, "days": days, "currency": "USD",
        "spent": spent, "forecast": usd_f(pace(spent)), "budget": budget,
        "daily": daily,
        "projects": projects,
        "lists": lists.iter().map(|(slug, (p, total, _))| json!({ "slug": slug, "project": p, "spent": usd(*total) })).collect::<Vec<_>>(),
        "landed": { "rows": rows, "cost_per_row": per(spent_micros, rows), "prev_cost_per_row": per(prev_spent, prev_rows) },
        "waste": { "total": usd_f(waste), "by_kind": { "rework": usd(rework), "killed_checks": usd_f(killed / 1e6), "idle": usd(idle), "other": usd(other) } },
        "act_on": act_on,
    })
}

pub fn spend(store: &Store, args: &Args) -> i32 {
    let m = match crate::budget::month_arg(args) {
        Ok(Some(m)) => Month::parse(&m).expect("checked"),
        Ok(None) => Month::now(),
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };
    let r = report(store, m, util::epoch_secs());
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
        return 0;
    }
    let p = Paint::new();
    let money = |v: &Value| v.as_f64().map_or("—".to_string(), |d| format!("${d:.2}"));
    println!(
        "{}  {} spent · {} forecast · budget {}   {}",
        p.bold(r["month"].as_str().unwrap_or("")),
        p.bold(&money(&r["spent"])),
        money(&r["forecast"]),
        money(&r["budget"]),
        p.dim(&format!("day {} of {}", r["day"], r["days"])),
    );
    let l = &r["landed"];
    println!(
        "landed {} rows · {} a row (last month {})",
        l["rows"],
        money(&l["cost_per_row"]),
        money(&l["prev_cost_per_row"])
    );
    let w = &r["waste"];
    let k = &w["by_kind"];
    println!(
        "waste {} — rework {} · killed checks {} · idle {} · dropped {}",
        money(&w["total"]),
        money(&k["rework"]),
        money(&k["killed_checks"]),
        money(&k["idle"]),
        money(&k["other"])
    );
    let projects = r["projects"].as_array().cloned().unwrap_or_default();
    if !projects.is_empty() {
        println!();
        println!("{}", p.dim(&format!("{:<24} {:>10} {:>10} {:>10}", "PROJECT", "SPENT", "FORECAST", "BUDGET")));
        for pr in &projects {
            println!(
                "{:<24} {:>10} {:>10} {:>10}  {}",
                pr["id"].as_str().unwrap_or(""),
                money(&pr["spent"]),
                money(&pr["forecast"]),
                money(&pr["budget"]),
                p.dim(pr["note"].as_str().unwrap_or(""))
            );
        }
    }
    let acts = r["act_on"].as_array().cloned().unwrap_or_default();
    if !acts.is_empty() {
        println!();
        println!("{}", p.bold("act on"));
        for a in &acts {
            println!("  {:<8} {:>9}  {}", a["severity"].as_str().unwrap_or(""), money(&a["amount"]), a["title"].as_str().unwrap_or(""));
            println!("  {}", p.dim(&format!("{:<8} {:>9}  → {}", "", "", a["action"].as_str().unwrap_or(""))));
        }
    }
    println!();
    println!("{}", p.dim(&format!("list prices read {} from the stop ledger (ended seats only; wsp burn has the live ones) · forecast is the pace so far carried to month end · --json for compound", crate::place_super::PRICES_READ)));
    0
}

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["spend"],
    run: spend,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Plumbing,
        order: 112,
        text: r##"  wsp spend [--month YYYY-MM] [--json]  the month's spend off the stop
                                    ledger: by day and model, project and list,
                                    cost per landed row (a verifier's holds),
                                    waste (rework sent back, checks timed out,
                                    idle seats, dropped attempts) and what to
                                    act on. Forecast: spent ÷ days elapsed ×
                                    days in the month. Not in v1: teams,
                                    risk and compliance, invoice reconciling"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Status, Task, Worklist, WorklistStatus};

    fn line(state: &Path, seat: &str, ended: &str, model: &str, micros: u64) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(state.join("spent.jsonl")).unwrap();
        writeln!(f, "{}", json!({ "seat": seat, "ended": ended, "session_id": "s", "model": model, "turns": 3, "cost": micros })).unwrap();
    }

    fn fixture(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(tag);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        for (id, log) in [
            ("m-1", "- 2026-11-02T01:00:00Z claimed by pane cpd-1\n- 2026-11-02T01:30:00Z → review\n- 2026-11-02T02:00:00Z claimed by pane cpd-2\n- 2026-11-02T02:30:00Z → review\n"),
            ("m-2", "- 2026-11-03T01:00:00Z claimed by pane cpd-3\n- 2026-11-03T01:30:00Z → review\n"),
        ] {
            let mut t = Task::new(id, id);
            t.project = Some("compounding".into());
            t.status_raw = Status::Review.as_str().into();
            t.body = format!("## Log\n{log}");
            store.save_task(&t).unwrap();
        }
        let mut w = Worklist::new("pass-9", "pass-9");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group { members: vec!["m-1".into(), "m-2".into()], ..Group::default() }]);
        store.save_worklist(&w).unwrap();
        let st = env.state();
        line(&st, "cpd-1", "2026-11-02T01:40:00Z", "claude-opus-5-5", 3_000_000);
        line(&st, "cpd-2", "2026-11-02T02:40:00Z", "claude-opus-5-5", 2_000_000);
        line(&st, "cpd-3", "2026-11-03T01:40:00Z", "claude-sonnet-5-5", 1_000_000);
        line(&st, "cpd-9", "2026-11-04T09:00:00Z", "claude-opus-5-5", 4_000_000);
        line(&st, "cpd-8", "2026-10-30T09:00:00Z", "claude-haiku-4-5", 500_000);
        (env, store)
    }

    const NOV_10: &str = "2026-11-10T12:00:00Z";

    #[test]
    fn a_month_has_every_field_of_the_shape_and_its_money_in_dollars() {
        let (_env, store) = fixture("spend-shape");
        let r = report(&store, Month::parse("2026-11").unwrap(), util::epoch_of(NOV_10));
        for k in ["month", "day", "days", "currency", "spent", "forecast", "budget", "daily", "projects", "lists", "landed", "waste", "act_on"] {
            assert!(r.get(k).is_some(), "{k} missing: {r}");
        }
        assert_eq!((r["month"].as_str(), r["day"].as_i64(), r["days"].as_i64()), (Some("2026-11"), Some(10), Some(30)));
        assert_eq!(r["spent"].as_f64(), Some(10.0), "October's line is not November's");
        assert_eq!(r["forecast"].as_f64(), Some(30.0), "$10 in 10 days carried to 30");
        assert!(r["budget"].is_null());
        let daily = r["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 30);
        assert_eq!(daily[29]["date"], "2026-11-10");
        assert_eq!(daily[0]["date"], "2026-10-12");
        let oct30 = daily.iter().find(|d| d["date"] == "2026-10-30").unwrap();
        assert_eq!(oct30["by_model"]["haiku"].as_f64(), Some(0.5));
        let nov2 = daily.iter().find(|d| d["date"] == "2026-11-02").unwrap();
        assert_eq!((nov2["total"].as_f64(), nov2["by_model"]["opus"].as_f64()), (Some(5.0), Some(5.0)));
        for k in ["opus", "sonnet", "haiku", "other"] {
            assert!(nov2["by_model"][k].is_number(), "{k}");
        }
        for k in ["rows", "cost_per_row", "prev_cost_per_row"] {
            assert!(r["landed"].get(k).is_some(), "landed.{k}");
        }
        assert!(r["landed"]["cost_per_row"].is_null(), "no row landed: null, not left out");
    }

    #[test]
    fn spend_is_placed_by_the_seat_and_waste_is_counted_by_kind() {
        let (_env, store) = fixture("spend-place");
        let r = report(&store, Month::parse("2026-11").unwrap(), util::epoch_of(NOV_10));
        let proj = |id: &str| r["projects"].as_array().unwrap().iter().find(|p| p["id"] == id).cloned();
        assert_eq!(proj("compounding").unwrap()["spent"].as_f64(), Some(6.0), "{r}");
        assert_eq!(proj("unplaced").unwrap()["spent"].as_f64(), Some(4.0));
        assert_eq!(r["lists"][0]["slug"], "pass-9");
        assert_eq!(r["lists"][0]["spent"].as_f64(), Some(6.0));
        let k = &r["waste"]["by_kind"];
        assert_eq!(k["rework"].as_f64(), Some(3.0), "cpd-1 reached review and was claimed again: {r}");
        assert_eq!(k["idle"].as_f64(), Some(4.0), "cpd-9 held nothing");
        assert_eq!(k["other"].as_f64(), Some(0.0), "both later attempts reached review");
        assert_eq!(r["waste"]["total"].as_f64(), Some(7.0));
        let acts = r["act_on"].as_array().unwrap();
        assert_eq!(acts[0]["target"], "cpd-9", "the largest first: {acts:?}");
        assert!(acts.iter().any(|a| a["target"] == "pass-9" && a["action"].as_str().unwrap().starts_with("wsp worklist park pass-9")), "{acts:?}");
        let mut w = store.worklist("pass-9").unwrap();
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        let r = report(&store, Month::parse("2026-11").unwrap(), util::epoch_of(NOV_10));
        assert!(!r["act_on"].as_array().unwrap().iter().any(|a| a["target"] == "pass-9"), "a finished list has nothing to park: {}", r["act_on"]);
    }

    #[test]
    fn a_budget_is_read_into_its_project_and_summed_and_a_pace_past_it_is_acted_on() {
        let (_env, store) = fixture("spend-budget");
        crate::budget::set(&store.state, "compounding", Some(10.0), None).unwrap();
        crate::budget::set(&store.state, "wsp", Some(5.0), Some("2026-11")).unwrap();
        let r = report(&store, Month::parse("2026-11").unwrap(), util::epoch_of(NOV_10));
        assert_eq!(r["budget"].as_f64(), Some(15.0));
        let c = r["projects"].as_array().unwrap().iter().find(|p| p["id"] == "compounding").unwrap().clone();
        assert_eq!((c["forecast"].as_f64(), c["budget"].as_f64()), (Some(18.0), Some(10.0)));
        assert!(c["note"].as_str().unwrap().contains("pace"), "{c}");
        let w = r["projects"].as_array().unwrap().iter().find(|p| p["id"] == "wsp").unwrap().clone();
        assert_eq!((w["spent"].as_f64(), w["note"].is_null()), (Some(0.0), true), "a budget with no spend is still a project");
        assert!(r["act_on"].as_array().unwrap().iter().any(|a| a["target"] == "compounding"), "{}", r["act_on"]);
    }

    #[test]
    fn a_past_month_is_whole_and_its_forecast_is_its_spend() {
        let (_env, store) = fixture("spend-past");
        let r = report(&store, Month::parse("2026-10").unwrap(), util::epoch_of(NOV_10));
        assert_eq!((r["day"].as_i64(), r["days"].as_i64()), (Some(31), Some(31)));
        assert_eq!((r["spent"].as_f64(), r["forecast"].as_f64()), (Some(0.5), Some(0.5)));
        assert_eq!(r["daily"][29]["date"], "2026-10-31");
    }

    #[test]
    fn a_month_is_parsed_strictly() {
        assert_eq!(Month::parse("2026-11").map(Month::days), Some(30));
        assert_eq!(Month::parse("2028-02").map(Month::days), Some(29));
        assert!(Month::parse("2026-13").is_none());
        assert!(Month::parse("2026-1").is_none());
        assert_eq!(Month::parse("2026-01").unwrap().prev().label(), "2025-12");
    }
}
