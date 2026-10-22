//! A list member that turns out bigger than one seat, and the two ways it can
//! be split (`wsp-311`).
//!
//! compound-758 was a member sized "M-L. Split it if larger." Its seat wrote the
//! design, filed four sub-tasks with `wsp add --parent` (titles only, no
//! overview, no done-when), built all four itself in its one tree, and landed
//! them in two commits, one of about 4,000 lines. None of the four was a
//! member, so none was spawned, verified or seen at a barrier, and all four
//! stayed at `todo` while the member went to review. One verifier was left to
//! read about 4,700 lines against 758's done-when in a single pass, and nothing
//! in wsp said any of it.
//!
//! The way that runs is **legs** (`docs/legs.md`). Each piece gets its own
//! seat, commit and baton, and the row's verifier reads the whole. A seat
//! already holding the row hands it over: it commits, adds the legs, releases
//! the claim and stops. While a claim is held the run opens no leg
//! (`cycle::take_legs`), so no leg is ever started in the tree of a seat that
//! has not yet left it. The route is [`ROUTE`], which the member's brief
//! carries.
//!
//! This module holds the rest:
//!
//! - [`refusal`]: `wsp add --parent <its own row>` from a member's seat is
//!   refused without `--anyway`. A warning would print under a `+` receipt that
//!   reads as success, after the row already exists.
//! - [`unrun`]: the sub-tasks no run will start. `wsp worklist show`, the
//!   governor and the member's verifier are each told about them.

use crate::model::{Status, Task, Worklist};
use crate::worklist::Running;

/// What a member's seat does when its row turns out bigger than one seat. One
/// sentence with no newline, because the brief draws it as a single row.
pub(crate) fn route(id: &str) -> String {
    format!(
        "bigger than one seat (past ~80 turns, or more than one layer)? commit, then \
         `wsp leg add {id} <name> --order FILE` per piece ending in `finish`, then \
         `wsp release {id}` and stop: the run starts the legs in this tree (docs/legs.md)"
    )
}

/// The sentence that goes with [`route`]: why not a sub-task.
pub(crate) const NOT_A_SUBTASK: &str =
    "never `wsp add --parent` for a piece of this row: a sub-task is no member, so nothing runs or verifies it";

/// Why `wsp add --parent <parent>` is refused from `pane`, or `None` when it is
/// not. Refused when the pane is bound to the parent (its claim-holder or one of
/// its legs), the parent is a member of a running list, and it is at `doing`.
///
/// A member at review is left alone: a follow-up filed under finished work is
/// what the rules ask for. So is every seat not bound to the row, such as a
/// barrier check filing follow-ups, a governor or a person.
pub(crate) fn refusal(
    parent: &Task,
    bindings: &std::collections::BTreeMap<String, serde_json::Value>,
    running: &Running,
    pane: Option<&str>,
) -> Option<String> {
    let bound = pane
        .and_then(|p| bindings.get(p))
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        == Some(parent.id.as_str());
    let list = running.list_of(&parent.id)?;
    (bound && parent.status() == Status::Doing).then(|| {
        format!(
            "{id} is a member of the {list} list and this seat is working it: a sub-task filed under it is \
             not a member, so the run will never start it, verify it or bring it to a barrier. \
             Stage what is left as legs instead: {route}",
            id = parent.id,
            route = route(&parent.id),
        )
    })
}

/// What `--anyway` prints under the receipt: the same fact as the refusal, now
/// that the row exists.
pub(crate) fn filed_anyway(parent: &str, child: &str) -> String {
    format!(
        "{child} is not a member of {parent}'s list: no run starts or verifies it, and it shows as unrun under {parent} until it closes"
    )
}

/// The open sub-tasks of `member` that no run will start: children at `todo`
/// that are not themselves members of a running list. A barrier's follow-up
/// joins a group, so it is not counted.
pub(crate) fn unrun<'a>(tasks: &'a [Task], member: &str, running: &Running) -> Vec<&'a Task> {
    tasks
        .iter()
        .filter(|t| t.parent.as_deref() == Some(member))
        .filter(|t| t.status() == Status::Todo)
        .filter(|t| running.list_of(&t.id).is_none())
        .collect()
}

/// Whether a member's unrun sub-tasks are worth saying. They are while the
/// member is being worked or waiting on its verdict. Before it starts they are
/// an author's plan, and after it is done they are a person's to read.
pub(crate) fn worth_saying(member: &Task) -> bool {
    matches!(member.status(), Status::Doing | Status::Review)
}

/// Every member of `w` worth saying that has unrun sub-tasks, in the list's
/// order, with them.
pub(crate) fn on_list<'a>(tasks: &'a [Task], w: &Worklist, running: &Running) -> Vec<(&'a Task, Vec<&'a Task>)> {
    w.groups()
        .iter()
        .flat_map(|g| g.members.clone())
        .filter_map(|m| tasks.iter().find(|t| t.id == m))
        .filter(|t| worth_saying(t))
        .map(|t| (t, unrun(tasks, &t.id, running)))
        .filter(|(_, subs)| !subs.is_empty())
        .collect()
}

