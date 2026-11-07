//! The timing retro of a cleared worklist (`wsp-472`): when a list clears, a
//! seat reads where its wall went and files what it finds. This half is the
//! evidence — the guards that say whether a list gets a retro at all, and the
//! packet the retro seat is handed. The verbs the seat files through
//! (`wsp retro file | note | done`) are the next leg's.

use serde_json::Value;

use crate::model::Worklist;
use crate::store::Store;
use crate::Args;

/// A list under three rows, or under half an hour of wall, has nothing a
/// retro would learn from.
pub const MIN_ROWS: usize = 3;
pub const MIN_WALL: i64 = 30 * 60;
/// New rows one retro may file.
pub const MAX_NEW: usize = 5;
/// How many cleared lists stand as the baseline.
pub const BASELINE: usize = 3;

/// The only verbs the retro seat files through, in the one place both the
/// packet and the verbs' usage text read, so they cannot disagree.
pub const SYNTAX: &str = "\
wsp retro file <list> --defect|--strategic --title T --evidence FILE --fix FILE --done-when FILE [--ask FILE]
wsp retro note <list> <row> FILE
wsp retro done <list> FILE";

/// The bucket names of the wall, as `effort.json` spells them under
/// `totals.wall_buckets` (`<name>_seconds`).
pub const BUCKETS: &[&str] = &["working", "waiting", "serialised", "barrier_run", "check_seat", "between_groups"];

/// A cleared list's wall figures: what the baseline is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Figures {
    pub list: String,
    pub wall: i64,
    pub rows: usize,
    pub buckets: Vec<(String, i64)>,
}

impl Figures {
    pub fn of(effort: &Value) -> Figures {
        let wb = &effort["totals"]["wall_buckets"];
        let secs = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        let rows = effort["groups"].as_array().map_or(0, |gs| gs.iter().map(|g| g["members"].as_array().map_or(0, Vec::len)).sum());
        Figures {
            list: effort["list"].as_str().unwrap_or("").to_string(),
            wall: secs(wb, "wall_seconds").max(secs(&effort["totals"], "wall_seconds")),
            rows,
            buckets: BUCKETS.iter().map(|b| (b.to_string(), secs(wb, &format!("{b}_seconds")))).collect(),
        }
    }

    fn line(&self) -> String {
        let parts: Vec<String> = self.buckets.iter().map(|(b, s)| format!("{b} {}", mins(*s))).collect();
        format!("{} · {} rows · wall {} · {}", self.list, self.rows, mins(self.wall), parts.join(" · "))
    }
}

fn mins(s: i64) -> String {
    format!("{}m", (s.max(0) + 30) / 60)
}

/// Whether a project's `## Settings` (or `## Retro`) says `retro = off`.
fn project_off(store: &Store, w: &Worklist) -> bool {
    let Some(home) = crate::worklist::home(store, w) else { return false };
    let Some(p) = store.project(&home.project) else { return false };
    ["Settings", "Retro"].iter().filter_map(|s| p.section(s)).any(|s| {
        s.lines().any(|l| {
            let l = l.trim().trim_start_matches("- ").trim_start_matches("* ").replace(' ', "").to_lowercase();
            matches!(l.as_str(), "retro=off" | "retro:off" | "retro=no" | "retro:no" | "retro=false" | "retro:false")
        })
    })
}

/// The guards, given the figures the list has.
pub fn eligible_with(store: &Store, w: &Worklist, rows: usize, wall: i64) -> Result<(), String> {
    if w.no_retro {
        return Err(format!("{} was run with --no-retro", w.id));
    }
    if project_off(store, w) {
        return Err(format!("the project of {} says retro = off", w.id));
    }
    if let Some((at, seat)) = &w.retro {
        return Err(format!("a retro already ran for {} ({at}, seat {seat})", w.id));
    }
    if rows < MIN_ROWS {
        return Err(format!("{} has {rows} rows; a retro wants at least {MIN_ROWS}", w.id));
    }
    if wall < MIN_WALL {
        return Err(format!("{} ran {} of wall; a retro wants at least {}", w.id, mins(wall), mins(MIN_WALL)));
    }
    Ok(())
}

/// Whether the list gets a retro, and if not why not.
pub fn eligible(store: &Store, w: &Worklist) -> Result<(), String> {
    let f = Figures::of(&crate::cmd_report::effort(store, w, None));
    eligible_with(store, w, f.rows.max(w.groups().iter().map(|g| g.members.len()).sum()), f.wall)
}

