//! Member dependencies: `after:`, the edge that says **this task rests on
//! that one** (`wsp-256`; compound `docs/ux-design.md` §19.8, Ed's answers in
//! compound-425 d6).
//!
//! # A wait, never provenance
//!
//! Group order already says that every member of group N waits on barrier
//! N-1. `after:` says *which* earlier member, and inside one group it adds a
//! wait the group cannot express. What made a task is `parent`; an edge here
//! that only means "this came out of that" would hold a member for nothing,
//! so the verb's help says so too.
//!
//! # On the task, read by every list
//!
//! The field is the task's ([`crate::model::Task::after`]), and every list
//! that holds both ends reads the one edge. That is why two of the five
//! refusals are checked against lists: an edge has to make sense in each list
//! it lands in, and a list edit has to keep making sense of the edges its
//! members already carry. **An edge to a task that is not on the list is not
//! an error** — the list carries it, the graph draws it as `not on this list`,
//! and the run ignores it.
//!
//! # What the run does with it
//!
//! Only a **same-group** edge holds anything: the run starts a member once
//! each member of its own group it rests on has **landed** ([`landed`]). Not
//! once it *holds* — that adds a verifier's whole turn to every wait — and
//! not once it is `done`, which is the person's. A cross-group edge is
//! already held by the barrier between, and an off-list edge by nothing the
//! list can see. A held member is at `todo` and unclaimed, so it is never
//! counted against the group's `parallel`; and cycles are refused when the
//! edge is set, so a hold always ends.
//!
//! Sent back after it landed, the member waited on changes nothing further
//! here: whatever started on it has started, and the barrier reads both.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::model::{Group, Status, Task, Worklist, WorklistStatus};
use crate::store::Store;
use crate::util::Paint;
use crate::worklist::{self, Reading, Repos, Settlement};
use crate::Args;

/// `wsp after <task> <on>…` replaces the task's list, `wsp after <task>
/// --clear` empties it, and `wsp after <task>` reads it.
///
/// Replaces rather than adds, so the line typed is the whole of the answer and
/// a reader of the shell history never has to reconstruct a list from a
/// sequence of edits.
pub fn run(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "usage: wsp after <task> <on>…   |   wsp after <task> --clear";
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let typed: Vec<String> = args.rest.iter().skip(1).flat_map(|s| split(s)).collect();
    let clear = args.has("clear");
    if clear && !typed.is_empty() {
        eprintln!("wsp: --clear empties the list, and `wsp after {} <on>…` replaces it — not both", t.id);
        return 2;
    }
    if !clear && typed.is_empty() {
        return print(store, args, &t);
    }

    let on = match resolve(store, &t.id, &typed) {
        Ok(on) => on,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };
    let saved = store.locked(|| -> Result<Task, String> {
        let mut t = store.find_task(&t.id).ok_or_else(|| format!("{} went away", t.id))?;
        refuse(store, &t, &on)?;
        t.after = on.clone();
        t.touch();
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok(t)
    });
    let t = match saved {
        Ok(t) => t,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };
    store.log_event("task-after", json!({ "id": t.id, "after": t.after }));
    store.git_commit(&match t.after.is_empty() {
        true => format!("wsp: {} rests on nothing", t.id),
        false => format!("wsp: {} after {}", t.id, t.after.join(" ")),
    });
    // A same-group edge cannot change under a started group (refusal 5), so
    // nothing the run is doing is changed by this. No poke.
    print(store, args, &t)
}

/// The receipt, which is also the reading: what it rests on now.
fn print(store: &Store, args: &Args, t: &Task) -> i32 {
    if args.json() {
        println!("{}", json!({ "id": t.id, "after": t.after }));
        return 0;
    }
    let p = Paint::new();
    if t.after.is_empty() {
        println!("{}  {}", p.bold(&t.id), p.dim("rests on nothing"));
        return 0;
    }
    println!("{}  {}", p.bold(&t.id), p.dim("after"));
    for on in &t.after {
        match store.find_task(on) {
            Some(o) => println!("  {}  {}  {}", p.dim(&o.id), p.dim(&crate::util::pad(o.status().as_str(), 8)), crate::util::truncate(&o.title, 56)),
            None => println!("  {}  {}", p.dim(on), p.dim("no task answers to it")),
        }
    }
    0
}

