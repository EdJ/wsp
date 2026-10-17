//! A leg is a seat: `docs/legs.md`, *A leg is a seat* and *The baton, and what a
//! leg reads*, decided on wsp-287 d3.
//!
//! **A leg claims nothing.** A row has one claim and `claim` refuses a second
//! live holder, so two siblings could not both hold it. A leg is recorded on its
//! own `## Legs` entry, as a verifier is on its pass (`verification::name_seat`),
//! and its pane is *bound* to the row, so every command that asks which task a
//! pane is on gets the row (`cmd_agent::task_in_hand` reads the binding first).
//! What a claim would have written, the log gets as two lines per leg:
//!
//! ```text
//! leg build-store started by pane cpd-36 · spawned at sonnet/medium
//! leg build-store ended after 41m · ran sonnet-5-5/medium · 63 turns · 4.1M cache …
//! ```
//!
//! The clauses are the claim and release lines' own (`cmd_agent::spawned_at`,
//! `ran_in_seat`), so `cmd_attempts` can read each leg as one attempt.
//!
//! # The choices the doc left open (wsp-292, v1)
//!
//! - **A hand spawn opens the leg.** `wsp spawn <row> --agent --leg <name>` is
//!   also the run's way in (legs-run types it), so it writes `running` under the
//!   store lock before the seat exists and takes the opening back if the seat
//!   never comes. A leg that is not ready is refused, naming what is open.
//! - **A `todo` row becomes `doing`** when its first leg opens, as a claim would
//!   have moved it. A blocked, parked, review or done row is refused.
//! - **`leg done` is the leg's own seat's.** It finds its leg by the pane, as the
//!   brief does, so a person cannot end a leg they are not seated in.
//! - **A tree that is not a git work tree is not refused** by `leg done`'s clean
//!   check: there is nothing to commit, and a sandbox row has none.
//! - **`review` from a leg gives the leg its caption** from the first words of
//!   the account, when it has none, so the table has a line to show.
//! - **Not here:** the sibling tree and the merge (legs-tree), the run opening
//!   legs (legs-run), `blocked` on the leg that ran `wsp block`.

use crate::legs::{self, Leg, State};
use crate::model::{Policy, Status, Task};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// Open `name` on `id`: `running`, stamped, under the store lock, before any seat
/// exists. Idempotent for a leg already open with no seat.
pub(crate) fn open(store: &Store, id: &str, name: &str) -> Result<Leg, String> {
    store.locked(|| {
        let mut t = store.find_task(id).ok_or_else(|| format!("no task {id}"))?;
        match t.status() {
            Status::Todo | Status::Doing => {}
            other => return Err(format!("{id} is {} — a leg opens on a row at doing", other.as_str())),
        }
        let old = legs::legs(&t);
        if old.is_empty() {
            return Err(format!("{id} has no legs — `wsp leg add` writes them"));
        }
        let new = legs::open(&old, name, &util::now_iso())?;
        if t.status() == Status::Todo {
            t.set_status(Status::Doing);
            t.log("→ doing");
        }
        legs::write(&mut t, &new);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        new.iter().find(|l| l.name == name).cloned().ok_or_else(|| format!("no leg called {name}"))
    })
}

/// A spawn that never got a seat gives the leg back.
pub(crate) fn unopen(store: &Store, id: &str, name: &str) {
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return };
        let old = legs::legs(&t);
        let new = legs::unopen(&old, name);
        if new != old {
            legs::write(&mut t, &new);
            let _ = store.save_task(&t);
        }
    });
}

/// What a seat is bound with, besides the task: the room and tree it stands in
/// and the kind that runs in it.
pub(crate) struct Standing<'a> {
    pub room: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub kind: &'a str,
}

