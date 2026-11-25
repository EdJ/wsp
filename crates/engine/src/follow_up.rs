//! Follow-ups: work found while doing a row, which records where it came from
//! and never holds that row open (`wsp-345`).
//!
//! A row has two ways to come from another, and until this the store had one
//! field for both. **A sub-task** (`parent:`, `wsp add --parent`) is part of its
//! parent's own done-when: the parent is not done until it is, and `wsp done`
//! refuses a parent with open sub-tasks. **A follow-up** (`found_by:`, `wsp add
//! --from`) is work somebody found while doing a row: a barrier's finding, a
//! verifier's, a seat's "found while". Filed as a sub-task, every follow-up made
//! its origin unclosable without `--force`, and one on no list was run by
//! nothing — wsp-155/wsp-156's 86 rows (wsp-303), and wsp-121 held open by
//! wsp-341, a doc fix nobody was running.
//!
//! This module is the follow-up's half: the rows a row found ([`of`]), the
//! ones nothing will run ([`unheld`], named by `wsp ls` and `wsp wip`), and the
//! one-time migration of the follow-ups already filed as sub-tasks
//! ([`migrate`], [`migrate_once`]).

use std::collections::BTreeSet;

use serde_json::json;

use crate::model::{Status, Task, Worklist, WorklistStatus};
use crate::store::Store;
use crate::util::Paint;

/// The rows found while doing `id`, by id.
pub fn of<'a>(tasks: &'a [Task], id: &str) -> Vec<&'a Task> {
    let mut found: Vec<&Task> = tasks.iter().filter(|t| t.found_by.as_deref() == Some(id)).collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Whether a row's seat has let it go: at review, done, or gone from the live
/// store. The origin of a follow-up nothing holds, and the parent the
/// migration reads a sub-task under as found work.
fn closed(s: Status) -> bool {
    matches!(s, Status::Review | Status::Done)
}

/// Every row a list still to run, or running, holds. A list that is over runs
/// nothing more, so its members are held by nobody.
fn on_lists(lists: &[Worklist]) -> BTreeSet<String> {
    lists
        .iter()
        .filter(|w| w.status() != WorklistStatus::Done)
        .flat_map(|w| w.groups().into_iter().flat_map(|g| g.members))
        .collect()
}

/// A follow-up nothing will run: open, found by a row that has closed, and on
/// no list and under no seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unheld {
    pub id: String,
    pub title: String,
    pub found_by: String,
}

