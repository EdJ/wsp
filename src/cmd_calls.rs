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
//! **Two logs** (`wsp-488`): actions and the daemon's ticks in `calls.jsonl`,
//! polls in `polls.jsonl` (`docs/calls.md` says which calls are polls); the
//! default view is the actions, `--polls` the polls, `--all` both.
//!
//! The report is sorted by total CPU, so its top line is the next thing to cut.
//!
//! **`--incidents`** (`docs/agent-manager.md` 6.1) is the other half of a
//! step's measure: not what wsp's calls cost but how often it acted on a
//! guess. It counts the `cycle.log` lines the daemon writes when it acts on
//! one of `docs/state-core.md` 1.6's inferences, by kind (see [`SIGNS`]), and
//! the two incident classes 6.1 names: wsp-478's (a process stopped for
//! standing in a finished tree) and wsp-411's (a seat ended on the strength of
//! a census, which was heard from afterwards: [`answered_after`]).

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
    if args.has("incidents") {
        return incidents(store, args);
    }
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
    let (polls, all) = (args.has("polls"), args.has("all"));
    if polls && all {
        eprintln!("wsp: --polls and --all are two ways to choose the log; give one");
        return 2;
    }
    let logs: &[calls::Log] = if all {
        &[calls::Log::Actions, calls::Log::Polls]
    } else if polls {
        &[calls::Log::Polls]
    } else {
        &[calls::Log::Actions]
    };
    let mut lines = Vec::new();
    let mut covers = Vec::new();
    for log in logs {
        let mine = calls::read_log(&store.state, *log);
        covers.push(calls::Coverage::of_log(&mine, &store.state, *log));
        lines.extend(mine);
    }
    if lines.is_empty() {
        let files: Vec<String> = logs.iter().map(|l| util::contract(&store.state.join(l.file()))).collect();
        println!("wsp calls: nothing recorded in {} yet (WSP_NO_CALLS={})", files.join(" or "), if calls::off() { "set" } else { "unset" });
        return 0;
    }
    let covers_json = if covers.len() == 1 { covers[0].json(since) } else { Value::Array(covers.iter().map(|c| c.json(since)).collect()) };
    let say = |covers: &[calls::Coverage]| {
        for c in covers {
            println!("  {}", coverage_text(c, span, since));
        }
    };
    let which_log = if all { "actions and polls" } else if polls { "polls" } else { "actions" };
    let (ticks, n_ticks) = calls::summarize_ticks(&lines, since);
    let tick_json = |rows: &[Row]| -> Vec<Value> {
        rows.iter().map(|r| json!({"phase": r.name, "ticks": r.count, "p50_ms": r.p(0.5), "p95_ms": r.p(0.95), "total_ms": (r.walls.iter().sum::<f64>() * 10.0).round() / 10.0})).collect()
    };
    if by == "phase" {
        if args.json() {
            println!("{}", json!({"since": span, "log": which_log, "covers": covers_json, "ticks": n_ticks, "phases": tick_json(&ticks)}));
            return 0;
        }
        say(&covers);
        if polls {
            println!("  (the daemon's ticks are in calls.jsonl; `wsp calls --by phase` without --polls)");
        }
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
                "log": which_log,
                "covers": covers_json,
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
        println!("{}", p.bold(&format!("wsp calls by child ({which_log}) — last {span_text}: {:.1} s of child wall", child_wall / 1000.0)));
        say(&covers);
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
    println!("{}", p.bold(&format!("wsp calls by {by} ({which_log}) — last {span_text}: {n_calls} calls, {:.1} CPU-s", total / 1000.0)));
    say(&covers);
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

// ------------------------------------------------------------- incidents

/// One sign that wsp acted on an inference: the `cycle.log` sentence its
/// writer prints, matched on the line after its timestamp.
pub(crate) struct Sign {
    /// `I1` .. `I4`, as `docs/state-core.md` 1.6 numbers them.
    pub kind: &'static str,
    pub name: &'static str,
    /// Where the sentence is written, for whoever changes it.
    pub site: &'static str,
    pub test: fn(&str) -> bool,
}

/// What each inference is, in 1.6's words.
pub(crate) const KINDS: &[(&str, &str)] = &[
    ("I1", "who owns a process"),
    ("I2", "is a seat alive, and in what state"),
    ("I3", "was an order delivered and taken"),
    ("I4", "has a seat ended"),
    ("I5", "what a row's state is"),
];

/// Every sign, by kind. A sentence here that its writer rewords stops being
/// counted, which the test beside this reads from each writer's own words.
/// I5 has none: a row's state is joined per call and never logged as a guess
/// (it is state-core's rows machine, not the manager's).
pub(crate) const SIGNS: &[Sign] = &[
    Sign { kind: "I1", name: "stopped in a finished row's tree (wsp-478)", site: "ending.rs stop_finished", test: |l| l.starts_with("stopped in finished row ") },
    Sign { kind: "I1", name: "stopped in a barrier check tree", site: "check_tree.rs signalled_line", test: |l| l.starts_with("stopped in barrier check tree ") },
    Sign { kind: "I1", name: "leaked agent ended", site: "leaked_agents.rs sentence", test: |l| l.starts_with("leaked agent: ") && (l.ends_with("ended it") || l.ends_with("killed it")) },
    Sign { kind: "I1", name: "reaped by a person", site: "cmd_role.rs", test: |l| l.starts_with("reap ") && l.contains(" by ") },
    Sign { kind: "I1", name: "ended pane closed (lsof)", site: "ended_panes.rs", test: |l| l.starts_with("closed ended seat ") },
    Sign { kind: "I2", name: "never became ready (screen)", site: "cmd_spawn.rs", test: |l| l.contains("started but never became ready for input") },
    Sign { kind: "I2", name: "screen overruled by opencode's record", site: "repair.rs", test: |l| l.contains("the screen says working and opencode finished") },
    Sign { kind: "I3", name: "typed, no turn seen", site: "wake.rs", test: |l| l.starts_with("told ") && l.contains("typed at the seat — no turn seen yet") },
    Sign { kind: "I3", name: "pane did not get its order", site: "cmd_task.rs", test: |l| l.contains("but its pane did not get it") },
    Sign { kind: "I3", name: "first order retyped or resent", site: "repair.rs first orders", test: |l| {
        l.contains("its first work order sat typed and unsent") || l.contains("no turn started on its first work order") || l.contains("its first work order was sent")
    } },
    Sign { kind: "I3", name: "shell waiting on itself ended", site: "repair.rs stuck", test: |l| l.contains("ended the shell waiting on itself") },
    Sign { kind: "I3", name: "prompt read off the screen", site: "waiting.rs", test: |l| l.contains("is at a prompt on its screen") },
    Sign { kind: "I4", name: "an accepted exit's seat already gone", site: "ending.rs EXIT_GONE", test: |l| l.contains("the seat of an accepted exit had already gone") },
    Sign { kind: "I4", name: "already gone, nothing to end", site: "cycle.rs barrier ends", test: |l| l.contains("has already gone — nothing to end") },
];

/// The lines that end seats on what a census said (`ps`, the screen, the
/// absence of a pane): wsp-411's class is one of these whose seat was heard
/// from afterwards.
fn ends_seats(line: &str) -> bool {
    line.contains("with nothing left to do, ended: ")
        || (line.contains(") ended — ") && line.ends_with("and no pid of it is running"))
        || SIGNS.iter().any(|s| s.kind == "I4" && (s.test)(line))
}

/// The seat labels a line names (`cpd-12`, `w64:p1`'s `p1` aside: compound's).
fn seats_in(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in line.match_indices("cpd-") {
        if i > 0 && line.as_bytes()[i - 1].is_ascii_alphanumeric() {
            continue;
        }
        let n: String = line[i + 4..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if !n.is_empty() {
            out.push(format!("cpd-{n}"));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One `cycle.log` line: its timestamp and its text, or `None` for a
/// continuation line.
fn dated(line: &str) -> Option<(&str, &str)> {
    let (at, rest) = line.split_at_checked(20)?;
    (at.len() == 20 && at.as_bytes()[4] == b'-' && at.ends_with('Z') && rest.starts_with(' ')).then(|| (at, &rest[1..]))
}

/// `cycle.log`'s entries: each dated line with the undated lines after it (a
/// failed spawn's reasons, a told text's tail) joined on, so a sign said on a
/// continuation line counts once, for its entry.
fn entries(cycle: &str) -> Vec<(&str, String)> {
    let mut out: Vec<(&str, String)> = Vec::new();
    for line in cycle.lines() {
        match (dated(line), out.last_mut()) {
            (Some((at, text)), _) => out.push((at, text.to_string())),
            (None, Some((_, text))) if !line.trim().is_empty() => {
                text.push_str(" / ");
                text.push_str(line.trim());
            }
            _ => {}
        }
    }
    out
}

/// Whether `seat` was heard from after `at`: an event it caused (its claim, a
/// session learnt, a verdict, a message it sent or acknowledged — not one wsp
/// sent it), or a `cycle.log` line read off its own hooks (`<seat>: …`).
fn answered_after(seat: &str, at: &str, cycle: &[(&str, String)], events: &[Value]) -> bool {
    let heard_event = events.iter().any(|e| {
        let t = e["ts"].as_str().unwrap_or("");
        let d = &e["data"];
        t > at && e["kind"] != "agent-told" && (d["pane"].as_str() == Some(seat) || d["by"].as_str() == Some(seat))
    });
    heard_event || cycle.iter().any(|(t, l)| *t > at && l.strip_prefix(seat).is_some_and(|r| r.starts_with(": ")) && !ends_seats(l))
}

/// The counts, over `cycle.log` and `events.jsonl`, of lines at or after `from`
/// (an ISO time).
pub(crate) fn count_incidents(cycle: &str, events: &str, from: &str) -> Value {
    let lines = entries(cycle);
    let evs: Vec<Value> = events.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let inside: Vec<&(&str, String)> = lines.iter().filter(|(t, _)| *t >= from).collect();
    let mut kinds = Vec::new();
    for (k, what) in KINDS {
        let signs: Vec<Value> = SIGNS
            .iter()
            .filter(|s| s.kind == *k)
            .map(|s| json!({"sign": s.name, "site": s.site, "count": inside.iter().filter(|(_, l)| (s.test)(l)).count()}))
            .collect();
        let count: u64 = signs.iter().map(|s| s["count"].as_u64().unwrap_or(0)).sum();
        kinds.push(json!({"kind": k, "what": what, "count": count, "signs": signs, "logged": !signs.is_empty()}));
    }
    let wsp_478 = inside.iter().filter(|(_, l)| (SIGNS[0].test)(l)).count();
    let mut ended = Vec::new();
    let mut answered = Vec::new();
    for (t, l) in &inside {
        if !ends_seats(l) {
            continue;
        }
        for seat in seats_in(l) {
            if answered_after(&seat, t, &lines, &evs) {
                answered.push(json!({"seat": seat, "at": t, "line": l}));
            }
            ended.push(seat);
        }
    }
    json!({
        "from": from,
        "cycle_from": lines.first().map(|(t, _)| *t),
        "cycle_to": lines.last().map(|(t, _)| *t),
        "kinds": kinds,
        "wsp_478": wsp_478,
        "wsp_411": {"seats_ended_on_a_census": ended.len(), "heard_from_after": answered.len(), "seats": answered},
    })
}

/// `wsp calls --incidents [--since 7d] [--json]`.
fn incidents(store: &Store, args: &Args) -> i32 {
    let span = match args.get("since") {
        None => 7 * 86_400,
        Some(t) => match window(&t) {
            Some(s) => s,
            None => {
                eprintln!("wsp: --since takes a span such as 90s, 15m, 1h or 2d, not `{t}`");
                return 2;
            }
        },
    };
    let from = util::iso_at(util::epoch_secs() - span);
    let cycle = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
    let events = std::fs::read_to_string(store.state_file("events.jsonl")).unwrap_or_default();
    let got = count_incidents(&cycle, &events, &from);
    if args.json() {
        println!("{}", json!({"since": span, "incidents": got}));
        return 0;
    }
    let p = util::Paint::new();
    println!("{}", p.bold(&format!("wsp incidents — last {}: wsp acting on a guess (docs/state-core.md 1.6)", util::duration_human(span))));
    match (got["cycle_from"].as_str(), got["cycle_to"].as_str()) {
        (Some(a), Some(z)) => println!("  cycle.log covers {a} to {z}"),
        _ => println!("  cycle.log is empty"),
    }
    for k in got["kinds"].as_array().into_iter().flatten() {
        println!("  {} {:<38} {:>6}", k["kind"].as_str().unwrap_or(""), k["what"].as_str().unwrap_or(""), k["count"]);
        if k["logged"] == false {
            println!("       (never logged as a guess: not counted here)");
        }
        for s in k["signs"].as_array().into_iter().flatten().filter(|s| s["count"].as_u64() > Some(0)) {
            println!("       {:<40} {:>5}", s["sign"].as_str().unwrap_or(""), s["count"]);
        }
    }
    let w = &got["wsp_411"];
    println!();
    println!("  wsp-478 kind (stopped in a finished tree): {}", got["wsp_478"]);
    println!(
        "  wsp-411 kind (ended on a census, heard from after): {} of {} seat(s) ended so",
        w["heard_from_after"], w["seats_ended_on_a_census"]
    );
    for s in w["seats"].as_array().into_iter().flatten().take(10) {
        println!("       {} at {}: {}", s["seat"].as_str().unwrap_or(""), s["at"].as_str().unwrap_or(""), util::truncate(s["line"].as_str().unwrap_or(""), 90));
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
        text: r##"  wsp calls [--since 1h] [--by verb|caller|child|phase] [--polls|--all] [--json]
                                    what wsp's own calls cost: count, p50/p95 wall,
                                    CPU-s and share, dearest first. Actions and
                                    ticks (calls.jsonl) by default, --polls the
                                    polls (polls.jsonl), --all both; each view
                                    says the time its file covers
                                    (WSP_NO_CALLS=1 stops the recording)
  wsp calls --incidents [--since 7d] [--json]
                                    how often wsp acted on a guess, by kind
                                    (who owns a process, is a seat alive, was an
                                    order taken, has a seat ended), from
                                    cycle.log and events.jsonl"##,
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

    /// Each view reads its own file and says what that file covers; `--all`
    /// reads both and says it twice; asking for both ways at once is refused.
    #[test]
    fn polls_and_actions_are_read_apart_and_each_says_its_span() {
        let env = crate::util::isolated("calls-two");
        let store = Store::open();
        let now = util::now_iso();
        let line = |verb: &str| {
            json!({"k":"call","t":now,"verb":verb,"sub":"","flags":["json"],"role":"none","parent":"compound-host","wall_ms":10.0,
                   "user_ms":5.0,"sys_ms":0.0,"kids_user_ms":0.0,"kids_sys_ms":0.0,"children":{}})
        };
        calls::append_to(&env.state(), calls::Log::Polls, &line("wip"), calls::POLL_CAP);
        calls::append_to(&env.state(), calls::Log::Actions, &line("tell"), calls::CAP);
        let a = calls::read_log(&env.state(), calls::Log::Actions);
        let p = calls::read_log(&env.state(), calls::Log::Polls);
        assert_eq!((a.len(), p.len()), (1, 1));
        assert_eq!(a[0]["verb"], "tell");
        assert_eq!(p[0]["verb"], "wip");
        let say = |log| calls::Coverage::of_log(&calls::read_log(&env.state(), log), &env.state(), log).line(3600, 0);
        assert!(say(calls::Log::Actions).starts_with("calls.jsonl covers "), "{}", say(calls::Log::Actions));
        assert!(say(calls::Log::Polls).starts_with("polls.jsonl covers "), "{}", say(calls::Log::Polls));
        assert_eq!(calls(&store, &args("calls", &[], &[])), 0);
        assert_eq!(calls(&store, &args("calls", &[], &[("polls", "1")])), 0);
        assert_eq!(calls(&store, &args("calls", &[], &[("all", "1"), ("json", "1")])), 0);
        assert_eq!(calls(&store, &args("calls", &[], &[("all", "1"), ("polls", "1")])), 2);
    }

    /// Each sign is the sentence its writer prints: the writer's own source
    /// holds the words the sign looks for, so a rewording there fails here
    /// rather than counting nothing in silence.
    #[test]
    fn each_sign_reads_the_words_its_writer_prints() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let words: &[(&str, &[&str])] = &[
            ("src/ending.rs", &["stopped in finished row {}", "wsp: the seat of an accepted exit had already gone —"]),
            ("crates/trees/src/check_tree.rs", &["stopped in barrier check tree {path}"]),
            ("src/leaked_agents.rs", &["leaked agent: {}", "ended it", "killed it"]),
            ("src/cmd_role.rs", &["reap {pid} by {}"]),
            ("src/ended_panes.rs", &["closed {}", "ended seat {}"]),
            ("src/cmd_spawn.rs", &["started but never became ready for input"]),
            ("src/wake.rs", &["typed at the seat — no turn seen yet"]),
            ("src/cmd_task.rs", &["but its pane did not get it"]),
            ("src/repair.rs", &[
                "the screen says working and opencode finished",
                "its first work order sat typed and unsent",
                "no turn started on its first work order",
                "its first work order was sent",
                "ended the shell waiting on itself",
                "verifier seat(s) with nothing left to do, ended: {}",
            ]),
            ("src/waiting.rs", &["is at a prompt on its screen"]),
            ("src/cycle.rs", &["has already gone — nothing to end"]),
        ];
        for (file, want) in words {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            for w in *want {
                assert!(text.contains(w), "{file} no longer says {w:?}; change the sign in SIGNS with it");
            }
        }
        // And each sign matches a line as its writer would print it.
        let lines = [
            "stopped in finished row wsp-1 tree ~/w/wsp-1: TERM 4 5",
            "stopped in barrier check tree ~/c/x: TERM 9",
            "leaked agent: claude pid 7 in seat cpd-1 of /tmp/s: no daemon kept that store for the grace; ended it",
            "reap 77 by person: its row is done — cargo test",
            "closed ended seat cpd-3: pane idle 12 min in ~/w (supervisor 4, 2 processes)",
            "FAILED spawn wsp-2: wsp: claude did not start in cpd-9: claude started but never became ready for input",
            "cpd-4: the screen says working and opencode finished this session at 10:00 — read as idle",
            "told m0: typed at the seat — no turn seen yet, and still owed · 1 held",
            "wsp-6 is at doing with the reason on its row, but its pane did not get it — wsp retries",
            "cpd-5: its first work order sat typed and unsent - submitted it",
            "cpd-5: ended the shell waiting on itself (pid 99)",
            "wsp-1 (cpd-2) is at a prompt on its screen — raised to the m0 governor",
            "wsp-1: the seat of an accepted exit had already gone — cpd-126 (member) was ended by something else",
            "m0: wsp-3 settled its barrier and has already gone — nothing to end",
        ];
        for sign in SIGNS {
            assert!(lines.iter().any(|l| (sign.test)(l)), "no line reads as {:?}", sign.name);
        }
        assert!(!SIGNS.iter().any(|s| (s.test)("leaked agent: claude pid 7 in seat cpd-1 of /tmp/s: no daemon keeps that store; ending it in 30 min unless one starts")), "a warning is not an act");
    }

    /// The counts are by kind and in the window; a seat ended on a census and
    /// heard from afterwards is wsp-411's class, one never heard from again is not.
    #[test]
    fn incidents_are_counted_by_kind_in_the_window_and_a_census_end_heard_from_after_is_wsp_411() {
        let cycle = "\
2026-11-01T00:00:00Z stopped in finished row old-1 tree ~/w: TERM 1
2026-11-10T00:00:00Z stopped in finished row wsp-1 tree ~/w/wsp-1: TERM 4
2026-11-10T00:01:00Z told m0: typed at the seat — no turn seen yet, and still owed · 1 held
2026-11-10T00:02:00Z verifier seat(s) with nothing left to do, ended: cpd-7 cpd-8
  a continuation line, undated
2026-11-10T00:03:00Z cpd-7: a command has been waiting for 300s: cargo test
2026-11-10T00:04:00Z m0 wsp-2 (cpd-9) ended — verification holds at abc, and no pid of it is running
2026-11-10T00:07:00Z FAILED spawn wsp-3: wsp: an agent did not start
wsp: claude did not start in cpd-10: claude started but never became ready for input
wsp: claude did not start in cpd-10: claude started but never became ready for input, and it is still holding what failed
";
        let events = format!(
            "{}\n{}\n",
            json!({"kind": "agent-told", "ts": "2026-11-10T00:05:00Z", "data": {"pane": "cpd-8"}}),
            json!({"kind": "message-acknowledged", "ts": "2026-11-10T00:06:00Z", "data": {"by": "cpd-9"}}),
        );
        let got = count_incidents(cycle, &events, "2026-11-05T00:00:00Z");
        let kind = |k: &str| got["kinds"].as_array().unwrap().iter().find(|x| x["kind"] == k).unwrap()["count"].as_u64().unwrap();
        assert_eq!((kind("I1"), kind("I2"), kind("I3"), kind("I4"), kind("I5")), (1, 1, 1, 0, 0), "a sign on continuation lines counts once, for its entry");
        assert_eq!(got["wsp_478"], 1, "the line before the window is not counted");
        assert_eq!(got["wsp_411"]["seats_ended_on_a_census"], 3);
        let heard: Vec<&str> = got["wsp_411"]["seats"].as_array().unwrap().iter().map(|s| s["seat"].as_str().unwrap()).collect();
        assert_eq!(heard, ["cpd-7", "cpd-9"], "cpd-8 was only told something, which is wsp speaking, not the seat");
        assert_eq!(got["cycle_from"], "2026-11-01T00:00:00Z");
        assert_eq!(seats_in("ended: cpd-7 cpd-8, xcpd-9"), ["cpd-7", "cpd-8"]);
    }

    #[test]
    fn the_incidents_view_reads_the_state_and_refuses_a_bad_span() {
        let env = crate::util::isolated("calls-incidents");
        let store = Store::open();
        std::fs::write(env.state().join("cycle.log"), format!("{} stopped in finished row a-1 tree ~/w: TERM 3\n", util::now_iso())).unwrap();
        assert_eq!(calls(&store, &args("calls", &[], &[("incidents", "1")])), 0);
        assert_eq!(calls(&store, &args("calls", &[], &[("incidents", "1"), ("json", "1"), ("since", "1d")])), 0);
        assert_eq!(calls(&store, &args("calls", &[], &[("incidents", "1"), ("since", "soon")])), 2);
    }

}
