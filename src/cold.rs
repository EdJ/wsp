//! The cold build (`wsp-342`): once, at the end of a worklist, the project's
//! `at barrier` and `at end` steps in a checkout made for the purpose, with no
//! `target/` carried over, to catch what a warm tree hides.
//!
//! `wsp cold <list|project> [--plan] [--at COMMIT] [--keep]`. The tree is
//! `cold/<label>-<n>/<repo>/tree`, made fresh and removed after unless `--keep`;
//! the log stays at `cold/<label>-<n>.log` and the account at `cold/<label>.md`.
//! Every step runs whatever the one before did. A cold green is recorded in the
//! greens ledger, and a step already green cold at the tip is not run again.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use crate::check::{self, Done, Ran, Step};
use crate::cmd_verify::git;
use crate::greens::{Env, Greens, Reuse};
use crate::model::{Landed, WorklistStatus};
use crate::store::Store;
use crate::base::args::Args;
use crate::util;

const USAGE: &str = "wsp cold <list|project> [--plan] [--at COMMIT] [--keep]";

/// How long after one away-built cold build the next may start.
const AWAY_REPEAT_SECS: i64 = 6 * 3600;

/// One repository's cold build.
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub project: String,
    pub repo: PathBuf,
    pub sha: String,
    pub steps: Vec<Step>,
}

/// What `wsp cold <label>` builds: a worklist's repositories or a project's own.
pub(crate) struct Plan {
    pub label: String,
    pub jobs: Vec<Job>,
}

/// The project's steps for a cold build: those held for the barrier, then those
/// held for the end, a command named twice once.
pub(crate) fn steps_of(store: &Store, project: &str) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::new();
    for s in check::barrier_steps(store, project).into_iter().chain(check::end_steps(store, project)) {
        if !out.iter().any(|o| o.command == s.command) {
            out.push(s);
        }
    }
    out
}

fn job(store: &Store, project: &str, repo: PathBuf, at: Option<&str>) -> Option<Job> {
    let sha = match at {
        Some(rev) => git(&repo, &["rev-parse", "--verify", &format!("{rev}^{{commit}}")])?.trim().to_string(),
        None => crate::barrier_run::tip_of(&repo)?,
    };
    Some(Job { project: project.to_string(), repo, sha, steps: steps_of(store, project) })
}

/// The repositories a list's members landed in, one job each. A member that
/// landed nothing is passed over; `landed` is `worklist::touched`'s record.
pub(crate) fn jobs_for_list(store: &Store, members: &[String], landed: &[Landed], at: Option<&str>) -> Vec<Job> {
    let mut jobs: Vec<Job> = Vec::new();
    for id in members {
        if !landed.iter().any(|l| &l.member == id && l.commit.is_some()) {
            continue;
        }
        let Some(project) = store.find_task(id).and_then(|t| check::project_of(&t)) else { continue };
        let Some(repo) = crate::barrier_run::repo_of(store, &project) else { continue };
        if jobs.iter().any(|j| j.repo == repo) {
            continue;
        }
        if let Some(j) = job(store, &project, repo, at) {
            jobs.push(j);
        }
    }
    jobs
}

fn plan_of(store: &Store, arg: &str, at: Option<&str>) -> Result<Plan, String> {
    if let Some(w) = store.worklist(arg) {
        let members: Vec<String> = w.groups().iter().flat_map(|g| g.members.clone()).collect();
        let landed = crate::worklist::touched(store, &members).landed;
        return Ok(Plan { label: w.id.clone(), jobs: jobs_for_list(store, &members, &landed, at) });
    }
    if store.project(arg).is_some() {
        let repo = crate::barrier_run::repo_of(store, arg).ok_or_else(|| format!("{arg} has no repository"))?;
        let jobs = job(store, arg, repo, at).into_iter().collect();
        return Ok(Plan { label: arg.to_string(), jobs });
    }
    Err(format!("no worklist or project named {arg}"))
}

/// Whether a step is green cold at the job's commit.
fn green_cold(store: &Store, j: &Job, s: &Step) -> bool {
    Greens::open(store, &j.project, &j.repo).lookup(&j.sha, &s.command, Env::Cold).is_some()
}

