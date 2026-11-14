//! The running half of the timing retro (`wsp-472`): the trigger when a list
//! clears, the three verbs the retro seat files through, `retro.html` and
//! `retro.json` beside the other reports, and the line in `wsp yours`.

use serde_json::{json, Value};

use crate::cmd_report::{esc, write};
use crate::retro_yours::{arr, state, FILE};
use crate::model::{Status, Task, Worklist};
use crate::retro::{self, Figures, MAX_NEW};
use crate::retro_series as series;
use crate::store::Store;
use crate::util;
use crate::base::args::Args;

fn save(store: &Store, w: &Worklist, s: &Value, note: &str) -> Result<(), String> {
    write(&w.id, FILE, &serde_json::to_string_pretty(s).unwrap_or_default(), note)?;
    write(&w.id, "retro.html", &html(store, w, s), note)?;
    Ok(())
}

fn mins(s: i64) -> String {
    format!("{}m", (s.max(0) + 30) / 60)
}

/// `retro.html`: where the wall went against the baseline, each cause linked
/// to its row, the defects apart from the questions waiting for the person.
pub fn html(_store: &Store, w: &Worklist, s: &Value) -> String {
    let mut b = format!("<h1>Retro · {}</h1><p>{}</p>", esc(&w.id), esc(&w.title));
    b.push_str("<p class=\"dim\">Time and cost are in <a href=\"effort.html\">effort.html</a>; the work in <a href=\"work.html\">work.html</a>.</p>");
    // A baseline list with no bucket figures (an older `effort.json`) has no figure.
    let base: Vec<&Value> = arr(s, "baseline").iter().filter(|f| f["buckets"].as_object().is_some_and(|o| o.values().filter_map(Value::as_i64).sum::<i64>() > 0)).collect();
    // Where a person parked the whole list, the wall without the park beside the
    // full one: a retro attributes the working wall, not a night off (`compounding-464`).
    if let Some(wall) = s["wall"].as_i64().filter(|_| s["parked"].as_i64().unwrap_or(0) > 0) {
        let parked = s["parked"].as_i64().unwrap_or(0);
        b.push_str(&format!("<p>Wall {}; without the {} a person parked the list, {}.</p>", mins(wall), mins(parked), mins((wall - parked).max(0))));
    }
    b.push_str("<h2>Where the time went</h2><table><thead><tr><th>bucket</th><th>this list</th><th>baseline</th><th>ratio</th></tr></thead><tbody>");
    for (name, secs) in s["buckets"].as_object().into_iter().flatten() {
        let secs = secs.as_i64().unwrap_or(0);
        let others: Vec<i64> = base.iter().filter_map(|f| f["buckets"][name].as_i64()).collect();
        let avg = if others.is_empty() { None } else { Some(others.iter().sum::<i64>() / others.len() as i64) };
        let ratio = match avg {
            Some(a) if a > 0 => format!("{:.1}×", secs as f64 / a as f64),
            _ => if avg.is_none() { "no figure".into() } else { "—".into() },
        };
        b.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{ratio}</td></tr>",
            esc(name),
            mins(secs),
            avg.map(mins).unwrap_or_else(|| "no figure".into())
        ));
    }
    b.push_str("</tbody></table>");
    let filed = arr(s, "filed");
    let row = |f: &Value| {
        format!(
            "<tr><td>{}</td><td><a href=\"wsp:{}\">{}</a></td><td>{}</td></tr>",
            esc(f["cause"].as_str().unwrap_or("")),
            esc(f["row"].as_str().unwrap_or("")),
            esc(f["row"].as_str().unwrap_or("")),
            esc(f["list"].as_str().unwrap_or(""))
        )
    };
    for (kind, head) in [("defect", "Defects"), ("question", "Questions waiting for the person")] {
        let mine: Vec<&Value> = filed.iter().filter(|f| f["kind"] == kind).collect();
        b.push_str(&format!("<h2>{head}</h2>"));
        if mine.is_empty() {
            b.push_str("<p>None.</p>");
        } else {
            b.push_str("<table><thead><tr><th>cause</th><th>row</th><th>list</th></tr></thead><tbody>");
            for f in mine {
                b.push_str(&row(f));
            }
            b.push_str("</tbody></table>");
        }
    }
    let notes = arr(s, "notes");
    if !notes.is_empty() {
        b.push_str("<h2>Noted on existing rows</h2><ul>");
        for n in notes {
            b.push_str(&format!("<li><a href=\"wsp:{0}\">{0}</a> — {1}</li>", esc(n["row"].as_str().unwrap_or("")), esc(n["text"].as_str().unwrap_or(""))));
        }
        b.push_str("</ul>");
    }
    if let Some(t) = s["summary"].as_str() {
        // A seat that handed `done` JSON still gets its summary shown as text.
        let text = serde_json::from_str::<Value>(t).ok().and_then(|v| v["summary"].as_str().map(str::to_string)).unwrap_or_else(|| t.to_string());
        let by = s["done_by"].as_str().map(|b| format!("<p class=\"dim\">Written by {}.</p>", esc(b))).unwrap_or_default();
        b.push_str(&format!("<h2>Summary</h2>{by}<pre>{}</pre>", esc(&text)));
    } else {
        b.push_str("<p class=\"dim\">The retro seat has not finished.</p>");
    }
    b
}

