//! The barrier's mechanical part, run by wsp: `wsp-326`.
//!
//! Ed, 2026-11-01. Of 186 barrier-check transcripts the median is nine turns and
//! 0.54M tokens, and 24% of the calls build and test, 20% inspect git and 19%
//! read wsp state to gather members and verdicts; 0.2% drive the app. A barrier
//! does three jobs — integration, the stop condition, and steering the run — and
//! only the third needs an agent every time.
//!
//! # What wsp runs, once per group, at the trunk
//!
//! For each repository the group's members live in, at the trunk's tip:
//!
//! 1. the project's `at barrier` steps — the full suite ([`crate::check`]'s third
//!    tier);
//! 2. every member's own recorded `## Checks`, replayed on the integrated trunk;
//! 3. the group's `stop-checks:`, the mechanical half of the stop condition.
//!
//! Every step runs whatever the one before did, because a red suite is no reason
//! not to read a member's own check: a failure is **attributed** to a member by
//! the member's recorded check that failed, and for the suite's by the paths each
//! member landed against the files the output names.
//!
//! # When no agent is asked
//!
//! The barrier is passed by wsp when nothing needs judgment: every member's
//! newest verdict holds, every check was run and passed, and the stop has no
//! prose. The verdict says why, and the entry on the group says `by:wsp`. Any
//! other barrier gets its agent as before, with the **packet** — members,
//! verdicts, the stop and these results, inline — in its work order, so the
//! turns it spent gathering and rebuilding go to what only it can do.
//!
//! **The automatic pass is off until a project turns it on**, with `auto-pass:
//! on` under a `## Barrier` heading in its body. The replay (`docs/barrier.md`)
//! is what decides that; without the line wsp still runs everything, still logs
//! `would pass without an agent`, and still hands the agent the packet.
//!
//! # Where it runs
//!
//! `wsp worklist advance <list> --barrier-run <n>`, started detached by the
//! step that opens the check, because the suite takes minutes and the tick does
//! not wait. The entry is written `suite:running` before it starts and is the
//! only record; the packet is a file under the state directory, keyed by list
//! and group, and the agent's brief reads it.

use std::path::PathBuf;
use std::time::Instant;

use crate::barrier::{self, Suite};
use crate::check::{self, Done, Of, Ran, Step};
use crate::cmd_verify::{git, toplevel};
use crate::model::{Group, Landed, Task, Worklist};
use crate::store::Store;
use crate::util;

/// Where a step came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// The project's `at barrier` step.
    Suite,
    /// A member's own recorded check.
    Member,
    /// The group's `stop-checks:`.
    Stop,
}

/// One step to run, and whose it is.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub step: Step,
    pub kind: Kind,
    /// The members whose recorded checks name this command.
    pub owners: Vec<String>,
}

/// One repository's steps, run at its trunk's tip.
#[derive(Clone, Debug)]
pub(crate) struct Unit {
    pub project: String,
    pub repo: PathBuf,
    pub sha: String,
    pub items: Vec<Item>,
}

/// Everything a group's barrier would run.
#[derive(Clone, Debug, Default)]
pub(crate) struct Plan {
    pub units: Vec<Unit>,
    /// Stop checks with no repository to run in.
    pub unplaced: Vec<String>,
}

impl Plan {
    /// Whether anything is declared to run.
    pub(crate) fn declared(&self) -> bool {
        self.units.iter().any(|u| !u.items.is_empty()) || !self.unplaced.is_empty()
    }
}

fn repo_of(store: &Store, project: &str) -> Option<PathBuf> {
    let mut at = Some(project.to_string());
    while let Some(p) = at.take() {
        let found = store.project(&p)?;
        if !found.roots.is_empty() {
            return found.roots.iter().find_map(|r| toplevel(&util::expand(r)));
        }
        at = found.parent.clone();
    }
    None
}

