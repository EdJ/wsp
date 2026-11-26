//! `wsp initiative run <id> [--json]` (`wsp-794`): the feed the building view
//! reads (init-3 C4, wsp side).
//!
//! For an initiative being built: the increment and group running, each list's
//! groups in order with each row's state, the counts, what the rows decided or
//! assumed after the approval, and the spend of the initiative's lists. It
//! reads what wsp already keeps — the worklists and rows, the rows' logs (an
//! open attempt is what the census counts as a seat), `wsp spend`'s report —
//! and writes nothing.
//!
//! **A row is in one state:** `landed` (done), `stuck` (blocked, or its newest
//! check or verifier verdict failed and no seat has claimed it since),
//! `working` (a seat holds it, or it is at doing or review) or `queued` (not
//! begun, or parked). `stuck` carries `since`, the `reason` (the block's
//! question, or the failed check's first failing step, or the verifier's
//! caption) and the retry spend; `working` carries the model and the minutes.
//!
//! **The shape is built from values** ([`feed`]): the spend report comes in as
//! the JSON `wsp spend --json` prints, so a test hands it a fixture and the
//! verb fetches it from this binary as a child (the engine does not call up
//! into the `spend` verb).

use serde_json::{json, Value};

use crate::attempt::{attempts_of, priced, Outcome};
use crate::base::args::Args;
use crate::cmd_initiative::{find, Initiative};
use crate::model::{decisions_of, Status, Task};
use crate::store::Store;
use crate::util;
use crate::verification::{passes, runs, RunState, State};

/// The states a row is in, as the feed names them.
pub const LANDED: &str = "landed";
pub const WORKING: &str = "working";
pub const STUCK: &str = "stuck";
pub const QUEUED: &str = "queued";

fn usd(micros: u64) -> f64 {
    (micros as f64 / 10_000.0).round() / 100.0
}

/// The newest failure on a row: when, and why in one line.
fn last_failure(t: &Task) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    let mut keep = |at: &str, why: String| {
        if best.as_ref().map_or(true, |(b, _)| util::epoch_of(at) >= util::epoch_of(b)) {
            best = Some((at.to_string(), why));
        }
    };
    for r in runs(t).iter().filter(|r| r.state == RunState::Failed) {
        let step = r.text.lines().find(|l| l.trim_start().starts_with('✗')).unwrap_or("").trim_start_matches(|c: char| c == '✗' || c.is_whitespace());
        let step = step.split(" · ").next().unwrap_or("");
        let why = if step.is_empty() { "check failed".to_string() } else { format!("check failed: {step}") };
        keep(&r.at, why);
    }
    for p in passes(t).iter().filter(|p| p.state == State::Blocks) {
        let at = p.decided.as_deref().unwrap_or(&p.at);
        let said = p.caption.clone().unwrap_or_else(|| p.text.lines().next().unwrap_or("").to_string());
        keep(at, format!("verifier blocked: {}", util::truncate(said.trim(), 120)));
    }
    best
}

/// When the row's newest `blocked:` line was written.
fn blocked_since(t: &Task) -> Option<String> {
    t.section("Log")?.lines().rev().find_map(|l| {
        let (stamp, rest) = l.trim().strip_prefix("- ")?.split_once(' ')?;
        (util::is_stamp(stamp) && rest.starts_with("blocked:")).then(|| stamp.to_string())
    })
}