fn read_effort(slug: &str) -> Option<Value> {
    let t = std::fs::read_to_string(crate::cmd_report::dir(slug).join("effort.json")).ok()?;
    serde_json::from_str(&t).ok()
}

/// The last [`BASELINE`] cleared lists' figures, newest first, read from the
/// `effort.json` each left: this list is never its own baseline.
pub fn baseline(store: &Store, w: &Worklist) -> Vec<Figures> {
    let mut found: Vec<(String, Figures)> = store
        .worklists()
        .into_iter()
        .filter(|o| o.id != w.id && o.status() == crate::model::WorklistStatus::Done && !o.is_cancelled())
        .filter_map(|o| {
            let v = read_effort(&o.id)?;
            let f = Figures::of(&v);
            (f.wall > 0).then(|| (v["generated"].as_str().unwrap_or("").to_string(), Figures { list: o.id.clone(), ..f }))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().take(BASELINE).map(|(_, f)| f).collect()
}

fn tail(text: &str, keep: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(keep);
    lines[from..].join("\n")
}

/// The row's own log lines about its life: started, claimed, first turn,
/// landed, held by a verifier.
fn row_life(store: &Store, id: &str) -> String {
    let log = store.task(id).and_then(|t| t.section("Log")).unwrap_or_default();
    let keys = ["started", "claimed", "spawn", "first", "landed", "review", "verif", "released", "blocked"];
    let lines: Vec<&str> = log.lines().filter(|l| keys.iter().any(|k| l.to_lowercase().contains(k))).collect();
    tail(&lines.join("\n"), 8)
}

/// The packet for the retro seat, off the figures handed in.
pub fn packet_from(store: &Store, w: &Worklist, effort: &Value, base: &[Figures], cycle: &str) -> String {
    let mut o = String::new();
    let f = Figures::of(effort);
    o.push_str(&format!("# Retro · {} · {}\n\n", w.id, w.title));
    o.push_str(&format!("You are the retro seat of `{}`, a cleared list: {} rows, {} of wall. Read where the wall went, name the causes, and file what is worth filing. Nothing here is a task to implement.\n\n", w.id, f.rows, mins(f.wall)));

    o.push_str("## Rules\n\n");
    for r in [
        "Attribute the wall to its causes within 10 %: the causes you name must add up to the wall to within a tenth of it.",
        "Name every cause that is over 10 minutes in total or over 10 % of the wall — even one with nothing to fix.",
        "Search the open rows before you file anything: `wsp ls`, then `wsp show <row>` on what looks like it.",
        "Where an open row already covers a cause, add the new evidence to it with `wsp retro note` — do not file a duplicate.",
        &format!("File at most {MAX_NEW} new rows."),
        "A **defect** is something that did not do what it was built or documented to do: a bug, a stall that a documented rule should have prevented, a measurement that is wrong. It goes on the planned retro list. A **strategic** question is a choice between ways of working — cap, parallelism, ordering, the shape of a plan, what to spend — where nothing is broken and a person must decide. It goes on the asked list, with `--ask`. When in doubt, strategic.",
        "A retro never edits a plan, a project file, a setting or any other list. It files rows, notes rows, and ends with `done`.",
    ] {
        o.push_str(&format!("- {r}\n"));
    }

    o.push_str("\n## How you file\n\nOnly through these verbs; each FILE is a file you wrote (prose goes through a file, written with `<<'EOF'`):\n\n```\n");
    o.push_str(SYNTAX);
    o.push_str("\n```\n\n`done` takes JSON: a `summary` and an `attribution` table of `{cause, seconds, row?}` entries covering the wall.\n\n");

    o.push_str(&format!("## The wall: {}\n\n", mins(f.wall)));
    for (b, s) in &f.buckets {
        let pct = if f.wall > 0 { *s as f64 * 100.0 / f.wall as f64 } else { 0.0 };
        o.push_str(&format!("- {b}: {} ({pct:.0} %)\n", mins(*s)));
    }
    let wb = &effort["totals"]["wall_buckets"];
    if let Some(a) = wb["waiting_by_cause"].as_array().filter(|a| !a.is_empty()) {
        o.push_str("\nWaiting, by cause:\n");
        for c in a {
            o.push_str(&format!("- {} — {}\n", c["cause"].as_str().unwrap_or(""), mins(c["seconds"].as_i64().unwrap_or(0))));
        }
    }
    if let Some(a) = wb["named"].as_array().filter(|a| !a.is_empty()) {
        o.push_str("\nThe biggest stalls and barrier runs:\n");
        for c in a {
            o.push_str(&format!("- group {} · {} · {}\n", c["group"], c["what"].as_str().unwrap_or(""), mins(c["seconds"].as_i64().unwrap_or(0))));
        }
    }

    o.push_str("\n## effort.json\n\n```json\n");
    o.push_str(&serde_json::to_string_pretty(effort).unwrap_or_default());
    o.push_str("\n```\n");

    o.push_str("\n## The rows: spawn → claimed → first turn → landed → verifier-held\n\n");
    for (gi, g) in effort["groups"].as_array().into_iter().flatten().enumerate() {
        for m in g["members"].as_array().into_iter().flatten() {
            let id = m["task"].as_str().unwrap_or("");
            let s = |k: &str| m[k].as_i64().map_or("—".to_string(), |v| format!("{v}s"));
            o.push_str(&format!(
                "### {id} (group {})\n\nwall {} · spawn to first line {} · verifier spawn to first line {} · verifier tests {}s · verdict to despawn {}\n\n",
                gi + 1,
                s("wall_seconds"),
                s("spawn_to_first_line_seconds"),
                s("verifier_spawn_to_first_line_seconds"),
                m["verifier_test_seconds"].as_i64().unwrap_or(0),
                s("verdict_to_despawn_seconds"),
            ));
            let life = row_life(store, id);
            if !life.is_empty() {
                o.push_str(&format!("```\n{life}\n```\n\n"));
            }
        }
    }

    o.push_str("## The list's log\n\n```\n");
    o.push_str(w.section("Log").unwrap_or_default().trim());
    o.push_str("\n```\n\n## cycle.log lines naming the list\n\n```\n");
    let mine: Vec<&str> = cycle.lines().filter(|l| l.contains(w.id.as_str())).collect();
    o.push_str(&tail(&mine.join("\n"), 300));
    o.push_str("\n```\n\n## Baseline: the last cleared lists\n\n");
    if base.is_empty() {
        o.push_str("No cleared list has left an `effort.json` to compare with.\n");
    }
    for b in base {
        o.push_str(&format!("- {}\n", b.line()));
    }
    o.push_str(&format!("\nThis list: {}\n", f.line()));
    o
}

/// The packet for the retro seat of a cleared list.
pub fn packet(store: &Store, w: &Worklist) -> String {
    let effort = crate::cmd_report::effort(store, w, None);
    let cycle = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
    packet_from(store, w, &effort, &baseline(store, w), &cycle)
}

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["retro"],
    run: retro,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Worklists,
        order: 290,
        text: r##"  wsp retro <list> --packet|--check   the timing retro of a cleared list: its
                                    packet, or whether it gets one and why not"##,
    }],
}];