fn tip_of(repo: &std::path::Path) -> Option<String> {
    let trunk = crate::cmd_checkout::trunk(repo)?;
    let branch = crate::cmd_checkout::trunk_branch(&trunk)?;
    git(&trunk, &["rev-parse", &branch]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// What group `at` of `w` would run at its barrier.
pub(crate) fn plan(store: &Store, w: &Worklist, at: usize) -> Plan {
    let groups = w.groups();
    let Some(g) = groups.get(at - 1) else { return Plan::default() };
    let mut units: Vec<Unit> = Vec::new();
    let mut members_in: Vec<usize> = Vec::new();
    let mut seen: Vec<(usize, String)> = Vec::new();
    for id in &g.members {
        let Some(task) = store.find_task(id) else { continue };
        let Some(project) = check::project_of(&task) else { continue };
        let Some(repo) = repo_of(store, &project) else { continue };
        let i = match units.iter().position(|u| u.repo == repo) {
            Some(i) => i,
            None => {
                let Some(sha) = tip_of(&repo) else { continue };
                units.push(Unit { project: project.clone(), repo, sha, items: Vec::new() });
                members_in.push(0);
                units.len() - 1
            }
        };
        members_in[i] += 1;
        if !seen.contains(&(i, project.clone())) {
            seen.push((i, project.clone()));
            for step in check::barrier_steps(store, &project) {
                add(&mut units[i], step, Kind::Suite, None);
            }
        }
        for step in check::task_steps(&task) {
            add(&mut units[i], step, Kind::Member, Some(&task.id));
        }
    }
    let mut plan = Plan { units, unplaced: Vec::new() };
    let home = members_in.iter().enumerate().rev().max_by_key(|(_, n)| **n).map(|(i, _)| i);
    for item in &g.stop_checks {
        let Some(step) = check::step_of(item, Of::Project) else { continue };
        let step = Step { name: format!("stop:{}", step.name), barrier: true, ..step };
        match home {
            Some(i) => plan.units[i].items.push(Item { step, kind: Kind::Stop, owners: Vec::new() }),
            None => plan.unplaced.push(item.clone()),
        }
    }
    for u in &mut plan.units {
        u.items.sort_by_key(|i| i.kind);
    }
    plan
}

/// A command named twice runs once; the members who named it share the answer.
fn add(unit: &mut Unit, step: Step, kind: Kind, owner: Option<&str>) {
    if let Some(have) = unit.items.iter_mut().find(|i| i.kind != Kind::Stop && i.step.command == step.command) {
        if let Some(o) = owner.filter(|o| !have.owners.iter().any(|x| x == o)) {
            have.owners.push(o.to_string());
        }
        return;
    }
    unit.items.push(Item { step, kind, owners: owner.map(|o| vec![o.to_string()]).unwrap_or_default() });
}

/// Whether the barrier of group `at` has anything for wsp to run.
pub(crate) fn owed(store: &Store, w: &Worklist, at: usize) -> bool {
    plan(store, w, at).declared()
}

// ---- what happened ---------------------------------------------------------

/// One unit's answers, or why it could not be run.
pub(crate) enum Ranged {
    Ran(Vec<Ran>),
    Could(String),
}

/// Whose fault a failed step is read to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blame {
    pub step: String,
    /// (member, why it is named).
    pub members: Vec<(String, String)>,
}

/// The members a failed step is read to belong to.
///
/// A member's own recorded check that fails is that member's. A suite or stop
/// step that fails is named for the members who landed a file the output
/// mentions, by its path, its file name, or for a Rust file its module
/// (`store::`) — the form a test failure is written in.
pub(crate) fn blame(item: &Item, ran: &Ran, landed: &[Landed]) -> Blame {
    let mut out = Blame { step: item.step.name.clone(), members: Vec::new() };
    if item.kind == Kind::Member {
        out.members = item.owners.iter().map(|o| (o.clone(), "its own recorded check".to_string())).collect();
        return out;
    }
    let mut text = ran.tail.clone();
    if let Some((_, first)) = &ran.first {
        text.push('\n');
        text.push_str(first);
    }
    for l in landed {
        let hit = l.files.iter().find(|f| {
            let name = f.rsplit('/').next().unwrap_or(f);
            let module = name.strip_suffix(".rs").map(|s| format!("{s}::"));
            text.contains(f.as_str()) || (name.contains('.') && text.contains(name)) || module.is_some_and(|m| text.contains(&m))
        });
        if let Some(f) = hit {
            out.members.push((l.member.clone(), format!("landed {f}, which the output names")));
        }
    }
    out
}

/// Why wsp could not decide, or the clean pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Decision {
    /// Nothing needs judgment.
    pub would_pass: bool,
    /// And the project has turned the automatic pass on.
    pub pass: bool,
    /// What needs an agent, when something does.
    pub needs: Vec<String>,
}

