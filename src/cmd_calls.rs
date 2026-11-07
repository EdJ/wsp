//! `wsp calls` — what wsp's own calls cost (`wsp-481`).
//!
//! The recording is `wsp_base::calls`: every `wsp` process appends one line to
//! `$WSP_STATE/calls.jsonl` as it exits, and the daemon one per tick. This is the
//! reading half. The start of the record is `record_call` in `main.rs`, which
//! reduces the command line to the three things that are safe to write down — the
//! verb, a sub-verb that is one of the words the help documents, and the flag
//! *names*.
//!
//! **Never prose, ids or file contents.** `wsp tell cpd-3 "…"` is recorded as
//! `tell`, with no sub-verb because `cpd-3` is no documented word, and no flag
//! values at all.
//!
//! The report is sorted by total CPU, so its top line is the next thing to cut.

use serde_json::{json, Value};
use wsp_base::calls::{self, By, Row};

use crate::store::Store;
use crate::util;
use crate::base::args::Args;

/// `90`, `90s`, `15m`, `1h`, `2d`, as seconds.
fn window(text: &str) -> Option<i64> {
    let t = text.trim();
    let (num, unit) = match t.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => t.split_at(i),
        None => (t, "s"),
    };
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "s" | "" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    Some(n * mult)
}

fn row_json(r: &Row, total: f64) -> Value {
    let kids: serde_json::Map<String, Value> = r
        .kids
        .iter()
        .map(|(p, (n, ms))| (p.clone(), json!({"n": n, "ms": (ms * 10.0).round() / 10.0})))
        .collect();
    json!({
        "name": r.name,
        "count": r.count,
        "p50_ms": r.p(0.5),
        "p95_ms": r.p(0.95),
        "cpu_s": (r.cpu_ms / 100.0).round() / 10.0,
        "share": if total > 0.0 { (r.cpu_ms / total * 1000.0).round() / 1000.0 } else { 0.0 },
        "children": Value::Object(kids),
    })
}

fn kids_line(r: &Row) -> String {
    let mut k: Vec<(&String, &(u64, f64))> = r.kids.iter().collect();
    k.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap_or(std::cmp::Ordering::Equal));
    k.iter().take(6).map(|(p, (n, ms))| format!("{p} {n}x {:.1}s", ms / 1000.0)).collect::<Vec<_>>().join(", ")
}

pub fn calls(store: &Store, args: &Args) -> i32 {
    let by = match args.get("by").as_deref() {
        None | Some("verb") => "verb",
        Some("caller") => "caller",
        Some("child") => "child",
        Some("phase") => "phase",
        Some(other) => {
            eprintln!("wsp: --by takes verb, caller, child or phase, not `{other}`");
            return 2;
        }
    };
    let span = match args.get("since") {
        None => 3600,
        Some(t) => match window(&t) {
            Some(s) => s,
            None => {
                eprintln!("wsp: --since takes a span such as 90s, 15m, 1h or 2d, not `{t}`");
                return 2;
            }
        },
    };
    let since = util::epoch_secs() - span;
    let lines = calls::read_all(&store.state);
    if lines.is_empty() {
        println!("wsp calls: nothing recorded in {} yet (WSP_NO_CALLS={})", util::contract(&store.state.join(calls::FILE)), if calls::off() { "set" } else { "unset" });
        return 0;
    }
    let cov = calls::Coverage::of(&lines, &store.state);
    let (ticks, n_ticks) = calls::summarize_ticks(&lines, since);
    let tick_json = |rows: &[Row]| -> Vec<Value> {
        rows.iter().map(|r| json!({"phase": r.name, "ticks": r.count, "p50_ms": r.p(0.5), "p95_ms": r.p(0.95), "total_ms": (r.walls.iter().sum::<f64>() * 10.0).round() / 10.0})).collect()
    };
    if by == "phase" {
        if args.json() {
            println!("{}", json!({"since": span, "covers": cov.json(since), "ticks": n_ticks, "phases": tick_json(&ticks)}));
            return 0;
        }
        println!("  {}", coverage_text(&cov, span, since));
        print_phases(&ticks, n_ticks, span);
        return 0;
    }
    let which = match by {
        "caller" => By::Caller,
        "child" => By::Child,
        _ => By::Verb,
    };
    let (rows, total) = calls::summarize(&lines, since, which);
    let n_calls: u64 = if which == By::Child { 0 } else { rows.iter().map(|r| r.count).sum() };
    let child_wall: f64 = rows.iter().map(|r| r.walls.iter().sum::<f64>()).sum();
    if args.json() {
        let basis = if which == By::Child { child_wall } else { total };
        println!(
            "{}",
            json!({
                "since": span,
                "covers": cov.json(since),
                "by": by,
                "calls": n_calls,
                "cpu_s": (total / 100.0).round() / 10.0,
                "rows": rows.iter().map(|r| {
                    if which == By::Child {
                        let w: f64 = r.walls.iter().sum();
                        json!({"name": r.name, "runs": r.count, "p50_ms": r.p(0.5), "p95_ms": r.p(0.95), "wall_s": (w / 100.0).round() / 10.0, "share": if basis > 0.0 { (w / basis * 1000.0).round() / 1000.0 } else { 0.0 }})
                    } else {
                        row_json(r, total)
                    }
                }).collect::<Vec<_>>(),
                "ticks": {"count": n_ticks, "phases": tick_json(&ticks)},
            })
        );
        return 0;
    }
    let p = util::Paint::new();
    let span_text = util::duration_human(span);
    if which == By::Child {
        println!("{}", p.bold(&format!("wsp calls by child — last {span_text}: {:.1} s of child wall", child_wall / 1000.0)));
        println!("  {}", coverage_text(&cov, span, since));
        println!("  {:<22} {:>7} {:>8} {:>8} {:>9} {:>6}", "child", "runs", "p50 ms", "p95 ms", "wall s", "share");
        for r in rows.iter().take(25) {
            let w: f64 = r.walls.iter().sum();
            println!(
                "  {:<22} {:>7} {:>8.0} {:>8.0} {:>9.1} {:>5.1}%",
                util::truncate(&r.name, 22),
                r.count,
                r.p(0.5),
                r.p(0.95),
                w / 1000.0,
                if child_wall > 0.0 { w / child_wall * 100.0 } else { 0.0 }
            );
        }
        println!("  (child CPU is not split by program — getrusage has only the total; see `--by verb`)");
        return 0;
    }
    println!("{}", p.bold(&format!("wsp calls by {by} — last {span_text}: {n_calls} calls, {:.1} CPU-s", total / 1000.0)));
    println!("  {}", coverage_text(&cov, span, since));
    println!("  {:<34} {:>7} {:>8} {:>8} {:>9} {:>6}", by, "count", "p50 ms", "p95 ms", "CPU-s", "share");
    for (i, r) in rows.iter().take(25).enumerate() {
        println!(
            "  {:<34} {:>7} {:>8.0} {:>8.0} {:>9.1} {:>5.1}%",
            util::truncate(&r.name, 34),
            r.count,
            r.p(0.5),
            r.p(0.95),
            r.cpu_ms / 1000.0,
            if total > 0.0 { r.cpu_ms / total * 100.0 } else { 0.0 }
        );
        if i < 5 && !r.kids.is_empty() {
            println!("      children: {}", kids_line(r));
        }
    }
    if rows.len() > 25 {
        println!("  … and {} more (--json has every row)", rows.len() - 25);
    }
    if n_ticks > 0 {
        println!();
        print_phases(&ticks, n_ticks, span);
    }
    0
}