/// `wsp retro <list> --packet | --check`.
pub fn retro(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(String::as_str) {
        Some("file") if args.rest.len() > 1 => return crate::retro_run::file(store, args),
        Some("note") if args.rest.len() > 1 => return crate::retro_run::note(store, args),
        Some("done") if args.rest.len() > 1 => return crate::retro_run::done(store, args),
        _ => {}
    }
    let Some(slug) = args.rest.first().cloned() else {
        eprintln!("usage: wsp retro <list> [--packet | --check]\n{SYNTAX}");
        return 2;
    };
    let Some(w) = store.worklist(&slug) else {
        eprintln!("wsp: no list `{slug}`");
        return 1;
    };
    if args.has("packet") {
        print!("{}", packet(store, &w));
        return 0;
    }
    if args.has("check") {
        match eligible(store, &w) {
            Ok(()) => println!("{} {}: eligible", "retro", w.id),
            Err(why) => println!("{} {}: not eligible — {why}", "retro", w.id),
        }
        return 0;
    }
    // By hand: a seat over the evidence there is, whatever ran before.
    match crate::retro_run::start(store, &w, true) {
        Ok(row) => {
            println!("{} retro started on {row}", w.id);
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
    use serde_json::json;

    fn list(store: &Store, id: &str) -> Worklist {
        let mut w = Worklist::new(id, "A list");
        w.body = "## Groups\n\n1. a-1 a-2 a-3\n".into();
        store.save_worklist(&w).unwrap();
        w
    }

    fn effort(list: &str) -> Value {
        json!({
            "list": list, "generated": "2026-11-07T10:00:00Z",
            "totals": { "wall_seconds": 7200, "wall_buckets": {
                "wall_seconds": 7200, "working_seconds": 3000, "waiting_seconds": 2400, "serialised_seconds": 600,
                "barrier_run_seconds": 600, "check_seat_seconds": 300, "between_groups_seconds": 300,
                "waiting_by_cause": [{ "cause": "blocked on a person", "seconds": 1800 }],
                "named": [{ "group": 1, "bucket": "waiting", "what": "a-2 — blocked", "row": "a-2", "seconds": 1800 }],
            }},
            "groups": [{ "group": 1, "members": [
                { "task": "a-1", "wall_seconds": 900, "spawn_to_first_line_seconds": 40 },
                { "task": "a-2", "wall_seconds": 2000 }, { "task": "a-3", "wall_seconds": 700 } ]}],
        })
    }

    #[test]
    fn the_guards_each_say_why() {
        let (_env, store) = crate::testkit::isolated_store("retro-guards");
        let mut w = list(&store, "l");
        assert_eq!(eligible_with(&store, &w, 3, 1800), Ok(()));
        assert!(eligible_with(&store, &w, 2, 7200).unwrap_err().contains("at least 3"));
        assert!(eligible_with(&store, &w, 3, 1799).unwrap_err().contains("wall"));
        w.no_retro = true;
        assert!(eligible_with(&store, &w, 9, 9999).unwrap_err().contains("--no-retro"));
        w.no_retro = false;
        w.retro = Some(("2026-11-07T10:00:00Z".into(), "cpd-1".into()));
        assert!(eligible_with(&store, &w, 9, 9999).unwrap_err().contains("already ran"));
    }

    #[test]
    fn the_flag_and_the_record_survive_a_save() {
        let (_env, store) = crate::testkit::isolated_store("retro-record");
        let mut w = list(&store, "l");
        w.no_retro = true;
        w.retro = Some(("t".into(), "s".into()));
        store.save_worklist(&w).unwrap();
        let r = store.worklist("l").unwrap();
        assert!(r.no_retro);
        assert_eq!(r.retro, Some(("t".into(), "s".into())));
    }

    #[test]
    fn a_project_saying_retro_off_stops_it() {
        let (_env, store) = crate::testkit::isolated_store("retro-off");
        let mut p = crate::model::Project::new("proj");
        p.body = "## Settings\n\nretro = off\n".into();
        store.save_project(&p).unwrap();
        let mut w = list(&store, "l");
        w.project = Some("proj".into());
        assert!(eligible_with(&store, &w, 9, 9999).unwrap_err().contains("retro = off"));
    }

    #[test]
    fn the_packet_carries_the_figures_the_rules_and_the_syntax() {
        let (_env, store) = crate::testkit::isolated_store("retro-packet");
        let w = list(&store, "l");
        let base = vec![Figures::of(&effort("old-1"))];
        let p = packet_from(&store, &w, &effort("l"), &base, "x l group 2: waits — room\nx other\n");
        for want in ["within 10 %", "10 minutes", "at most 5 new rows", "When in doubt, strategic", "never edits a plan", SYNTAX, "blocked on a person", "a-2 — blocked", "### a-1 (group 1)", "waits — room", "old-1 · 3 rows · wall 120m", "\"wall_seconds\": 7200"] {
            assert!(p.contains(want), "packet lacks `{want}`:\n{p}");
        }
        assert!(!p.contains("x other"), "only the list's own cycle lines");
    }

    #[test]
    fn the_baseline_is_the_last_three_cleared_lists_never_itself() {
        let (_env, store) = crate::testkit::isolated_store("retro-baseline");
        let w = list(&store, "now");
        for (i, id) in ["c1", "c2", "c3", "c4"].iter().enumerate() {
            let mut o = list(&store, id);
            o.set_status(crate::model::WorklistStatus::Done);
            store.save_worklist(&o).unwrap();
            let mut e = effort(id);
            e["generated"] = json!(format!("2026-11-0{}T10:00:00Z", i + 1));
            let d = crate::cmd_report::dir(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("effort.json"), e.to_string()).unwrap();
        }
        let b = baseline(&store, &w);
        assert_eq!(b.iter().map(|f| f.list.as_str()).collect::<Vec<_>>(), ["c4", "c3", "c2"]);
    }
}