/// The seat onto the entry, the pane bound to the row, and the start line on the
/// log — one section under the lock, so a reader never sees a leg with a seat and
/// no binding. Whether it wrote.
pub(crate) fn attach(store: &Store, id: &str, name: &str, seat: &str, stand: &Standing) -> bool {
    let wrote = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        let old = legs::legs(&t);
        let Ok(new) = legs::attach(&old, name, seat) else { return false };
        let Some(leg) = new.iter().find(|l| l.name == name) else { return false };
        let why = leg.why.as_deref().map(|w| format!(" · why {}", crate::cmd_task::fold(w))).unwrap_or_default();
        t.log(&format!(
            "leg {name} started by pane {seat}{}{why}",
            crate::cmd_agent::spawned_at(leg.policy.model.as_deref(), leg.policy.effort.as_deref())
        ));
        legs::write(&mut t, &new);
        if store.save_task(&t).is_err() {
            return false;
        }
        let mut binding = serde_json::json!({
            "task_id": t.id,
            "pane_id": seat,
            "leg": name,
            "agent_kind": stand.kind,
            "started_at": util::now_iso(),
        });
        if let Some(r) = stand.room {
            binding["workspace_id"] = serde_json::json!(r);
        }
        if let Some(c) = stand.cwd {
            binding["cwd"] = serde_json::json!(c);
        }
        store.set_binding(seat, binding);
        true
    });
    if wrote {
        store.git_commit(&format!("wsp: {id} leg {name} started in {seat}"));
    }
    wrote
}

/// The tier a leg spawns at: its policy's kind, model and effort.
pub(crate) fn tier_of(leg: &Leg) -> (String, Option<String>, Option<String>) {
    let Policy { kind, model, effort } = leg.policy.clone();
    (kind, model, effort)
}

/// The leg's own order text, for a work order that is one line.
pub(crate) fn order_line(task: &str, leg: &str, fetch: bool) -> String {
    let fetch = match fetch {
        true => "Please run `wsp brief --session` first — your context is empty. ",
        false => "",
    };
    format!(
        "{fetch}You are leg {leg} of {task}: one seat in a row run as legs. Your brief is already above, \
         with your order and the batons you were handed; read the tree they point at rather than \
         re-deriving them. Work in this tree, commit, and end with `wsp leg done {task} --from FILE \
         --caption \"...\"`. If you are the last leg, `wsp review {task} \"...\"` ends the row. \
         `wsp ask {task} \"...\"` reaches whoever coordinates this."
    )
}

// ---- ending ---------------------------------------------------------------------