/// A row for the retro seat to sit on: its packet is the row's overview.
fn seat_row(store: &Store, w: &Worklist) -> Result<Task, String> {
    let project = crate::worklist::home(store, w).map(|h| h.project).or_else(|| w.project.clone());
    let id = store.alloc_task_id(project.as_deref()).map_err(|e| format!("cannot allocate id: {e}"))?;
    let mut t = Task::new(&format!("Timing retro of {}", w.id), &id);
    t.project = project.clone();
    // The tag a project's `light:` can name: a retro builds nothing (`wsp-601`).
    t.tags = vec!["retro".into()];
    if project.is_none() {
        t.status_raw = "inbox".into();
    }
    crate::model::set_section_in(&mut t.body, "Overview", retro::packet(store, w).trim());
    store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
    store.log_event("task-added", json!({ "id": t.id, "project": t.project, "title": t.title }));
    Ok(t)
}

/// Start a retro seat over `w`: a row carrying the packet, a record on the
/// list so a clear has one, the evidence file, and the row placed on the retros
/// list — which the run starts; nothing is spawned here (`wsp-558`). `by_hand` skips
/// the guards (and the one-per-clear record) but not the evidence.
pub fn start(store: &Store, w: &Worklist, by_hand: bool) -> Result<String, String> {
    if !by_hand {
        retro::eligible(store, w)?;
    }
    let t = seat_row(store, w)?;
    let on = crate::retro_series::place_retro(store, &t.id, t.project.as_deref())?;
    let mut w = store.worklist(&w.id).unwrap_or_else(|| w.clone());
    w.retro = Some((util::now_iso(), t.id.clone()));
    w.log(&format!("timing retro — {} on {on} {}", t.id, if by_hand { "by hand" } else { "at the clear" }));
    store.save_worklist(&w).map_err(|e| format!("write failed: {e}"))?;
    let f = Figures::of(&crate::cmd_report::effort(store, &w, None));
    let mut s = match state(&w.id) {
        Value::Object(o) => Value::Object(o),
        _ => json!({ "filed": [], "notes": [] }),
    };
    s["list"] = json!(w.id);
    s["row"] = json!(t.id);
    s["retros"] = json!(on);
    s["started"] = json!(util::now_iso());
    s["wall"] = json!(f.wall);
    s["parked"] = json!(f.parked);
    s["rows"] = json!(f.rows);
    s["buckets"] = json!(f.buckets.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>());
    s["baseline"] = json!(retro::baseline(store, &w).iter().map(|b| json!({ "list": b.list, "wall": b.wall, "buckets": b.buckets.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, Value>>() })).collect::<Vec<_>>());
    s["done"] = json!(false);
    s["summary"] = Value::Null;
    save(store, &w, &s, "timing retro started")?;
    store.log_event("retro-started", json!({ "list": w.id, "row": t.id, "retros": on, "by_hand": by_hand }));
    Ok(t.id)
}

/// The retro of record on a list, as text: row, seat, state, the `done` text and
/// the outputs. Reads only.
pub fn show_text(store: &Store, w: &Worklist) -> String {
    let s = state(&w.id);
    let Some((_, row)) = w.retro.clone() else {
        return format!("{}: no retro has run — `wsp retro start {}` starts one\n", w.id, w.id);
    };
    let t = store.task(&row);
    let seat = store
        .claims()
        .get(&row)
        .and_then(|c| c["pane"].as_str().or_else(|| c["seat"].as_str()).map(str::to_string))
        .unwrap_or_else(|| "—".into());
    let mut o = format!(
        "{} retro: {row} · seat {seat} · {} · on {}\n",
        w.id,
        t.as_ref().map(|t| t.status_raw.clone()).unwrap_or_else(|| "no row".into()),
        s["retros"].as_str().unwrap_or("—")
    );
    if s["done"] == true {
        o.push_str(&format!("done {}{}\n\n{}\n", s["finished"].as_str().unwrap_or(""), s["done_by"].as_str().map(|b| format!(" by {b}")).unwrap_or_default(), s["summary"].as_str().unwrap_or("").trim()));
    } else {
        o.push_str("not done: the retro seat has not recorded its `done`\n");
    }
    for f in arr(&s, "filed") {
        o.push_str(&format!("filed {} on {}\n", f["row"].as_str().unwrap_or("—"), f["list"].as_str().unwrap_or("—")));
    }
    let d = crate::cmd_report::dir(&w.id);
    for name in [FILE, "retro.html"] {
        if d.join(name).is_file() {
            o.push_str(&format!("output {}\n", d.join(name).display()));
        }
    }
    o
}