/// What a barrier's agent-free pass is read from.
pub(crate) struct Facts<'a> {
    pub enabled: bool,
    pub stop: &'a str,
    /// Each member and whether its newest verdict holds with every item it
    /// checked held.
    pub members: Vec<(String, bool)>,
    pub ran: usize,
    pub failed: Vec<String>,
    pub could_not: Vec<String>,
    pub unplaced: usize,
}

pub(crate) fn decide(f: &Facts) -> Decision {
    let mut needs = Vec::new();
    if !f.stop.trim().is_empty() {
        needs.push("the stop condition has prose to read".to_string());
    }
    let unheld: Vec<&str> = f.members.iter().filter(|(_, held)| !held).map(|(m, _)| m.as_str()).collect();
    if !unheld.is_empty() {
        needs.push(format!("{} did not hold cleanly", unheld.join(", ")));
    }
    if f.ran == 0 && f.could_not.is_empty() && f.unplaced == 0 {
        needs.push("nothing was declared to check".to_string());
    }
    for c in &f.could_not {
        needs.push(format!("could not run: {c}"));
    }
    if f.unplaced > 0 {
        needs.push(format!("{} stop check(s) with no repository to run in", f.unplaced));
    }
    for s in &f.failed {
        needs.push(format!("{s} failed"));
    }
    let would_pass = needs.is_empty();
    Decision { would_pass, pass: would_pass && f.enabled, needs }
}

/// Whether every project the group ran in has turned the automatic pass on:
/// `auto-pass: on` under `## Barrier` in its body.
pub(crate) fn enabled(store: &Store, plan: &Plan) -> bool {
    !plan.units.is_empty()
        && plan.units.iter().all(|u| {
            store
                .project(&u.project)
                .and_then(|p| p.section("Barrier"))
                .is_some_and(|s| s.lines().any(|l| matches!(l.trim().replace(' ', "").to_lowercase().as_str(), "auto-pass:on" | "auto-pass:yes" | "auto-pass:true")))
        })
}

// ---- the packet ------------------------------------------------------------

/// Where the results of group `at` are kept for its agent.
pub(crate) fn packet_file(store: &Store, list: &str, at: usize) -> PathBuf {
    store.state_file("barrier").join(format!("{list}-{at}.md"))
}