/// One row's node.
fn row_json(store: &Store, id: &str, policy: Option<String>, now: i64) -> Value {
    let Some(t) = store.task(id) else {
        return json!({ "id": id, "title": null, "state": QUEUED, "missing": true });
    };
    let attempts: Vec<_> = attempts_of(&t).into_iter().filter(|a| !a.never_took()).collect();
    let open = attempts.iter().rev().find(|a| a.outcome == Outcome::Open && a.until.is_empty() && a.reviewed.is_empty());
    let failure = last_failure(&t);
    let status = t.status();
    let failed_after_claim = match (&failure, open) {
        (Some((at, _)), Some(a)) => util::epoch_of(at) >= util::epoch_of(&a.at),
        (Some(_), None) => true,
        _ => false,
    };
    let state = if status == Status::Done {
        LANDED
    } else if status == Status::Blocked || (failed_after_claim && matches!(status, Status::Doing | Status::Review | Status::Blocked)) {
        STUCK
    } else if open.is_some() || matches!(status, Status::Doing | Status::Review) {
        WORKING
    } else {
        QUEUED
    };
    let mut row = json!({
        "id": t.id, "title": t.title, "caption": crate::caption::of_task(&t).text,
        "status": status.as_str(), "state": state,
    });
    match state {
        WORKING => {
            let model = open.map(|a| crate::attempt::Row(a.clone()).tier()).filter(|m| !m.is_empty()).or(policy);
            row["model"] = json!(model);
            row["minutes"] = json!(open.map(|a| (now - util::epoch_of(&a.at)).max(0) / 60));
        }
        STUCK => {
            let (since, reason) = if status == Status::Blocked {
                (blocked_since(&t), t.blocked_question().or_else(|| failure.as_ref().map(|f| f.1.clone())))
            } else {
                (failure.as_ref().map(|f| f.0.clone()), failure.as_ref().map(|f| f.1.clone()))
            };
            let micros: u64 = attempts.iter().filter_map(|a| priced(if a.ran.is_empty() { &a.spawned } else { &a.ran }, &a.spend)).sum();
            row["since"] = json!(since.unwrap_or_else(|| t.updated.clone()));
            row["reason"] = json!(reason.unwrap_or_default());
            row["retries"] = json!({ "attempts": attempts.len(), "retries": attempts.len().saturating_sub(1), "cost": usd(micros) });
        }
        _ => {}
    }
    row
}

/// The post-approval notes of one record: dated lines of its `## Decisions`
/// and `## Assumptions`, at or after `since`.
fn raised(body: &str, section: &str, since: i64) -> Vec<(String, String)> {
    let text = crate::model::section_of(body, section).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("- ")?;
            let (stamp, rest) = l.split_once(' ')?;
            (util::is_stamp(stamp) && util::epoch_of(stamp) >= since && !rest.trim().is_empty()).then(|| (stamp.to_string(), rest.trim().to_string()))
        })
        .collect()
}

/// The feed, from the store and the spend report.
pub fn feed(store: &Store, i: &Initiative, spend: &Value, now: i64) -> Value {
    let incs = i.increments();
    // The lists in plan order, then any linked and in no increment.
    let mut order: Vec<(Option<usize>, String)> = Vec::new();
    for (k, inc) in incs.iter().enumerate() {
        for l in &inc.lists {
            order.push((Some(k), l.clone()));
        }
    }
    for l in &i.worklists {
        if !order.iter().any(|(_, o)| o == l) {
            order.push((None, l.clone()));
        }
    }
    let (mut landed, mut working, mut stuck, mut queued) = (0usize, 0usize, 0usize, 0usize);
    let mut running: Option<(Option<usize>, usize, String)> = None;
    let mut row_ids: Vec<String> = Vec::new();
    let mut lists = Vec::new();
    for (inc, id) in &order {
        let Some(w) = store.worklist(id) else {
            lists.push(json!({ "id": id, "increment": inc.map(|k| k + 1), "missing": true, "groups": [] }));
            continue;
        };
        let groups: Vec<Value> = w
            .groups()
            .iter()
            .enumerate()
            .map(|(gi, g)| {
                let rows: Vec<Value> = g.members.iter().map(|m| row_json(store, m, g.policy_for(m).map(|p| p.label()), now)).collect();
                let n = |s: &str| rows.iter().filter(|r| r["state"] == s).count();
                let (l, wk, st, q) = (n(LANDED), n(WORKING), n(STUCK), n(QUEUED));
                landed += l;
                working += wk;
                stuck += st;
                queued += q;
                row_ids.extend(g.members.iter().cloned());
                let done = l == rows.len();
                if !done && running.is_none() {
                    running = Some((inc.map(|k| k + 1), gi + 1, w.id.clone()));
                }
                json!({
                    "id": g.id, "ordinal": gi + 1, "kind": g.kind,
                    "state": if done { LANDED } else if wk + st > 0 || l > 0 { WORKING } else { QUEUED },
                    "rows": rows,
                })
            })
            .collect();
        lists.push(json!({ "id": w.id, "title": w.title, "increment": inc.map(|k| k + 1), "status": w.status().as_str(), "groups": groups }));
    }
    let total = landed + working + stuck + queued;

    // What was decided while building: after the approval, by the rows (and by
    // the initiative itself).
    let approved = decisions_of(&i.body).into_iter().find(|d| d.text.contains("approved for build")).map(|d| util::epoch_of(&d.at));
    let mut decided: Vec<Value> = Vec::new();
    if let Some(since) = approved {
        let mut add = |row: Option<&str>, kind: &str, body: &str, section: &str| {
            for (at, text) in raised(body, section, since).into_iter().filter(|(_, t)| !t.contains("approved for build")) {
                decided.push(json!({ "row": row, "kind": kind, "at": at, "text": text }));
            }
        };
        add(None, "decision", &i.body, "Decisions");
        for id in &row_ids {
            if let Some(t) = store.task(id) {
                add(Some(id), "decision", &t.body, "Decisions");
                add(Some(id), "assumption", &t.body, "Assumptions");
            }
        }
        decided.sort_by(|a, b| a["at"].as_str().cmp(&b["at"].as_str()));
    }

    // Spend: the initiative's lists, off `wsp spend --json`'s `lists`.
    let (mut so_far, mut today) = (0.0f64, 0.0f64);
    for l in spend["lists"].as_array().into_iter().flatten() {
        if order.iter().any(|(_, id)| l["slug"].as_str() == Some(id)) {
            so_far += l["cost"].as_f64().unwrap_or(0.0);
            today += l["today"].as_f64().unwrap_or(0.0);
        }
    }
    let round = |d: f64| (d * 100.0).round() / 100.0;

    let (increment, group_n, list_id) = running.clone().map_or((None, None, None), |(k, g, l)| (k, Some(g), Some(l)));
    let summary = format!("{landed} of {total} landed · {working} working · {stuck} stuck");
    json!({
        "initiative": i.id, "title": i.title, "state": i.state,
        "running": {
            "increment": increment, "increments": incs.len(), "group": group_n, "list": list_id,
            "text": match (&increment, group_n) {
                (Some(k), Some(g)) => format!("increment {k} of {} · group {g}", incs.len()),
                (None, Some(g)) => format!("group {g}"),
                _ => "nothing running".to_string(),
            },
        },
        "lists": lists,
        "counts": { "rows": total, "landed": landed, "working": working, "stuck": stuck, "queued": queued, "text": summary },
        "decided": decided,
        "spend": { "so_far": round(so_far), "today": round(today), "currency": "USD" },
    })
}

