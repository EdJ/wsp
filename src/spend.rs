//! `wsp spend [--json]` — token usage and cost over the last 30 days, for
//! compound's governance view (wsp-661; compounding-744 draws it, -745 joins
//! it).
//!
//! **The figures are the stop ledger** (`<state>/spent.jsonl`): one line per
//! ended seat, its tokens and its `cost` in micro-dollars at list prices
//! ([`crate::cmd_burn`]), dated by `ended`. A session is one ledger line; a seat
//! still up is in no day yet (`wsp burn` has it).
//!
//! **Each line is placed by the seat that spent it**, by the join
//! `wsp attempts --cost` and `wsp worklist cost` make ([`crate::cmd_cost`]): a
//! list's governor and barrier-check seats, a row's verifier passes, then a
//! row's attempts (claims and legs), first one wins. A row is in the first list
//! that holds it; its project is its own, else its list's home. A seat none of
//! those name is `unplaced`. Only rows written since the window opened are
//! read, which is what keeps this under a second on a store of thousands.
//!
//! **A landed row** is one with a verifier's `holds` decided in the 30 days;
//! the cost per landed row is the 30 days' cost over them.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{json, Value};

use crate::cmd_attempts::attempts_of;
use crate::store::Store;
pub use wsp_view::spend::{Line, Place};
use crate::util::{self, Paint};
use crate::base::args::Args;

const DAY: i64 = 86_400;
const WINDOW: i64 = 30;

pub fn ledger(state: &Path) -> Vec<Line> {
    let text = std::fs::read_to_string(state.join(crate::place_super::SPENT_FILE)).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let n = |k: &str| v[k].as_u64().unwrap_or(0);
            Some(Line {
                seat: v["seat"].as_str()?.to_string(),
                ended: util::epoch_of(v["ended"].as_str()?),
                model: v["model"].as_str().unwrap_or("").to_string(),
                micros: n("cost"),
                input: n("input"),
                output: n("output"),
                cache_read: n("cache_read"),
                cache_write: n("cache_write"),
                turns: n("turns"),
            })
        })
        .filter(|l| l.ended > 0)
        .collect()
}

/// Every seat the store can place, by seat id (the order is in the module
/// docs), and the rows with a `holds` decided at or after `since`.
fn places(store: &Store, since: i64) -> (HashMap<String, Place>, usize) {
    let heads = store.task_heads();
    let lists = store.worklists();
    let index = crate::resolve::Index::new(store.projects());
    let project_of: HashMap<&str, &Option<String>> = heads.iter().map(|h| (h.id.as_str(), &h.project)).collect();
    let home: HashMap<String, Option<String>> = lists
        .iter()
        .map(|w| (w.id.clone(), crate::worklist::home_with(w, &index, |id| project_of.get(id).map(|p| (*p).clone())).map(|h| h.project)))
        .collect();
    let at_list = |id: &str| Place { list: Some(id.to_string()), project: home.get(id).cloned().flatten() };
    let mut out: HashMap<String, Place> = HashMap::new();

    // A governor's seat: a list's when the scope is a list (the seats
    // `cmd_cost::governors` counts for it, which reads the log once a list),
    // else its project's own. Only the lines that can be one are parsed.
    let events = std::fs::read_to_string(store.state_file("events.jsonl")).unwrap_or_default();
    for e in events.lines().filter(|l| l.contains("\"governor-set\"")).filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let (Some(p), Some(ws)) = (e["data"]["project"].as_str(), e["data"]["workspace"].as_str()) else { continue };
        let place = if home.contains_key(p) { at_list(p) } else { Place { list: None, project: Some(p.to_string()) } };
        out.entry(ws.to_string()).or_insert(place);
    }
    let mut list_of: HashMap<String, String> = HashMap::new();
    for w in &lists {
        let groups = w.groups();
        for m in groups.iter().flat_map(|g| g.members.iter()) {
            list_of.entry(m.clone()).or_insert_with(|| w.id.clone());
        }
        for pane in groups.iter().flat_map(|g| g.checks.iter()).filter_map(|c| c.pane.clone()) {
            out.entry(pane).or_insert_with(|| at_list(&w.id));
        }
    }

    // Rows written since the window opened: a seat that ended in it wrote its
    // row when it did (a release line, a pass's spend). A day of slack for a
    // clock that is not the ledger's.
    let tasks: Vec<crate::model::Task> =
        heads.iter().filter(|h| util::epoch_of(&h.updated) >= since - DAY).filter_map(|h| store.task(&h.id)).collect();
    let row = |t: &crate::model::Task| {
        let list = list_of.get(&t.id).cloned();
        let project = t.project.clone().or_else(|| list.as_ref().and_then(|l| home.get(l).cloned().flatten()));
        Place { list, project }
    };
    let mut landed = 0;
    for t in &tasks {
        let passes = crate::verification::passes(t);
        if passes.iter().any(|p| {
            p.state == crate::verification::State::Holds && p.decided.as_deref().is_some_and(|d| util::epoch_of(d) >= since)
        }) {
            landed += 1;
        }
        for pane in passes.into_iter().filter_map(|p| p.pane) {
            out.entry(pane).or_insert_with(|| row(t));
        }
    }
    for t in &tasks {
        for a in attempts_of(t).into_iter().filter(|a| !a.never_took() && !a.pane.is_empty()) {
            out.entry(a.pane).or_insert_with(|| row(t));
        }
    }
    (out, landed)
}