/// `wsp retro <list>` and `wsp retro show <list>`: print the retro, start nothing.
pub fn show(store: &Store, slug: &str) -> i32 {
    let Some(w) = store.worklist(slug) else { return fail(&format!("no list `{slug}`")) };
    print!("{}", show_text(store, &w));
    0
}

/// Why a hand start is refused, if it is: from a verifier seat, or while the
/// list's retro row is still open or at review and not named as replaced.
pub fn start_refusal(store: &Store, w: &Worklist, replaces: Option<&str>, seat: Option<&str>) -> Option<String> {
    if let Some(seat) = seat {
        if let Some(member) = crate::verification::seats(&store.tasks()).get(seat) {
            return Some(format!("a verifier seat is read-only (it verifies {member}); `wsp retro {}` shows the retro without starting one", w.id));
        }
    }
    let (_, row) = w.retro.clone()?;
    let t = store.task(&row)?;
    if t.status() != Status::Done && replaces != Some(row.as_str()) {
        return Some(format!(
            "{}'s retro {row} is still {} — `wsp retro {}` shows it; to start another over it, `wsp retro start {} --replaces {row}`",
            w.id, t.status_raw, w.id, w.id
        ));
    }
    None
}

/// `wsp retro start <list> [--replaces ROW]`.
pub fn start_by_hand(store: &Store, args: &Args) -> i32 {
    let Some(w) = args.rest.get(1).and_then(|s| store.worklist(s)) else {
        return fail(&format!("no list `{}`", args.rest.get(1).map(String::as_str).unwrap_or("")));
    };
    let replaces = args.get("replaces");
    if let Some(why) = start_refusal(store, &w, replaces.as_deref(), crate::place::my_pane().as_deref()) {
        return fail(&why);
    }
    match start(store, &w, true) {
        Ok(row) => {
            println!("{} retro started on {row}", w.id);
            0
        }
        Err(e) => fail(&e),
    }
}

/// When a list clears: a retro if the guards pass. Quiet when they do not.
pub fn at_clear(store: &Store, w: &Worklist) {
    let Some(w) = store.worklist(&w.id) else { return };
    if w.is_cancelled() {
        return;
    }
    if let Err(why) = start(store, &w, false) {
        store.log_event("retro-skipped", json!({ "list": w.id, "why": why }));
    }
}

fn read(path: &str) -> Result<String, String> {
    use std::io::Read;
    let mut s = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
    } else {
        s = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    }
    Ok(s)
}

fn words(t: &str) -> std::collections::BTreeSet<String> {
    t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(str::to_string).collect()
}

/// The open row whose title the new one is too close to, if any.
fn near_duplicate(store: &Store, title: &str) -> Option<(String, String)> {
    let a = words(title);
    if a.is_empty() {
        return None;
    }
    store.tasks().into_iter().filter(|t| t.status() != Status::Done).find_map(|t| {
        let b = words(&t.title);
        let both = a.intersection(&b).count() as f64;
        let any = a.union(&b).count() as f64;
        (any > 0.0 && both / any >= 0.6).then(|| (t.id.clone(), t.title.clone()))
    })
}

fn fail(msg: &str) -> i32 {
    eprintln!("wsp: {msg}");
    1
}

/// Why the caller may not write `w`'s retro, if it may not: its seat is bound
/// to a row that is not the retro row of record (`s["row"]`). A hand outside
/// any seat, or in a pane bound to no row, is let through (`wsp-626`). Pure
/// over the bindings and the pane, so each caller is proved without an
/// environment.
pub fn wrong_seat(store: &Store, w: &Worklist, s: &Value, pane: Option<&str>) -> Option<String> {
    let pane = pane.filter(|p| !p.is_empty())?;
    let mine = store.bindings().get(pane).and_then(|b| b["task_id"].as_str()).filter(|t| !t.is_empty()).map(str::to_string)?;
    let record = s["row"].as_str().or_else(|| w.retro.as_ref().map(|(_, r)| r.as_str()));
    if record == Some(mine.as_str()) {
        return None;
    }
    let of = store.worklists().into_iter().find(|l| l.retro.as_ref().is_some_and(|(_, r)| *r == mine));
    let who = match of {
        Some(l) => format!("you are the retro of {} ({mine})", l.id),
        None => format!("you are on {mine}, which is not a retro row"),
    };
    Some(format!("{who}; this is {}'s, whose retro is {} — a retro seat writes only its own list's retro", w.id, record.unwrap_or("not on a row")))
}

