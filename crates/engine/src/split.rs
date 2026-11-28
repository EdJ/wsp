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
//! - [`Refused`]: the verbs a working seat is refused, with what it does
//!   instead. A packet whose row names one lists it, `wsp review` warns once
//!   about a done-when that needs one, and `wsp edit --overview` says when an
//!   overview names one (`wsp-312`).

use crate::model::{Status, Task, Worklist};
use crate::worklist::Running;

/// What a member's seat does when its row turns out bigger than one seat. One
/// sentence with no newline, because the brief draws it as a single row.
pub fn route(id: &str) -> String {
    format!("bigger than one seat (past ~80 turns, or more than one layer)? {}", steps(id))
}

/// The three steps of [`route`], for a sentence that has already said why.
fn steps(id: &str) -> String {
    format!(
        "commit, then `wsp leg add {id} <name> --order FILE` per piece ending in `finish`, then \
         `wsp release {id}` and stop: the run starts the legs in this tree (docs/legs.md)"
    )
}

/// What a member's seat does with a done-when item that only another list's
/// run can show (`wsp-870`).
pub fn wait_elsewhere(id: &str) -> String {
    format!(
        "a done-when only another list's run can show? `wsp after {id} <that row or list>`, commit, `wsp release {id}`: \
         the run seats you again once it has finished; do not ask"
    )
}

/// The sentence that goes with [`route`]: why not a sub-task.
pub const NOT_A_SUBTASK: &str =
    "never `wsp add --parent` for a piece of this row: a sub-task is no member, so nothing runs or verifies it; \
     work found while doing it, outside its done-when, is a follow-up: `wsp add \"…\" --from <this row>`, which never holds the row open";

/// Why `wsp add --parent <parent>` is refused from `pane`, or `None` when it is
/// not. Refused when the pane is bound to the parent (its claim-holder or one of
/// its legs), the parent is a member of a running list, and it is at `doing`.
///
/// A member at review is left alone, as it was when follow-ups were filed
/// under finished work; they are filed with `--from` now (`wsp-345`). So is every seat not bound to the row, such as a
/// barrier check filing follow-ups, a governor or a person.
pub fn refusal(
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
             Stage what is left as legs instead: {steps}",
            id = parent.id,
            steps = steps(&parent.id),
        )
    })
}

/// What `--anyway` prints under the receipt: the same fact as the refusal, now
/// that the row exists.
pub fn filed_anyway(parent: &str, child: &str) -> String {
    format!(
        "{child} is not a member of {parent}'s list: no run starts or verifies it, and it shows as unrun under {parent} until it closes"
    )
}

/// A verb a seat working a row is refused (`wsp-312`).
///
/// spec-breakdown's orders named three. wsp-154's overview told its own seat to
/// file sub-tasks with `wsp add --parent`, which [`refusal`] refuses to the seat
/// working a member. A seat working one row is refused `wsp leg add` on another
/// (`legs::another_rows_seat`). wsp-303's barrier order told its seat to
/// `wsp mv` 86 rows, and `wsp mv` is on the person's deny list, so every agent
/// seat is refused it. Each one became an ask and a done-when nobody could
/// finish. A packet whose row names one lists it with what to do instead
/// ([`seat_rule`]), and a done-when or overview that names one is warned about
/// ([`named_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    AddParent,
    LegAdd,
    Mv,
}

impl Refused {
    pub const ALL: [Refused; 3] = [Refused::AddParent, Refused::LegAdd, Refused::Mv];

    /// The verb as an order names it.
    pub fn verb(self) -> &'static str {
        match self {
            Refused::AddParent => "wsp add --parent",
            Refused::LegAdd => "wsp leg add",
            Refused::Mv => "wsp mv",
        }
    }

    /// When it is refused, and what the seat working `id` does instead, as one
    /// line. The words are the refusals' own: [`steps`] for the sub-task,
    /// `legs::GIVEN_BY` for legs.
    pub fn line(self, id: &str) -> String {
        match self {
            Refused::AddParent => format!(
                "`wsp add --parent {id}`: refused to the seat working {id} as a list member, because a sub-task is no member and nothing runs or verifies it. Instead: {}",
                steps(id)
            ),
            Refused::LegAdd => format!(
                "`wsp leg add <other row>`: refused, because {}. Instead: {}. A row with `## Pieces` takes no legs either: `wsp piece add` stages a piece",
                crate::legs::GIVEN_BY,
                crate::legs::ask_a_planner("that row")
            ),
            Refused::Mv => format!(
                "`wsp mv`: on the person's deny list, so every agent seat is refused it. Instead: `wsp ask {id} --person` for the person to run it, and hold: what rests on it stays not held until they say it is done"
            ),
        }
    }
}