/// Tracked changes in a tree, or `None` when it is not a git work tree.
fn dirty(dir: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["-C", dir, "status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

/// Where this seat stands: its binding's tree, else where the process is.
fn standing_dir(store: &Store, pane: &str) -> Option<String> {
    store
        .bindings()
        .get(pane)
        .and_then(|b| b.get("cwd"))
        .and_then(|c| c.as_str())
        .map(|c| util::expand(c).display().to_string())
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
}

fn ended_line(store: &Store, leg: &Leg, pane: &str, extra: &str) -> String {
    let started = leg.started.clone().unwrap_or_default();
    let secs = util::since(&started);
    let after = if secs > 0 { format!(" after {}", util::duration_human(secs)) } else { String::new() };
    let ran = if started.is_empty() { String::new() } else { crate::cmd_agent::ran_in_seat(store, pane, &started) };
    format!("leg {} ended{after}{extra}{ran}", leg.name)
}

fn refuse(why: String) -> i32 {
    eprintln!("wsp: {}", why.trim_start_matches("wsp: "));
    1
}

/// The baton: `--from FILE`, or `-` for stdin. Optional, and empty is a leg with
/// nothing to hand on.
fn baton_text(args: &Args) -> Result<String, String> {
    let src = match args.get("from") {
        Some(v) if v == "true" && args.rest.iter().skip(2).any(|r| r == "-") => "-".to_string(),
        Some(v) if v != "true" => v,
        _ => return Ok(String::new()),
    };
    if src == "-" && util::stdin_is_tty() {
        return Err("nothing is piped in — `--from -` reads the baton from a stream".into());
    }
    crate::cmd_task::read_source(&src).map_err(|e| format!("cannot read {src}: {e}"))
}

/// `wsp leg done <task> --from FILE --caption "…"`: this seat's leg is finished.
///
/// Refuses a tree with uncommitted tracked changes, a missing caption and a baton
/// over its cap — every leg boundary is a commit, so the legs that wait on it can
/// trust `git log` over the baton. Writes the baton, marks the leg `done`, writes
/// its end line, and pokes the run.
pub(crate) fn cmd_done(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let Some(task) = args.rest.get(1) else {
        eprintln!("usage: wsp leg done <task> --from FILE|- --caption \"…\"");
        return 2;
    };
    let caption = args.get("caption").map(|c| crate::cmd_task::fold(&c)).filter(|c| c != "true").unwrap_or_default();
    if caption.is_empty() {
        return refuse("a leg's end takes a caption: the line a glance reads, --caption \"…\"".into());
    }
    let baton = match baton_text(args) {
        Ok(b) => b,
        Err(why) => return refuse(why),
    };
    let t = match store.task_or_why(task) {
        Ok(t) => t,
        Err(why) => return refuse(why),
    };
    let old = legs::legs(&t);
    let Some(name) = legs::actor_of(&old, pane) else {
        return refuse(format!(
            "{} is not the seat of a running leg of {} — only a leg's own seat ends it",
            pane.unwrap_or("this process"),
            t.id
        ));
    };
    let pane = pane.unwrap_or_default();
    if let Some(dir) = standing_dir(store, pane) {
        if let Some(changed) = dirty(&dir).filter(|c| !c.is_empty()) {
            let shown: Vec<&str> = changed.iter().take(5).map(String::as_str).collect();
            return refuse(format!(
                "{dir} has {} uncommitted change(s) — every leg ends at a commit, so the next leg can trust \
                 `git log`. Commit, then end the leg again:\n  {}",
                changed.len(),
                shown.join("\n  ")
            ));
        }
    }
    let started = old.iter().find(|l| l.name == name).cloned().unwrap_or_else(|| Leg::pending(&name, ""));
    let line = ended_line(store, &started, pane, "");
    let result = store.locked(|| {
        let mut t = store.find_task(&t.id).ok_or_else(|| format!("no task {}", t.id))?;
        let old = legs::legs(&t);
        let new = legs::finish(&old, &name, &baton, &caption, &util::now_iso())?;
        t.log(&line);
        legs::write(&mut t, &new);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok::<_, String>((t, new))
    });
    let (t, new) = match result {
        Ok(v) => v,
        Err(why) => return refuse(why),
    };
    store.log_event("leg-done", serde_json::json!({ "id": t.id, "leg": name }));
    store.git_commit(&format!("wsp: {} leg {name} done", t.id));
    crate::cycle::poke_task(store, &t.id, "leg-done");
    let p = Paint::new();
    println!("{} {}  leg {name} done · {caption}", p.green("✓"), t.id);
    let ready = legs::ready(&new);
    match (ready.is_empty(), legs::lasts(&new).first() == Some(&name)) {
        (_, true) => println!("  {}", p.dim(&format!("{name} is the last leg — `wsp review {} \"…\"` ends the row", t.id))),
        (true, false) => println!("  {}", p.dim("nothing is ready yet — legs still running hold the rest")),
        (false, false) => println!("  {}", p.dim(&format!("ready: {}", ready.join(", ")))),
    }
    0
}

/// `review` typed in a leg's seat. Refused while another leg is running, naming
/// it. Otherwise the leg is done, the pending legs are skipped, and the end line
/// is written — and the caller goes on to move the row. Nothing, for a pane that
/// is no leg's, or a row with no legs.
pub(crate) fn on_review(store: &Store, needle: &str, account: &str, pane: Option<&str>) -> Result<(), String> {
    let Some(t) = store.find_task(needle) else { return Ok(()) };
    let old = legs::legs(&t);
    let Some(name) = legs::actor_of(&old, pane) else { return Ok(()) };
    let pane = pane.unwrap_or_default();
    // Refused before the transcript is read, which costs a subprocess for some kinds.
    legs::end_by_review(&old, &name, account, &util::now_iso())?;
    let mine = old.iter().find(|l| l.name == name).cloned().unwrap_or_else(|| Leg::pending(&name, ""));
    let skipping = old.iter().filter(|l| l.state == State::Pending).count();
    let extra = if skipping > 0 { format!(" · ended the row, {skipping} pending leg(s) skipped") } else { String::new() };
    let line = ended_line(store, &mine, pane, &extra);
    store.locked(|| {
        let mut t = store.find_task(&t.id).ok_or_else(|| format!("no task {}", t.id))?;
        let old = legs::legs(&t);
        let new = legs::end_by_review(&old, &name, account, &util::now_iso())?;
        t.log(&line);
        legs::write(&mut t, &new);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))
    })
}