fn coverage_text(cov: &calls::Coverage, span: i64, since: i64) -> String {
    cov.line(span, since)
}

fn print_phases(rows: &[Row], n: u64, span: i64) {
    let p = util::Paint::new();
    println!("{}", p.bold(&format!("daemon ticks — last {}: {n} ticks", util::duration_human(span))));
    println!("  {:<28} {:>7} {:>8} {:>8} {:>10}", "phase", "ticks", "p50 ms", "p95 ms", "total s");
    for r in rows.iter().take(14) {
        println!("  {:<28} {:>7} {:>8.0} {:>8.0} {:>10.1}", util::truncate(&r.name, 28), r.count, r.p(0.5), r.p(0.95), r.walls.iter().sum::<f64>() / 1000.0);
    }
}

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["calls"],
    run: calls,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Plumbing,
        order: 111,
        text: r##"  wsp calls [--since 1h] [--by verb|caller|child|phase] [--json]
                                    what wsp's own calls cost: count, p50/p95 wall,
                                    CPU-s and share, dearest first (calls.jsonl;
                                    WSP_NO_CALLS=1 stops the recording)"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &str, rest: &[&str], flags: &[(&str, &str)]) -> Args {
        Args::synth(cmd, rest, flags)
    }

    #[test]
    fn a_span_is_seconds_minutes_hours_or_days() {
        assert_eq!(window("90"), Some(90));
        assert_eq!(window("15m"), Some(900));
        assert_eq!(window("1h"), Some(3600));
        assert_eq!(window("2d"), Some(172_800));
        assert_eq!(window("x"), None);
        assert_eq!(window("5w"), None);
    }

    /// The report reads the file `begin`/`exit` and the daemon write, in the
    /// window asked for, and names the verb with its count and children.
    #[test]
    fn the_report_names_the_costliest_verb_with_its_children() {
        let env = crate::util::isolated("calls-report");
        let store = Store::open();
        let now = util::now_iso();
        let line = |verb: &str, cpu: f64| {
            json!({"k":"call","t":now,"verb":verb,"sub":"","flags":["json"],"role":"none","parent":"compound-host","wall_ms":900.0,
                   "user_ms":cpu,"sys_ms":0.0,"kids_user_ms":0.0,"kids_sys_ms":0.0,"children":{"ps":{"n":2,"ms":300.0}}})
        };
        for v in [line("wip", 9000.0), line("wip", 8000.0), line("ls", 10.0)] {
            calls::append(&env.state(), &v, calls::CAP);
        }
        calls::append(&env.state(), &json!({"k":"tick","t":now,"wall_ms":50.0,"phases":{"sync":40.0}}), calls::CAP);
        assert_eq!(calls(&store, &args("calls", &[], &[("json", "1")])), 0);
        assert_eq!(calls(&store, &args("calls", &[], &[("since", "2d")])), 0);
        let (rows, total) = calls::summarize(&calls::read_all(&env.state()), 0, By::Verb);
        assert_eq!(rows[0].name, "wip --json");
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].kids["ps"].0, 4);
        assert_eq!(total, 17010.0);
        let (ticks, n) = calls::summarize_ticks(&calls::read_all(&env.state()), 0);
        assert_eq!(n, 1);
        assert!(ticks.iter().any(|r| r.name == "sync"));
        assert_eq!(calls(&store, &args("calls", &[], &[("by", "bogus")])), 2);
    }
}