/// Who is running the verb: its pane, when it is in one.
fn caller_pane() -> Option<String> {
    crate::place::my_pane()
}

/// The seat that recorded a `done`, as `retro show` and `retro.html` say it:
/// the pane, else a hand.
fn writer(store: &Store) -> String {
    match caller_pane().filter(|p| !p.is_empty()) {
        Some(p) => match store.bindings().get(&p).and_then(|b| b["task_id"].as_str()) {
            Some(row) => format!("{p} on {row}"),
            None => p,
        },
        None => "a hand outside any seat".into(),
    }
}

fn list_of(store: &Store, args: &Args) -> Result<(Worklist, Value), i32> {
    let Some(slug) = args.rest.get(1) else {
        eprintln!("usage:\n{}", retro::SYNTAX);
        return Err(2);
    };
    let Some(w) = store.worklist(slug) else { return Err(fail(&format!("no list `{slug}`"))) };
    let s = state(&w.id);
    if s.is_null() || s["done"] == true {
        return Err(fail(&format!("{} has no retro running — `wsp retro show {}` reads the one that ran, `wsp retro start {}` starts one", w.id, w.id, w.id)));
    }
    if let Some(why) = wrong_seat(store, &w, &s, caller_pane().as_deref()) {
        return Err(fail(&why));
    }
    Ok((w, s))
}

pub fn file(store: &Store, args: &Args) -> i32 {
    let (w, mut s) = match list_of(store, args) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let (defect, strategic) = (args.has("defect"), args.has("strategic"));
    if defect == strategic {
        return fail("say --defect or --strategic, one of them");
    }
    let Some(title) = args.get("title") else { return fail("--title is needed") };
    let run = s["row"].as_str().unwrap_or("").to_string();
    let this_run = arr(&s, "filed").iter().filter(|f| f["run"] == run.as_str()).count();
    if this_run >= MAX_NEW {
        return fail(&format!("{MAX_NEW} rows filed already — a retro files at most {MAX_NEW}. List the rest in your summary (`wsp retro done`)"));
    }
    if let Some((id, t)) = near_duplicate(store, &title) {
        return fail(&format!("`{title}` is too close to the open row {id} ({t}) — add what you found with `wsp retro note {} {id} FILE` instead", w.id));
    }
    let get = |flag: &str| -> Result<String, i32> {
        let p = args.get(flag).ok_or_else(|| fail(&format!("--{flag} FILE is needed")))?;
        read(&p).map_err(|e| fail(&e))
    };
    let project = crate::worklist::home(store, &w).map(|h| h.project).or_else(|| w.project.clone());
    let made = if defect {
        let (e, f, d) = match (get("evidence"), get("fix"), get("done-when")) {
            (Ok(e), Ok(f), Ok(d)) => (e, f, d),
            (Err(c), _, _) | (_, Err(c), _) | (_, _, Err(c)) => return c,
        };
        series::file_defect(store, &title, &e, &f, &d, project.as_deref()).map(|id| (id, series::DEFECTS))
    } else {
        let (e, a) = match (get("evidence"), get("ask")) {
            (Ok(e), Ok(a)) => (e, a),
            (Err(c), _) | (_, Err(c)) => return c,
        };
        let ask: Value = match serde_json::from_str(&a) {
            Ok(v) => v,
            Err(e) => return fail(&format!("--ask is not JSON: {e}")),
        };
        series::file_question(store, &title, &e, &ask, project.as_deref()).map(|id| (id, series::QUESTIONS))
    };
    match made {
        Ok((id, series_name)) => {
            let list = store.worklists().into_iter().find(|l| l.groups().iter().any(|g| g.members.contains(&id))).map(|l| l.id).unwrap_or_else(|| series_name.to_string());
            let mut f = arr(&s, "filed").to_vec();
            f.push(json!({ "kind": if defect { "defect" } else { "question" }, "row": id, "list": list, "cause": title, "run": run }));
            s["filed"] = json!(f);
            if let Err(e) = save(store, &w, &s, "timing retro filed a row") {
                return fail(&e);
            }
            println!("{id} filed on {list} ({} of {MAX_NEW})", this_run + 1);
            0
        }
        Err(e) => fail(&e),
    }
}