/// Whether anything in the plan still lacks a cold green at its tip: the whole
/// of the decision to build at the close of a list, so a tip built once is not
/// built again.
pub(crate) fn owed(store: &Store, p: &Plan) -> bool {
    p.jobs.iter().any(|j| j.steps.iter().any(|s| !green_cold(store, j, s)))
}

/// Ask for the cold build of a list that has just closed, once. Nothing waits on it.
pub(crate) fn at_close(store: &Store, list: &str, launch: &dyn Fn(&Store, &[&str])) {
    let Ok(p) = plan_of(store, list, None) else { return };
    if owed(store, &p) {
        launch(store, &["cold", list]);
    }
}

fn dir_of(store: &Store) -> PathBuf {
    store.state_file("cold")
}

fn next_n(dir: &Path, label: &str) -> usize {
    (1..).find(|n| !dir.join(format!("{label}-{n}.log")).exists()).unwrap_or(1)
}

fn tree_of(dir: &Path, label: &str, n: usize, repo: &Path) -> PathBuf {
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    dir.join(format!("{label}-{n}")).join(name).join("tree")
}

/// Remove a tree and its registration, whatever state it was left in.
fn remove_tree(repo: &Path, tree: &Path) {
    if tree.exists() {
        let _ = git(repo, &["worktree", "remove", "--force", &tree.display().to_string()]);
        let _ = std::fs::remove_dir_all(tree);
    }
    let _ = git(repo, &["worktree", "prune"]);
}

/// A tree made fresh: a leftover is removed first, so nothing built before is seen.
fn fresh_tree(repo: &Path, tree: &Path, sha: &str) -> Result<(), String> {
    remove_tree(repo, tree);
    check::checkout(repo, tree, sha)
}

pub(crate) struct Built {
    pub project: String,
    pub sha: String,
    pub ran: Vec<Ran>,
    pub could_not: Option<String>,
}

const ALREADY: &str = "every step is already green cold at this commit";

fn build(store: &Store, label: &str, n: usize, j: &Job, keep: bool, log: &Path) -> Built {
    let mut built = Built { project: j.project.clone(), sha: j.sha.clone(), ran: Vec::new(), could_not: None };
    if j.steps.iter().all(|s| green_cold(store, j, s)) {
        built.could_not = Some(ALREADY.into());
        return built;
    }
    let repo_name = j.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = dir_of(store);
    let tree = tree_of(&dir, label, n, &j.repo);
    let lock = match check::TreeLock::take(dir.join(format!("{}-{repo_name}.lock", j.project)), check::TREE_WAIT) {
        Ok(l) => l,
        Err(why) => {
            built.could_not = Some(why);
            return built;
        }
    };
    if let Err(why) = fresh_tree(&j.repo, &tree, &j.sha) {
        drop(lock);
        built.could_not = Some(format!("could not make {}: {why}", util::contract(&tree)));
        return built;
    }
    // Cargo builds into the fresh tree, not into whatever the caller points at.
    std::env::remove_var("CARGO_TARGET_DIR");
    let greens = Greens::open(store, &j.project, &j.repo);
    let who = format!("cold {label} {n}");
    let reuse = Reuse { greens: &greens, sha: &j.sha, env: Env::Cold, who: &who };
    built.ran = {
        // The cold build stays in this process, each step a job on
        // `wsp-host`'s runner with the tree where it is (wsp-057).
        check::run_every(&tree, &j.steps, log, Some(&reuse), &check::Runner::Inproc, |_| {})
    };
    if !keep {
        remove_tree(&j.repo, &tree);
    }
    drop(lock);
    built
}

fn failed_names(built: &[Built]) -> Vec<String> {
    let mut out = Vec::new();
    for b in built {
        out.extend(b.ran.iter().filter(|r| matches!(r.done, Done::Failed { .. } | Done::Malformed(_) | Done::Hung(_) | Done::TreeMissing(_))).map(|r| r.step.name.clone()));
        if let Some(why) = b.could_not.as_ref().filter(|w| w.as_str() != ALREADY) {
            out.push(format!("{} ({why})", b.project));
        }
    }
    out
}