/// The feed as the lines `wsp initiative run` prints.
pub fn text(v: &Value) -> String {
    let mut o = String::new();
    o.push_str(&format!("◇ {}  {} [{}]\n", v["initiative"].as_str().unwrap_or(""), v["title"].as_str().unwrap_or(""), v["state"].as_str().unwrap_or("")));
    o.push_str(&format!("{}\n{}\n", v["running"]["text"].as_str().unwrap_or(""), v["counts"]["text"].as_str().unwrap_or("")));
    o.push_str(&format!("spend ${:.2} so far · ${:.2} today\n", v["spend"]["so_far"].as_f64().unwrap_or(0.0), v["spend"]["today"].as_f64().unwrap_or(0.0)));
    for l in v["lists"].as_array().into_iter().flatten() {
        o.push_str(&format!("\n{}  {}", l["id"].as_str().unwrap_or(""), l["title"].as_str().unwrap_or("")));
        if let Some(k) = l["increment"].as_u64() {
            o.push_str(&format!("  (increment {k})"));
        }
        o.push('\n');
        for g in l["groups"].as_array().into_iter().flatten() {
            o.push_str(&format!("  group {}  {}\n", g["ordinal"], g["state"].as_str().unwrap_or("")));
            for r in g["rows"].as_array().into_iter().flatten() {
                let state = r["state"].as_str().unwrap_or("");
                let mark = match state {
                    LANDED => "✓",
                    WORKING => "●",
                    STUCK => "✗",
                    _ => "○",
                };
                let mut line = format!("    {mark} {}  {}  {state}", r["id"].as_str().unwrap_or(""), util::truncate(r["caption"].as_str().unwrap_or(""), 48));
                match state {
                    WORKING => {
                        if let Some(m) = r["model"].as_str() {
                            line.push_str(&format!(" · {m}"));
                        }
                        if let Some(n) = r["minutes"].as_i64() {
                            line.push_str(&format!(" · {n}m"));
                        }
                    }
                    STUCK => line.push_str(&format!(
                        " since {} — {} · {} retries ${:.2}",
                        r["since"].as_str().unwrap_or(""),
                        r["reason"].as_str().unwrap_or(""),
                        r["retries"]["retries"],
                        r["retries"]["cost"].as_f64().unwrap_or(0.0)
                    )),
                    _ => {}
                }
                o.push_str(&line);
                o.push('\n');
            }
        }
    }
    let decided = v["decided"].as_array().cloned().unwrap_or_default();
    if !decided.is_empty() {
        o.push_str("\ndecided while building\n");
        for d in &decided {
            o.push_str(&format!("  {} {}  {}\n", d["row"].as_str().unwrap_or("initiative"), d["kind"].as_str().unwrap_or(""), d["text"].as_str().unwrap_or("")));
        }
    }
    o
}