/// Ids as typed, one per word or comma: `--after 351,352` and
/// `--after 351 --after 352` say the same thing.
pub fn split(s: &str) -> Vec<String> {
    s.split([',', ' ']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect()
}

/// The ids typed, as the tasks' own ids, in order and once each — and
/// refusals 1 and 2, which need nothing but the typing.
///
/// `task` is the id of the task the edges are for, or `None` for one that does
/// not exist yet (`wsp add --after`), which cannot be named and so cannot be
/// named twice.
pub fn resolve(store: &Store, task: &str, typed: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for needle in typed {
        let on = store.task_or_why(needle).map_err(|why| {
            format!("{task} cannot rest on `{needle}`: {}", why.trim_start_matches("wsp: "))
        })?;
        // 1. On itself. Not a wait on anything, and the cycle of length one.
        if on.id == task {
            return Err(format!("{task} cannot rest on itself"));
        }
        if !out.contains(&on.id) {
            out.push(on.id);
        }
    }
    Ok(out)
}

/// Refusals 3 to 5 for setting `t`'s edges to `on`: a cycle among tasks, and
/// against every list holding both ends, an end in the wrong order or a
/// same-group edge changed under a started group.
///
/// Only the edges being added are checked for order, and only the edges
/// changing for a started group, so an edge a list already carries is never
/// the reason a different edit is refused.
pub fn refuse(store: &Store, t: &Task, on: &[String]) -> Result<(), String> {
    let tasks = store.tasks();
    // 3. A cycle: something `t` would rest on rests, by some path, on `t`.
    let edges: BTreeMap<&str, &[String]> = tasks.iter().map(|x| (x.id.as_str(), x.after.as_slice())).collect();
    for first in on {
        if let Some(path) = path_to(&edges, first, &t.id) {
            return Err(format!(
                "{} cannot rest on {first}: it would make a cycle — {} → {}",
                t.id,
                t.id,
                path.join(" → ")
            ));
        }
    }

    let added: Vec<&String> = on.iter().filter(|o| !t.after.contains(o)).collect();
    let changed: Vec<&String> =
        added.iter().copied().chain(t.after.iter().filter(|o| !on.contains(o))).collect();
    for w in store.worklists().iter().filter(|w| w.status() != WorklistStatus::Done) {
        let groups = w.groups();
        let Some(mine) = ordinal_of(&groups, &t.id) else { continue };
        let started = crate::cmd_worklist::started_groups(store, w);
        for o in &changed {
            let Some(theirs) = ordinal_of(&groups, o) else { continue };
            // 4. The task it rests on would run after it.
            if theirs > mine && added.contains(o) {
                return Err(format!(
                    "{} cannot rest on {o}: on `{}`, {o} is in group {theirs}, after {}'s group {mine} — \
                     `wsp worklist mv {} {o} --group {mine}` or earlier puts them in order",
                    t.id, w.id, t.id, w.id
                ));
            }
            // 5. A same-group edge is a hold the run reads, and a group the
            // run has started has already been read.
            if theirs == mine && mine <= started {
                let verb = if added.contains(o) { "cannot rest on" } else { "cannot stop resting on" };
                return Err(format!(
                    "{} {verb} {o} now: both are in group {mine} of `{}`, which its run has started, \
                     and the run read that group's holds when it did",
                    t.id, w.id
                ));
            }
        }
    }
    Ok(())
}

/// Refusals 4 and 5 for a list edit: `before` is the list as it stands, and
/// `groups` the membership it is about to be written with.
///
/// Only what the edit **changes** is refused — an edge out of order that the
/// edit made, or a same-group edge in a started group that it added or took
/// away. A list already carrying an out-of-order edge, from a hand edit or
/// from before this, is not made uneditable by it.
pub fn refuse_membership(store: &Store, before: &Worklist, groups: &[Group]) -> Result<(), String> {
    let tasks = store.tasks();
    let after_of: BTreeMap<&str, &[String]> = tasks.iter().map(|x| (x.id.as_str(), x.after.as_slice())).collect();
    let old = before.groups();
    let late = |gs: &[Group]| -> BTreeSet<(String, String, usize, usize)> {
        edges_on(gs, &after_of)
            .into_iter()
            .filter(|(_, _, mine, theirs)| theirs > mine)
            .collect()
    };
    let was: BTreeSet<(String, String)> = late(&old).into_iter().map(|(m, o, _, _)| (m, o)).collect();
    if let Some((m, o, mine, theirs)) = late(groups).into_iter().find(|(m, o, _, _)| !was.contains(&(m.clone(), o.clone()))) {
        return Err(format!(
            "{m} rests on {o} (after:), and this would put {o} in group {theirs} of `{}`, after {m}'s group {mine} — \
             `wsp after {m}` says what it rests on",
            before.id
        ));
    }

    let started = crate::cmd_worklist::started_groups(store, before);
    let same = |gs: &[Group], n: usize| -> BTreeSet<(String, String)> {
        edges_on(gs, &after_of)
            .into_iter()
            .filter(|(_, _, mine, theirs)| *mine == n && *theirs == n)
            .map(|(m, o, _, _)| (m, o))
            .collect()
    };
    for n in 1..=started.min(old.len()) {
        let (then, now) = (same(&old, n), same(groups, n));
        if let Some((m, o)) = now.symmetric_difference(&then).next() {
            let verb = if now.contains(&(m.clone(), o.clone())) { "add" } else { "take away" };
            return Err(format!(
                "this would {verb} a hold in group {n} of `{}` — {m} rests on {o} (after:), and the run \
                 has started that group and already read its holds",
                before.id
            ));
        }
    }
    Ok(())
}

/// Every edge whose two ends are both on these groups: `(member, on, member's
/// group, on's group)`, ordinals 1-based.
fn edges_on(groups: &[Group], after_of: &BTreeMap<&str, &[String]>) -> Vec<(String, String, usize, usize)> {
    let mut out = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        for m in &g.members {
            for o in after_of.get(m.as_str()).copied().unwrap_or_default() {
                if let Some(theirs) = ordinal_of(groups, o) {
                    out.push((m.clone(), o.clone(), i + 1, theirs));
                }
            }
        }
    }
    out
}

