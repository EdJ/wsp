//! The timing retro of a cleared worklist (`wsp-472`): when a list clears, a
//! seat reads where its wall went and files what it finds. This half is the
//! evidence — the guards that say whether a list gets a retro at all, and the
//! packet the retro seat is handed. The verbs the seat files through
//! (`wsp retro file | note | done`) are the next leg's.

use serde_json::Value;

use crate::model::Worklist;
use crate::store::Store;
use crate::base::args::Args;

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
wsp retro done <list> FILE
wsp retro withdraw <list> [--notes]
wsp retro show <list>
wsp retro start <list> [--replaces ROW]";

/// The bucket names of the wall, as `effort.json` spells them under
/// `totals.wall_buckets` (`<name>_seconds`).
pub const BUCKETS: &[&str] = &crate::wallbuckets::BUCKETS;

/// A cleared list's wall figures: what the baseline is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Figures {
    pub list: String,
    pub wall: i64,
    pub rows: usize,
    pub buckets: Vec<(String, i64)>,
    /// Of `wall`, the seconds a person parked the whole list (`compounding-464`):
    /// the list stood still, so a retro attributes `working_wall()`.
    pub parked: i64,
    /// Where the list is still running (no pass for its last started group): the
    /// time its wall was cut at, which is then to that time and not to a last
    /// pass (`wsp-628`).
    pub cut_at: Option<String>,
}

impl Figures {
    pub fn of(effort: &Value) -> Figures {
        let wb = &effort["totals"]["wall_buckets"];
        let secs = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        let rows = effort["groups"].as_array().map_or(0, |gs| gs.iter().map(|g| g["members"].as_array().map_or(0, Vec::len)).sum());
        Figures {
            list: effort["list"].as_str().unwrap_or("").to_string(),
            // The header's wall is the buckets' total, so the lines under it sum
            // to it (`compounding-468`: 629m over buckets that summed to 622m).
            // An older list with no `attributed_seconds` keeps the larger wall.
            wall: wb.get("attributed_seconds").and_then(Value::as_i64).filter(|a| *a > 0).unwrap_or_else(|| secs(wb, "wall_seconds").max(secs(&effort["totals"], "wall_seconds"))),
            rows,
            // An older list left no `wall_buckets`: it has no figure, not zeros.
            // `parked` is shown only where a person parked the list.
            buckets: if wb.is_object() { BUCKETS.iter().map(|b| (b.to_string(), secs(wb, &format!("{b}_seconds")))).filter(|(b, s)| *s > 0 || *b != "parked").collect() } else { Vec::new() },
            parked: secs(wb, "parked_seconds"),
            cut_at: effort["totals"]["cut_at"].as_str().filter(|_| effort["totals"]["still_running"] == true).map(str::to_string),
        }
    }

    /// The wall without the stretches a person parked the list.
    pub fn working_wall(&self) -> i64 {
        (self.wall - self.parked).max(0)
    }

    /// `1310m`, or `642m without a 668m park` where a person parked the list.
    pub fn wall_words(&self) -> String {
        let w = if self.parked > 0 {
            format!("{} ({} without a {} park)", mins(self.wall), mins(self.working_wall()), mins(self.parked))
        } else {
            mins(self.wall)
        };
        match &self.cut_at {
            Some(t) => format!("{w} so far, still running, cut at {t}"),
            None => w,
        }
    }

