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
//! prose, **and the group is not the list's last** (Ed, 2026-11-05: a list never
//! ends on an automatic pass). The verdict says why, and the entry on the group
//! says `by:wsp`. The pass is on unless a project says `auto-pass: off` under
//! `## Barrier` in its body (`wsp-396`). Any
//! other barrier gets its agent as before, with the **packet** — members,
//! verdicts, the stop and these results, inline — in its work order, so the
//! turns it spent gathering and rebuilding go to what only it can do.
//!
//! **The automatic pass is on by default** (Ed, 2026-11-05), and a project turns
//! it off with `auto-pass: off` under a `## Barrier` heading in its body. Off,
//! wsp still runs everything, still logs `would pass without an agent`, and
//! still hands the agent the packet.
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
use crate::scope;
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

pub(crate) fn repo_of(store: &Store, project: &str) -> Option<PathBuf> {
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

pub(crate) fn tip_of(repo: &std::path::Path) -> Option<String> {
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
    /// The group is the last of its list, which always gets its check agent.
    pub last: bool,
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
    if f.last {
        needs.push("the list's last group always gets its check agent".to_string());
    }
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

/// Whether the automatic pass is on for the group: it is, unless a project the
/// group ran in says `auto-pass: off` under `## Barrier` in its body.
pub(crate) fn enabled(store: &Store, plan: &Plan) -> bool {
    !plan.units.is_empty()
        && plan.units.iter().all(|u| {
            !store.project(&u.project).and_then(|p| p.section("Barrier")).is_some_and(|s| {
                s.lines().any(|l| matches!(l.trim().replace(' ', "").to_lowercase().as_str(), "auto-pass:off" | "auto-pass:no" | "auto-pass:false"))
            })
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
                out.push_str(&format!("  {}\n", check::tally_line(ran)));
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
    let changed = checks_changed_in(tasks, g);
    if !changed.is_empty() {
        out.push_str(&format!(
            "- checks changed after they were written (wsp-681), on: {} — each row's log has the lines old → new, who and why\n",
            changed.join(", ")
        ));
    }
    out
}

/// The members of `g` whose `## Checks` were changed after they were first
/// written, as their logs record it.
pub(crate) fn checks_changed_in(tasks: &[Task], g: &Group) -> Vec<String> {
    g.members
        .iter()
        .filter(|m| tasks.iter().find(|t| &t.id == *m).is_some_and(|t| !crate::check::changes_logged(t).is_empty()))
        .cloned()
        .collect()
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
    use crate::store::log_line;
    let Some(w) = store.worklist(list) else { return 1 };
    if barrier::suite_of(store, list, at) != Some(Suite::Running) {
        log_line(store, &format!("{list} group {at}: the barrier's run was asked for and nothing is waiting on it"));
        return 0;
    }
    // Who is making the run, and the lease it beats until it returns, so that
    // being killed is seen and not waited out.
    let _lease = barrier::note_run_lease(store, list, at);
    // A run that builds waits while the disk is nearly full (`wsp-373`) rather
    // than failing partway; it says so once and goes on when room appears.
    if let Some(repo) = repo_of(store, &w.project.clone().unwrap_or_default()) {
        let mut said = false;
        while let Some(why) = crate::disk::hold(store, &repo) {
            if !said {
                log_line(store, &format!("{list} group {at}: the barrier's run waits: {why}"));
                said = true;
            }
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }
    let groups = w.groups();
    let Some(g) = groups.get(at - 1).cloned() else { return 1 };
    let plan = plan(store, &w, at);
    let started = Instant::now();
    let mut ranged: Vec<Ranged> = Vec::new();
    // What a copy of this run spent waiting for the tree lock behind another is
    // not the run's own time, and the line below does not count it.
    let mut waited = std::time::Duration::ZERO;
    // What the group touched decides how much of the suite the barrier runs
    // (`wsp-431`): observed from the landed commits, never declared.
    let landed = crate::worklist::touched(store, &g.members).landed;
    let mut changed: Vec<String> = landed.iter().flat_map(|l| l.files.iter().cloned()).collect();
    changed.sort();
    changed.dedup();
    let project = plan.units.first().map(|u| u.project.clone()).unwrap_or_default();
    let config = scope::Config::of(store, &project);
    let mut net = scope::Net::read(store, &project);
    let scope = scope::decide(&scope::Ask {
        kind: scope::GroupKind::parse(&g.kind),
        last: at == groups.len(),
        changed: &changed,
        config: &config,
        since_full: net.since_full.len(),
    });
    let mut effective: Vec<Vec<Item>> = Vec::new();
    let mut applied = scope.clone();
    let mut targets: Vec<String> = Vec::new();
    let mut any_narrowed = false;
    // How each unit's tree stood when its steps began (`wsp-566`).
    let mut starts: Vec<String> = Vec::new();
    for unit in &plan.units {
        let (r, w, narrowed, start) = run_unit_scoped(store, unit, &barrier_key(list, at), &format!("barrier {list} {at}"), Some((&scope, &changed)));
        waited += w;
        starts.extend(start.map(|s| format!("{}: {}", unit.project, s.line())));
        ranged.push(r);
        match narrowed {
            Some(n) => {
                any_narrowed |= n.narrowed;
                if n.scope.is_full() {
                    applied = n.scope;
                }
                for t in n.targets {
                    if !targets.contains(&t) {
                        targets.push(t);
                    }
                }
                effective.push(n.items);
            }
            None => effective.push(unit.items.clone()),
        }
    }
    let applied = applied_scope(applied, any_narrowed);
    let mut blames = Vec::new();
    let mut failed = Vec::new();
    let mut could_not = Vec::new();
    let mut count = 0;
    let (mut reused, mut ran_n) = (0, 0);
    let mut per_step: Vec<String> = Vec::new();
    for ((u, items), r) in plan.units.iter().zip(&effective).zip(&ranged) {
        match r {
            Ranged::Could(why) => could_not.push(format!("{} — {why}", u.project)),
            Ranged::Ran(ran) => {
                count += ran.len();
                per_step.extend(ran.iter().filter(|x| x.reused.is_none()).map(|x| format!("{} {}s", x.step.name, x.took.as_secs())));
                let (a, b) = check::tally(ran);
                reused += a;
                ran_n += b;
                for (item, ran) in items.iter().zip(ran) {
                    if let Done::TreeMissing(_) = &ran.done {
                        could_not.push(format!("{} — {}", u.project, check::why_failed(ran)));
                    } else if matches!(ran.done, Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_)) {
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
        last: at == groups.len(),
        stop: &g.stop,
        members,
        ran: count,
        failed: failed.clone(),
        could_not,
        unplaced: plan.unplaced.len(),
    });
    let reads: Vec<String> = plan.units.iter().map(|u| u.sha.clone()).collect();
    let results = results_text(&plan, &ranged, &blames, &decision);
    let secs = started.elapsed().saturating_sub(waited).as_secs();
    let note = scope_note(store, &project, &mut net, list, at, &applied, &targets, &scope, secs, failed.is_empty(), reused, !could_not_any(&ranged) && !nothing_narrowed(&scope, &applied));
    note.write(store, list, at);
    let results = format!("Scope: {}\n{results}", note.line());
    let file = packet_file(store, list, at);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let read = reads.first().cloned().unwrap_or_default();
    let _ = std::fs::write(&file, format!("read: {read}\n{results}"));
    log_line(
        store,
        &format!(
            "{list} group {at}: barrier run — {count} step(s), {reused} reused, {ran_n} ran, {} failed, {}s{}{}{}{}",
            failed.len(),
            secs,
            // Per-step seconds of the steps that ran: `wsp timeline` reads them (`wsp-390`).
            if per_step.is_empty() { String::new() } else { format!(" · per step: {}", per_step.join("; ")) },
            // Whether the trees began cold or warm and how busy the machine was (`wsp-566`), after the per-step figures so the timeline's reading of them is whole.
            if starts.is_empty() { String::new() } else { format!(" · start: {}", starts.join("; ")) },
            // Scoped against full (`wsp-431`).
            format!(" · scope: {}", note.line()),
            if decision.would_pass && !on { " · would pass without an agent (auto-pass is off for a project)" } else { "" }
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

/// Whether a live run holds the tree lock of one of group `at`'s units: a run
/// in its steps, or writing its result. Under `WSP_JOBS=manager` the lock names
/// the job running in the tree and is read by key from the job table of the
/// tree's host's manager ([`check::TreeLock::live`]); in-process, by the
/// lock's own lease, which its holder beats.
pub(crate) fn lock_held(store: &Store, w: &Worklist, at: usize) -> bool {
    plan(store, w, at).units.iter().any(|u| check::TreeLock::live(&check::barrier_dir(store, &u.project, &u.repo).join("lock")))
}

#[cfg(test)]
fn run_unit(store: &Store, unit: &Unit, who: &str) -> Ranged {
    run_unit_scoped(store, unit, &barrier_key("run", 1), who, None).0
}

/// What a barrier's jobs are keyed under in `row`, since a barrier has no row
/// (wsp-044): `<list>#<group>`.
pub(crate) fn barrier_key(list: &str, at: usize) -> String {
    format!("{list}#{at}")
}

/// What a scoped run is given: the scope decided for the group, and the paths
/// its members landed.
type Scoping<'a> = Option<(&'a scope::Scope, &'a [String])>;

/// [`run_unit`], and how long it waited for the tree lock, and what scoping did
/// to the unit's steps (so the caller reads results against the steps that ran).
/// A wait is said in the log, with whose lock it was.
fn run_unit_scoped(store: &Store, unit: &Unit, key: &str, who: &str, scoping: Scoping) -> (Ranged, std::time::Duration, Option<scope::Narrowed>, Option<check::Start>) {
    if unit.items.is_empty() {
        return (Ranged::Ran(Vec::new()), std::time::Duration::ZERO, None, None);
    }
    let dir = check::barrier_dir(store, &unit.project, &unit.repo);
    let before = Instant::now();
    let lock = check::TreeLock::take_saying(dir.join("lock"), check::TREE_WAIT, |by| {
        let by = match by {
            Some((pid, since)) => format!("pid {pid}, since {since}"),
            None => "another run".to_string(),
        };
        crate::store::log_line(store, &format!("{who}: waits for the tree lock in {} — held by {by}", util::contract(&dir)));
    });
    // Everything up to the lock is waiting; the checkout and the steps are not.
    let waited = if lock.is_ok() { before.elapsed() } else { std::time::Duration::ZERO };
    let (ranged, narrowed, start) = run_unit_locked(store, unit, key, who, &dir, lock, scoping);
    (ranged, waited, narrowed, start)
}

fn run_unit_locked(
    store: &Store,
    unit: &Unit,
    key: &str,
    who: &str,
    dir: &std::path::Path,
    lock: Result<check::TreeLock, String>,
    scoping: Scoping,
) -> (Ranged, Option<scope::Narrowed>, Option<check::Start>) {
    let tree = dir.join("tree");
    let lock = match lock {
        Ok(l) => l,
        Err(why) => return (Ranged::Could(why), None, None),
    };
    let start = match check::checkout_started(&unit.repo, &tree, &unit.sha) {
        Ok(start) => start,
        Err(why) => {
            drop(lock);
            return (Ranged::Could(format!("could not put {} in {}: {why}", &unit.sha[..12], util::contract(&tree))), None, None);
        }
    };
    // The steps as scoped, in the tree at the commit: the targets a diff reaches
    // are read from the crate graph and the module graph there.
    let narrowed = scoping.map(|(scope, changed)| scope::narrow(&tree, &unit.items, scope, changed));
    let items = narrowed.as_ref().map(|n| n.items.as_slice()).unwrap_or(&unit.items);
    let steps: Vec<Step> = items.iter().map(|i| i.step.clone()).collect();
    let greens = crate::greens::Greens::open(store, &unit.project, &unit.repo);
    let reuse = crate::greens::Reuse { greens: &greens, sha: &unit.sha, env: crate::greens::Env::Warm, who };
    // Under `WSP_JOBS=manager` each step is one `job.run` keyed by the list and
    // the group, at the place `place_of` gives the barrier tree on the list's
    // barrier host, and the lock names the job running there.
    let runner = match runner_for(store, key, &tree, &dir.join("lock")) {
        Ok(r) => r,
        Err(why) => {
            drop(lock);
            return (Ranged::Could(why), narrowed, Some(start));
        }
    };
    // A `node_modules/` the tree lacks is installed before the steps that need
    // it (`compound-react-045`), and said on the start's seed lines.
    let mut start = start;
    start.install(&tree, &dir.join("barrier.log"), &runner);
    let ran = check::run_every(&tree, &steps, &dir.join("barrier.log"), Some(&reuse), &runner, |_| {});
    drop(lock);
    (Ranged::Ran(ran), narrowed, Some(start))
}

/// What runs barrier `key`'s steps in its tree at `tree`, holding the lock at
/// `lock`: [`check::Runner::chosen`] for the barrier's own tree. Under `inproc`
/// this process, as before; under `manager` the manager of the list's barrier
/// host, the trunk's host by default ([`check::Whose::Barrier`]).
fn runner_for(store: &Store, key: &str, tree: &std::path::Path, lock: &std::path::Path) -> Result<check::Runner<'static>, String> {
    #[cfg(test)]
    if let Some(host) = tests::MANAGER.with(|m| m.get()) {
        let place = check::place_of(check::Whose::Barrier(key), check::TreeKind::Barrier, tree)?;
        return Ok(check::Runner::Jobs(check::Jobs::new(host, place, Some(lock.to_path_buf()))));
    }
    check::Runner::chosen(store, check::Whose::Barrier(key), check::TreeKind::Barrier, tree, Some(lock))
}

fn could_not_any(ranged: &[Ranged]) -> bool {
    ranged.iter().any(|r| matches!(r, Ranged::Could(_)))
}

/// The scope a barrier ran under: a narrow scope that removed no whole-suite
/// step from any unit narrowed nothing, so it reads as no scope (`wsp-439`).
fn applied_scope(applied: scope::Scope, any_narrowed: bool) -> scope::Scope {
    if applied.is_full() || any_narrowed {
        applied
    } else {
        scope::Scope::Full("no whole-suite step to narrow".into())
    }
}

/// Whether the scope decided was narrow and nothing came of it: such a run is
/// neither a scoped barrier nor a full one for the safety net's count.
fn nothing_narrowed(decided: &scope::Scope, applied: &scope::Scope) -> bool {
    !decided.is_full() && applied.is_full() && matches!(applied, scope::Scope::Full(w) if w == "no whole-suite step to narrow")
}

/// What the group's barrier did about scope, and the project's count of scoped
/// barriers since its last full one moved on (`wsp-431`).
///
/// A scoped run adds the group to the count. A full run empties it, and is
/// **the safety net's** when scoped barriers stood behind it: red, it names
/// them, because the graph missed something they let through. A full run that
/// reused nothing is a figure for what a full run costs, kept to price what the
/// scoped ones saved.
#[allow(clippy::too_many_arguments)]
fn scope_note(
    store: &Store,
    project: &str,
    net: &mut scope::Net,
    list: &str,
    at: usize,
    applied: &scope::Scope,
    targets: &[String],
    decided: &scope::Scope,
    secs: u64,
    green: bool,
    reused: usize,
    ran_all: bool,
) -> scope::Note {
    let label = format!("{list}/{at}");
    let mut note = scope::Note {
        scoped: !applied.is_full(),
        why: match applied {
            scope::Scope::Full(w) => w.clone(),
            scope::Scope::Narrow { .. } => String::new(),
        },
        targets: targets.to_vec(),
        widened: match decided {
            scope::Scope::Narrow { widened } if !applied.is_full() => widened.clone(),
            _ => Vec::new(),
        },
        seconds: secs,
        full_seconds: net.full_secs,
        net_red: Vec::new(),
    };
    if ran_all && !project.is_empty() {
        if note.scoped {
            if !net.since_full.contains(&label) {
                net.since_full.push(label);
            }
        } else {
            let behind: Vec<String> = net.since_full.iter().filter(|l| **l != label).cloned().collect();
            if !green && !behind.is_empty() {
                note.net_red = behind;
            }
            net.since_full.clear();
            if green && reused == 0 {
                net.full_secs = Some(secs);
            }
        }
        net.write(store, project);
    }
    note
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
    let changed = checks_changed_in(&store.tasks(), g);
    let sentence = match changed.is_empty() {
        true => sentence,
        false => format!("{sentence} Checks changed after they were written, on: {} (each row's log has old → new, who and why).", changed.join(", ")),
    };
    let caption = format!("all {} hold; {count} checks green, no stop prose", g.members.len());
    let dir = store.state_file("barrier");
    let file = dir.join(format!("{list}-verdict.md"));
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::write(&file, &sentence).is_err() {
        return false;
    }
    let Ok(exe) = wsp_base::util::self_exe() else { return false };
    if cfg!(test) {
        return false;
    }
    wsp_base::calls::Command::new(exe)
        .args(["worklist", "go", list, "--from"])
        .arg(&file)
        .args(["--caption", &caption])
        .env_remove(crate::base::seat::SEAT_ENV)
        .env_remove("WSP_BACKEND_PANE_ID")
        .env_remove("WSP_BACKEND_WORKSPACE_ID")
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// A test's own manager for the barrier's steps (`WSP_JOBS=manager`),
        /// kept to its thread: `wsp-host`'s runner in this process.
        pub(super) static MANAGER: std::cell::Cell<Option<&'static dyn crate::host::Host>> = const { std::cell::Cell::new(None) };
    }

    /// Runs `f` with the barrier's steps sent as jobs to a manager whose tree
    /// root is the store's state (where the barrier tree is), and answers the
    /// manager, so a test reads its job table.
    fn under_manager<T>(store: &Store, f: impl FnOnce(&'static crate::host::InProcess) -> T) -> T {
        let root = store.state.clone();
        wsp_trees::job_place::set_tree_root(Some(root.clone()));
        let host: &'static crate::host::InProcess = Box::leak(Box::new(crate::host::InProcess::new().with_jobs(&root.join("jobs"), &root)));
        MANAGER.with(|m| m.set(Some(host)));
        let out = f(host);
        MANAGER.with(|m| m.set(None));
        wsp_trees::job_place::set_tree_root(None);
        out
    }

    fn dones(r: Ranged) -> Vec<(String, Done)> {
        ran_of(r).into_iter().map(|r| (r.step.name, r.done)).collect()
    }

    /// The barrier tree's place is `place_of(barrier, barrier)` on the list's
    /// barrier host (the trunk's host by default), relative to that host's
    /// tree root and never a path on wsp's machine; each step is one `job.run`
    /// keyed by the list and the group.
    #[test]
    fn a_barrier_under_manager_runs_each_step_as_a_job_keyed_by_list_and_group_at_its_place() {
        let (_env, store) = world("jobs", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        let u = &plan.units[0];
        let dir = check::barrier_dir(&store, &u.project, &u.repo);
        under_manager(&store, |host| {
            use crate::host::Host;
            let runner = runner_for(&store, &barrier_key("run", 1), &dir.join("tree"), &dir.join("lock")).unwrap();
            let check::Runner::Jobs(jobs) = &runner else { panic!("not run as jobs under manager") };
            let crate::host::JobAt::Dir(at) = &jobs.place.at else { panic!("{:?}", jobs.place.at) };
            let barrier = check::Whose::Barrier("run#1");
            assert_eq!(at.host.0, wsp_trees::job_place::host_of(barrier), "the list's barrier host");
            assert_eq!(jobs.place, check::place_of(barrier, check::TreeKind::Barrier, &dir.join("tree")).unwrap(), "from place_of");
            assert!(!at.escapes() && !at.path.starts_with('/') && at.path.ends_with("/tree"), "{}", at.path);
            let run = jobs.run_of(&u.items[0].step, &dir.join("tree"));
            assert_eq!((run.key.row.0.as_str(), run.at.clone()), ("run#1", jobs.place.at.clone()));
            drop(runner);

            let ran = dones(run_unit(&store, u, "barrier run 1"));
            assert!(ran.iter().all(|(_, d)| *d == Done::Passed), "{ran:?}");
            let table = host.jobs().unwrap();
            assert_eq!(table.len(), u.items.len(), "one job.run per step: {table:?}");
            assert!(table.iter().all(|r| r.key.row.0 == "run#1" && r.class == crate::host::JobClass::Check && r.state == crate::host::JobState::Ended), "{table:?}");
            assert!(!check::TreeLock::live(&dir.join("lock")), "the lock went with the run");
        });
    }

    /// The same group reads the same under `manager` as under `inproc`: a red
    /// suite, the member's own check and the stop's, step for step.
    #[test]
    fn a_barrier_under_manager_reads_the_same_verdict_as_inproc() {
        let suite = "echo 'store::tests::x FAILED'; exit 1\n";
        // One isolated world at a time: the second waits for the first's end.
        let (a, here) = world("verdict-inproc", suite, "");
        let inproc = dones(run_unit(&here, &plan(&here, &here.worklist("run").unwrap(), 1).units[0], "barrier run 1"));
        drop((here, a));
        let (_b, store) = world("verdict-manager", suite, "");
        let plan = plan(&store, &store.worklist("run").unwrap(), 1);
        let manager = under_manager(&store, |_| dones(run_unit(&store, &plan.units[0], "barrier run 1")));
        assert_eq!(manager, inproc);
        assert!(matches!(inproc[0].1, Done::Failed { exit: Some(1), .. }), "{inproc:?}");
    }

    /// A step past its limit is ended by the manager (its `job.ended` says
    /// `timed_out`), with what it started, and reads as a timed-out step.
    #[test]
    fn a_barrier_step_past_its_limit_is_ended_by_the_manager() {
        let (_env, store) = world("limit", "sleep 30 & sleep 30\n", "");
        let mut plan = plan(&store, &store.worklist("run").unwrap(), 1);
        plan.units[0].items[0].step.limit = std::time::Duration::from_secs(1);
        let started = std::time::Instant::now();
        let ran = under_manager(&store, |host| {
            use crate::host::Host;
            let ran = dones(run_unit(&store, &plan.units[0], "barrier run 1"));
            assert!(host.jobs().unwrap().iter().all(|r| r.state == crate::host::JobState::Ended), "the manager ended it");
            ran
        });
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "waited on the sleep");
        assert_eq!(ran[0].1, Done::Failed { exit: None, timed_out: true }, "{ran:?}");
    }

    /// A whole barrier run under `manager` leaves the steps, their bounds and
    /// the lock to the manager and its job table; under `inproc` they are
    /// `wsp-host`'s runner's in this process. wsp has no process probe left to
    /// make (wsp-057: the lock and the run are leases, and
    /// `tests/host_seam.rs` fails a spawn outside a job).
    #[test]
    fn a_barrier_run_under_manager_probes_no_process_from_wsp() {
        let (env, store) = world("probes", "exit 1\n", "");
        let w = store.worklist("run").unwrap();
        assert!(barrier::open_within(&store, &w, 1, Some("abc".into()), None, barrier::Mechanical::Yes, &|_: &str| false, &|| true).is_some());
        assert_eq!(under_manager(&store, |_| run(&store, "run", 1)), 0);
        assert_eq!(barrier::suite_of(&store, "run", 1), Some(Suite::Red));
        drop((store, env));
        let (_b, here) = world("probes-inproc", "exit 1\n", "");
        let ranged = run_unit(&here, &plan(&here, &here.worklist("run").unwrap(), 1).units[0], "barrier run 1");
        assert!(matches!(ranged, Ranged::Ran(_)), "inproc runs jobs too (wsp-057)");
    }

    fn step(name: &str, command: &str) -> Step {
        Step { name: name.into(), command: command.into(), limit: std::time::Duration::from_secs(60), of: Of::Task, on: Vec::new(), barrier: false, end: false }
    }

    fn failing(tail: &str) -> Ran {
        Ran {
            step: step("s", "c"),
            done: Done::Failed { exit: Some(101), timed_out: false },
            took: std::time::Duration::from_secs(1),
            tail: tail.into(),
            first: None,
            reused: None,
            build: None,
        }
    }

    fn landed(member: &str, files: &[&str]) -> Landed {
        Landed { member: member.into(), commit: None, files: files.iter().map(|f| f.to_string()).collect() }
    }

    fn facts<'a>(stop: &'a str) -> Facts<'a> {
        Facts {
            enabled: true,
            last: false,
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
    fn a_lists_last_group_is_never_passed_by_wsp_and_an_intermediate_one_still_is() {
        let mut last = facts("");
        last.last = true;
        let d = decide(&last);
        assert!(!d.pass && !d.would_pass, "everything else would pass");
        assert!(d.needs.iter().any(|n| n.contains("last group always gets its check agent")));

        let mid = decide(&facts(""));
        assert!(mid.pass && mid.would_pass && mid.needs.is_empty());
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
        let out = wsp_base::calls::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A project with a repository, a full suite at its barrier, two members
    /// with checks of their own and a group whose stop carries one check.
    fn world(name: &str, suite: &str, project_body: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("barrier-run-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
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
        assert!(enabled(&store, &plan), "on unless the project says off");
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
    fn the_automatic_pass_is_on_by_default_and_a_project_turns_it_off_under_a_barrier_heading() {
        let (_env, store) = world("on", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        assert!(enabled(&store, &plan(&store, &w, 1)), "the default is on");

        let mut p = store.project("pj").unwrap();
        p.body.push_str("\n## Barrier\nauto-pass: off\n");
        store.save_project(&p).unwrap();
        assert!(!enabled(&store, &plan(&store, &w, 1)), "an explicit off holds");
    }

    #[test]
    fn every_step_runs_after_a_red_suite_and_the_failure_is_read_to_the_member_who_owns_it() {
        let (_env, store) = world("red", "echo 'store::tests::x FAILED'; exit 1\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        let Ranged::Ran(ran) = run_unit(&store, &plan.units[0], "barrier run 1") else { panic!("the tree was there") };
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

    fn ran_of(r: Ranged) -> Vec<Ran> {
        match r {
            Ranged::Ran(ran) => ran,
            Ranged::Could(why) => panic!("could not run: {why}"),
        }
    }

    fn commit_more(env: &util::Isolated, name: &str) {
        let repo = env.path("repo");
        std::fs::write(repo.join(name), "x\n").unwrap();
        git_run(&repo, &["add", "."]);
        git_run(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "pj-001: more"]);
    }

    #[test]
    fn the_barrier_runs_in_its_own_tree_and_not_in_the_per_row_checks_tree() {
        let (env, store) = world("own-tree", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        assert!(ran_of(run_unit(&store, &plan.units[0], "barrier run 1")).iter().all(|r| r.done == Done::Passed));
        let tree = check::barrier_dir(&store, "pj", &plan.units[0].repo).join("tree");
        assert!(tree.join(".git").exists(), "the barrier tree is made under barrier/");
        assert!(tree.starts_with(store.state_file("barrier")));
        assert!(!store.state_file("checks").exists(), "nothing was put under checks/");
        assert!(check::barrier_tree_line(&store, "pj", &plan.units[0].repo).contains(&plan.units[0].sha[..12]));
        drop(env);
    }

    #[test]
    fn a_live_holder_of_the_barrier_lock_is_seen_from_the_plan() {
        let (_env, store) = world("lock-held", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        assert!(!lock_held(&store, &w, 1));
        let u = &plan.units[0];
        let lock = check::TreeLock::take(check::barrier_dir(&store, &u.project, &u.repo).join("lock"), std::time::Duration::from_secs(1)).unwrap();
        assert!(lock_held(&store, &w, 1));
        drop(lock);
        assert!(!lock_held(&store, &w, 1));
    }

    /// `wsp-391`: a copy that waits behind another's lock says so, with whose,
    /// and its 'barrier run' line is its own steps' time and not the wait.
    #[test]
    fn a_run_that_waits_for_the_tree_lock_says_for_whom_and_does_not_count_the_wait() {
        let (_env, store) = world("waits", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        let u = &plan.units[0];
        assert!(barrier::open_within(&store, &w, 1, Some("abc".into()), None, barrier::Mechanical::Yes, &|_: &str| false, &|| true).is_some());
        let first = check::TreeLock::take(check::barrier_dir(&store, &u.project, &u.repo).join("lock"), std::time::Duration::from_secs(1)).unwrap();
        let _ = crate::cycle::tests::drained(&crate::cycle::tests::SAID);
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(12));
            drop(first);
        });
        assert_eq!(run(&store, "run", 1), 0);
        holder.join().unwrap();
        let said = crate::cycle::tests::drained(&crate::cycle::tests::SAID);
        let me = format!("pid {}", std::process::id());
        assert!(said.iter().any(|l| l.contains("waits for the tree lock") && l.contains(&me)), "{said:?}");
        let line = said.iter().find(|l| l.contains("barrier run —")).unwrap_or_else(|| panic!("{said:?}"));
        let secs: u64 = line.split(" failed, ").nth(1).unwrap().split('s').next().unwrap().parse().unwrap_or_else(|_| panic!("{line}"));
        assert!(secs < 10, "the 12 s wait is not in the line: {line}");
    }

    #[test]
    fn a_second_look_at_the_same_tip_runs_nothing_and_the_packet_says_reused() {
        let (_env, store) = world("twice", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        let first = ran_of(run_unit(&store, &plan.units[0], "barrier run 1"));
        assert!(first.iter().all(|r| r.reused.is_none()));
        let second = run_unit(&store, &plan.units[0], "barrier run 1");
        let reused = ran_of(run_unit(&store, &plan.units[0], "barrier run 1"));
        assert!(reused.iter().all(|r| r.done == Done::Passed && r.reused.is_some()), "every step reused");
        let text = results_text(&plan, &[second], &[], &Decision { pass: false, would_pass: true, needs: vec![] });
        assert!(text.contains("reused"), "{text}");
    }

    #[test]
    fn a_moved_trunk_runs_the_suite_again_in_the_same_tree_without_a_second_worktree() {
        let (env, store) = world("moved", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let before = plan(&store, &w, 1);
        ran_of(run_unit(&store, &before.units[0], "barrier run 1"));
        commit_more(&env, "more.txt");
        let after = plan(&store, &w, 1);
        assert_ne!(before.units[0].sha, after.units[0].sha);
        let ran = ran_of(run_unit(&store, &after.units[0], "barrier run 1"));
        assert!(ran.iter().all(|r| r.done == Done::Passed && r.reused.is_none()), "a new commit has no green");
        let list = wsp_base::calls::Command::new("git").arg("-C").arg(env.path("repo")).args(["worktree", "list"]).output().unwrap();
        let list = String::from_utf8_lossy(&list.stdout).into_owned();
        assert_eq!(list.lines().filter(|l| l.contains("/state/barrier/")).count(), 1, "{list}");
        assert_eq!(list.lines().count(), 2, "the repository and the one barrier tree: {list}");
    }

    #[test]
    fn the_barrier_tree_is_not_a_candidate_for_disk_removal() {
        let (_env, store) = world("kept", "exit 0\n", "");
        let w = store.worklist("run").unwrap();
        let plan = plan(&store, &w, 1);
        ran_of(run_unit(&store, &plan.units[0], "barrier run 1"));
        let barrier_state = store.state_file("barrier");
        assert!(crate::disk::candidates(&store).iter().all(|c| !c.starts_with(&barrier_state)));
    }

    #[test]
    fn scoped_barriers_are_counted_and_a_red_full_run_behind_them_is_the_safety_nets() {
        let (_env, store) = world("net", "exit 0\n", "");
        let mut net = scope::Net::default();
        let narrow = scope::Scope::Narrow { widened: vec![] };
        let full = scope::Scope::Full("safety net".into());
        let note = |net: &mut scope::Net, at: usize, applied: &scope::Scope, secs: u64, green: bool| {
            scope_note(&store, "pj", net, "run", at, applied, &["crate:a".to_string()], &narrow, secs, green, 0, true)
        };
        let full_first = note(&mut net, 1, &full, 400, true);
        assert!(!full_first.scoped && net.full_secs == Some(400) && net.since_full.is_empty());
        let a = note(&mut net, 2, &narrow, 40, true);
        assert!(a.scoped && a.saved() == 360, "{a:?}");
        note(&mut net, 2, &narrow, 40, true);
        note(&mut net, 3, &narrow, 50, true);
        assert_eq!(net.since_full, vec!["run/2", "run/3"], "a group is counted once however often it runs");
        assert_eq!(scope::Net::read(&store, "pj"), net, "kept between barriers");
        let red = note(&mut net, 4, &full, 410, false);
        assert_eq!(red.net_red, vec!["run/2", "run/3"]);
        assert!(net.since_full.is_empty() && net.full_secs == Some(400), "a red run is not a price for a green one");
        assert!(red.line().contains("SAFETY-NET RED"));
        red.write(&store, "run", 4);
        assert_eq!(scope::Note::read(&store, "run", 4).unwrap().net_red, red.net_red);
    }

    #[test]
    fn a_narrow_scope_that_narrowed_nothing_is_no_scope_and_stays_out_of_the_net() {
        let (_env, store) = world("nothing", "exit 0\n", "");
        let narrow = scope::Scope::Narrow { widened: vec![] };
        let applied = applied_scope(narrow.clone(), false);
        assert!(applied.is_full() && nothing_narrowed(&narrow, &applied));
        assert_eq!(applied_scope(narrow.clone(), true), narrow, "one unit narrowed, so the barrier was scoped");
        let full = scope::Scope::Full("last".into());
        assert!(!nothing_narrowed(&full, &applied_scope(full.clone(), false)), "a full scope is a full run");
        let mut net = scope::Net { full_secs: Some(400), since_full: vec!["run/1".into()] };
        let note = scope_note(&store, "pj", &mut net, "run", 2, &applied, &[], &narrow, 5, true, 0, !nothing_narrowed(&narrow, &applied));
        assert!(!note.scoped && note.saved() == 0, "{note:?}");
        assert_eq!(net.since_full, vec!["run/1"], "neither counted as scoped nor taken for a full run");
        assert_eq!(net.full_secs, Some(400));
    }
}