/// The group a task is in on this list, 1-based.
fn ordinal_of(groups: &[Group], id: &str) -> Option<usize> {
    groups.iter().position(|g| g.members.iter().any(|m| m == id)).map(|i| i + 1)
}

/// The path from `from` to `to` along `after:` edges, `from` first and `to`
/// last, if there is one. Depth-first, each task once.
fn path_to(edges: &BTreeMap<&str, &[String]>, from: &str, to: &str) -> Option<Vec<String>> {
    fn walk(edges: &BTreeMap<&str, &[String]>, at: &str, to: &str, seen: &mut BTreeSet<String>, path: &mut Vec<String>) -> bool {
        path.push(at.to_string());
        if at == to {
            return true;
        }
        if seen.insert(at.to_string()) {
            for next in edges.get(at).copied().unwrap_or_default() {
                if walk(edges, next, to, seen, path) {
                    return true;
                }
            }
        }
        path.pop();
        false
    }
    let mut path = Vec::new();
    walk(edges, from, to, &mut BTreeSet::new(), &mut path).then_some(path)
}

/// Each member of `g` with edges, and every edge it has — on the list or off
/// it. What `worklist show --json` carries as a group's `after`.
pub fn of_group(tasks: &[Task], g: &Group) -> BTreeMap<String, Vec<String>> {
    g.members
        .iter()
        .filter_map(|m| tasks.iter().find(|t| &t.id == m))
        .filter(|t| !t.after.is_empty())
        .map(|t| (t.id.clone(), t.after.clone()))
        .collect()
}