    fn line(&self) -> String {
        if self.buckets.is_empty() {
            return format!("{} · {} rows · wall {} · no bucket figures", self.list, self.rows, self.wall_words());
        }
        let parts: Vec<String> = self.buckets.iter().map(|(b, s)| format!("{b} {}", mins(*s))).collect();
        format!("{} · {} rows · wall {} · {}", self.list, self.rows, self.wall_words(), parts.join(" · "))
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
    if crate::retro_series::is_series_id(&w.id) || crate::retro_series::is_retros_list(w) {
        return Err(format!("{} is a retro list; a retro list gets no retro", w.id));
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
    eligible_with(store, w, f.rows.max(w.groups().iter().map(|g| g.members.len()).sum()), f.working_wall())
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

/// The retro doc, compiled in: its `## Standing questions` section goes into every packet.
const DOC: &str = include_str!("../docs/retro.md");

/// The body of the `## Standing questions` section of `doc`, up to the next `## ` heading.
pub fn standing_questions(doc: &str) -> String {
    let mut out = Vec::new();
    let mut on = false;
    for l in doc.lines() {
        if l.starts_with("## ") {
            if on {
                break;
            }
            on = l.trim_end() == "## Standing questions";
            continue;
        }
        if on {
            out.push(l);
        }
    }
    out.join("\n").trim().to_string()
}

/// The packet for the retro seat, off the figures handed in.
pub fn packet_from(store: &Store, w: &Worklist, effort: &Value, base: &[Figures], cycle: &str) -> String {
    let mut o = String::new();
    let f = Figures::of(effort);
    o.push_str(&format!("# Retro · {} · {}\n\n", w.id, w.title));
    if f.cut_at.is_some() {
        o.push_str(&format!("You are the retro seat of `{}`, a list still running: {} rows, {} of wall. The list has no pass yet, so the wall is partial — from the first spawn to the cut, not to a last pass — and a retro row on the list counts in it. Read where the wall went so far, name the causes, and file what is worth filing. Nothing here is a task to implement.\n\n", w.id, f.rows, f.wall_words()));
    } else {
        o.push_str(&format!("You are the retro seat of `{}`, a cleared list: {} rows, {} of wall. Read where the wall went, name the causes, and file what is worth filing. Nothing here is a task to implement.\n\n", w.id, f.rows, f.wall_words()));
    }

    o.push_str("## Rules\n\n");
    for r in [
        "Attribute the wall to its causes within 10 %: the causes you name must add up to the wall to within a tenth of it. Where a person parked the whole list the wall is shown with and without the park: the park is not a cause to fix (the list stood still, and who parked it is in the bucket), so attribute the wall without it.",
        "Name every cause that is over 10 minutes in total or over 10 % of the wall — even one with nothing to fix.",
        "Search every project's rows before you file anything: `wsp ls --all` (every project, rows at review included, and the `landed` ones too — a bare `wsp ls` lists only your own project), then `wsp show <row>` on what looks like it. Where one of these commands is denied (`wsp find --all`, a second `wsp ls --all`, `wsp show`), do not stop: file the unambiguous defects anyway, and name in each row's evidence the search you could not finish.",
        "Where an open row already covers a cause, add the new evidence to it with `wsp retro note` — do not file a duplicate. Where a `landed` row already fixed the cause, note it on that row (\"recurs / fixed by\") instead of filing it again.",
        &format!("File at most {MAX_NEW} new rows."),
        "A **defect** is something that did not do what it was built or documented to do: a bug, a stall that a documented rule should have prevented, a measurement that is wrong. It goes on the planned retro list. A **strategic** question is a choice between ways of working — cap, parallelism, ordering, the shape of a plan, what to spend — where nothing is broken and a person must decide. It goes on the asked list, with `--ask`. When in doubt, strategic.",
        "A retro never edits a plan, a project file, a setting or any other list. It files rows, notes rows, and ends with `done`.",
    ] {
        o.push_str(&format!("- {r}\n"));
    }

    o.push_str("\n## How you file\n\nOnly through these verbs; each FILE is a file you wrote (prose goes through a file, written with `<<'EOF'`):\n\n```\n");
    o.push_str(SYNTAX);
    o.push_str("\n```\n\n`done` takes prose: a summary in plain text, with the causes you attributed the wall to and what you filed or noted. Nothing in it is parsed; `retro.html` shows it as written.\n\n");

    let standing = standing_questions(DOC);
    if !standing.is_empty() {
        o.push_str("## Standing questions\n\nAnswer each standing question below for this list in `done` — a sentence or two, with figures from effort.json. File under `--strategic` only when this list's evidence makes the case; a question with nothing to show is answered \"nothing here\", not filed.\n\n");
        o.push_str(&standing);
        o.push_str("\n\n");
    }

    o.push_str(&format!("## The wall: {}\n\n", f.wall_words()));
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
        text: r##"  wsp retro <list> | show <list>      the retro that ran on a cleared list: row, seat, the
                                    `done` text, outputs; starts nothing
  wsp retro <list> --packet|--check   its packet, or whether it gets a retro and why not
  wsp retro start <list> [--replaces ROW]
                                    start a retro by hand; refused while the list's retro
                                    row is open or at review unless ROW names it, and
                                    from a verifier seat"##,
    }],
}];