/// The packet section for the verbs in `named`, for the seat working `id`:
/// each with what to do instead. `None` when the row names none, so a packet
/// says nothing of verbs nobody asked for.
pub fn seat_rule(id: &str, named: &[Refused]) -> Option<String> {
    if named.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## Verbs this seat is refused\nThis row's words name verbs this seat is refused. Where they ask you to run one, do its instead, and where that leaves part of the work undone, mark that item not held in your exit and ask about it.",
    );
    for r in named {
        out.push_str(&format!("\n- {}", r.line(id)));
    }
    Some(out)
}

/// What `wsp review` says, once, when the done-when of `id` needs a verb its
/// seat is refused: one line naming every such verb, or `None`.
pub fn warn_done_when(done_when: &str, id: &str) -> Option<String> {
    let named = named_in(done_when, Some(id));
    (!named.is_empty()).then(|| {
        let verbs: Vec<String> = named.iter().map(|r| format!("`{}`", r.verb())).collect();
        let instead: Vec<String> = named.iter().map(|r| r.line(id)).collect();
        format!(
            "{id}'s done-when needs {}, which its seat is refused: an item that rests on it is not held until someone else runs it — {}",
            verbs.join(", "),
            instead.join(" · ")
        )
    })
}

/// The verbs in [`Refused`] that `text` tells the seat working `own` to run.
/// `own` is `None` when no one row's seat reads it, such as a worklist's
/// overview.
///
/// `wsp mv` is refused every agent seat, so it counts wherever it is named.
/// `wsp add --parent` counts unless it names a parent other than `own`, because
/// [`refusal`] refuses only the seat's own row. `wsp leg add` counts unless it
/// names `own`: legs on its own row are the seat's to give, and a bare one, or
/// one with a placeholder such as `<id>`, is a planner's step the seat cannot
/// take. With no `own`, a row's legs are anyone's call, so it does not count.
pub fn named_in(text: &str, own: Option<&str>) -> Vec<Refused> {
    let after = |line: &str, at: &str| -> Option<String> {
        let rest = &line[line.find(at)? + at.len()..];
        let word = rest.split_whitespace().next()?;
        let word = word.trim_matches(|c: char| "`\"'.,;:()[]".contains(c));
        (!word.is_empty() && !word.contains('<') && word.chars().any(|c| c.is_ascii_digit())).then(|| word.to_string())
    };
    let other = |id: Option<String>| match (id, own) {
        (Some(id), Some(own)) => id != own,
        (Some(_), None) => true,
        (None, _) => false,
    };
    let mut found = Vec::new();
    for line in text.lines() {
        if (line.contains("add --parent") || (line.contains("wsp add") && line.contains("--parent")))
            && !other(after(line, "--parent"))
        {
            found.push(Refused::AddParent);
        }
        if line.contains("wsp leg add") && own.is_some() && after(line, "wsp leg add").as_deref() != own {
            found.push(Refused::LegAdd);
        }
        if line.split("wsp mv").skip(1).any(|rest| rest.is_empty() || rest.starts_with([' ', '`', ':', ','])) {
            found.push(Refused::Mv);
        }
    }
    Refused::ALL.into_iter().filter(|r| found.contains(r)).collect()
}

/// What `wsp edit --overview` prints when the overview of `own` names a
/// refused verb, one line per verb, or nothing.
pub fn warn_order(text: &str, own: Option<&str>) -> Vec<String> {
    let id = own.unwrap_or("<row>");
    named_in(text, own)
        .into_iter()
        .map(|r| format!("this overview names `{}`, which the seat that reads it is refused — {}", r.verb(), r.line(id)))
        .collect()
}

/// The open sub-tasks of `member` that no run will start: children at `todo`
/// that are not themselves members of a running list. A barrier's follow-up
/// joins a group, so it is not counted.
pub fn unrun<'a>(tasks: &'a [Task], member: &str, running: &Running) -> Vec<&'a Task> {
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
pub fn worth_saying(member: &Task) -> bool {
    matches!(member.status(), Status::Doing | Status::Review)
}