// ---- the brief ------------------------------------------------------------------

/// The leg block of a brief: which leg this seat is, the table, its order, the
/// batons it was handed in full, and the others by caption. `None` for a seat
/// that is no running leg's.
pub(crate) fn brief_lines(tasks: &[Task], pane: &str) -> Option<Vec<String>> {
    let (t, all, name) = tasks.iter().find_map(|t| {
        let ls = legs::legs(t);
        let name = legs::actor_of(&ls, Some(pane))?;
        Some((t, ls, name))
    })?;
    let waits = legs::waits(&all);
    let at = all.iter().position(|l| l.name == name)?;
    let me = &all[at];
    let count = |s: State| all.iter().filter(|l| l.state == s).count();
    let after = if waits[at].is_empty() { "-".to_string() } else { waits[at].join(", ") };
    let mut out = vec![format!(
        "leg {name} on {} · after {after} · {} running, {} done of {}",
        t.id,
        count(State::Running),
        count(State::Done),
        all.len()
    )];
    out.extend(legs::table(&all).lines().map(str::to_string));
    out.push(format!("your order: {}", me.order));
    for w in &waits[at] {
        let Some(from) = all.iter().find(|l| &l.name == w) else { continue };
        out.push(match (&from.state, from.baton.is_empty()) {
            (State::Skipped, _) => format!("baton from {w}: skipped: {}", from.skipped.as_deref().unwrap_or("no reason given")),
            (_, true) => format!("baton from {w}: none — read `git log` and the tree"),
            _ => format!("baton from {w}:\n{}", from.baton),
        });
    }
    let others: Vec<&Leg> = all
        .iter()
        .filter(|l| l.state == State::Done && l.name != name && !waits[at].contains(&l.name))
        .collect();
    for l in others {
        out.push(format!(
            "{} (done): {} · `wsp leg show {} {}` reads the baton whole",
            l.name,
            l.caption.as_deref().unwrap_or("no caption"),
            t.id,
            l.name
        ));
    }
    let (cap, _) = legs::baton_cap(&all, &name);
    out.push(format!(
        "finish: commit, then `wsp leg done {} --from FILE --caption \"…\"` — a baton of at most {cap} characters, pointers not a plan; \
         the last leg types `wsp review {} \"…\"` instead",
        t.id, t.id
    ));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legs::Leg;

    /// A store with a `doing` row `m-1` carrying `scout → plan → build`, and a
    /// directory that is no git tree for the legs to stand in.
    fn staged(env: &util::Isolated, body_legs: Vec<Leg>) -> (Store, String) {
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a staged row", "m-1");
        t.set_status(Status::Doing);
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-10-18 started\n".to_string();
        legs::write(&mut t, &body_legs);
        store.save_task(&t).unwrap();
        let tree = env.home().join("not-a-repo");
        std::fs::create_dir_all(&tree).unwrap();
        (store, tree.display().to_string())
    }

    fn chain() -> Vec<Leg> {
        vec![Leg::pending("scout", "find the layers"), Leg::pending("plan", "decide"), Leg::pending("build", "make it")]
    }

    fn seat_leg(store: &Store, tree: &str, name: &str, pane: &str) {
        open(store, "m-1", name).unwrap();
        let stand = Standing { room: Some("w1"), cwd: Some(tree), kind: "claude" };
        assert!(attach(store, "m-1", name, pane, &stand));
    }

    fn log_of(store: &Store) -> String {
        store.find_task("m-1").unwrap().section("Log").unwrap_or_default()
    }

    /// The seat is recorded on the entry and the pane is bound to the row, with
    /// no claim written anywhere, and the log gets the start line.
    #[test]
    fn a_leg_is_seated_on_its_entry_and_bound_to_the_row_and_claims_nothing() {
        let env = util::isolated("leg-seat");
        let (store, tree) = staged(&env, chain());
        let leg = open(&store, "m-1", "scout").unwrap();
        assert_eq!(leg.state, State::Running);
        let stand = Standing { room: Some("w1"), cwd: Some(&tree), kind: "claude" };
        assert!(attach(&store, "m-1", "scout", "cpd-31", &stand));

        assert!(store.claims().is_empty(), "a leg claims nothing");
        assert_eq!(store.bindings()["cpd-31"]["task_id"], "m-1", "the pane is on the row");
        let ls = legs::legs(&store.find_task("m-1").unwrap());
        assert_eq!(ls[0].pane.as_deref(), Some("cpd-31"));
        assert!(ls[0].started.is_some());
        assert!(log_of(&store).contains("leg scout started by pane cpd-31 · spawned at sonnet/medium"), "{}", log_of(&store));
    }

    /// A leg that waits cannot open; the refusal names what is open.
    #[test]
    fn a_leg_that_is_not_ready_is_refused_and_one_that_is_open_opens_again_quietly() {
        let env = util::isolated("leg-open");
        let (store, _) = staged(&env, chain());
        let why = open(&store, "m-1", "plan").unwrap_err();
        assert!(why.contains("not ready") && why.contains("scout"), "{why}");
        open(&store, "m-1", "scout").unwrap();
        open(&store, "m-1", "scout").expect("a retried spawn finds the open leg");
        unopen(&store, "m-1", "scout");
        assert_eq!(legs::legs(&store.find_task("m-1").unwrap())[0].state, State::Pending, "no seat, so given back");
    }

    /// The Done-when: scout done, the next leg by hand, review at the last.
    #[test]
    fn a_staged_row_goes_scout_done_then_the_next_leg_then_review_with_a_line_for_each() {
        let env = util::isolated("leg-flow");
        let (store, tree) = staged(&env, chain());
        seat_leg(&store, &tree, "scout", "cpd-31");

        let baton = env.home().join("baton.md");
        std::fs::write(&baton, "three layers: store (src/legs.rs:1–40)").unwrap();
        let done = |pane: &str, caption: Option<&str>, from: Option<&str>| {
            let mut flags: Vec<(&str, &str)> = Vec::new();
            if let Some(c) = caption {
                flags.push(("caption", c));
            }
            if let Some(f) = from {
                flags.push(("from", f));
            }
            cmd_done(&store, &Args::synth("leg", &["done", "m-1"], &flags), Some(pane))
        };
        assert_eq!(done("cpd-31", None, None), 1, "no caption, no end");
        assert_eq!(done("cpd-99", Some("x"), None), 1, "a seat that is no leg's cannot end one");
        assert_eq!(done("cpd-31", Some("three layers found"), baton.to_str()), 0);

        let t = store.find_task("m-1").unwrap();
        let ls = legs::legs(&t);
        assert_eq!(ls[0].state, State::Done);
        assert_eq!(ls[0].caption.as_deref(), Some("three layers found"));
        assert!(ls[0].baton.contains("three layers"));
        assert!(ls[0].closed.is_some());
        assert!(log_of(&store).contains("leg scout ended"), "{}", log_of(&store));
        assert_eq!(legs::ready(&ls), vec!["plan".to_string()]);

        // The next leg by hand, and the brief it reads carries scout's baton whole.
        seat_leg(&store, &tree, "plan", "cpd-32");
        let tasks = store.tasks();
        let brief = brief_lines(&tasks, "cpd-32").expect("a brief block for the leg's seat").join("\n");
        assert!(brief.contains("leg plan on m-1"), "{brief}");
        assert!(brief.contains("baton from scout:\nthree layers: store"), "{brief}");
        assert!(brief.contains("your order: decide"), "{brief}");
        assert!(brief_lines(&tasks, "cpd-99").is_none(), "no block for a seat that is no leg's");

        // Plan, then build, which is the last leg and types review.
        assert_eq!(done("cpd-32", Some("plan made"), None), 0);
        seat_leg(&store, &tree, "build", "cpd-33");
        on_review(&store, "m-1", "built it", Some("cpd-33")).unwrap();
        let ls = legs::legs(&store.find_task("m-1").unwrap());
        assert!(ls.iter().all(|l| l.state == State::Done), "{ls:?}");
        let log = log_of(&store);
        for l in ["scout", "plan", "build"] {
            assert!(log.contains(&format!("leg {l} started")) && log.contains(&format!("leg {l} ended")), "{l}: {log}");
        }
        assert!(store.claims().is_empty(), "no claim was written at any point");
    }

    /// `review` from one of two running siblings is refused, naming the other;
    /// from the last running leg it skips the rest.
    #[test]
    fn review_is_refused_while_a_sibling_runs_and_from_the_last_running_leg_skips_the_rest() {
        let env = util::isolated("leg-review");
        let legs_v = vec![
            Leg::pending("plan", "decide"),
            Leg { after: Some(vec!["plan".into()]), ..Leg::pending("a", "side a") },
            Leg { after: Some(vec!["plan".into()]), ..Leg::pending("b", "side b") },
            Leg { after: Some(vec!["a".into(), "b".into()]), ..Leg::pending("finish", "join") },
        ];
        let (store, tree) = staged(&env, legs_v);
        let mut t = store.find_task("m-1").unwrap();
        let mut ls = legs::legs(&t);
        ls[0].state = State::Done;
        legs::write(&mut t, &ls);
        store.save_task(&t).unwrap();
        seat_leg(&store, &tree, "a", "cpd-41");
        seat_leg(&store, &tree, "b", "cpd-42");

        let why = on_review(&store, "m-1", "done early", Some("cpd-41")).unwrap_err();
        assert!(why.contains("b is still running"), "{why}");
        let ls = legs::legs(&store.find_task("m-1").unwrap());
        assert_eq!(ls[1].state, State::Running, "a refused review changes nothing");

        // A governor's review of the row is not a leg's.
        on_review(&store, "m-1", "from a governor", Some("cpd-1")).unwrap();

        let mut t = store.find_task("m-1").unwrap();
        let mut ls = legs::legs(&t);
        ls[2].state = State::Done;
        legs::write(&mut t, &ls);
        store.save_task(&t).unwrap();
        on_review(&store, "m-1", "the scout found nothing to do", Some("cpd-41")).unwrap();
        let ls = legs::legs(&store.find_task("m-1").unwrap());
        assert_eq!(ls[1].state, State::Done);
        assert_eq!(ls[3].state, State::Skipped);
        assert_eq!(ls[3].skipped.as_deref(), Some("ended by review"));
    }

    /// A tree with uncommitted tracked changes cannot end a leg, and a baton over
    /// its cap is refused rather than cut.
    #[test]
    fn a_dirty_tree_and_a_long_baton_both_refuse_the_end() {
        let env = util::isolated("leg-dirty");
        let (store, _) = staged(&env, chain());
        let repo = env.home().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(["-C", repo.to_str().unwrap()])
                .args(args)
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
                .output().unwrap().status.success());
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "one\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "one"]);
        open(&store, "m-1", "scout").unwrap();
        let stand = Standing { room: None, cwd: repo.to_str(), kind: "claude" };
        assert!(attach(&store, "m-1", "scout", "cpd-31", &stand));

        std::fs::write(repo.join("f.txt"), "two\n").unwrap();
        let end = |from: Option<&str>| {
            let mut flags = vec![("caption", "c")];
            if let Some(f) = from {
                flags.push(("from", f));
            }
            cmd_done(&store, &Args::synth("leg", &["done", "m-1"], &flags), Some("cpd-31"))
        };
        assert_eq!(end(None), 1, "an uncommitted change refuses the end");
        git(&["commit", "-q", "-am", "two"]);

        let long = env.home().join("long.md");
        std::fs::write(&long, "x".repeat(3_001)).unwrap();
        assert_eq!(end(long.to_str()), 1, "a baton over 3,000 is refused");
        assert_eq!(legs::legs(&store.find_task("m-1").unwrap())[0].state, State::Running);
        assert_eq!(end(None), 0, "committed, within the cap, it ends");
    }
}