/// `wsp retro <list> --packet | --check`.
pub fn retro(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(String::as_str) {
        Some("file") if args.rest.len() > 1 => return crate::retro_run::file(store, args),
        Some("note") if args.rest.len() > 1 => return crate::retro_run::note(store, args),
        Some("done") if args.rest.len() > 1 => return crate::retro_run::done(store, args),
        Some("withdraw") if args.rest.len() > 1 => return crate::retro_run::withdraw(store, args),
        Some("show") if args.rest.len() > 1 => return crate::retro_run::show(store, &args.rest[1]),
        Some("start") if args.rest.len() > 1 => return crate::retro_run::start_by_hand(store, args),
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
    if args.has("at-clear") {
        // What a clear launches (`cycle::launch_out`), so the engine names no
        // module above it: the guards run here, and a skip is logged.
        crate::retro_run::at_clear(store, &w);
        return 0;
    }
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
    // Bare: read, never start. Starting takes `wsp retro start <list>`.
    crate::retro_run::show(store, &slug)
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
    fn a_retro_series_list_gets_no_retro_even_with_the_flag_off() {
        let (_env, store) = crate::testkit::isolated_store("retro-guards-series");
        for id in ["retro-7", "retro-ask-2"] {
            let w = list(&store, id);
            assert!(!w.no_retro);
            assert!(eligible_with(&store, &w, 9, 9999).unwrap_err().contains("a retro list gets no retro"), "{id}");
        }
        let w = list(&store, "retro-notes");
        assert_eq!(eligible_with(&store, &w, 9, 9999), Ok(()));
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
    fn the_packets_bucket_list_reads_each_of_wall_buckets_by_its_name() {
        let mut e = json!({ "totals": { "wall_buckets": {} } });
        for (n, b) in crate::wallbuckets::BUCKETS.iter().enumerate() {
            e["totals"]["wall_buckets"][format!("{b}_seconds")] = json!(60 * (n + 1));
        }
        let got = Figures::of(&e).buckets;
        let of = |name: &str| got.iter().find(|(b, _)| b == name).map(|(_, s)| *s);
        assert_eq!((of("waiting"), of("check_seat")), (Some(120), Some(300)), "{got:?}");
        assert_eq!(got.len(), crate::wallbuckets::BUCKETS.len(), "{got:?}");
    }

    #[test]
    fn the_headers_wall_is_the_buckets_total_so_the_lines_under_it_sum_to_it() {
        // totals.wall_seconds reads 3780 s; the buckets are 3600 s and say so.
        let mut e = effort("l");
        e["totals"]["wall_seconds"] = json!(3780);
        let wb = &mut e["totals"]["wall_buckets"];
        for (k, v) in [("wall_seconds", 3600), ("working_seconds", 1000), ("waiting_seconds", 1200), ("serialised_seconds", 600), ("barrier_run_seconds", 300), ("check_seat_seconds", 300), ("between_groups_seconds", 200), ("attributed_seconds", 3600)] {
            wb[k] = json!(v);
        }
        let f = Figures::of(&e);
        assert_eq!(f.wall, 3600);
        assert_eq!(f.buckets.iter().map(|(_, s)| s).sum::<i64>(), f.wall, "{f:?}");
        assert!(f.line().contains("wall 60m"), "{}", f.line());
    }

    #[test]
    fn the_packet_carries_the_figures_the_rules_and_the_syntax() {
        let (_env, store) = crate::testkit::isolated_store("retro-packet");
        let w = list(&store, "l");
        let base = vec![Figures::of(&effort("old-1"))];
        let p = packet_from(&store, &w, &effort("l"), &base, "x l group 2: waits — room\nx other\n");
        for want in ["wsp ls --all", "the search you could not finish", "`landed` row already fixed", "`done` takes prose", "within 10 %", "10 minutes", "at most 5 new rows", "When in doubt, strategic", "never edits a plan", SYNTAX, "blocked on a person", "a-2 — blocked", "### a-1 (group 1)", "waits — room", "old-1 · 3 rows · wall 120m", "\"wall_seconds\": 7200"] {
            assert!(p.contains(want), "packet lacks `{want}`:\n{p}");
        }
        assert!(!p.contains("x other"), "only the list's own cycle lines");
    }

    #[test]
    fn the_packet_carries_the_standing_questions_verbatim() {
        let (_env, store) = crate::testkit::isolated_store("retro-standing");
        let w = list(&store, "l");
        let p = packet_from(&store, &w, &effort("l"), &[], "");
        let want = standing_questions(DOC);
        assert!(want.contains("The governor cycle"), "docs/retro.md lacks the governor-cycle question:\n{want}");
        assert!(p.contains("## Standing questions\n"), "{p}");
        assert!(p.contains(&want), "the section is carried verbatim:\n{p}");
        assert!(p.contains("Answer each standing question"), "{p}");
        assert!(p.contains("only when this list's evidence makes the case"), "{p}");
        assert!(p.find("## Standing questions") < p.find("## The wall"));
    }

    #[test]
    fn the_section_ends_at_the_next_heading() {
        let doc = "# T\n\n## Standing questions\n\n- **A.** q\n\n## Other\n\nx\n";
        assert_eq!(standing_questions(doc), "- **A.** q");
        assert_eq!(standing_questions("# T\n## Other\n"), "");
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

    /// A list a person parked: the packet and the baseline line give the wall with
    /// and without the park, and eligibility reads the working wall (`compounding-464`).
    #[test]
    fn a_parked_list_shows_the_wall_with_and_without_the_park() {
        let mut e = effort("l");
        e["totals"]["wall_buckets"]["parked_seconds"] = json!(4000);
        e["totals"]["wall_buckets"]["working_seconds"] = json!(0);
        let f = Figures::of(&e);
        assert_eq!((f.wall, f.parked, f.working_wall()), (7200, 4000, 3200));
        assert_eq!(f.wall_words(), "120m (53m without a 67m park)");
        assert!(f.line().contains("wall 120m (53m without a 67m park)") && f.line().contains("parked 67m"), "{}", f.line());
        let plain = Figures::of(&effort("l"));
        assert_eq!(plain.wall_words(), "120m");
        assert!(!plain.buckets.iter().any(|(b, _)| b == "parked"), "no park, no bucket line");
    }
}