/// What wsp ran, as the agent reads it.
pub(crate) fn results_text(plan: &Plan, ranged: &[Ranged], blames: &[Blame], decision: &Decision) -> String {
    let mut out = String::new();
    for (u, r) in plan.units.iter().zip(ranged) {
        out.push_str(&format!("**{}** at `{}` ({})\n", u.project, &u.sha[..u.sha.len().min(12)], util::contract(&u.repo)));
        match r {
            Ranged::Could(why) => out.push_str(&format!("  could not run: {why}\n")),
            Ranged::Ran(ran) if ran.is_empty() => out.push_str("  nothing declared here\n"),
            Ranged::Ran(ran) => {
                for line in check::account(ran).lines() {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    for s in &plan.unplaced {
        out.push_str(&format!("not run, no repository: {s}\n"));
    }
    for b in blames {
        if b.members.is_empty() {
            out.push_str(&format!("`{}` failed and names no file a member landed — unattributed.\n", b.step));
        } else {
            let who: Vec<String> = b.members.iter().map(|(m, why)| format!("{m} ({why})")).collect();
            out.push_str(&format!("`{}` failed — attributed to {}.\n", b.step, who.join("; ")));
        }
    }
    if decision.would_pass {
        out.push_str("Everything declared ran and passed.\n");
    } else {
        out.push_str(&format!("Why this is yours: {}.\n", decision.needs.join("; ")));
    }
    out
}

/// The packet's head, which needs no run: the members, their verdicts, and the
/// stop. Composed at the time it is asked for.
pub(crate) fn head_text(tasks: &[Task], g: &Group) -> String {
    let mut out = String::new();
    for m in &g.members {
        let Some(p) = crate::verification::latest(tasks, m) else {
            out.push_str(&format!("- {m}: no verdict\n"));
            continue;
        };
        let title = tasks.iter().find(|t| &t.id == m).map(|t| util::truncate(&t.title, 70)).unwrap_or_default();
        let caption = p.caption.clone().unwrap_or_default();
        let unheld: Vec<String> = p.checked.iter().filter(|c| !c.held).map(|c| c.item.clone()).collect();
        let tail = if unheld.is_empty() { String::new() } else { format!(" — not held: {}", unheld.join("; ")) };
        out.push_str(&format!("- {m} · {title} · {} {caption}{tail}\n", p.state.as_str()));
    }
    out
}

/// What the agent is given of a run, when there was one for this entry.
pub(crate) fn packet_for(store: &Store, list: &str, at: usize, read: Option<&str>) -> Option<String> {
    let text = std::fs::read_to_string(packet_file(store, list, at)).ok()?;
    let (first, rest) = text.split_once('\n')?;
    let was = first.strip_prefix("read: ")?.trim();
    match read {
        Some(r) if !was.starts_with(r) && !r.starts_with(was) => None,
        _ => Some(rest.to_string()),
    }
}

// ---- the run ---------------------------------------------------------------

/// `wsp worklist advance <list> --barrier-run <n>`: run what group `n`'s
/// barrier owes, record it, and either pass the barrier or start its agent.
pub(crate) fn run(store: &Store, list: &str, at: usize) -> i32 {
    use crate::cycle::log_line;
    let Some(w) = store.worklist(list) else { return 1 };
    if barrier::suite_of(store, list, at) != Some(Suite::Running) {
        log_line(store, &format!("{list} group {at}: the barrier's run was asked for and nothing is waiting on it"));
        return 0;
    }
    let groups = w.groups();
    let Some(g) = groups.get(at - 1).cloned() else { return 1 };
    let plan = plan(store, &w, at);
    let started = Instant::now();
    let mut ranged: Vec<Ranged> = Vec::new();
    for unit in &plan.units {
        ranged.push(run_unit(store, unit));
    }
    let landed = crate::worklist::touched(store, &g.members).landed;
    let mut blames = Vec::new();
    let mut failed = Vec::new();
    let mut could_not = Vec::new();
    let mut count = 0;
    for (u, r) in plan.units.iter().zip(&ranged) {
        match r {
            Ranged::Could(why) => could_not.push(format!("{} — {why}", u.project)),
            Ranged::Ran(ran) => {
                count += ran.len();
                for (item, ran) in u.items.iter().zip(ran) {
                    if matches!(ran.done, Done::Failed { .. }) {
                        failed.push(item.step.name.clone());
                        blames.push(blame(item, ran, &landed));
                    }
                }
            }
        }
    }
    let tasks = store.tasks_shared();
    let members: Vec<(String, bool)> = g
        .members
        .iter()
        .map(|m| {
            let clean = crate::verification::verified(&tasks, m)
                && crate::verification::latest(&tasks, m).is_some_and(|p| p.checked.iter().all(|c| c.held));
            (m.clone(), clean)
        })
        .collect();
    let on = enabled(store, &plan);
    let decision = decide(&Facts {
        enabled: on,
        stop: &g.stop,
        members,
        ran: count,
        failed: failed.clone(),
        could_not,
        unplaced: plan.unplaced.len(),
    });
    let reads: Vec<String> = plan.units.iter().map(|u| u.sha.clone()).collect();
    let results = results_text(&plan, &ranged, &blames, &decision);
    let file = packet_file(store, list, at);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let read = reads.first().cloned().unwrap_or_default();
    let _ = std::fs::write(&file, format!("read: {read}\n{results}"));
    log_line(
        store,
        &format!(
            "{list} group {at}: barrier run — {count} step(s), {} failed, {}s{}",
            failed.len(),
            started.elapsed().as_secs(),
            if decision.would_pass && !on { " · would pass without an agent (auto-pass is off)" } else { "" }
        ),
    );
    let suite = if failed.is_empty() && decision.needs.iter().all(|n| !n.starts_with("could not run")) { Suite::Green } else { Suite::Red };

    if decision.pass {
        barrier::note_run(store, list, at, suite, Some("wsp"));
        if pass(store, list, &g, &plan, count, &file) {
            log_line(store, &format!("{list} group {at}: passed by wsp with no agent — {count} check(s) green, every member holds, no stop prose"));
            return 0;
        }
        barrier::note_run(store, list, at, suite, Some(""));
        log_line(store, &format!("{list} group {at}: wsp could not record the pass, so its agent is asked"));
    } else {
        barrier::note_run(store, list, at, suite, None);
    }
    let Some(policy) = g.policy() else { return 0 };
    let label = barrier::seat_label(list, at);
    if let Some(why) = crate::cycle::start_check(store, list, at, &policy) {
        log_line(store, &format!("FAILED spawn the check on {label}: {why}"));
        crate::cycle::check_start_failed(store, &w, at, &why);
        return 1;
    }
    0
}

fn run_unit(store: &Store, unit: &Unit) -> Ranged {
    if unit.items.is_empty() {
        return Ranged::Ran(Vec::new());
    }
    let tg = check::Target { sha: unit.sha.clone(), repo: unit.repo.clone(), project: unit.project.clone(), since: None };
    let dir = check::dir_of(store, &tg);
    let tree = dir.join("tree");
    let lock = match check::TreeLock::take(dir.join("lock"), check::TREE_WAIT) {
        Ok(l) => l,
        Err(why) => return Ranged::Could(why),
    };
    if let Err(why) = check::checkout(&unit.repo, &tree, &unit.sha) {
        drop(lock);
        return Ranged::Could(format!("could not put {} in {}: {why}", &unit.sha[..12], util::contract(&tree)));
    }
    let steps: Vec<Step> = unit.items.iter().map(|i| i.step.clone()).collect();
    let ran = check::run_every(&tree, &steps, &dir.join("barrier.log"), |_| {});
    drop(lock);
    Ranged::Ran(ran)
}

/// Record the pass through `wsp worklist go`, the one way a barrier is passed.
fn pass(store: &Store, list: &str, g: &Group, plan: &Plan, count: usize, packet: &std::path::Path) -> bool {
    let projects: Vec<&str> = plan.units.iter().map(|u| u.project.as_str()).collect();
    let sentence = format!(
        "Passed by wsp with no agent. All {} member(s) hold ({}); {count} check(s) ran at the trunk in {} and every one passed; \
         the stop condition has no prose{}. What ran is in {}.",
        g.members.len(),
        g.members.join(", "),
        projects.join(", "),
        if g.stop_checks.is_empty() { String::new() } else { format!(", only {} stop check(s), which passed", g.stop_checks.len()) },
        util::contract(packet),
    );
    let caption = format!("all {} hold; {count} checks green, no stop prose", g.members.len());
    let dir = store.state_file("barrier");
    let file = dir.join(format!("{list}-verdict.md"));
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::write(&file, &sentence).is_err() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    if cfg!(test) {
        return false;
    }
    std::process::Command::new(exe)
        .args(["worklist", "go", list, "--from"])
        .arg(&file)
        .args(["--caption", &caption])
        .env_remove(crate::place::SEAT_ENV)
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, command: &str) -> Step {
        Step { name: name.into(), command: command.into(), limit: std::time::Duration::from_secs(60), of: Of::Task, on: Vec::new(), barrier: false }
    }

    fn failing(tail: &str) -> Ran {
        Ran {
            step: step("s", "c"),
            done: Done::Failed { exit: Some(101), timed_out: false },
            took: std::time::Duration::from_secs(1),
            tail: tail.into(),
            first: None,
        }
    }

    fn landed(member: &str, files: &[&str]) -> Landed {
        Landed { member: member.into(), commit: None, files: files.iter().map(|f| f.to_string()).collect() }
    }

    fn facts<'a>(stop: &'a str) -> Facts<'a> {
        Facts {
            enabled: true,
            stop,
            members: vec![("a-1".into(), true), ("a-2".into(), true)],
            ran: 3,
            failed: Vec::new(),
            could_not: Vec::new(),
            unplaced: 0,
        }
    }

    #[test]
    fn nothing_to_judge_passes_and_each_thing_that_needs_judgment_stops_it() {
        let d = decide(&facts(""));
        assert!(d.pass && d.would_pass && d.needs.is_empty());

        let prose = decide(&facts("flag Ed if the cost doubles"));
        assert!(!prose.pass && !prose.would_pass);
        assert!(prose.needs[0].contains("prose"));

        let mut unheld = facts("");
        unheld.members[1].1 = false;
        let d = decide(&unheld);
        assert!(!d.would_pass && d.needs[0].contains("a-2"));

        let mut red = facts("");
        red.failed = vec!["tests".into()];
        assert!(!decide(&red).would_pass);

        let mut stuck = facts("");
        stuck.could_not = vec!["wsp: the tree was in use".into()];
        assert!(!decide(&stuck).would_pass);

        let mut unplaced = facts("");
        unplaced.unplaced = 1;
        assert!(!decide(&unplaced).would_pass);
    }

    #[test]
    fn a_barrier_with_nothing_declared_is_never_passed_by_wsp() {
        let mut none = facts("");
        none.ran = 0;
        let d = decide(&none);
        assert!(!d.would_pass && d.needs[0].contains("nothing was declared"));
    }

    #[test]
    fn an_automatic_pass_that_is_off_still_says_it_would_have_passed() {
        let mut off = facts("");
        off.enabled = false;
        let d = decide(&off);
        assert!(d.would_pass && !d.pass);
    }

    #[test]
    fn a_members_own_failing_check_is_theirs_whatever_the_output_says() {
        let item = Item { step: step("targeted", "cargo test a"), kind: Kind::Member, owners: vec!["a-1".into()] };
        let b = blame(&item, &failing("nothing useful"), &[landed("a-1", &["src/x.rs"]), landed("a-2", &["src/y.rs"])]);
        assert_eq!(b.members, vec![("a-1".to_string(), "its own recorded check".to_string())]);
    }

    #[test]
    fn a_suite_failure_is_named_for_the_members_who_landed_the_files_the_output_names() {
        let item = Item { step: step("suite", "cargo test"), kind: Kind::Suite, owners: Vec::new() };
        let ran = failing("---- store::tests::keeps_order stdout ----\nthread panicked at src/store.rs:41:9");
        let b = blame(&item, &ran, &[landed("a-1", &["src/store.rs", "README.md"]), landed("a-2", &["src/other.rs"])]);
        assert_eq!(b.members.len(), 1);
        assert_eq!(b.members[0].0, "a-1");

        let module = failing("test store::tests::keeps_order ... FAILED");
        let b = blame(&item, &module, &[landed("a-1", &["src/store.rs"]), landed("a-2", &["src/other.rs"])]);
        assert_eq!(b.members[0].0, "a-1");

        let none = failing("linker error: out of disk");
        assert!(blame(&item, &none, &[landed("a-1", &["src/store.rs"])]).members.is_empty());
    }

    #[test]
    fn a_command_two_members_recorded_runs_once_and_is_both_of_theirs() {
        let mut u = Unit { project: "p".into(), repo: PathBuf::from("/r"), sha: "a".repeat(40), items: Vec::new() };
        add(&mut u, step("t", "cargo test x"), Kind::Member, Some("a-1"));
        add(&mut u, step("t", "cargo test x"), Kind::Member, Some("a-2"));
        add(&mut u, step("t", "cargo test x"), Kind::Member, Some("a-2"));
        assert_eq!(u.items.len(), 1);
        assert_eq!(u.items[0].owners, vec!["a-1".to_string(), "a-2".to_string()]);
    }

    fn git_run(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A project with a repository, a full suite at its barrier, two members
    /// with checks of their own and a group whose stop carries one check.
    fn world(name: &str, suite: &str, project_body: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("barrier-run-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_run(&repo, &["init", "--quiet", "-b", "master"]);
        for (f, body) in [("suite.sh", suite), ("mine.sh", "exit 0\n"), ("gate.sh", "exit 0\n")] {
            std::fs::write(repo.join(f), body).unwrap();
        }
        git_run(&repo, &["add", "."]);
        git_run(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "pj-001: first"]);
        let store = Store::open();
        let mut p = crate::model::Project::new("pj");
        p.roots = vec![repo.display().to_string()];
        p.body = format!("## Checks\n- full (1m) at barrier: sh suite.sh\n- build on src/**: true\n\n{project_body}");
        store.save_project(&p).unwrap();
        for (id, check) in [("pj-001", "- mine: sh mine.sh"), ("pj-002", "- full-again: sh suite.sh")] {
            let mut t = Task::new("a row", id);
            t.project = Some("pj".into());
            t.body = format!("## Checks\n{check}\n");
            store.save_task(&t).unwrap();
        }
        let mut w = Worklist::new("run", "r");
        let g = Group {
            members: vec!["pj-001".into(), "pj-002".into()],
            agent: "claude opus high".into(),
            stop_checks: vec!["gate (1m): sh gate.sh".into()],
            ..Default::default()
        };
        w.set_groups(&[g]);
        w.set_status(crate::model::WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
        (env, store)
    }

    #[test]
    fn the_plan_is_the_suite_then_each_members_checks_then_the_stop_and_a_shared_command_runs_once() {
        let (_env, store) = world("plan", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        assert_eq!(plan.units.len(), 1, "one repository");
        let items: Vec<(&str, Kind, Vec<&str>)> =
            plan.units[0].items.iter().map(|i| (i.step.name.as_str(), i.kind, i.owners.iter().map(String::as_str).collect())).collect();
        assert_eq!(
            items,
            vec![
                ("full", Kind::Suite, vec!["pj-002"]),
                ("mine", Kind::Member, vec!["pj-001"]),
                ("stop:gate", Kind::Stop, vec![]),
            ],
            "pj-002's recorded command is the suite, so it runs once and is theirs too; the path rule is not a barrier step"
        );
        assert!(plan.declared() && owed(&store, &w, 1));
        assert!(!enabled(&store, &plan), "off until the project says so");
    }

    #[test]
    fn a_group_with_nothing_declared_owes_nothing() {
        let (_env, store) = world("none", "exit 0\n", "");
        let mut p = store.project("pj").unwrap();
        p.body = String::new();
        store.save_project(&p).unwrap();
        for id in ["pj-001", "pj-002"] {
            let mut t = store.find_task(id).unwrap();
            t.body = String::new();
            store.save_task(&t).unwrap();
        }
        let mut w = store.worklist("run").unwrap();
        let mut gs = w.groups();
        gs[0].stop_checks.clear();
        w.set_groups(&gs);
        store.save_worklist(&w).unwrap();
        assert!(!owed(&store, &w, 1));
    }

    #[test]
    fn the_project_turns_the_automatic_pass_on_under_a_barrier_heading() {
        let (_env, store) = world("on", "exit 0\n", "## Barrier\nauto-pass: on\n");
        let w = store.worklist("run").unwrap();
        assert!(enabled(&store, &plan(&store, &w, 1)));
    }

    #[test]
    fn every_step_runs_after_a_red_suite_and_the_failure_is_read_to_the_member_who_owns_it() {
        let (_env, store) = world("red", "echo 'store::tests::x FAILED'; exit 1\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        let Ranged::Ran(ran) = run_unit(&store, &plan.units[0]) else { panic!("the tree was there") };
        let done: Vec<(&str, bool)> = ran.iter().map(|r| (r.step.name.as_str(), r.done == Done::Passed)).collect();
        assert_eq!(done, vec![("full", false), ("mine", true), ("stop:gate", true)], "a member's own check is still read");

        let landed = vec![landed("pj-001", &["src/store.rs"]), landed("pj-002", &["src/other.rs"])];
        let b = blame(&plan.units[0].items[0], &ran[0], &landed);
        assert_eq!(b.members[0].0, "pj-001", "the output names store:: and pj-001 landed src/store.rs");
    }

    #[test]
    fn a_run_leaves_the_packet_for_the_agent_and_the_entry_says_how_the_suite_went() {
        let (_env, store) = world("packet", "exit 1\n", "");
        let w = store.worklist("run").unwrap();
        let o = barrier::open_within(&store, &w, 1, Some("abc".into()), None, barrier::Mechanical::Yes, &|_: &str| false, &|| true);
        assert!(o.is_some());
        assert_eq!(run(&store, "run", 1), 0);
        assert_eq!(barrier::suite_of(&store, "run", 1), Some(Suite::Red));
        let packet = packet_for(&store, "run", 1, None).expect("written");
        assert!(packet.contains("✗ full") && packet.contains("Why this is yours"), "{packet}");
        assert!(packet.contains("✓ mine") && packet.contains("full failed"), "{packet}");
        assert!(packet_for(&store, "run", 1, Some("deadbeef")).is_none(), "a packet for another commit is not this check's");
        let tip = packet_file(&store, "run", 1);
        let was = std::fs::read_to_string(tip).unwrap();
        let sha = was.lines().next().unwrap().trim_start_matches("read: ").to_string();
        assert!(packet_for(&store, "run", 1, Some(&sha[..12])).is_some(), "and one for this commit is, by its short name too");
        let head = head_text(&store.tasks_shared(), &store.worklist("run").unwrap().groups()[0]);
        assert!(head.contains("pj-001") && head.contains("no verdict"), "{head}");
    }
}