fn result_text(label: &str, n: usize, built: &[Built], secs: u64) -> String {
    let mut out = format!("# Cold build {label} #{n}\n\n{secs}s\n");
    for b in built {
        out.push_str(&format!("\n## {} at {}\n\n", b.project, &b.sha[..12.min(b.sha.len())]));
        match &b.could_not {
            Some(why) => out.push_str(&format!("{why}\n")),
            None => out.push_str(&format!("{}\n", check::account(&b.ran))),
        }
    }
    out
}

fn flag(store: &Store, label: &str, failed: &[String], log: &Path) {
    use crate::message::{self, About, Kind, Message, Party};
    let text = format!(
        "{label}: the cold build failed — {}\n\nA clean checkout at the trunk's tip did not pass. The log is {}.",
        failed.join(", "),
        util::contract(log)
    );
    let m = Message::new(Party::wsp(), Kind::Note, &text).about(About::Scope(label.to_string()));
    let _ = message::raise(store, &m);
}

fn record(store: &Store, label: &str, built: &[Built], secs: u64, log: &Path) {
    let steps: usize = built.iter().map(|b| b.ran.len()).sum();
    let failed = failed_names(built);
    let sha = built.first().map(|b| b.sha.as_str()).unwrap_or_default();
    let sha12 = &sha[..12.min(sha.len())];
    if let Some(mut w) = store.worklist(label) {
        let _ = crate::log::note_to_list(store, &mut w, &format!("wsp: cold build at {sha12}: {steps} steps, {} failed, {secs}s", failed.len()));
    }
    store.log_event(
        "cold-built",
        json!({ "id": label, "sha": sha, "steps": steps, "failed": failed, "secs": secs, "log": log.display().to_string() }),
    );
    if !failed.is_empty() {
        flag(store, label, &failed, log);
    }
}

/// `wsp cold <list|project> [--plan] [--at COMMIT] [--keep]`
pub fn cold(store: &Store, args: &Args) -> i32 {
    let Some(arg) = args.rest.first().cloned() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let at = args.get("at");
    let p = match plan_of(store, &arg, at.as_deref()) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("wsp cold: {why}");
            return 1;
        }
    };
    let dir = dir_of(store);
    let n = next_n(&dir, &p.label);
    if args.has("plan") {
        for j in &p.jobs {
            let tree = tree_of(&dir, &p.label, n, &j.repo);
            println!("{} at {} in {} (a fresh tree)", j.project, &j.sha[..12.min(j.sha.len())], util::contract(&tree));
            for s in &j.steps {
                let reuse = if green_cold(store, j, s) { " · green cold at this commit, would not run" } else { "" };
                println!("  {} · {}{reuse}", s.name, s.command.lines().next().unwrap_or_default());
            }
        }
        if p.jobs.is_empty() {
            println!("{}: no repository to build", p.label);
        }
        return 0;
    }
    if let Some(low) = crate::disk::loud(store) {
        eprintln!("wsp cold: not building, the disk is nearly full\n{low}");
        return 1;
    }
    let _ = std::fs::create_dir_all(&dir);
    let log = dir.join(format!("{}-{n}.log", p.label));
    let started = Instant::now();
    let built: Vec<Built> = p.jobs.iter().map(|j| build(store, &p.label, n, j, args.has("keep"), &log)).collect();
    let secs = started.elapsed().as_secs();
    let text = result_text(&p.label, n, &built, secs);
    let _ = std::fs::write(dir.join(format!("{}.md", p.label)), &text);
    let failed = failed_names(&built);
    if built.iter().all(|b| b.ran.is_empty()) && failed.is_empty() {
        println!("{}: nothing to build — every step is already green cold at its tip", p.label);
        return 0;
    }
    record(store, &p.label, &built, secs, &log);
    println!("{text}");
    i32::from(!failed.is_empty())
}

// ---- when the person is away -------------------------------------------------

/// Whether an idle machine is given a cold build now: the tip is owed one, no
/// list is running, the person is away, and the last such build was long enough ago.
pub(crate) fn away_wants(owed: bool, list_running: bool, away: bool, last: Option<i64>, now: i64) -> bool {
    owed && !list_running && away && last.is_none_or(|t| now - t >= AWAY_REPEAT_SECS)
}