/// `wsp spend --json` as this binary prints it, or nothing where it cannot be
/// had: the feed is still the feed, with its spend at zero.
fn spend_report() -> Value {
    // A test's binary is the harness: running it would run the suite again.
    if cfg!(test) {
        return Value::Null;
    }
    let Ok(exe) = wsp_base::util::self_exe() else { return Value::Null };
    std::process::Command::new(exe)
        .args(["spend", "--json"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or(Value::Null)
}

/// `wsp initiative run <id> [--json]`.
pub fn run(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp initiative run <id> [--json]";
    let Some(needle) = args.rest.get(1) else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let Some(i) = find(store, needle) else {
        eprintln!("wsp: no initiative `{needle}`");
        return 1;
    };
    if !matches!(i.state.as_str(), "building" | "closed") {
        eprintln!("wsp: {} is {} — only an initiative approved for build has a run", i.id, i.state);
        return 1;
    }
    let v = feed(store, &i, &spend_report(), util::epoch_secs());
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        print!("{}", text(&v));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Project, Worklist};

    const NOW: &str = "2026-11-20T12:00:00Z";

    fn row(store: &Store, id: &str, status: Status, log: &str, extra: &str) {
        let mut t = Task::new(id, id);
        t.status_raw = status.as_str().into();
        t.body = format!("## Log\n{log}{extra}");
        store.save_task(&t).unwrap();
    }

    /// An initiative in two increments: list `pa` (group 1 landed, group 2 with
    /// a row working and a row stuck at a failed check), `pb` not begun.
    fn fixture(tag: &str) -> (Store, Initiative) {
        let store = crate::testkit::store(tag);
        let mut prod = Project::new("compound");
        prod.kind = "product".into();
        store.save_project(&prod).unwrap();
        let run = |rest: &[&str], flags: &[(&str, &str)]| crate::cmd_initiative::initiative(&store, &Args::synth("initiative", rest, flags));
        assert_eq!(run(&["add", "Build"], &[("product", "compound")]), 0);

        row(&store, "r-1", Status::Done, "- 2026-11-20T08:00:00Z claimed by pane cpd-1\n- 2026-11-20T08:30:00Z → done\n", "");
        row(&store, "r-2", Status::Done, "- 2026-11-20T08:00:00Z claimed by pane cpd-2\n- 2026-11-20T08:40:00Z → done\n", "");
        row(
            &store,
            "r-3",
            Status::Doing,
            "- 2026-11-20T11:48:00Z claimed by pane cpd-3 · spawned at sonnet/high\n",
            "\n## Decisions\n- 2026-11-20T11:50:00Z d1 use the shared reader\n- 2026-11-19T11:50:00Z d0 before the approval\n\n## Assumptions\n- 2026-11-20T11:51:00Z the feed is read every minute\n",
        );
        row(
            &store,
            "r-4",
            Status::Review,
            "- 2026-11-20T09:00:00Z claimed by pane cpd-4 · spawned at opus/high\n- 2026-11-20T09:30:00Z → review\n- 2026-11-20T09:31:00Z claimed by pane cpd-5 · spawned at opus/high\n- 2026-11-20T09:50:00Z → review\n",
            "\n## Verification\n- 2026-11-20T10:00:00Z checked failed · read 55c4839abcde · 141s\n  ✓ build · cargo build · 41s\n  ✗ tests · cargo test · 100s · exit 101\n",
        );
        row(&store, "r-5", Status::Todo, "", "");
        let g = |m: &[&str]| Group { members: m.iter().map(|s| s.to_string()).collect(), ..Group::default() };
        let mut a = Worklist::new("pa", "List A");
        a.set_groups(&[g(&["r-1", "r-2"]), g(&["r-3", "r-4"])]);
        store.save_worklist(&a).unwrap();
        let mut b = Worklist::new("pb", "List B");
        b.set_groups(&[g(&["r-5"])]);
        store.save_worklist(&b).unwrap();
        assert_eq!(run(&["increment", "add", "init-1", "First"], &[("list", "pa")]), 0);
        assert_eq!(run(&["increment", "add", "init-1", "Second"], &[("list", "pb")]), 0);
        let mut i = find(&store, "init-1").unwrap();
        crate::model::append_decision(&mut i.body, "approved for build: go", &[]);
        let appr = decisions_of(&i.body).len();
        assert_eq!(appr, 1);
        i.body = i.body.replace(&decisions_of(&i.body)[0].at, "2026-11-20T00:00:00Z");
        i.state = "building".into();
        crate::cmd_initiative::save(&store, &i).unwrap();
        let i = find(&store, "init-1").unwrap();
        (store, i)
    }

    fn spend() -> Value {
        json!({ "lists": [
            { "slug": "pa", "cost": 4.5, "today": 1.25 },
            { "slug": "pb", "cost": 0.5, "today": 0.0 },
            { "slug": "other", "cost": 99.0, "today": 9.0 },
        ] })
    }

    #[test]
    fn a_landed_group_a_working_row_and_a_stuck_row_are_each_in_the_feed() {
        let (store, i) = fixture("initiative-run");
        let v = feed(&store, &i, &spend(), util::epoch_of(NOW));
        let rows = &v["lists"][0]["groups"];
        assert_eq!(rows[0]["state"], json!("landed"), "{v}");
        assert!(rows[0]["rows"].as_array().unwrap().iter().all(|r| r["state"] == json!("landed")));
        let working = &rows[1]["rows"][0];
        assert_eq!((working["id"].as_str(), working["state"].as_str()), (Some("r-3"), Some("working")), "{v}");
        assert_eq!(working["model"], json!("sonnet/high"));
        assert_eq!(working["minutes"], json!(12));
        let stuck = &rows[1]["rows"][1];
        assert_eq!((stuck["id"].as_str(), stuck["state"].as_str()), (Some("r-4"), Some("stuck")), "{v}");
        assert_eq!(stuck["since"], json!("2026-11-20T10:00:00Z"));
        assert!(stuck["reason"].as_str().unwrap().contains("tests"), "{stuck}");
        assert_eq!(stuck["retries"]["attempts"], json!(2));
        assert_eq!(stuck["retries"]["retries"], json!(1));
        assert_eq!(v["lists"][1]["groups"][0]["rows"][0]["state"], json!("queued"));
    }

    #[test]
    fn the_counts_the_running_group_the_decisions_and_the_spend_are_summed() {
        let (store, i) = fixture("initiative-run-counts");
        let v = feed(&store, &i, &spend(), util::epoch_of(NOW));
        assert_eq!(v["counts"]["text"], json!("2 of 5 landed · 1 working · 1 stuck"), "{v}");
        assert_eq!(v["running"]["text"], json!("increment 1 of 2 · group 2"), "{v}");
        assert_eq!(v["spend"], json!({ "so_far": 5.0, "today": 1.25, "currency": "USD" }), "only the initiative's lists: {v}");
        let decided = v["decided"].as_array().unwrap();
        let seen: Vec<(&str, &str)> = decided.iter().map(|d| (d["row"].as_str().unwrap(), d["kind"].as_str().unwrap())).collect();
        assert_eq!(seen, vec![("r-3", "decision"), ("r-3", "assumption")], "after the approval only: {v}");
        let t = text(&v);
        for want in ["2 of 5 landed · 1 working · 1 stuck", "increment 1 of 2 · group 2", "✗ r-4", "decided while building"] {
            assert!(t.contains(want), "{want} missing:\n{t}");
        }
    }

    #[test]
    fn the_verb_prints_json_and_refuses_what_is_not_being_built() {
        let (store, _i) = fixture("initiative-run-verb");
        let go = |rest: &[&str], flags: &[(&str, &str)]| crate::cmd_initiative::initiative(&store, &Args::synth("initiative", rest, flags));
        assert_eq!(go(&["run", "init-1"], &[]), 0);
        assert_eq!(go(&["run", "init-1"], &[("json", "true")]), 0);
        assert_eq!(go(&["run"], &[]), 2);
        assert_eq!(go(&["run", "init-9"], &[]), 1);
        assert_eq!(go(&["set", "init-1", "state=planned"], &[]), 0);
        assert_eq!(go(&["run", "init-1"], &[]), 1, "a planned initiative has no run");
    }
}