/// The run's holds on group `g`: each member it has not started, and the
/// members **of its own group** it rests on that have not landed.
///
/// Not started is `todo` and unclaimed, which is [`crate::cycle`]'s own test
/// for a member it may start; a member already going is waiting on nothing.
pub fn waiting(store: &Store, g: &Group) -> BTreeMap<String, Vec<String>> {
    let claims = store.claims();
    let mut repos: Option<Repos> = None;
    let mut out = BTreeMap::new();
    for m in &g.members {
        let Some(t) = store.find_task(m) else { continue };
        if t.status() != Status::Todo || claims.contains_key(&t.id) {
            continue;
        }
        let on: Vec<String> = t
            .after
            .iter()
            .filter(|o| g.members.contains(o))
            .filter(|o| !landed(store, &mut repos, o))
            .cloned()
            .collect();
        if !on.is_empty() {
            out.insert(t.id.clone(), on);
        }
    }
    out
}

/// Whether a member's work is on the trunk — the event a hold waits for.
///
/// **The member's own record first**: `wsp land` writes [`crate::repair::LANDED`]
/// on it, and so does the repair that finds a landing nobody recorded. It is
/// read rather than git because the run asks on every tick, and because it
/// stays true when the member is sent back afterwards — which is the rule
/// (§19.8): what started on it has started, and the barrier reads both.
///
/// Then, for a member that is settled with nothing recorded, git: a member
/// with no repository at all (design-only work) or whose branch holds nothing
/// the trunk has not got is as landed as it will ever be. One git reading per
/// such member, and only for one at `review` or `done` without the line.
fn landed(store: &Store, repos: &mut Option<Repos>, id: &str) -> bool {
    let Some(t) = store.find_task(id) else { return true };
    if crate::repair::landed(&t).is_some() {
        return true;
    }
    if !Settlement::of(&t).settled() {
        return false;
    }
    let repos = repos.get_or_insert_with(|| Repos::new(store));
    worklist::member(store, repos, id, Reading::Landed).finished()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Group;
    use crate::store::Store;

    /// A store of its own and nothing ambient, as `cmd_worklist`'s tests
    /// have it: nothing here reads the environment, so these run side by side.
    fn store_named(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-after-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    fn task(store: &Store, id: &str, after: &[&str]) {
        let mut t = Task::new(id, id);
        t.project = Some("p".into());
        t.after = after.iter().map(|s| s.to_string()).collect();
        store.save_task(&t).unwrap();
    }

    fn list(store: &Store, id: &str, groups: &[&[&str]], status: WorklistStatus) {
        let mut w = Worklist::new(id, id);
        w.set_status(status);
        let gs: Vec<Group> = groups
            .iter()
            .map(|g| Group { members: g.iter().map(|s| s.to_string()).collect(), agent: "claude".into(), ..Group::default() })
            .collect();
        w.set_groups(&gs);
        store.save_worklist(&w).unwrap();
    }

    fn after(store: &Store, words: &[&str], flags: &[(&str, &str)]) -> i32 {
        run(store, &Args::synth("after", words, flags))
    }

    fn edges(store: &Store, id: &str) -> Vec<String> {
        store.find_task(id).unwrap().after
    }

    #[test]
    fn the_edge_is_written_on_the_task_and_comes_back_in_its_json() {
        let s = store_named("1");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &[]);
        task(&s, "t-3", &[]);
        assert_eq!(after(&s, &["t-3", "t-1", "t-2,t-1"], &[]), 0);
        assert_eq!(edges(&s, "t-3"), ["t-1", "t-2"], "in the order typed, once each");
        let t = s.find_task("t-3").unwrap();
        assert_eq!(t.json()["after"], json!(["t-1", "t-2"]), "show --json reads Task::json");
        assert!(t.render().contains("after:"), "on disk, in the frontmatter");
        // Replaces, never appends: the line typed is the whole answer.
        assert_eq!(after(&s, &["t-3", "t-2"], &[]), 0);
        assert_eq!(edges(&s, "t-3"), ["t-2"]);
        assert_eq!(after(&s, &["t-3"], &[("clear", "")]), 0);
        assert!(edges(&s, "t-3").is_empty());
        assert!(!s.find_task("t-3").unwrap().render().contains("after:"), "no empty line on a task with none");
    }

    /// `wsp add --after` sets the edge at filing, and refuses a typo before
    /// an id is taken, as `--parent` does.
    #[test]
    fn add_after_files_the_task_resting_on_those_and_refuses_one_that_does_not_exist() {
        let s = store_named("add");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &[]);
        let add = |title: &str, on: &str| {
            crate::cmd_task::add(&s, &Args::synth("add", &[title], &[("inbox", ""), ("after", on)]))
        };
        assert_eq!(add("rests", "t-1,t-2"), 0);
        let t = s.tasks().into_iter().find(|t| t.title == "rests").expect("filed");
        assert_eq!(t.after, ["t-1", "t-2"]);
        assert_eq!(add("nowhere", "t-404"), 1);
        assert!(!s.tasks().iter().any(|t| t.title == "nowhere"), "refused, and nothing filed");
    }

    #[test]
    fn a_task_cannot_rest_on_itself() {
        let s = store_named("2");
        task(&s, "t-1", &[]);
        assert_eq!(after(&s, &["t-1", "t-1"], &[]), 1);
        assert!(edges(&s, "t-1").is_empty());
    }

    #[test]
    fn an_edge_to_a_task_that_does_not_exist_is_refused() {
        let s = store_named("3");
        task(&s, "t-1", &[]);
        assert_eq!(after(&s, &["t-1", "t-404"], &[]), 1);
        assert!(edges(&s, "t-1").is_empty());
    }

    #[test]
    fn an_edge_that_closes_a_cycle_is_refused_and_names_it() {
        let s = store_named("4");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &["t-1"]);
        task(&s, "t-3", &["t-2"]);
        let t1 = s.find_task("t-1").unwrap();
        let why = refuse(&s, &t1, &["t-3".into()]).unwrap_err();
        assert!(why.contains("t-1 → t-3 → t-2 → t-1"), "{why}");
        assert_eq!(after(&s, &["t-1", "t-3"], &[]), 1);
        assert!(edges(&s, "t-1").is_empty());
    }

    /// Ed's trade (§19.8): wsp-142 rests on wsp-143 in one list and the two
    /// sit the other way round in another, so the edge is refused naming the
    /// list where it does not fit.
    #[test]
    fn an_edge_on_a_task_in_a_later_group_is_refused_naming_the_list() {
        let s = store_named("5");
        task(&s, "t-142", &[]);
        task(&s, "t-143", &[]);
        list(&s, "runs-itself", &[&["t-143"], &["t-142"]], WorklistStatus::Draft);
        list(&s, "process", &[&["t-142"], &["t-143"]], WorklistStatus::Draft);
        let t = s.find_task("t-142").unwrap();
        let why = refuse(&s, &t, &["t-143".into()]).unwrap_err();
        assert!(why.contains("`process`") && why.contains("group 2"), "{why}");
        assert_eq!(after(&s, &["t-142", "t-143"], &[]), 1);
    }

    #[test]
    fn a_same_group_edge_is_refused_once_the_run_has_started_that_group() {
        let s = store_named("6");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &[]);
        task(&s, "t-3", &[]);
        task(&s, "t-4", &[]);
        list(&s, "run", &[&["t-1", "t-2"], &["t-3", "t-4"]], WorklistStatus::Running);
        let why = refuse(&s, &s.find_task("t-2").unwrap(), &["t-1".into()]).unwrap_err();
        assert!(why.contains("`run`") && why.contains("started"), "{why}");
        // A group the run has not reached is still being composed.
        assert!(refuse(&s, &s.find_task("t-4").unwrap(), &["t-3".into()]).is_ok());
        // And a cross-group edge may change at any time: it only changes the picture.
        assert!(refuse(&s, &s.find_task("t-3").unwrap(), &["t-1".into()]).is_ok());
    }

    #[test]
    fn taking_a_same_group_edge_away_in_a_started_group_is_refused_too() {
        let s = store_named("7");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &["t-1"]);
        list(&s, "run", &[&["t-1", "t-2"]], WorklistStatus::Running);
        assert_eq!(after(&s, &["t-2"], &[("clear", "")]), 1);
        assert_eq!(edges(&s, "t-2"), ["t-1"]);
    }

    /// The edge is the task's and the list only reads it: one to a task the
    /// list does not hold is carried, not refused.
    #[test]
    fn an_edge_off_the_list_is_kept_and_not_refused() {
        let s = store_named("8");
        task(&s, "t-1", &[]);
        task(&s, "t-9", &[]);
        list(&s, "run", &[&["t-1"]], WorklistStatus::Running);
        assert_eq!(after(&s, &["t-1", "t-9"], &[]), 0);
        assert_eq!(edges(&s, "t-1"), ["t-9"]);
        let w = s.worklist("run").unwrap();
        let tasks = s.tasks();
        assert_eq!(of_group(&tasks, &w.groups()[0]).get("t-1"), Some(&vec!["t-9".to_string()]));
        assert!(waiting(&s, &w.groups()[0]).is_empty(), "the run ignores an edge off the list");
    }

    #[test]
    fn a_list_edit_that_puts_a_rested_on_task_later_is_refused() {
        let s = store_named("9");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &["t-1"]);
        list(&s, "plan", &[&["t-1"], &["t-2"]], WorklistStatus::Draft);
        let w = s.worklist("plan").unwrap();
        let swapped = vec![
            Group { members: vec!["t-2".into()], ..Group::default() },
            Group { members: vec!["t-1".into()], ..Group::default() },
        ];
        let why = refuse_membership(&s, &w, &swapped).unwrap_err();
        assert!(why.contains("t-2 rests on t-1") && why.contains("`plan`"), "{why}");
        // Into one group is in order: it becomes a hold.
        let together = vec![Group { members: vec!["t-1".into(), "t-2".into()], ..Group::default() }];
        assert!(refuse_membership(&s, &w, &together).is_ok());
    }

    #[test]
    fn a_member_joining_a_started_group_with_a_hold_in_it_is_refused() {
        let s = store_named("10");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &["t-1"]);
        list(&s, "run", &[&["t-1"]], WorklistStatus::Running);
        let w = s.worklist("run").unwrap();
        let joined = vec![Group { members: vec!["t-1".into(), "t-2".into()], ..Group::default() }];
        let why = refuse_membership(&s, &w, &joined).unwrap_err();
        assert!(why.contains("group 1") && why.contains("started"), "{why}");
        // The same member joining as a group of its own is a cross-group edge.
        let next = vec![
            Group { members: vec!["t-1".into()], ..Group::default() },
            Group { members: vec!["t-2".into()], ..Group::default() },
        ];
        assert!(refuse_membership(&s, &w, &next).is_ok());
    }

    #[test]
    fn a_member_waits_on_a_same_group_member_until_it_lands() {
        let s = store_named("11");
        task(&s, "t-1", &[]);
        task(&s, "t-2", &["t-1"]);
        list(&s, "run", &[&["t-1", "t-2"]], WorklistStatus::Running);
        let g = s.worklist("run").unwrap().groups().remove(0);
        assert_eq!(waiting(&s, &g).get("t-2"), Some(&vec!["t-1".to_string()]));

        // Going is not landed: a hold is on the trunk, not on the status.
        let mut t1 = s.find_task("t-1").unwrap();
        t1.status_raw = "doing".into();
        t1.log("wsp: something else");
        s.save_task(&t1).unwrap();
        assert!(waiting(&s, &g).contains_key("t-2"));

        t1.log(&format!("{} 1234567abc on master", crate::repair::LANDED));
        s.save_task(&t1).unwrap();
        assert!(waiting(&s, &g).is_empty(), "landed is the whole test");
    }
}