pub fn note(store: &Store, args: &Args) -> i32 {
    let (w, mut s) = match list_of(store, args) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let (Some(row), Some(path)) = (args.rest.get(2), args.rest.get(3)) else {
        eprintln!("usage: wsp retro note <list> <row> FILE");
        return 2;
    };
    let text = match read(path) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    if let Err(e) = series::add_note(store, row, &format!("timing retro of {}: {}", w.id, text.trim())) {
        return fail(&e);
    }
    let mut n = arr(&s, "notes").to_vec();
    n.push(json!({ "row": row, "text": text.trim() }));
    s["notes"] = json!(n);
    if let Err(e) = save(store, &w, &s, "timing retro noted a row") {
        return fail(&e);
    }
    println!("{row} noted");
    0
}

/// Whether the list's retro of record has `done` recorded but its row is open
/// again: the verifier sent it back.
fn sent_back(store: &Store, args: &Args) -> bool {
    let Some(w) = args.rest.get(1).and_then(|s| store.worklist(s)) else { return false };
    let s = state(&w.id);
    s["done"] == true
        && s["row"].as_str().and_then(|r| store.task(r)).is_some_and(|t| !matches!(t.status(), Status::Review | Status::Done))
}

pub fn done(store: &Store, args: &Args) -> i32 {
    // A retro row sent back after its `done` (open again, not at review or
    // done) records `done` again.
    let again = sent_back(store, args);
    let (w, mut s) = if again {
        let w = store.worklist(&args.rest[1]).unwrap();
        let s = state(&w.id);
        if let Some(why) = wrong_seat(store, &w, &s, caller_pane().as_deref()) {
            return fail(&why);
        }
        (w, s)
    } else {
        match list_of(store, args) {
            Ok(x) => x,
            Err(c) => return c,
        }
    };
    let Some(path) = args.rest.get(2) else {
        eprintln!("usage: wsp retro done <list> FILE");
        return 2;
    };
    let text = match read(path) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let none = arr(&s, "filed").is_empty() && arr(&s, "notes").is_empty();
    if text.trim().is_empty() || (none && !text.to_lowercase().contains("nothing")) {
        return fail("you filed and noted nothing — say so in the summary (\"nothing to file, because …\")");
    }
    s["summary"] = json!(text.trim());
    s["done"] = json!(true);
    s["finished"] = json!(util::now_iso());
    s["done_by"] = json!(writer(store));
    if let Err(e) = save(store, &w, &s, "timing retro finished") {
        return fail(&e);
    }
    if let Some(mut t) = s["row"].as_str().and_then(|r| store.task(r)) {
        t.status_raw = "review".into();
        t.log("retro done");
        let _ = store.save_task(&t);
    }
    store.log_event("retro-done", json!({ "list": w.id }));
    println!("{} retro recorded", w.id);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_report::dir;

    fn put(dirp: &std::path::Path, name: &str, text: &str) -> String {
        let p = dirp.join(name);
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn a_clear_starts_a_retro_the_verbs_file_and_yours_says_so() {
        let (env, store) = crate::testkit::isolated_store("retro-run");
        let mut w = Worklist::new("l", "A list");
        w.body = "## Groups\n\n1. a-1 a-2 a-3\n".into();
        for id in ["a-1", "a-2", "a-3"] {
            let mut t = Task::new(&format!("row {id}"), id);
            t.status_raw = "done".into();
            store.save_task(&t).unwrap();
        }
        store.save_worklist(&w).unwrap();
        // the eligibility figures: 3 rows, 2 hours of wall
        assert!(retro::eligible_with(&store, &w, 3, 7200).is_ok());
        assert!(start(&store, &w, false).is_err(), "no wall recorded, so the automatic path declines");
        let seat = start(&store, &w, true).unwrap();
        assert!(store.task(&seat).is_some());
        assert!(store.worklist("l").unwrap().retro.is_some());
        assert!(dir("l").join("retro.html").is_file());
        let pending = crate::retro_yours::yours_lines(&store);
        assert_eq!(pending, vec![format!("l cleared; its retro {seat} is on retro-1")]);

        let tmp = env.home().join("scratch");
        std::fs::create_dir_all(&tmp).unwrap();
        let args = |rest: &[&str], flags: &[(&str, &str)]| Args::synth("retro", rest, flags);
        let ev = put(&tmp, "ev", "start waited 20m unclaimed");
        let fx = put(&tmp, "fx", "claim sooner");
        let dw = put(&tmp, "dw", "start under 2m");
        let code = file(&store, &args(&["file", "l"], &[("defect", ""), ("title", "Unclaimed start waits twenty minutes"), ("evidence", &ev), ("fix", &fx), ("done-when", &dw)]));
        assert_eq!(code, 0);
        assert!(store.worklist("retro-2").is_some(), "the defects go on a list of their own, not the retros list");
        let dup = file(&store, &args(&["file", "l"], &[("defect", ""), ("title", "Unclaimed start waits twenty minutes"), ("evidence", &ev), ("fix", &fx), ("done-when", &dw)]));
        assert_eq!(dup, 1, "a near title is refused");
        let sum = put(&tmp, "sum", "one defect filed");
        assert_eq!(done(&store, &args(&["done", "l", &sum], &[])), 0);
        let html = std::fs::read_to_string(dir("l").join("retro.html")).unwrap();
        assert!(html.contains("Defects") && html.contains("Unclaimed start"));
        let lines = crate::retro_yours::yours_lines(&store);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("l cleared in") && lines[0].contains("1 defects on retro-2") && lines[0].contains("0 questions"), "{}", lines[0]);
    }

    #[test]
    fn a_clear_places_the_retro_on_the_open_retros_list_or_files_one_and_spawns_nothing() {
        let (_env, store) = crate::testkit::isolated_store("retro-placed");
        for id in ["p", "q"] {
            let mut w = Worklist::new(id, "A list");
            w.body = "## Groups\n\n1. a-1\n".into();
            store.save_worklist(&w).unwrap();
        }
        let first = start(&store, &store.worklist("p").unwrap(), true).unwrap();
        let r = store.worklist("retro-1").expect("no retros list open: retro-1 is filed");
        assert!(r.is_planned() && r.alone == Some(false));
        let g = &r.groups()[0];
        assert_eq!((g.members.clone(), g.cap, g.agent.as_str()), (vec![first.clone()], Some(2), "claude sonnet high"));
        assert!(g.stop.contains("verifier holding") && g.stop.contains("no fix"), "{}", g.stop);
        assert!(store.worklist("p").unwrap().retro.is_some());
        let second = start(&store, &store.worklist("q").unwrap(), true).unwrap();
        assert!(store.worklist("retro-2").is_none(), "the open retros list takes the next retro");
        assert_eq!(store.worklist("retro-1").unwrap().groups()[0].members, vec![first.clone(), second]);
        // a retros list whose retro group has passed its barrier is not reused
        let mut r = store.worklist("retro-1").unwrap();
        let mut gs = r.groups();
        gs[0].verdict = "passed".into();
        r.set_groups(&gs);
        store.save_worklist(&r).unwrap();
        let mut w = Worklist::new("s", "A list");
        w.body = "## Groups\n\n1. a-1\n".into();
        store.save_worklist(&w).unwrap();
        let third = start(&store, &w, true).unwrap();
        assert_eq!(store.worklist("retro-2").unwrap().groups()[0].members, vec![third]);
    }

    fn snapshot(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        fn walk(d: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
            let mut es: Vec<_> = std::fs::read_dir(d).into_iter().flatten().flatten().collect();
            es.sort_by_key(|e| e.path());
            for e in es {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push((p.display().to_string(), std::fs::read(&p).unwrap_or_default()));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, &mut out);
        out
    }

    #[test]
    fn a_bare_retro_prints_the_retro_and_starts_nothing() {
        let (env, store) = crate::testkit::isolated_store("retro-bare");
        let mut w = Worklist::new("c", "A list");
        w.body = "## Groups\n\n1. a-1\n".into();
        store.save_worklist(&w).unwrap();
        let seat = start(&store, &w, true).unwrap();
        let tmp = env.home().join("scratch");
        std::fs::create_dir_all(&tmp).unwrap();
        let ok = put(&tmp, "ok", "nothing to file: the wall was inside the baseline");
        assert_eq!(done(&store, &Args::synth("retro", &["done", "c", &ok], &[])), 0);
        assert_eq!(store.task(&seat).unwrap().status_raw, "review");
        let before = (snapshot(&env.home()), snapshot(&env.state()), snapshot(&crate::artefact::root()));
        let w = store.worklist("c").unwrap();
        let text = show_text(&store, &w);
        assert!(text.contains(&seat) && text.contains("nothing to file: the wall was inside the baseline"), "{text}");
        assert_eq!(crate::retro::retro(&store, &Args::synth("retro", &["c"], &[])), 0);
        assert_eq!(crate::retro::retro(&store, &Args::synth("retro", &["show", "c"], &[])), 0);
        let after = (snapshot(&env.home()), snapshot(&env.state()), snapshot(&crate::artefact::root()));
        assert!(before == after, "the store and the artefacts are byte-identical");
        // starting takes its own verb, and is refused over a retro still at review
        assert_eq!(crate::retro::retro(&store, &Args::synth("retro", &["start", "c"], &[])), 1);
        assert!(snapshot(&env.home()) == before.0);
        let why = start_refusal(&store, &w, None, None).unwrap();
        assert!(why.contains("--replaces"), "{why}");
        assert!(start_refusal(&store, &w, Some(&seat), None).is_none());
    }

    #[test]
    fn the_retro_row_of_record_can_record_done_again_after_it_is_sent_back() {
        let (env, store) = crate::testkit::isolated_store("retro-sent-back");
        let mut w = Worklist::new("d", "A list");
        w.body = "## Groups\n\n1. a-1\n".into();
        store.save_worklist(&w).unwrap();
        let seat = start(&store, &w, true).unwrap();
        let tmp = env.home().join("scratch");
        std::fs::create_dir_all(&tmp).unwrap();
        let one = put(&tmp, "one", "nothing to file: first reading");
        let two = put(&tmp, "two", "nothing to file: corrected reading");
        let a = |f: &str| Args::synth("retro", &["done", "d", f], &[]);
        assert_eq!(done(&store, &a(&one)), 0);
        assert_eq!(done(&store, &a(&two)), 1, "at review, the record stands");
        let mut t = store.task(&seat).unwrap();
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
        assert_eq!(done(&store, &a(&two)), 0);
        assert!(show_text(&store, &store.worklist("d").unwrap()).contains("corrected reading"));
        assert_eq!(store.task(&seat).unwrap().status_raw, "review");
    }

    #[test]
    fn a_seat_that_files_nothing_says_so() {
        let (env, store) = crate::testkit::isolated_store("retro-nothing");
        let mut w = Worklist::new("m", "A list");
        w.body = "## Groups\n\n1. a-1\n".into();
        store.save_worklist(&w).unwrap();
        start(&store, &w, true).unwrap();
        let tmp = env.home().join("scratch");
        std::fs::create_dir_all(&tmp).unwrap();
        let bad = put(&tmp, "bad", "all fine");
        let ok = put(&tmp, "ok", "nothing to file: the wall was inside the baseline");
        assert_eq!(done(&store, &Args::synth("retro", &["done", "m", &bad], &[])), 1);
        assert_eq!(done(&store, &Args::synth("retro", &["done", "m", &ok], &[])), 0);
    }

    #[test]
    fn a_rerun_notes_the_open_row_and_a_strategic_finding_goes_to_the_ask_list() {
        let (env, store) = crate::testkit::isolated_store("retro-rerun");
        let mut w = Worklist::new("n", "A list");
        w.body = "## Groups\n\n1. a-1\n".into();
        store.save_worklist(&w).unwrap();
        start(&store, &w, true).unwrap();
        let tmp = env.home().join("scratch");
        std::fs::create_dir_all(&tmp).unwrap();
        let ev = put(&tmp, "ev", "start waited 20m unclaimed");
        let fx = put(&tmp, "fx", "claim sooner");
        let dw = put(&tmp, "dw", "start under 2m");
        let args = |rest: &[&str], flags: &[(&str, &str)]| Args::synth("retro", rest, flags);
        let flags = [("defect", ""), ("title", "Unclaimed start waits twenty minutes"), ("evidence", ev.as_str()), ("fix", fx.as_str()), ("done-when", dw.as_str())];
        assert_eq!(file(&store, &args(&["file", "n"], &flags)), 0);
        // the same finding again is refused, and a note goes on the open row instead
        assert_eq!(file(&store, &args(&["file", "n"], &flags)), 1);
        let rows = store.worklist("retro-2").unwrap().groups()[0].members.clone();
        assert_eq!(rows.len(), 1, "no duplicate");
        let nt = put(&tmp, "nt", "seen again: 18m unclaimed");
        assert_eq!(note(&store, &args(&["note", "n", &rows[0], &nt], &[])), 0);
        assert!(store.task(&rows[0]).unwrap().body.contains("seen again: 18m unclaimed"));
        assert_eq!(store.worklist("retro-2").unwrap().groups()[0].members.len(), 1);
        assert!(store.worklist("retro-ask-1").is_none());
    }

    #[test]
    fn retro_html_shows_the_summary_as_prose_and_an_old_baseline_as_no_figure() {
        let w = Worklist::new("h", "A list");
        let (_env, store) = crate::testkit::isolated_store("retro-html");
        let s = json!({
            "buckets": { "working": 600, "waiting": 300 },
            "baseline": [{ "list": "old", "wall": 7200, "buckets": { "working": 0, "waiting": 0 } }, { "list": "older", "wall": 100, "buckets": {} }],
            "summary": "{\"summary\":\"the wall was a wrong-repo stall\"}",
        });
        let h = html(&store, &w, &s);
        assert!(h.contains("no figure"), "{h}");
        assert!(!h.contains("0m</td><td>—"), "{h}");
        assert!(h.contains("the wall was a wrong-repo stall") && !h.contains("{&quot;summary") && !h.contains("{\"summary"), "{h}");
        let plain = html(&store, &w, &json!({ "buckets": {}, "summary": "nothing to file: inside the baseline" }));
        assert!(plain.contains("<pre>nothing to file: inside the baseline</pre>"));
    }

    #[test]
    fn retro_html_shows_the_wall_without_a_park_beside_the_full_one() {
        let w = Worklist::new("h", "A list");
        let (_env, store) = crate::testkit::isolated_store("retro-html-park");
        let h = html(&store, &w, &json!({ "wall": 7200, "parked": 4000, "buckets": { "parked": 4000 }, "baseline": [] }));
        assert!(h.contains("Wall 120m; without the 67m a person parked the list, 53m."), "{h}");
        let none = html(&store, &w, &json!({ "wall": 7200, "parked": 0, "buckets": {}, "baseline": [] }));
        assert!(!none.contains("parked the list"), "{none}");
    }

    #[test]
    fn a_retro_verb_refuses_a_list_its_seat_is_not_the_retro_of() {
        let (env, store) = crate::testkit::isolated_store("retro-wrong-seat");
        let mut rows = Vec::new();
        for id in ["a", "b"] {
            let mut w = Worklist::new(id, "A list");
            w.body = "## Groups\n\n1. x-1\n".into();
            store.save_worklist(&w).unwrap();
            rows.push(start(&store, &w, true).unwrap());
        }
        store.set_binding("pane-a", json!({ "task_id": rows[0] }));
        store.set_binding("pane-b", json!({ "task_id": rows[1] }));
        let tmp = env.home().join("scratch");
        std::fs::create_dir_all(&tmp).unwrap();
        let ok = put(&tmp, "ok", "nothing to file: the wall was inside the baseline");
        let ev = put(&tmp, "ev", "start waited 20m unclaimed");
        let fx = put(&tmp, "fx", "claim sooner");
        let dw = put(&tmp, "dw", "start under 2m");
        let args = |rest: &[&str], flags: &[(&str, &str)]| Args::synth("retro", rest, flags);
        let flags = [("defect", ""), ("title", "Unclaimed start waits twenty minutes"), ("evidence", ev.as_str()), ("fix", fx.as_str()), ("done-when", dw.as_str())];
        let b_before = (snapshot(&dir("b")), store.task(&rows[1]).unwrap().status_raw);

        std::env::set_var(crate::base::seat::SEAT_ENV, "pane-a");
        assert_eq!(done(&store, &args(&["done", "b", &ok], &[])), 1);
        assert_eq!(note(&store, &args(&["note", "b", &rows[1], &ok], &[])), 1);
        assert_eq!(file(&store, &args(&["file", "b"], &flags)), 1);
        let why = wrong_seat(&store, &store.worklist("b").unwrap(), &state("b"), Some("pane-a")).unwrap();
        assert!(why.contains("you are the retro of a") && why.contains("this is b's"), "{why}");
        assert!(b_before == (snapshot(&dir("b")), store.task(&rows[1]).unwrap().status_raw), "b is unchanged");
        assert!(store.worklist("retro-2").is_none(), "nothing was filed");

        // its own list: file, note, done; and a sent-back retro records done again
        assert_eq!(file(&store, &args(&["file", "a"], &flags)), 0);
        assert_eq!(done(&store, &args(&["done", "a", &ok], &[])), 0);
        assert!(show_text(&store, &store.worklist("a").unwrap()).contains("by pane-a on "), "done says who wrote it");
        assert!(std::fs::read_to_string(dir("a").join("retro.html")).unwrap().contains("Written by pane-a"));
        let mut t = store.task(&rows[0]).unwrap();
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
        assert_eq!(done(&store, &args(&["done", "a", &ok], &[])), 0);
        // and pane-a's seat is still refused on b once a is sent back
        assert_eq!(done(&store, &args(&["done", "b", &ok], &[])), 1);

        // a hand outside any seat writes either
        std::env::remove_var(crate::base::seat::SEAT_ENV);
        assert_eq!(done(&store, &args(&["done", "b", &ok], &[])), 0);
        assert!(show_text(&store, &store.worklist("b").unwrap()).contains("by a hand outside any seat"));
    }
}
