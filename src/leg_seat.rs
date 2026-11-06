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
        // A leg opening begins with no baton: any file at its path is an earlier run's.
        crate::closing::clear_baton(store, id, name);
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

/// A running leg put back to pending whatever seat it names (`legs::reset`),
/// and the seat's binding to the row dropped with it: a binding left would
/// keep telling every command that asks which task the pane is on that it is
/// still this leg's. Returns the seat the entry named, for the caller that
/// ends it, or `None` when the leg was not running.
pub(crate) fn reset(store: &Store, id: &str, name: &str) -> Option<Option<String>> {
    let seat = store.locked(|| {
        let mut t = store.find_task(id)?;
        let old = legs::legs(&t);
        let new = legs::reset(&old, name).ok()?;
        let seat = old.iter().find(|l| l.name == name).and_then(|l| l.pane.clone());
        legs::write(&mut t, &new);
        store.save_task(&t).ok()?;
        crate::closing::clear_baton(store, id, name);
        if let Some(s) = &seat {
            store.clear_binding(s);
        }
        Some(seat)
    });
    if seat.is_some() {
        store.git_commit(&format!("wsp: {id} leg {name} reset"));
    }
    seat
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

/// The one command that ends a leg, in words: a leg that is not last ends with
/// `leg done`; the last leg ends with `review`, which closes its leg and the
/// row, unless it adds a leg after itself, which makes `leg done` the end.
pub(crate) fn ending_clause(task: &str, last: bool) -> String {
    match last {
        true => format!(
            "You are the last leg: end with `wsp review {task} \"...\"`, which closes your leg and ends the row; \
             if you add a leg after yourself, end with `wsp leg done {task} --caption \"...\"` instead."
        ),
        false => format!("End with `wsp leg done {task} --caption \"...\"`."),
    }
}

/// The leg's own order text, for a work order that is one line. `last` is
/// whether the record makes this leg the last (`legs::lasts`).
pub(crate) fn order_line(task: &str, leg: &str, fetch: bool, last: bool) -> String {
    let fetch = match fetch {
        true => "Please run `wsp brief --session` first — your context is empty. ",
        false => "",
    };
    format!(
        "{fetch}You are leg {leg} of {task}: one seat in a row run as legs. Your brief is already above, \
         with your order and the batons you were handed; read the tree they point at rather than \
         re-deriving them. Work in this tree, commit. {} {} \
         `wsp ask {task} \"...\"` reaches whoever coordinates this.",
        ending_clause(task, last),
        crate::closing::how_to_close(task, last)
    )
}

// ---- ending ---------------------------------------------------------------------

/// Tracked changes in a tree, or `None` when it is not a git work tree.
pub(crate) fn dirty(dir: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["-C", dir, "status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

/// Where this seat stands: its binding's tree, else where the process is.
pub(crate) fn standing_dir(store: &Store, pane: &str) -> Option<String> {
    store
        .bindings()
        .get(pane)
        .and_then(|b| b.get("cwd"))
        .and_then(|c| c.as_str())
        .map(|c| util::expand(c).display().to_string())
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
}

fn ended_line(store: &Store, leg: &Leg, pane: &str, extra: &str) -> String {
    ended_line_at(store, leg, pane, extra, &util::now_iso())
}

/// The end line for a leg that ended at `at`, which is the baton's write when a
/// close came late.
fn ended_line_at(store: &Store, leg: &Leg, pane: &str, extra: &str, at: &str) -> String {
    let started = leg.started.clone().unwrap_or_default();
    let secs = if started.is_empty() { 0 } else { (util::epoch_of(at) - util::epoch_of(&started)).max(0) };
    let after = if secs > 0 { format!(" after {}", util::duration_human(secs)) } else { String::new() };
    let ran = if started.is_empty() { String::new() } else { crate::cmd_agent::ran_in_seat(store, pane, &started) };
    format!("leg {} ended{after}{extra}{ran}", leg.name)
}

/// The baton: `--from FILE`, or `-` for stdin; with neither, the file wsp keeps
/// for this leg ([`crate::closing::baton_path`], `$WSP_BATON` in the seat) when
/// it is there. Optional, and empty is a leg with nothing to hand on.
fn baton_text(store: &Store, args: &Args, row: &str, leg: &str, fresh: bool) -> Result<String, String> {
    let src = match args.get("from") {
        Some(v) if v == "true" && args.rest.iter().skip(2).any(|r| r == "-") => "-".to_string(),
        Some(v) if v != "true" => v,
        _ => {
            let read = || std::fs::read_to_string(crate::closing::baton_path(store, row, leg)).unwrap_or_default();
            return Ok(if fresh { read() } else { String::new() });
        }
    };
    if src == "-" && util::stdin_is_tty() {
        return Err("nothing is piped in — `--from -` reads the baton from a stream".into());
    }
    crate::cmd_task::read_source(&src).map_err(|e| format!("cannot read {src}: {e}"))
}

/// How a leg came to be closed when it was not at the moment its seat finished.
pub(crate) struct Late {
    /// When the baton was written, epoch seconds: the leg's work ended then.
    pub baton_at: i64,
    /// Set when wsp closed it for an idle or ended seat: `seat idle 12m`.
    pub by_wsp: Option<String>,
}

/// What a finished leg leaves the caller to print.
pub(crate) struct Finished {
    pub task: Task,
    pub legs: Vec<Leg>,
}

/// `wsp leg done <task> --from FILE --caption "…"`: this seat's leg is finished.
///
/// Refuses a tree with uncommitted tracked changes, a missing caption and a baton
/// over its cap — every leg boundary is a commit, so the legs that wait on it can
/// trust `git log` over the baton. Writes the baton, marks the leg `done`, writes
/// its end line, and pokes the run.
///
/// **A refusal is one plain line on stderr, a non-zero exit, and a line in
/// `cycle.log`** (wsp-372): a close that was refused must not be mistaken for one
/// that never ran.
pub(crate) fn cmd_done(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let Some(task) = args.rest.get(1) else {
        eprintln!("usage: wsp leg done <task> --from FILE|- --caption \"…\"   (no --from: $WSP_BATON)");
        return 2;
    };
    match done(store, args, task, pane) {
        Ok(()) => 0,
        Err(why) => {
            let why = why.trim_start_matches("wsp: ").to_string();
            crate::cycle::log_line(
                store,
                &format!("leg done {task} refused for {}: {}", pane.unwrap_or("this process"), util::truncate(&crate::cmd_task::fold(&why), 300)),
            );
            eprintln!("wsp: {} — the leg is still open", why.lines().next().unwrap_or_default().trim_end_matches(':'));
            for rest in why.lines().skip(1) {
                eprintln!("{rest}");
            }
            1
        }
    }
}

fn done(store: &Store, args: &Args, task: &str, pane: Option<&str>) -> Result<(), String> {
    let caption = args.get("caption").map(|c| crate::cmd_task::fold(&c)).filter(|c| c != "true").unwrap_or_default();
    let t = store.task_or_why(task)?;
    let old = legs::legs(&t);
    let Some(name) = legs::actor_of(&old, pane) else {
        return Err(format!(
            "{} is not the seat of a running leg of {} — only a leg's own seat ends it",
            pane.unwrap_or("this process"),
            t.id
        ));
    };
    // The baton file wsp keeps, written in this run of the leg: older than the
    // leg's start it is a previous run's and is not read. Written some while ago,
    // the leg's work ended then, and the close is late by the difference.
    let started = old.iter().find(|l| l.name == name).and_then(|l| l.started.clone());
    let fresh = crate::closing::fresh_baton(store, &t.id, &name, started.as_deref());
    let baton = baton_text(store, args, &t.id, &name, fresh.is_some())?;
    let late = fresh
        .filter(|_| args.get("from").is_none())
        .filter(|at| util::epoch_secs() - at >= crate::closing::LATE_AFTER)
        .map(|baton_at| Late { baton_at, by_wsp: None });
    if caption.is_empty() {
        return Err("a leg's end takes a caption: the line a glance reads, --caption \"…\"".into());
    }
    let done = finish(store, &t.id, &name, pane.unwrap_or_default(), &baton, &caption, late.as_ref())?;
    let p = Paint::new();
    let (t, new) = (done.task, done.legs);
    println!("{} {}  leg {name} done · {caption}", p.green("✓"), t.id);
    // A sibling hears early which paths another has also changed; the join's
    // brief carries the same line, read again from the branches when it opens.
    let tree_mine = new.iter().find(|l| l.name == name).is_some_and(|l| l.tree.is_some());
    if let (true, Some(dir)) = (tree_mine, standing_dir(store, pane.unwrap_or_default())) {
        if let Some(line) = crate::leg_tree::overlap_for(std::path::Path::new(&dir), &t.id, &new, &name) {
            println!("  {}", p.yellow(&line));
        }
    }
    let ready = legs::ready(&new);
    match (ready.is_empty(), legs::lasts(&new).first() == Some(&name)) {
        (_, true) => println!("  {}", p.dim(&format!("{name} is the last leg — `wsp review {} \"…\"` ends the row", t.id))),
        (true, false) => println!("  {}", p.dim("nothing is ready yet — legs still running hold the rest")),
        (false, false) => println!("  {}", p.dim(&format!("ready: {}", ready.join(", ")))),
    }
    Ok(())
}

/// Close leg `name` of `row`, whoever is closing it: the seat's own `leg done`,
/// or wsp on a seat that went idle or ended ([`crate::closing`]). The checks are
/// the same in both — a clean tree at a commit, a caption, a baton in its cap —
/// so wsp closing a leg is never wsp closing it more loosely.
///
/// A [`Late`] close times the leg to the baton's write, and says it was late.
pub(crate) fn finish(
    store: &Store,
    row: &str,
    name: &str,
    pane: &str,
    baton: &str,
    caption: &str,
    late: Option<&Late>,
) -> Result<Finished, String> {
    let t = store.task_or_why(row)?;
    let old = legs::legs(&t);
    if let Some(dir) = standing_dir(store, pane) {
        if let Some(changed) = dirty(&dir).filter(|c| !c.is_empty()) {
            let shown: Vec<&str> = changed.iter().take(5).map(String::as_str).collect();
            return Err(format!(
                "{dir} has {} uncommitted change(s) — every leg ends at a commit, so the next leg can trust \
                 `git log`. Commit, then end the leg again:\n  {}",
                changed.len(),
                shown.join("\n  ")
            ));
        }
    }
    let started = old.iter().find(|l| l.name == name).cloned().unwrap_or_else(|| Leg::pending(name, ""));
    let at = late.map(|l| util::iso_at(l.baton_at)).unwrap_or_else(util::now_iso);
    let mut line = ended_line_at(store, &started, pane, "", &at);
    if let Some(l) = late {
        line.push_str(&crate::closing::late_clause(l.baton_at, util::epoch_secs(), l.by_wsp.as_deref()));
    }
    let ready_line = ready_signal(store, &t.id, &old, name, baton, caption)?;
    let result = store.locked(|| {
        let mut t = store.find_task(&t.id).ok_or_else(|| format!("no task {}", t.id))?;
        let old = legs::legs(&t);
        let new = legs::finish(&old, name, baton, caption, &at)?;
        t.log(&line);
        if let Some(l) = &ready_line {
            t.log(l);
        }
        legs::write(&mut t, &new);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok::<_, String>((t, new))
    });
    let (t, new) = result?;
    let mut data = serde_json::json!({ "id": t.id, "leg": name });
    if let Some(l) = late {
        data["baton_at"] = serde_json::json!(l.baton_at);
        if let Some(w) = &l.by_wsp {
            data["closed_by"] = serde_json::json!(format!("wsp: {w}"));
        }
    }
    store.log_event("leg-done", data);
    store.git_commit(&format!("wsp: {} leg {name} done", t.id));
    crate::cycle::poke_task(store, &t.id, "leg-done");
    Ok(Finished { task: t, legs: new })
}

/// The last leg's end is the row's ready signal, as `wsp review` is for a row
/// without legs: the `wsp: ready <sha> on <branch>` line from the row's tree,
/// for a member of a group wsp runs. `Err` when that tree holds uncommitted
/// work, before anything is written; `None` for any other leg or row.
fn ready_signal(store: &Store, task: &str, old: &[Leg], name: &str, baton: &str, caption: &str) -> Result<Option<String>, String> {
    let Ok(after) = legs::finish(old, name, baton, caption, &util::now_iso()) else { return Ok(None) };
    if legs::lasts(&after).first().map(String::as_str) != Some(name) || !crate::worklist::verified_by_a_run(store, task) {
        return Ok(None);
    }
    Ok(crate::cmd_checkout::ready_at(store, task)?.map(|(sha, branch)| format!("{} {sha} on {branch}", crate::repair::READY)))
}

/// A row whose legs have run out with the last one skipped is finished
/// (`wsp-360`): no seat is left to type `wsp review`, so this does what that
/// would — the row at `review`, ready at its tree's commit when it is a member
/// of a run, said on its log — and tells its governor. Once: the line it
/// writes is the mark. `true` when it finished the row now.
///
/// Called by `wsp leg skip` and by the repair tick, for a record that got
/// there by hand.
pub(crate) fn finish_skipped_out(store: &Store, row: &str) -> bool {
    let Some(t) = store.find_task(row) else { return false };
    if t.status() != crate::model::Status::Doing || !legs::staged(&t) || !legs::ended_in_a_skip(&legs::legs(&t)) {
        return false;
    }
    let all = legs::legs(&t);
    let last = legs::lasts(&all).into_iter().find(|n| all.iter().any(|l| &l.name == n && l.state == State::Skipped)).unwrap_or_default();
    let said = format!("every leg is done or skipped, and the last, {last}, was skipped — the row is finished, nothing is left to run");
    let ready = if crate::worklist::verified_by_a_run(store, row) {
        crate::cmd_checkout::ready_from(store, row, std::path::Path::new("/")).ok().flatten()
    } else {
        None
    };
    let done = store.locked(|| {
        let Some(mut t) = store.find_task(row) else { return false };
        if t.status() != crate::model::Status::Doing || !legs::ended_in_a_skip(&legs::legs(&t)) {
            return false;
        }
        t.set_status(crate::model::Status::Review);
        t.log(&format!("review: {said}"));
        if let Some((sha, branch)) = &ready {
            t.log(&format!("{} {sha} on {branch}", crate::repair::READY));
        }
        store.save_task(&t).is_ok()
    });
    if !done {
        return false;
    }
    store.git_commit(&format!("wsp: {row} finished, its last leg skipped"));
    crate::cycle::log_line(store, &format!("{row}: {said}"));
    if let Some(w) = crate::worklist::open_list_of(&store.worklists(), row) {
        crate::cycle::tell(store, &w, &format!(
            "{row} in the {} run: {said}. It is at review{}.",
            w.id,
            if ready.is_some() { " and ready to land" } else { "" }
        ));
    }
    store.log_event("row-finished-by-skips", serde_json::json!({ "id": row }));
    true
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
/// Whether the leg this seat runs names a packet of its own.
pub(crate) fn has_packet(tasks: &[Task], pane: &str) -> bool {
    tasks.iter().any(|t| {
        let ls = legs::legs(t);
        legs::actor_of(&ls, Some(pane))
            .and_then(|n| ls.into_iter().find(|l| l.name == n))
            .is_some_and(|l| crate::packet::leg_path(&l.order).is_some())
    })
}

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
    if let Some(tree) = &me.tree {
        out.push(format!(
            "your tree: {tree} (a branch of that name, cut from the row's tip) — you have siblings, so you stand apart; \
             the join that waits on you merges it"
        ));
    }
    if let Some(join) = &me.join {
        out.push(format!("the join:\n{join}"));
    }
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
    let last = legs::lasts(&all).first() == Some(&name);
    let verb = match last {
        true => format!(
            "you are the last leg: `wsp review {} \"…\"` closes your leg and ends the row (if you add a leg after yourself, \
             `wsp leg done` instead)",
            t.id
        ),
        false => format!("`wsp leg done {} --caption \"…\"`", t.id),
    };
    out.push(format!(
        "finish: commit, then {verb} — a baton of at most {cap} characters, pointers not a plan, \
         written first with a heredoc into $WSP_BATON, which the closing verb reads. \
         The closing verb is its own Bash call, never chained after another command",
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

    #[test]
    fn an_order_line_names_the_one_command_that_ends_the_leg() {
        let mid = order_line("m-1", "plan", false, false);
        assert!(mid.contains("End with `wsp leg done m-1 --caption") && !mid.contains("wsp review"), "{mid}");
        let last = order_line("m-1", "build", false, true);
        assert!(last.contains("last leg: end with `wsp review m-1") && last.contains("instead"), "{last}");
        assert!(!last.contains("separate Bash calls, never chained with anything else: first write the baton, `cat > \"$WSP_BATON\" <<'EOF'` ... `EOF` (a heredoc with its text, never a bare `cat`); then, as a call of its own, `wsp leg done"), "{last}");
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
        assert!(brief.contains("finish: commit, then `wsp leg done m-1") && !brief.contains("wsp review"), "{brief}");

        // Plan, then build, which is the last leg and types review.
        assert_eq!(done("cpd-32", Some("plan made"), None), 0);
        seat_leg(&store, &tree, "build", "cpd-33");
        let last = brief_lines(&store.tasks(), "cpd-33").unwrap().join("\n");
        assert!(last.contains("you are the last leg: `wsp review m-1") && !last.contains("then `wsp leg done"), "{last}");
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

    /// `wsp-341`: the last leg's end is the ready signal for a member of a run.
    #[test]
    fn the_last_leg_done_records_ready() {
        let env = util::isolated("leg-ready");
        let (store, _) = staged(&env, vec![Leg::pending("only", "do it")]);
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-C", dir.to_str().unwrap()])
                .args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
                .output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        crate::testkit::repo(&repo);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let tree = repo.join(".worktrees").join("m-1");
        git(&repo, &["worktree", "add", "-q", "-b", "m-1", tree.to_str().unwrap(), "master"]);
        std::fs::write(tree.join("w.txt"), "w\n").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-q", "-m", "work"]);
        let tip = git(&tree, &["rev-parse", "HEAD"]);

        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut t = store.find_task("m-1").unwrap();
        t.project = Some("p".into());
        store.save_task(&t).unwrap();
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["m-1".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();

        open(&store, "m-1", "only").unwrap();
        let stand = Standing { room: None, cwd: tree.to_str(), kind: "claude" };
        assert!(attach(&store, "m-1", "only", "cpd-31", &stand));
        let done = || cmd_done(&store, &Args::synth("leg", &["done", "m-1"], &[("caption", "c")]), Some("cpd-31"));
        assert_eq!(done(), 0);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(crate::repair::ready(&t).as_deref(), Some(tip.as_str()), "{}", log_of(&store));
        assert!(crate::repair::landed(&t).is_none());
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