/// The daemon's tick: build cold for a project whose tip has none while the
/// person is away (`throttle` auto mode, idle past `away_minutes`).
pub(crate) fn when_away(store: &Store, launch: &dyn Fn(&Store, &[&str])) {
    if cfg!(test) {
        return;
    }
    if store.worklists().iter().any(|w| w.status() == WorklistStatus::Running) {
        return;
    }
    let now = util::epoch_of(&util::now_iso());
    for pr in store.projects() {
        let Some(repo) = crate::barrier_run::repo_of(store, &pr.id) else { continue };
        let Some(j) = job(store, &pr.id, repo, None) else { continue };
        let stamp = dir_of(store).join(format!("away-{}.stamp", pr.id));
        let last = std::fs::read_to_string(&stamp).ok().and_then(|t| t.trim().parse::<i64>().ok());
        let owed = j.steps.iter().any(|s| !green_cold(store, &j, s));
        // The cheap reads first: the idle time costs a process.
        if !away_wants(owed, false, true, last, now) {
            continue;
        }
        let view = crate::throttle::View::read(&store.state);
        let away = view.settings.mode == crate::throttle::Mode::Auto && view.idle.is_some_and(|i| i >= view.settings.away_minutes * 60);
        if !away_wants(owed, false, away, last, now) {
            continue;
        }
        let _ = std::fs::create_dir_all(dir_of(store));
        let _ = std::fs::write(&stamp, now.to_string());
        launch(store, &["cold", &pr.id]);
    }
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["cold"],
        run: cold,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 60,
                text: r##"  wsp cold <list|project> [--plan] [--at COMMIT] [--keep]
                                    one cold build in a fresh checkout: the project's `at barrier`
                                    then `at end` steps; started once as a list closes"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Project, Task, Worklist};

    fn git_run(dir: &Path, args: &[&str]) {
        let out = wsp_base::calls::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A project with a repository and one worklist with one landed member.
    fn world(name: &str, body: &str) -> (util::Isolated, Store, Vec<Landed>) {
        let env = util::isolated(&format!("cold-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        std::fs::write(repo.join("a.sh"), "exit 0\n").unwrap();
        git_run(&repo, &["add", "."]);
        git_run(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "pj-001: first"]);
        let store = Store::open();
        let mut p = Project::new("pj");
        p.roots = vec![repo.display().to_string()];
        p.body = format!("## Checks\n{body}\n");
        store.save_project(&p).unwrap();
        let mut t = Task::new("a row", "pj-001");
        t.project = Some("pj".into());
        store.save_task(&t).unwrap();
        let mut w = Worklist::new("run", "r");
        w.set_groups(&[Group { members: vec!["pj-001".into()], ..Default::default() }]);
        w.set_status(WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
        let landed = vec![Landed { member: "pj-001".into(), commit: Some("x".into()), files: Vec::new() }];
        (env, store, landed)
    }

    fn jobs(store: &Store, landed: &[Landed]) -> Vec<Job> {
        jobs_for_list(store, &["pj-001".to_string()], landed, None)
    }

    fn logdir(store: &Store) -> PathBuf {
        let log = store.state_file("cold").join("run-1.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        log
    }

    #[test]
    fn the_plan_is_the_barrier_steps_then_the_end_steps_and_runs_nothing() {
        let (_env, store, landed) = world("plan", "- perf (1m) at end: sh a.sh && true\n- full (1m) at barrier: sh a.sh\n- fast on src/*.rs: true");
        let j = jobs(&store, &landed);
        assert_eq!(j.len(), 1);
        let names: Vec<&str> = j[0].steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["full", "perf"], "barrier first, then end");
        assert_eq!(cold(&store, &Args::synth("cold", &["pj"], &[("plan", "true")])), 0);
        assert!(!store.state_file("cold").join("pj-1").exists(), "--plan makes no tree");
    }

    #[test]
    fn a_run_uses_a_fresh_tree_and_never_sees_a_stale_target() {
        let (_env, store, landed) = world("fresh", "- seen (1m) at barrier: test ! -e target/stale\n- last (1m) at end: true");
        let j = jobs(&store, &landed).remove(0);
        let tree = tree_of(&store.state_file("cold"), "run", 1, &j.repo);
        // An older tree left behind with a built target the new one must not inherit.
        check::checkout(&j.repo, &tree, &j.sha).unwrap();
        std::fs::create_dir_all(tree.join("target")).unwrap();
        std::fs::write(tree.join("target/stale"), "x").unwrap();
        let b = build(&store, "run", 1, &j, false, &logdir(&store));
        assert!(b.could_not.is_none(), "{:?}", b.could_not);
        assert!(b.ran.iter().all(|r| r.done == Done::Passed), "{}", check::account(&b.ran));
        assert!(!tree.exists(), "the tree is removed after");

        let kept = build(&store, "run", 2, &j, true, &logdir(&store));
        assert!(kept.ran.iter().all(|r| r.reused.is_some()), "a cold green at this tip is not run again");
        assert!(!tree_of(&store.state_file("cold"), "run", 2, &j.repo).exists());
    }

    #[test]
    fn every_step_runs_after_a_failure() {
        let (_env, store, landed) = world("every", "- bad (1m) at barrier: false\n- good (1m) at end: true");
        let j = jobs(&store, &landed).remove(0);
        let b = build(&store, "run", 1, &j, false, &logdir(&store));
        assert!(matches!(b.ran[0].done, Done::Failed { .. }));
        assert_eq!(b.ran[1].done, Done::Passed, "it ran though the one before failed");
    }

    #[test]
    fn a_warm_green_is_not_reused_but_a_cold_one_is() {
        let (_env, store, landed) = world("reuse", "- full (1m) at barrier: true");
        let j = jobs(&store, &landed).remove(0);
        let g = Greens::open(&store, &j.project, &j.repo);
        g.record(&j.sha, &j.steps[0].command, Env::Warm, 3, "barrier", false);
        let plan = Plan { label: "run".into(), jobs: vec![j.clone()] };
        assert!(owed(&store, &plan), "a warm green does not satisfy a cold build");
        g.record(&j.sha, &j.steps[0].command, Env::Cold, 3, "cold", false);
        assert!(!owed(&store, &plan), "a cold one does");
    }

    #[test]
    fn a_red_run_flags_and_a_green_run_does_not() {
        let (_env, store, _) = world("flag", "- bad (1m) at barrier: false");
        let flags = |s: &Store| crate::message::raised_to_the_person(s).len();
        let before = flags(&store);
        let green = [Built { project: "pj".into(), sha: "a".repeat(40), ran: Vec::new(), could_not: None }];
        record(&store, "run", &green, 1, Path::new("/x.log"));
        assert_eq!(flags(&store), before, "green: no flag");
        let ran = Ran {
            step: Step {
                name: "bad".into(),
                command: "false".into(),
                limit: std::time::Duration::from_secs(1),
                of: check::Of::Project,
                on: Vec::new(),
                barrier: true,
                end: false,
            },
            done: Done::Failed { exit: Some(1), timed_out: false },
            took: std::time::Duration::from_secs(1),
            tail: String::new(),
            first: None,
            reused: None,
        };
        let red = [Built { project: "pj".into(), sha: "a".repeat(40), ran: vec![ran], could_not: None }];
        record(&store, "run", &red, 1, Path::new("/x.log"));
        assert_eq!(flags(&store), before + 1, "red: flagged");
        let log = store.worklist("run").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("cold build at aaaaaaaaaaaa: 1 steps, 1 failed"), "{log}");
    }

    #[test]
    fn the_close_asks_for_the_build_once_and_the_person_away_rule_waits_six_hours() {
        let (_env, store, landed) = world("once", "- full (1m) at barrier: true");
        let j = jobs(&store, &landed).remove(0);
        let plan = Plan { label: "run".into(), jobs: vec![j.clone()] };
        assert!(owed(&store, &plan), "no cold green yet: asks");
        Greens::open(&store, &j.project, &j.repo).record(&j.sha, &j.steps[0].command, Env::Cold, 1, "cold", false);
        assert!(!owed(&store, &plan), "built at this tip: does not ask again");

        assert!(away_wants(true, false, true, None, 100_000));
        assert!(!away_wants(false, false, true, None, 100_000), "nothing owed");
        assert!(!away_wants(true, true, true, None, 100_000), "a list is running");
        assert!(!away_wants(true, false, false, None, 100_000), "the person is here");
        assert!(!away_wants(true, false, true, Some(100_000 - 3600), 100_000), "built an hour ago");
        assert!(away_wants(true, false, true, Some(100_000 - 7 * 3600), 100_000));
    }
}