/// [`on_list`] as `wsp worklist show --json` carries it.
pub(crate) fn json(found: &[(&Task, Vec<&Task>)]) -> serde_json::Value {
    serde_json::Value::Array(
        found
            .iter()
            .map(|(m, subs)| {
                serde_json::json!({
                    "member": m.id,
                    "status": m.status().as_str(),
                    "subtasks": subs.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// One line for `wsp worklist show` and the governor: the member, how many, and
/// their ids.
pub(crate) fn line(member: &Task, subs: &[&Task]) -> String {
    format!(
        "{} is at {} with {} sub-task(s) at todo that no run starts or verifies: {}",
        member.id,
        member.status().as_str(),
        subs.len(),
        subs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(" ")
    )
}

/// The governor's sentence, said once for each landing of a member that has
/// unrun sub-tasks.
pub(crate) fn told(list: &str, member: &Task, subs: &[&Task]) -> String {
    format!(
        "{}. Its verifier is told to check the done-when item by item. If the pieces were meant to run on \
         their own, they are legs (`wsp leg add`), or members with a done-when each, and not sub-tasks · \
         wsp worklist show {list}",
        line(member, subs)
    )
}

/// What a verifier's order gains on a member with unrun sub-tasks. Empty when
/// it has none.
pub(crate) fn for_verifier(member: &Task, subs: &[&Task]) -> String {
    if subs.is_empty() {
        return String::new();
    }
    let rows: Vec<String> = subs.iter().map(|t| format!("- {} ({})", t.id, crate::util::truncate(&t.title, 80))).collect();
    format!(
        "\n\n**{m} was split into sub-tasks that never ran.** Its seat filed these under it, and every one is \
         still at todo:\n\n{rows}\n\n\
         None was a member, so no agent was started on any of them and none was verified. If {m}'s landing \
         holds their work, it was built in this one seat and you are its only reader. Do not take a sub-task's \
         title as done. Go through {m}'s **Done when** item by item, and for each item name the commit and \
         the test that cover it, or say that nothing does.",
        m = member.id,
        rows = rows.join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn row(id: &str, status: Status, parent: Option<&str>) -> Task {
        let mut t = Task::new(id, id);
        t.status_raw = status.as_str().into();
        t.parent = parent.map(str::to_string);
        t
    }

    /// A `Running` with `members` in one running list.
    fn running(members: &[&str]) -> Running {
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: members.iter().map(|m| m.to_string()).collect(), ..Default::default() }]);
        Running::over(&[w])
    }

    /// Who is refused `add --parent`, as a table: the seat bound to a member
    /// at doing is, and nobody else is.
    #[test]
    fn add_parent_is_refused_only_from_the_seat_working_a_member_at_doing() {
        let run = running(&["m-1"]);
        let bindings: BTreeMap<String, serde_json::Value> = [
            ("cpd-1".to_string(), json!({ "task_id": "m-1" })),
            ("cpd-2".to_string(), json!({ "task_id": "m-1", "leg": "build" })),
            ("cpd-3".to_string(), json!({ "task_id": "x-9" })),
        ]
        .into();
        let cases: [(&str, Status, &str, Option<&str>, bool); 7] = [
            ("the member's own seat, at doing", Status::Doing, "m-1", Some("cpd-1"), true),
            ("one of its legs", Status::Doing, "m-1", Some("cpd-2"), true),
            ("the same seat once the member is at review: a follow-up", Status::Review, "m-1", Some("cpd-1"), false),
            ("a seat on another row", Status::Doing, "m-1", Some("cpd-3"), false),
            ("no seat: a person or a barrier check", Status::Doing, "m-1", None, false),
            ("a seat bound to nothing", Status::Doing, "m-1", Some("cpd-7"), false),
            ("a row no list runs", Status::Doing, "x-9", Some("cpd-3"), false),
        ];
        for (what, status, parent, pane, refused) in cases {
            let p = row(parent, status, None);
            let got = refusal(&p, &bindings, &run, pane);
            assert_eq!(got.is_some(), refused, "{what}: {got:?}");
            if let Some(why) = got {
                assert!(why.contains(&format!("wsp leg add {parent}")) && why.contains(&format!("wsp release {parent}")), "{what}: names the legs route: {why}");
                assert!(why.contains("never start it, verify it"), "{what}: says what it costs: {why}");
            }
        }
    }

    /// Unrun is a child at todo that no running list holds: one already started,
    /// one closed, one that is a member (a barrier's follow-up), and another
    /// row's child are not.
    #[test]
    fn an_unrun_sub_task_is_a_child_at_todo_that_no_running_list_holds() {
        let run = running(&["m-1", "f-1"]);
        let tasks = vec![
            row("m-1", Status::Doing, None),
            row("s-1", Status::Todo, Some("m-1")),
            row("s-2", Status::Doing, Some("m-1")),
            row("s-3", Status::Done, Some("m-1")),
            row("f-1", Status::Todo, Some("m-1")),
            row("s-4", Status::Todo, Some("m-2")),
        ];
        let got: Vec<&str> = unrun(&tasks, "m-1", &run).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(got, ["s-1"]);
    }

    #[test]
    fn it_is_worth_saying_while_the_member_is_worked_or_waits_on_its_verdict() {
        for (status, worth) in [(Status::Todo, false), (Status::Doing, true), (Status::Review, true), (Status::Done, false), (Status::Blocked, false)] {
            assert_eq!(worth_saying(&row("m", status, None)), worth, "{status:?}");
        }
    }

    #[test]
    fn the_route_is_one_line_and_names_the_verbs() {
        let r = route("m-1");
        assert!(!r.contains('\n'));
        for verb in ["wsp leg add m-1", "finish", "wsp release m-1"] {
            assert!(r.contains(verb), "{verb}: {r}");
        }
    }
}
