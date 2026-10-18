//! `wsp worklist cost <slug> [--json]`: what a worklist spent, by group and member.
//!
//! Every seat that worked for the list is counted once, in this order of
//! precedence: governor, barrier check, verifier, member attempt. A seat's
//! figures are its `cmd_burn::reading` (live burn record, else the ledger
//! `stop()` appends to), kept apart as input, output, cache read and cache write
//! and never summed into one number. A seat with no reading falls back to the
//! rounded clause its log line or pass carries (`exact: false`), and with
//! neither it is *unmeasured*, which is not zero.
//!
//! **A governor counts in full toward the group the run stood at when it was
//! seated** (wsp-264): one plus the highest `passed` of the list's earlier
//! `worklist-go` events. A pass short of the end rotates in a fresh seat, so
//! each seat governs one group; burn is whole-session and cannot be split.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::cmd_attempts::{attempts_of, priced, Row};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// What one seat spent. `None` is unmeasured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fig {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub usd: Option<u64>,
    pub turns: usize,
    pub seconds: i64,
    pub exact: bool,
    pub measured: bool,
}

/// One seat in the tree.
#[derive(Clone, Debug)]
pub struct Entry {
    pub group: usize,
    pub member: Option<String>,
    /// `worker`, `rework`, `leg`, `verifier`, `barrier` or `governor`.
    pub role: &'static str,
    pub label: String,
    pub seat: String,
    pub tier: String,
    pub dropped: bool,
    /// A verifier pass whose seat is the list's governor, counted there.
    pub by_governor: bool,
    /// A pane that served an earlier attempt too: counted at the first.
    pub shared: bool,
    pub through: Option<usize>,
    pub fig: Fig,
}

fn of_reading(r: &Value, turns_hint: usize, seconds: i64) -> Fig {
    let n = |k: &str| r.get(k).and_then(Value::as_u64);
    Fig {
        input: n("input"),
        output: n("output"),
        cache_read: n("cache_read"),
        cache_write: n("cache_write"),
        usd: n("cost"),
        turns: n("turns").map_or(turns_hint, |t| t as usize),
        seconds,
        exact: true,
        measured: true,
    }
}

fn of_clause(ran: &str, s: &crate::agent_commands::Spend, turns: usize, seconds: i64) -> Fig {
    if s.nothing() {
        return Fig { turns, seconds, ..Fig::default() };
    }
    Fig {
        input: Some(s.input),
        output: Some(s.output),
        cache_read: Some(s.cache),
        cache_write: None,
        usd: priced(ran, s),
        turns,
        seconds,
        exact: false,
        measured: true,
    }
}

/// The figures for a seat: its reading, else the clause, else unmeasured.
fn fig_of(seat: &str, clause: Option<(&str, &crate::agent_commands::Spend, usize)>, seconds: i64) -> Fig {
    if let Some(r) = crate::cmd_burn::reading(seat) {
        return of_reading(&r, clause.map_or(0, |c| c.2), seconds);
    }
    match clause {
        Some((ran, s, turns)) => of_clause(ran, s, turns, seconds),
        None => Fig { seconds, ..Fig::default() },
    }
}