/// Every member of `w` worth saying that has unrun sub-tasks, in the list's
/// order, with them.
pub fn on_list<'a>(tasks: &'a [Task], w: &Worklist, running: &Running) -> Vec<(&'a Task, Vec<&'a Task>)> {
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
pub fn json(found: &[(&Task, Vec<&Task>)]) -> serde_json::Value {
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
pub fn line(member: &Task, subs: &[&Task]) -> String {
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
pub fn told(list: &str, member: &Task, subs: &[&Task]) -> String {
    format!(
        "{}. Its verifier is told to check the done-when item by item. If the pieces were meant to run on \
         their own, they are legs (`wsp leg add`), or members with a done-when each, and not sub-tasks · \
         wsp worklist show {list}",
        line(member, subs)
    )
}

/// What a verifier's order gains on a member with unrun sub-tasks. Empty when
/// it has none.
pub fn for_verifier(member: &Task, subs: &[&Task]) -> String {
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

    /// spec-breakdown's three, as wsp-154's overview and wsp-303's barrier
    /// order wrote them, are each found; the uses a seat may make are not.
    #[test]
    fn an_order_naming_a_verb_its_seat_is_refused_is_found_and_one_it_may_run_is_not() {
        let own = Some("wsp-154");
        assert_eq!(named_in("2. Sub-tasks of this row (`wsp add --parent`), each small enough for one seat", own), vec![Refused::AddParent]);
        assert_eq!(named_in("wsp add \"x\" --parent wsp-154", own), vec![Refused::AddParent]);
        assert_eq!(named_in("- `wsp mv <id> --parent none` for each of the 86.", own), vec![Refused::Mv]);
        assert_eq!(named_in("then `wsp leg add wsp-155 a --order FILE`", own), vec![Refused::LegAdd]);
        // What the seat may run: a sub-task under another row, legs on its own
        // row or a placeholder, and words that only look like the verb.
        assert!(named_in("file a follow-up with `wsp add \"y\" --parent wsp-200`", own).is_empty());
        assert!(named_in("commit, then `wsp leg add wsp-154 <name> --order FILE`", own).is_empty());
        // A planner's step, bare or with a placeholder, is not this seat's.
        assert_eq!(named_in("split it with `wsp leg add` per phase", own), vec![Refused::LegAdd]);
        assert_eq!(named_in("`wsp leg add <id> <name> --order FILE` per leg", own), vec![Refused::LegAdd]);
        assert!(named_in("wsp mvp is not a verb", own).is_empty());
        // A worklist's overview: no row reads it, so a leg is anyone's call.
        assert!(named_in("`wsp leg add wsp-9 a`", None).is_empty());
        assert_eq!(named_in("wsp mv it\nwsp add --parent", None), vec![Refused::AddParent, Refused::Mv]);
    }

    /// The packet section names the verbs the row named, each with what to do
    /// instead in the refusals' own words, and is absent when it named none.
    #[test]
    fn the_seat_rule_names_each_refused_verb_with_its_instead() {
        let rule = seat_rule("m-1", &Refused::ALL).unwrap();
        for r in Refused::ALL {
            assert!(rule.contains(&format!("`{}", r.verb())), "{r:?}: {rule}");
        }
        assert_eq!(rule.matches("Instead:").count(), 3, "{rule}");
        assert!(rule.contains(&steps("m-1")) && rule.contains(crate::legs::GIVEN_BY) && rule.contains("`wsp ask m-1 --person`"), "{rule}");
        let one = seat_rule("m-1", &[Refused::Mv]).unwrap();
        assert!(one.contains("`wsp mv`") && !one.contains("`wsp add --parent"), "{one}");
        assert_eq!(seat_rule("m-1", &[]), None);
        assert_eq!(warn_order("wsp mv x", Some("m-1")).len(), 1);
        assert!(warn_order("nothing refused here", Some("m-1")).is_empty());
    }

    /// wsp-303's done-when rested on `wsp mv`. Naming two verbs over several
    /// lines is still one warning, and a done-when that needs none has none.
    #[test]
    fn a_done_when_needing_refused_verbs_is_one_warning() {
        let dw = "- only wsp-302 keeps parent wsp-155, after `wsp mv <id> --parent none`\n- the 86 rows are detached with wsp mv\n- sub-tasks filed with `wsp add --parent`";
        let said = warn_done_when(dw, "wsp-303").unwrap();
        assert!(!said.contains('\n'), "{said}");
        assert_eq!(said.matches("`wsp mv`").count(), 2, "named once, and once in its instead: {said}");
        assert!(said.contains("`wsp add --parent`, `wsp mv`") && said.contains("`wsp ask wsp-303 --person`"), "{said}");
        assert_eq!(warn_done_when("- `cargo test packet::` passes", "wsp-303"), None);
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