/// The report as of `now`, in the shape compound draws.
pub fn report(store: &Store, now: i64) -> Value {
    let today = now - now.rem_euclid(DAY);
    let since = today - (WINDOW - 1) * DAY;
    let lines: Vec<Line> = ledger(&store.state).into_iter().filter(|l| l.ended >= since && l.ended < today + DAY).collect();
    let (places, landed) = places(store, since);
    wsp_view::spend::report(&lines, &places, landed, now)
}

pub fn spend(store: &Store, args: &Args) -> i32 {
    let r = report(store, util::epoch_secs());
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
        return 0;
    }
    let p = Paint::new();
    let money = |v: &Value| v.as_f64().map_or("—".to_string(), |d| format!("${d:.2}"));
    let t = &r["totals"];
    for (label, k) in [("today", "today"), ("7 days", "7d"), ("30 days", "30d")] {
        let s = &t[k];
        println!(
            "{:<8} {:>10}  {} sessions · {} turns · {} out · {} cache read",
            label,
            money(&s["cost"]),
            s["sessions"],
            s["turns"],
            util::count_human(s["output"].as_u64().unwrap_or(0)),
            util::count_human(s["cache_read"].as_u64().unwrap_or(0)),
        );
    }
    let rows = &r["rows"];
    println!("landed   {} rows in 30 days · {} a row", rows["landed_30d"], money(&rows["cost_per_landed_row"]));
    let projects = r["projects"].as_array().cloned().unwrap_or_default();
    if !projects.is_empty() {
        println!();
        println!("{}", p.dim(&format!("{:<24} {:>10} {:>9}", "PROJECT (30d)", "COST", "SESSIONS")));
        for pr in &projects {
            println!("{:<24} {:>10} {:>9}", pr["id"].as_str().unwrap_or(""), money(&pr["cost"]), pr["sessions"]);
        }
    }
    println!();
    println!("{}", p.dim(&format!("list prices read {}, from the stop ledger — ended seats only; wsp burn has the live ones · --json by day and model", crate::place_super::PRICES_READ)));
    0
}

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["spend"],
    run: spend,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Plumbing,
        order: 112,
        text: r##"  wsp spend [--json]                tokens and cost over 30 days off the stop
                                    ledger: by day and model, today/7d/30d,
                                    by project and list, and the cost per row
                                    landed (a verifier's holds) in those days"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Status, Task, Worklist};

    fn line(state: &Path, seat: &str, ended: &str, model: &str, micros: u64, output: u64) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(state.join("spent.jsonl")).unwrap();
        let rec = json!({ "seat": seat, "ended": ended, "session_id": "s", "model": model, "turns": 3, "input": 10, "output": output, "cache_read": 1000, "cache_write": 5, "cost": micros });
        writeln!(f, "{rec}").unwrap();
    }

    /// A recorded ledger over a store of two rows in one list, one of them
    /// landed, and one seat that held nothing.
    fn fixture(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(tag);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        for (id, log) in [
            ("m-1", "- 2026-11-02T01:00:00Z claimed by pane cpd-1\n- 2026-11-02T01:30:00Z → review\n- 2026-11-02T02:00:00Z claimed by pane cpd-2\n- 2026-11-02T02:30:00Z → review\n"),
            ("m-2", "- 2026-11-16T01:00:00Z claimed by pane cpd-3\n- 2026-11-16T01:30:00Z → review\n"),
        ] {
            let mut t = Task::new(id, id);
            t.project = Some("compounding".into());
            t.status_raw = Status::Review.as_str().into();
            t.body = format!("## Log\n{log}");
            if id == "m-1" {
                let mut p = crate::verification::Pass::opened(None, None);
                p.pane = Some("cpd-4".into());
                p.state = crate::verification::State::Holds;
                p.decided = Some("2026-11-03T00:00:00Z".into());
                crate::verification::write(&mut t, &[p]);
            }
            store.save_task(&t).unwrap();
        }
        let mut w = Worklist::new("pass-9", "pass-9");
        w.set_groups(&[Group { members: vec!["m-1".into(), "m-2".into()], ..Group::default() }]);
        store.save_worklist(&w).unwrap();
        let st = env.state();
        line(&st, "cpd-1", "2026-11-02T01:40:00Z", "claude-opus-5-5", 3_000_000, 100);
        line(&st, "cpd-2", "2026-11-02T02:40:00Z", "claude-opus-5-5", 2_000_000, 200);
        line(&st, "cpd-4", "2026-11-03T00:10:00Z", "claude-sonnet-5-5", 500_000, 50);
        line(&st, "cpd-3", "2026-11-16T01:40:00Z", "claude-sonnet-5-5", 1_000_000, 300);
        line(&st, "cpd-9", "2026-11-17T09:00:00Z", "claude-haiku-4-5", 250_000, 7);
        line(&st, "cpd-8", "2026-10-10T09:00:00Z", "claude-opus-5-5", 9_000_000, 1);
        (env, store)
    }

    const NOW: &str = "2026-11-17T12:00:00Z";

    #[test]
    fn the_report_has_every_field_of_the_shape() {
        let (_env, store) = fixture("spend-shape");
        let r = report(&store, util::epoch_of(NOW));
        for k in ["today", "currency", "daily", "totals", "projects", "lists", "rows"] {
            assert!(r.get(k).is_some(), "{k} missing: {r}");
        }
        assert_eq!((r["today"].as_str(), r["currency"].as_str()), (Some("2026-11-17"), Some("USD")));
        let daily = r["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 30);
        assert_eq!((daily[0]["date"].as_str(), daily[29]["date"].as_str()), (Some("2026-10-19"), Some("2026-11-17")));
        for m in ["opus", "sonnet", "haiku", "other"] {
            for k in ["cost", "input", "output", "cache_read", "cache_write", "sessions", "turns"] {
                assert!(daily[0]["by_model"][m][k].is_number(), "daily.by_model.{m}.{k}");
            }
        }
        for t in ["today", "7d", "30d"] {
            assert!(r["totals"][t]["cost"].is_number(), "totals.{t}");
        }
        for k in ["id", "cost", "output", "sessions"] {
            assert!(r["projects"][0].get(k).is_some(), "projects[].{k}");
        }
        for k in ["slug", "project", "cost", "output", "sessions"] {
            assert!(r["lists"][0].get(k).is_some(), "lists[].{k}");
        }
    }

    #[test]
    fn a_recorded_ledger_sums_by_day_model_window_project_and_list() {
        let (_env, store) = fixture("spend-sums");
        let r = report(&store, util::epoch_of(NOW));
        let day = |d: &str| r["daily"].as_array().unwrap().iter().find(|x| x["date"] == d).unwrap().clone();
        let nov2 = day("2026-11-02");
        assert_eq!(nov2["by_model"]["opus"], json!({ "cost": 5.0, "input": 20, "output": 300, "cache_read": 2000, "cache_write": 10, "sessions": 2, "turns": 6 }));
        assert_eq!(nov2["by_model"]["sonnet"]["sessions"], 0);
        assert_eq!(day("2026-11-17")["by_model"]["haiku"]["cost"].as_f64(), Some(0.25));
        let t = &r["totals"];
        assert_eq!(t["today"]["cost"].as_f64(), Some(0.25));
        assert_eq!(t["7d"]["cost"].as_f64(), Some(1.25), "the 16th and the 17th");
        assert_eq!((t["30d"]["cost"].as_f64(), t["30d"]["sessions"].as_u64()), (Some(6.75), Some(5)), "October 10th is outside: {t}");
        assert_eq!(r["projects"][0], json!({ "id": "compounding", "cost": 6.5, "output": 650, "sessions": 4 }), "the verifier's seat counts to its row");
        assert_eq!(r["projects"][1]["id"], "unplaced");
        assert_eq!(r["lists"][0], json!({ "slug": "pass-9", "project": "compounding", "cost": 6.5, "output": 650, "sessions": 4 }));
        assert_eq!(r["rows"], json!({ "landed_30d": 1, "cost_per_landed_row": 6.75 }));
    }

    #[test]
    fn nothing_landed_is_a_null_cost_per_row_and_an_empty_ledger_is_zeros() {
        let env = util::isolated("spend-empty");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let r = report(&store, util::epoch_of(NOW));
        assert_eq!(r["rows"], json!({ "landed_30d": 0, "cost_per_landed_row": null }));
        assert_eq!(r["totals"]["30d"]["sessions"], 0);
        assert_eq!(r["daily"].as_array().unwrap().len(), 30);
        assert!(r["projects"].as_array().unwrap().is_empty());
    }
}