/// The list's governor seats, each with the group it counts toward: from the
/// event log in file order, a `worklist-go` raising the run's pass and a
/// `governor-set` for the slug seating one. A workspace set twice counts at its
/// first group; `through` is set where it sat through a pass beyond its own group's, since
/// the pass that rotates it out is the one it was seated for.
fn governors(store: &Store, slug: &str, groups: usize) -> Vec<(String, usize, Option<usize>, i64)> {
    let text = std::fs::read_to_string(store.state_file("events.jsonl")).unwrap_or_default();
    let mut passed = 0usize;
    // seat, first group, group at the end of its tenure, set at, ended at
    let mut seats: Vec<(String, usize, usize, i64, i64)> = Vec::new();
    let mut current: Option<usize> = None;
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let kind = v["kind"].as_str().unwrap_or("");
        let d = &v["data"];
        let ts = util::epoch_of(v["ts"].as_str().unwrap_or(""));
        match kind {
            "worklist-go" if d["id"] == slug => {
                if let Some(p) = d["passed"].as_u64() {
                    passed = passed.max(p as usize);
                }
                if let Some(i) = current {
                    seats[i].2 = passed;
                }
            }
            "governor-set" | "governor-vacated" if d["project"] == slug => {
                if let Some(i) = current.take() {
                    seats[i].4 = ts;
                }
                if kind == "governor-set" {
                    let ws = d["workspace"].as_str().unwrap_or("").to_string();
                    let g = (passed + 1).min(groups.max(1));
                    match seats.iter().position(|s| s.0 == ws) {
                        Some(i) => current = Some(i),
                        None => {
                            seats.push((ws, g, g, ts, 0));
                            current = Some(seats.len() - 1);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let now = util::epoch_secs();
    seats
        .into_iter()
        .map(|(s, g, end, at, until)| (s, g, (end > g).then_some(end), ((if until == 0 { now } else { until }) - at).max(0)))
        .collect()
}

/// The whole tree for one list, governor and barrier entries first because
/// they take a seat before a verifier or member can.
pub fn entries(store: &Store, slug: &str) -> Option<Vec<Entry>> {
    let w = store.worklist(slug)?;
    let groups = w.groups();
    let mut out: Vec<Entry> = Vec::new();
    let mut counted: BTreeSet<String> = BTreeSet::new();
    let entry = |group: usize, member: Option<&str>, role: &'static str, seat: &str| Entry {
        group,
        member: member.map(str::to_string),
        role,
        label: String::new(),
        seat: seat.to_string(),
        tier: String::new(),
        dropped: false,
        by_governor: false,
        shared: false,
        through: None,
        fig: Fig::default(),
    };
    let mut gov_seats: BTreeSet<String> = BTreeSet::new();
    for (seat, g, through, seconds) in governors(store, slug, groups.len()) {
        counted.insert(seat.clone());
        gov_seats.insert(seat.clone());
        let mut e = entry(g, None, "governor", &seat);
        e.through = through;
        e.fig = fig_of(&seat, None, seconds);
        out.push(e);
    }
    for (i, g) in groups.iter().enumerate() {
        for c in &g.checks {
            let Some(pane) = c.pane.as_deref() else { continue };
            let mut e = entry(i + 1, None, "barrier", pane);
            e.tier = c.on.clone().unwrap_or_default();
            e.shared = !counted.insert(pane.to_string());
            if !e.shared {
                e.fig = fig_of(pane, None, 0);
            }
            out.push(e);
        }
    }
    let tasks: Vec<(usize, crate::model::Task)> = groups
        .iter()
        .enumerate()
        .flat_map(|(i, g)| g.members.iter().filter_map(|m| store.find_task(m)).map(move |t| (i + 1, t)))
        .collect();
    for (gi, t) in &tasks {
        for p in crate::verification::passes(t) {
            let Some(pane) = p.pane.as_deref() else { continue };
            let seconds = p.decided.as_deref().map_or(0, |d| (util::epoch_of(d) - util::epoch_of(&p.at)).max(0));
            let mut e = entry(*gi, Some(&t.id), "verifier", pane);
            e.tier = p.spent.as_ref().map_or_else(|| p.on.clone().unwrap_or_default(), |s| s.ran.clone());
            if gov_seats.contains(pane) {
                e.by_governor = true;
            } else if !counted.insert(pane.to_string()) {
                e.shared = true;
            } else {
                let clause = p.spent.as_ref().map(|s| (s.ran.as_str(), &s.spend, s.turns));
                e.fig = fig_of(pane, clause, seconds);
            }
            out.push(e);
        }
    }
    for (gi, t) in &tasks {
        let mut claims = 0;
        for a in attempts_of(t) {
            let role = match &a.leg {
                Some(_) => "leg",
                None => {
                    claims += 1;
                    if claims == 1 { "worker" } else { "rework" }
                }
            };
            let mut e = entry(*gi, Some(&t.id), role, &a.pane);
            e.label = a.leg.clone().unwrap_or_default();
            e.tier = Row(a.clone()).tier();
            e.dropped = a.outcome.as_str() == "dropped";
            if a.pane.is_empty() {
                e.fig = of_clause(&a.ran, &a.spend, a.turns, a.seconds());
            } else if counted.insert(a.pane.clone()) {
                e.fig = fig_of(&a.pane, Some((&a.ran, &a.spend, a.turns)), a.seconds());
            } else {
                e.shared = true;
            }
            out.push(e);
        }
    }
    out.sort_by_key(|e| e.group);
    Some(out)
}

#[derive(Default)]
struct Sum {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    usd: u64,
    turns: usize,
    seconds: i64,
    seats: usize,
    unmeasured: usize,
    inexact: usize,
}

impl Sum {
    fn of<'a>(es: impl Iterator<Item = &'a Entry>) -> Sum {
        let mut s = Sum::default();
        for e in es.filter(|e| !e.shared && !e.by_governor) {
            s.seats += 1;
            s.turns += e.fig.turns;
            s.seconds += e.fig.seconds;
            if !e.fig.measured {
                s.unmeasured += 1;
                continue;
            }
            if !e.fig.exact {
                s.inexact += 1;
            }
            s.input += e.fig.input.unwrap_or(0);
            s.output += e.fig.output.unwrap_or(0);
            s.cache_read += e.fig.cache_read.unwrap_or(0);
            s.cache_write += e.fig.cache_write.unwrap_or(0);
            s.usd += e.fig.usd.unwrap_or(0);
        }
        s
    }
    fn json(&self) -> Value {
        json!({
            "input_tokens": self.input, "output_tokens": self.output,
            "cache_tokens": self.cache_read, "cache_write_tokens": self.cache_write,
            "usd_micros": self.usd, "turns": self.turns, "seconds": self.seconds,
            "seats": self.seats, "unmeasured": self.unmeasured, "inexact": self.inexact,
        })
    }
}

fn entry_json(e: &Entry) -> Value {
    let f = &e.fig;
    json!({
        "role": e.role, "member": e.member, "leg": (!e.label.is_empty()).then(|| e.label.clone()),
        "seat": e.seat, "tier": e.tier, "dropped": e.dropped, "by_governor": e.by_governor,
        "shared": e.shared, "through": e.through,
        "measured": f.measured, "exact": f.exact,
        "input_tokens": f.input, "output_tokens": f.output,
        "cache_tokens": f.cache_read, "cache_write_tokens": f.cache_write,
        "usd_micros": f.usd, "turns": f.turns, "seconds": f.seconds,
    })
}

pub fn cost_json(slug: &str, es: &[Entry], groups: usize) -> Value {
    json!({
        "list": slug,
        "groups": (1..=groups).map(|g| {
            let of: Vec<&Entry> = es.iter().filter(|e| e.group == g).collect();
            json!({
                "group": g,
                "members": member_ids(&of).iter().map(|m| json!({
                    "task": m,
                    "seats": of.iter().filter(|e| e.member.as_deref() == Some(m)).map(|e| entry_json(e)).collect::<Vec<_>>(),
                    "total": Sum::of(of.iter().copied().filter(|e| e.member.as_deref() == Some(m))).json(),
                })).collect::<Vec<_>>(),
                "barrier": of.iter().filter(|e| e.role == "barrier").map(|e| entry_json(e)).collect::<Vec<_>>(),
                "governor": of.iter().filter(|e| e.role == "governor").map(|e| entry_json(e)).collect::<Vec<_>>(),
                "total": Sum::of(of.iter().copied()).json(),
            })
        }).collect::<Vec<_>>(),
        "total": Sum::of(es.iter()).json(),
    })
}

fn member_ids(of: &[&Entry]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for m in of.iter().filter_map(|e| e.member.clone()) {
        if !ids.contains(&m) {
            ids.push(m);
        }
    }
    ids
}

fn tok(n: Option<u64>) -> String {
    n.map_or(String::new(), |n| util::count_human(n))
}

fn line(p: &Paint, what: &str, tier: &str, f: &Fig, note: &str) -> String {
    let usd = f.usd.map(util::money_human).unwrap_or_default();
    let body = if f.measured {
        format!(
            "{}  {}  {}  {}  {}  {}  {}",
            util::pad(&tok(f.input), 6), util::pad(&tok(f.output), 6), util::pad(&tok(f.cache_read), 6),
            util::pad(&tok(f.cache_write), 6), util::pad(&f.turns.to_string(), 5), util::pad(&usd, 8),
            util::pad(&util::duration_human(f.seconds), 6)
        )
    } else {
        util::pad("unmeasured", 6 + 2 + 6 + 2 + 6 + 2 + 6 + 2 + 5 + 2 + 8 + 2 + 6)
    };
    format!("  {}  {}  {}  {}", util::pad(what, 28), util::pad(tier, 16), body, p.dim(note))
}

fn sum_line(p: &Paint, what: &str, s: &Sum) -> String {
    let mut note = format!("{} seats", s.seats);
    if s.unmeasured > 0 {
        note.push_str(&format!(" · {} unmeasured", s.unmeasured));
    }
    if s.inexact > 0 {
        note.push_str(&format!(" · {} rounded", s.inexact));
    }
    let f = Fig {
        input: Some(s.input), output: Some(s.output), cache_read: Some(s.cache_read), cache_write: Some(s.cache_write),
        usd: Some(s.usd), turns: s.turns, seconds: s.seconds, exact: true, measured: true,
    };
    p.bold(&line(p, what, "", &f, "")) + &format!("  {}", p.dim(&note))
}

fn render(slug: &str, es: &[Entry], groups: usize) -> String {
    let p = Paint::new();
    let mut out = vec![
        p.bold(&format!("{slug} cost")),
        p.dim(&format!(
            "  {}  {}  {}  {}  {}  {}  {}  {}  {}",
            util::pad("seat", 28), util::pad("tier", 16), util::pad("in", 6), util::pad("out", 6),
            util::pad("cache", 6), util::pad("write", 6), util::pad("turns", 5), util::pad("cost", 8), "clock"
        )),
    ];
    for g in 1..=groups {
        let of: Vec<&Entry> = es.iter().filter(|e| e.group == g).collect();
        if of.is_empty() {
            continue;
        }
        out.push(String::new());
        out.push(p.bold(&format!("group {g}")));
        let note = |e: &Entry| -> String {
            let mut n = Vec::new();
            if e.dropped { n.push("dropped".to_string()); }
            if e.by_governor { n.push("by the governor".to_string()); }
            if e.shared { n.push(format!("{} counted above", e.seat)); }
            if let Some(t) = e.through { n.push(format!("through group {t}")); }
            if !e.fig.measured && !e.by_governor && !e.shared { n.push("no reading".to_string()); }
            if e.fig.measured && !e.fig.exact { n.push("rounded".to_string()); }
            n.join(" · ")
        };
        let blank = Fig::default();
        for m in member_ids(&of) {
            out.push(format!("  {}", p.bold(&m)));
            for e in of.iter().filter(|e| e.member.as_deref() == Some(&m)) {
                let name = if e.label.is_empty() { e.role.to_string() } else { format!("{} {}", e.role, e.label) };
                let f = if e.shared || e.by_governor { &blank } else { &e.fig };
                let mut l = line(&p, &format!("  {name} {}", e.seat), &e.tier, f, &note(e));
                if e.shared || e.by_governor {
                    l = format!("  {}  {}  {}", util::pad(&format!("  {name} {}", e.seat), 28), util::pad(&e.tier, 16), p.dim(&note(e)));
                }
                out.push(l);
            }
        }
        for e in of.iter().filter(|e| matches!(e.role, "barrier" | "governor")) {
            let f = if e.shared { &blank } else { &e.fig };
            out.push(line(&p, &format!("{} {}", e.role, e.seat), &e.tier, f, &note(e)));
        }
        out.push(sum_line(&p, &format!("group {g} total"), &Sum::of(of.iter().copied())));
    }
    out.push(String::new());
    out.push(sum_line(&p, "list total", &Sum::of(es.iter())));
    out.push(p.dim("  tokens are kept apart; an unmeasured seat (ended before the ledger, or never read) is not zero. A governor counts whole toward the group it was seated at."));
    out.join("\n")
}

pub fn cost(store: &Store, args: &Args, slug: &str) -> i32 {
    let Some(es) = entries(store, slug) else {
        eprintln!("wsp: no worklist {slug}");
        return 2;
    };
    let groups = store.worklist(slug).map_or(0, |w| w.groups().len());
    if args.json() {
        println!("{}", cost_json(slug, &es, groups));
    } else if es.is_empty() {
        println!("nothing recorded to cost on {slug} yet");
    } else {
        println!("{}", render(slug, &es, groups));
    }
    0
}

/// A task's verifier passes as `attempts --json` rows, `role: "verifier"`.
pub fn verifier_rows(t: &crate::model::Task) -> Vec<Value> {
    crate::verification::passes(t)
        .iter()
        .filter_map(|p| {
            let pane = p.pane.as_deref()?;
            let seconds = p.decided.as_deref().map_or(0, |d| (util::epoch_of(d) - util::epoch_of(&p.at)).max(0));
            let clause = p.spent.as_ref().map(|s| (s.ran.as_str(), &s.spend, s.turns));
            let f = fig_of(pane, clause, seconds);
            Some(json!({
                "task": t.id, "role": "verifier", "leg": null, "at": p.at, "pane": pane,
                "ran_at": p.spent.as_ref().map(|s| s.ran.clone()).unwrap_or_default(),
                "turns": f.turns, "cache_tokens": f.cache_read, "input_tokens": f.input,
                "output_tokens": f.output, "cache_write_tokens": f.cache_write,
                "usd_micros": f.usd, "exact": f.exact, "seconds": seconds,
                "outcome": p.state.as_str(),
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Status, Task, Worklist, WorklistStatus};
    use serde_json::json;

    fn seat(state: &std::path::Path, id: &str, turns: u64, output: u64) {
        let d = state.join("compound-seats").join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("burn.json"), json!({ "turns": turns, "input": 10, "output": output, "cache_read": 5, "cache_write": 2, "cost": output * 100 }).to_string()).unwrap();
    }

    fn event(store: &Store, kind: &str, data: Value) {
        store.log_event(kind, data);
    }

    fn fixture(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(tag);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        for id in ["m-1", "m-2"] {
            let mut t = Task::new(id, id);
            t.status_raw = Status::Review.as_str().into();
            store.save_task(&t).unwrap();
        }
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[
            Group { members: vec!["m-1".into()], ..Group::default() },
            Group { members: vec!["m-2".into()], ..Group::default() },
        ]);
        store.save_worklist(&w).unwrap();
        (env, store)
    }

    /// A log of the given `(pane, ended)` claims, in order.
    fn claims(store: &Store, id: &str, log: &[(&str, bool)]) {
        let mut t = store.find_task(id).unwrap();
        let mut body = String::from("## Log\n");
        for (i, (pane, ended)) in log.iter().enumerate() {
            body.push_str(&format!("- 2026-10-18T0{}:00:00Z claimed by pane {pane}\n", i + 1));
            if *ended {
                body.push_str(&format!("- 2026-10-18T0{}:30:00Z released after 30m · ran opus-5-5/high · 3 turns · 1M cache 1k in 1k out\n", i + 1));
            }
        }
        t.body = body;
        store.save_task(&t).unwrap();
    }

    fn verifier(store: &Store, id: &str, pane: &str) {
        let mut t = store.find_task(id).unwrap();
        let mut p = crate::verification::Pass::opened(None, None);
        p.pane = Some(pane.into());
        let mut all = crate::verification::passes(&t);
        all.push(p);
        crate::verification::write(&mut t, &all);
        store.save_task(&t).unwrap();
    }

    fn go(store: &Store, passed: u64) {
        event(store, "worklist-go", json!({ "id": "run", "passed": passed }));
    }

    fn set(store: &Store, ws: &str) {
        event(store, "governor-set", json!({ "project": "run", "workspace": ws }));
    }

    fn seen(es: &[Entry], role: &str) -> Vec<(String, usize)> {
        es.iter().filter(|e| e.role == role).map(|e| (e.seat.clone(), e.group)).collect()
    }

    #[test]
    fn governors_land_in_the_group_the_run_stood_at_and_a_reset_counts_once() {
        let (env, store) = fixture("cost-gov");
        let st = env.state();
        set(&store, "cpd-1");
        set(&store, "cpd-1");
        go(&store, 1);
        set(&store, "cpd-2");
        set(&store, "cpd-1");
        event(&store, "governor-set", json!({ "project": "other", "workspace": "cpd-9" }));
        seat(&st, "cpd-1", 3, 100);
        seat(&st, "cpd-2", 4, 200);
        let es = entries(&store, "run").unwrap();
        assert_eq!(seen(&es, "governor"), vec![("cpd-1".into(), 1), ("cpd-2".into(), 2)]);
        let total = Sum::of(es.iter());
        assert_eq!((total.usd, total.output), (30_000, 300), "each seat's reading, once");
        assert!(es.iter().all(|e| e.through.is_none()), "a plain rotation sits through no later pass");
    }

    #[test]
    fn a_seat_that_sat_through_a_second_pass_stays_whole_and_says_through() {
        let (_env, store) = fixture("cost-through");
        set(&store, "cpd-1");
        go(&store, 1);
        go(&store, 2);
        let es = entries(&store, "run").unwrap();
        assert_eq!(es[0].group, 1);
        assert_eq!(es[0].through, Some(2), "the last group passed while it sat");
    }

    #[test]
    fn a_pass_by_the_governors_pane_is_not_counted_twice() {
        let (env, store) = fixture("cost-twice");
        seat(&env.state(), "cpd-1", 3, 100);
        seat(&env.state(), "cpd-2", 3, 200);
        set(&store, "cpd-1");
        verifier(&store, "m-1", "cpd-1");
        verifier(&store, "m-2", "cpd-2");
        let es = entries(&store, "run").unwrap();
        let v: Vec<&Entry> = es.iter().filter(|e| e.role == "verifier").collect();
        assert_eq!((v[0].by_governor, v[1].by_governor), (true, false), "{v:?}");
        assert_eq!((v[1].group, v[1].member.as_deref()), (2, Some("m-2")), "a verifier lands on its member's group");
        assert_eq!(Sum::of(es.iter()).usd, 10_000 + 20_000, "the governor once, the other verifier once");
    }

    #[test]
    fn a_member_attempt_is_counted_and_a_dropped_one_is_marked() {
        let (env, store) = fixture("cost-member");
        seat(&env.state(), "cpd-5", 2, 50);
        seat(&env.state(), "cpd-6", 2, 70);
        claims(&store, "m-1", &[("cpd-5", true), ("cpd-6", false)]);
        let es = entries(&store, "run").unwrap();
        let readings: u64 = ["cpd-5", "cpd-6"].iter().map(|s| crate::cmd_burn::reading(s).unwrap()["cost"].as_u64().unwrap()).sum();
        assert_eq!(Sum::of(es.iter()).usd, readings, "the list total is the sum of reading over its seats");
        assert_eq!(es.iter().map(|e| (e.role, e.dropped)).collect::<Vec<_>>(), vec![("worker", true), ("rework", false)]);
    }

    #[test]
    fn a_barrier_check_lands_on_its_group_and_an_unread_seat_is_unmeasured() {
        let (env, store) = fixture("cost-barrier");
        seat(&env.state(), "cpd-7", 2, 40);
        let mut w = store.worklist("run").unwrap();
        let mut gs = w.groups();
        let mut c = crate::barrier::Check::opened(None, None);
        c.pane = Some("cpd-7".into());
        gs[1].checks.push(c);
        let mut c = crate::barrier::Check::opened(None, None);
        c.pane = Some("cpd-8".into());
        gs[0].checks.push(c);
        w.set_groups(&gs);
        store.save_worklist(&w).unwrap();
        let es = entries(&store, "run").unwrap();
        assert_eq!(seen(&es, "barrier"), vec![("cpd-8".into(), 1), ("cpd-7".into(), 2)]);
        let sum = Sum::of(es.iter());
        assert_eq!((sum.usd, sum.unmeasured), (4_000, 1));
        let j = cost_json("run", &es, 2);
        assert_eq!(j["groups"][1]["barrier"][0]["measured"], true);
        assert_eq!(j["groups"][0]["barrier"][0]["measured"], false);
    }
}