/// [`unheld`] over what the store holds, given: `seated` is every row a seat
/// is bound to or a claim names.
pub fn unheld_in(tasks: &[Task], lists: &[Worklist], seated: &BTreeSet<String>) -> Vec<Unheld> {
    let held = on_lists(lists);
    let mut out: Vec<Unheld> = tasks
        .iter()
        .filter(|t| !closed(t.status()))
        .filter_map(|t| {
            let origin = t.found_by.as_deref()?;
            let origin_closed = tasks.iter().find(|o| o.id == origin).is_none_or(|o| closed(o.status()));
            (origin_closed && !held.contains(&t.id) && !seated.contains(&t.id)).then(|| Unheld {
                id: t.id.clone(),
                title: t.title.clone(),
                found_by: origin.to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The follow-ups found by a closed row that no list and no seat holds, so
/// nothing will run them unless somebody is told: `wsp ls` and `wsp wip` name
/// them.
pub fn unheld(store: &Store) -> Vec<Unheld> {
    let tasks: Vec<Task> = store.task_heads().iter().map(|h| h.bodiless()).collect();
    if !tasks.iter().any(|t| t.found_by.is_some()) {
        return Vec::new();
    }
    let mut seated: BTreeSet<String> = store
        .bindings()
        .values()
        .filter_map(|b| b.get("task_id").and_then(|x| x.as_str()).map(str::to_string))
        .collect();
    seated.extend(store.claims().into_keys());
    unheld_in(&tasks, &store.worklists(), &seated)
}

/// One line naming `rows`, or `None` when there are none.
pub fn line(rows: &[Unheld]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let named: Vec<String> = rows.iter().map(|u| format!("{} (from {})", u.id, u.found_by)).collect();
    Some(format!(
        "{} follow-up(s) found by a closed row, on no list and under no seat — nothing will run them: {}",
        rows.len(),
        named.join(", ")
    ))
}

/// The same rows as JSON, for `wip --json`.
pub fn json_of(rows: &[Unheld]) -> Vec<serde_json::Value> {
    rows.iter().map(|u| json!({ "id": u.id, "title": u.title, "found_by": u.found_by })).collect()
}

// ---- the one-time migration ----------------------------------------------

/// A sub-task that is a follow-up: `id`, filed under `parent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Convert {
    pub id: String,
    pub parent: String,
}

/// The sub-tasks filed before `--from` existed that are follow-ups: open, under
/// a parent at review or done (or archived), and not on any list that holds
/// the parent. A sub-task on its parent's list is the parent's own work, made a
/// member to be run (`worklist add --sub`), and one under a parent still being
/// worked may yet be part of its done-when; both are left alone.
pub fn plan(tasks: &[Task], lists: &[Worklist]) -> Vec<Convert> {
    let mut out = Vec::new();
    for t in tasks.iter().filter(|t| t.status() != Status::Done) {
        let Some(parent) = t.parent.as_deref() else { continue };
        let parent_closed = tasks.iter().find(|p| p.id == parent).is_none_or(|p| closed(p.status()));
        if !parent_closed {
            continue;
        }
        let on_parents_list = lists.iter().any(|w| {
            let members: Vec<String> = w.groups().into_iter().flat_map(|g| g.members).collect();
            members.iter().any(|m| m == parent) && members.iter().any(|m| m == &t.id)
        });
        if on_parents_list {
            continue;
        }
        out.push(Convert { id: t.id.clone(), parent: parent.to_string() });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Carry out `plan`: each row's parent cleared and `found_by` set to it, with a
/// line in the row's log and an event. Returns how many were converted.
pub fn apply(store: &Store, plan: &[Convert]) -> usize {
    let mut done = 0;
    for c in plan {
        let wrote = store.locked(|| -> std::io::Result<bool> {
            let Some(mut t) = store.task(&c.id) else { return Ok(false) };
            if t.parent.as_deref() != Some(c.parent.as_str()) {
                return Ok(false);
            }
            t.parent = None;
            t.found_by = Some(c.parent.clone());
            t.log(&format!(
                "a follow-up from {}, no longer its sub-task: found_by set, parent cleared, so it does not hold {} open (wsp-345 migration)",
                c.parent, c.parent
            ));
            t.touch();
            store.save_task(&t)?;
            Ok(true)
        });
        match wrote {
            Ok(true) => {
                store.log_event("task-follow-up-migrated", json!({ "id": c.id, "found_by": c.parent }));
                done += 1;
            }
            Ok(false) => {}
            Err(e) => eprintln!("wsp: {}: {e}", c.id),
        }
    }
    done
}

/// `wsp migrate --follow-ups [-n]`: the migration by hand, planned in full
/// under `-n` (`dry`). The daemon runs it once by itself ([`migrate_once`]).
/// It takes the flag rather than `Args` so this module links nothing above
/// `engine` (tests/layering.rs).
pub fn migrate(store: &Store, dry: bool) -> i32 {
    let p = Paint::new();
    let plan = plan(&store.tasks(), &store.worklists());
    for c in &plan {
        println!("{}  a follow-up from {}", p.bold(&c.id), c.parent);
    }
    println!("\n{} sub-task(s) under a closed row, on none of its lists, to become follow-ups", plan.len());
    if plan.is_empty() {
        return 0;
    }
    if dry {
        println!("{}", p.dim("nothing written — drop -n to apply"));
        return 0;
    }
    let n = apply(store, &plan);
    store.git_commit(&format!("wsp: {n} sub-task(s) of closed rows made follow-ups (wsp-345)"));
    println!("converted {n}");
    0
}

/// The marker [`migrate_once`] leaves in the store's state.
const MARKER: &str = "migrated-follow-ups";

/// The migration, once per store, as a starting daemon runs it: later
/// sub-tasks under a closed row are the person's to decide, not this pass's.
/// Returns how many rows it converted, `None` where it had already run.
pub fn migrate_once(store: &Store) -> Option<usize> {
    let marker = store.state_file(MARKER);
    if marker.exists() {
        return None;
    }
    let plan = plan(&store.tasks(), &store.worklists());
    let n = apply(store, &plan);
    if n > 0 {
        store.git_commit(&format!("wsp: {n} sub-task(s) of closed rows made follow-ups (wsp-345)"));
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, format!("{}\n{n}\n", crate::util::now_iso()));
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Group;

    fn row(id: &str, status: &str) -> Task {
        let mut t = Task::new(id, id);
        t.status_raw = status.into();
        t
    }

    fn list(id: &str, members: &[&str], status: WorklistStatus) -> Worklist {
        let mut w = Worklist::new(id, id);
        w.set_status(status);
        w.set_groups(&[Group { id: Some(1), members: members.iter().map(|m| m.to_string()).collect(), ..Default::default() }]);
        w
    }

    /// The rows nothing runs: found by a closed row, on no list still to run,
    /// under no seat. One held by any of those, or found by a row still being
    /// worked, or itself finished, is not named.
    #[test]
    fn a_follow_up_of_a_closed_row_on_no_list_and_no_seat_is_named_and_no_other() {
        let mut tasks = vec![row("o-done", "done"), row("o-review", "review"), row("o-doing", "doing")];
        let mut found = |id: &str, from: &str, status: &str| {
            let mut t = row(id, status);
            t.found_by = Some(from.into());
            tasks.push(t);
        };
        found("lost", "o-done", "todo");
        found("lost-too", "o-review", "inbox");
        found("origin-gone", "o-archived", "todo");
        found("on-a-list", "o-done", "todo");
        found("on-a-finished-list", "o-done", "todo");
        found("seated", "o-done", "doing");
        found("origin-open", "o-doing", "todo");
        found("finished", "o-done", "done");
        let lists = vec![list("l-1", &["on-a-list"], WorklistStatus::Running), list("l-2", &["on-a-finished-list"], WorklistStatus::Done)];
        let seated: BTreeSet<String> = ["seated".to_string()].into();
        let named: Vec<String> = unheld_in(&tasks, &lists, &seated).into_iter().map(|u| u.id).collect();
        assert_eq!(named, vec!["lost", "lost-too", "on-a-finished-list", "origin-gone"]);
        let said = line(&unheld_in(&tasks, &lists, &seated)).unwrap();
        assert!(said.starts_with("4 follow-up(s)") && said.contains("lost (from o-done)"), "{said}");
        assert_eq!(line(&[]), None);
    }

    /// The migration's fixture store: parented follow-ups (open, under a row at
    /// review or done, on none of the parent's lists) become follow-ups, and
    /// true sub-tasks — on the parent's list, under a parent still worked, or
    /// finished — are left as they were.
    #[test]
    fn the_migration_makes_parented_follow_ups_follow_ups_and_leaves_true_sub_tasks_alone() {
        let store = crate::testkit::store("follow-up-migration");
        let mut save = |id: &str, status: &str, parent: Option<&str>| {
            let mut t = row(id, status);
            t.parent = parent.map(str::to_string);
            store.save_task(&t).unwrap();
        };
        save("p-review", "review", None);
        save("p-done", "done", None);
        save("p-doing", "doing", None);
        save("found-at-review", "todo", Some("p-review"));
        save("found-after-done", "inbox", Some("p-done"));
        save("member-sub", "todo", Some("p-review"));
        save("still-worked", "todo", Some("p-doing"));
        save("finished-sub", "done", Some("p-done"));
        store.save_worklist(&list("l-1", &["p-review", "member-sub"], WorklistStatus::Running)).unwrap();

        let planned = plan(&store.tasks(), &store.worklists());
        let ids: Vec<&str> = planned.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["found-after-done", "found-at-review"]);
        assert_eq!(apply(&store, &planned), 2);

        for (id, from) in [("found-at-review", "p-review"), ("found-after-done", "p-done")] {
            let t = store.task(id).unwrap();
            assert_eq!((t.parent.as_deref(), t.found_by.as_deref()), (None, Some(from)), "{id}");
            assert!(t.body.contains("wsp-345 migration"), "a log line on each: {}", t.body);
        }
        for (id, parent) in [("member-sub", "p-review"), ("still-worked", "p-doing"), ("finished-sub", "p-done")] {
            let t = store.task(id).unwrap();
            assert_eq!((t.parent.as_deref(), t.found_by.as_deref()), (Some(parent), None), "{id} is a true sub-task");
        }

        // Once: the daemon's run leaves a marker, and a second does nothing.
        save("later", "todo", Some("p-done"));
        assert_eq!(migrate_once(&store), Some(1));
        assert_eq!(migrate_once(&store), None);
        save("later-still", "todo", Some("p-done"));
        assert_eq!(migrate_once(&store), None);
        assert_eq!(store.task("later-still").unwrap().parent.as_deref(), Some("p-done"));
        let _ = std::fs::remove_dir_all(&store.root);
    }
}
