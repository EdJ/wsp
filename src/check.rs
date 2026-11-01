//! `wsp check <task>` — a task's mechanical verification, run as a script: `wsp-324`.
//!
//! Ed, 2026-11-01. Of 405 verifier transcripts since 15 October, verifiers
//! spent 22% of their calls building and running tests, and only 23% of the
//! sessions that blocked saw a failing test or a compile error. Building and
//! running what a project already knows how to build and run needs no agent.
//!
//! # wsp knows nothing about any project
//!
//! It runs what is declared and nothing else. Two places declare:
//!
//! - **The project**, in a `## Checks` section of its own file: one list item
//!   per step, `- name (limit): command`, the limit optional (`90s`, `20m`,
//!   `2h`; ten minutes without one). A step passes when its command exits 0.
//! - **The task**, in a `## Checks` section of its prose: the same grammar, and
//!   a bare `- command` is a step too. These are the ones only the task knows:
//!   "this script reproduces its numbers", "this file exists".
//!
//! There is no inheritance. compound's parent is wsp, and a compound row has no
//! business running wsp's `cargo test`.
//!
//! # Where it runs: a warm tree, at the commit that landed
//!
//! A tree of its own per repository, under the state directory
//! (`checks/<project>-<repo>/tree`), moved to the landing with `git checkout
//! --detach` and left standing so the next check's build is incremental. It is
//! not under `.worktrees/`: `checkout --sweep`, `check_tree` and `leg_tree`
//! each own what is there, and none of them should find this. One check holds
//! it at a time — a lock beside it — because two builds in one tree is how a
//! linker segfaults and a green run is bought for the wrong tree.
//!
//! The commands run with every `WSP_*` and `herdr` variable removed. A test
//! suite that finds `WSP_HOME` set fails four store tests and reads as the
//! change under check being wrong.
//!
//! # The result, on the row
//!
//! One entry under `## Verification`, beside the verifiers' ([`crate::verification::Run`]):
//! each step, pass or fail, how long, and the tail of the one that failed. A
//! run is written `running` before anything starts, under the store lock, and
//! that entry is what keeps a second trigger from starting a second run.
//!
//! # By itself after a land
//!
//! [`gate`] sits in front of the verifier: a member that has landed and has
//! checks gets no verifier until a run for that landing has **passed**. A
//! failure sends the member back to `doing` in the same write that records it,
//! with the failing step as what is owed, so there is no instant at which a
//! failed landing reads as finished. `wsp land` starts the run too, for a row
//! that is in no list.
//!
//! A run that failed is not run again for the same landing: it has had its
//! say, and a member returned to review at the same commit has been answered
//! by a person. `wsp check <task>` by hand runs it again whenever asked.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cmd_verify::{git, git_ok};
use crate::model::{Status, Task};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::verification::{self as v, Run, RunState};
use crate::Args;

/// The heading a project or a task lists its checks under.
pub(crate) const SECTION: &str = "Checks";

const DEFAULT_LIMIT: Duration = Duration::from_secs(600);

/// How long a run waits for the tree before giving it up for the next trigger.
const TREE_WAIT: Duration = Duration::from_secs(45 * 60);

/// The longest a tail of output is kept on the row, in lines and in characters.
const TAIL_LINES: usize = 25;
const TAIL_COLS: usize = 200;

/// Where a step was declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Of {
    Project,
    Task,
}

/// One declared check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub name: String,
    pub command: String,
    pub limit: Duration,
    pub of: Of,
}

// ---- declarations ----------------------------------------------------------

/// The steps listed in a section's text.
pub(crate) fn parse_steps(text: &str, of: Of) -> Vec<Step> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("- ").or_else(|| l.strip_prefix("* "))
        })
        .filter_map(|item| step_of(item, of))
        .collect()
}

fn step_of(item: &str, of: Of) -> Option<Step> {
    let item = item.trim();
    let (head, command) = match item.split_once(": ") {
        Some((h, c)) if head_of(h).is_some() => (head_of(h), c),
        _ => (None, item),
    };
    let command = command.trim().trim_matches('`').trim();
    if command.is_empty() {
        return None;
    }
    let (name, limit) = match head {
        Some((n, l)) => (n, l),
        None => (named_by(command), None),
    };
    Some(Step { name, command: command.to_string(), limit: limit.unwrap_or(DEFAULT_LIMIT), of })
}

/// `name` or `name (limit)`, when the text before a `: ` is one.
fn head_of(h: &str) -> Option<(String, Option<Duration>)> {
    let (name, limit) = match h.split_once(" (") {
        Some((n, rest)) => (n, Some(limit_of(rest.strip_suffix(')')?)?)),
        None => (h, None),
    };
    let ok = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
    ok.then(|| (name.to_string(), limit))
}

fn limit_of(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = n.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return None,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// What an unnamed step is called: the command's first word, as a file name.
fn named_by(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("check");
    let base = first.rsplit('/').next().unwrap_or(first);
    util::truncate(base, 30)
}

/// Everything declared for a task: its project's steps, then its own.
pub(crate) fn declared(store: &Store, t: &Task) -> Vec<Step> {
    let mut out = project_of(t)
        .and_then(|p| store.project(&p))
        .and_then(|p| p.section(SECTION))
        .map(|s| parse_steps(&s, Of::Project))
        .unwrap_or_default();
    if let Some(s) = t.section(SECTION) {
        out.extend(parse_steps(&s, Of::Task));
    }
    out
}

fn project_of(t: &Task) -> Option<String> {
    t.project.clone().filter(|p| !p.is_empty()).or_else(|| t.id.rsplit_once('-').map(|(p, _)| p.to_string()))
}

// ---- where, and at what ----------------------------------------------------

/// The commit to run at, and the repository that holds it.
pub(crate) struct Target {
    pub sha: String,
    pub repo: PathBuf,
    pub project: String,
}

/// The landing of `t` — or `at`, when a commit is named — in the first of its
/// project's roots that has the commit.
pub(crate) fn target(store: &Store, t: &Task, at: Option<&str>) -> Result<Target, String> {
    let sha = match at {
        Some(a) => a.to_string(),
        None => crate::repair::landed(t).ok_or_else(|| format!("{} has no landing recorded — `wsp check {} --at <commit>` names one", t.id, t.id))?,
    };
    let project = project_of(t).ok_or_else(|| format!("{} belongs to no project", t.id))?;
    let mut roots = Vec::new();
    let mut at_project = Some(project.clone());
    while let Some(p) = at_project.take() {
        let Some(found) = store.project(&p) else { break };
        if !found.roots.is_empty() {
            roots = found.roots.clone();
            break;
        }
        at_project = found.parent.clone();
    }
    if roots.is_empty() {
        return Err(format!("project {project} has no roots to find {sha} in"));
    }
    for root in &roots {
        let Some(repo) = crate::cmd_verify::toplevel(&util::expand(root)) else { continue };
        if let Some(full) = git(&repo, &["rev-parse", "--verify", "--quiet", &format!("{sha}^{{commit}}")]) {
            return Ok(Target { sha: full.trim().to_string(), repo, project });
        }
    }
    Err(format!("{sha} is in none of {project}'s roots ({})", roots.join(", ")))
}

fn dir_of(store: &Store, tg: &Target) -> PathBuf {
    let repo = tg.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    store.state_file("checks").join(format!("{}-{repo}", tg.project))
}

/// Move the warm tree to `sha`, making it on the first use.
fn checkout(repo: &Path, tree: &Path, sha: &str) -> Result<(), String> {
    if tree.join(".git").exists() {
        git_ok(tree, &["checkout", "--detach", "--quiet", "--force", sha])?;
        // What a build left that git would track is cleaned; what it ignores
        // (`target/`, which is the warmth) is not.
        return git_ok(tree, &["clean", "-fdq"]);
    }
    if let Some(parent) = tree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", util::contract(parent)))?;
    }
    // A tree whose directory was removed leaves a registration that refuses
    // the path.
    let _ = git_ok(repo, &["worktree", "prune"]);
    git_ok(repo, &["worktree", "add", "--detach", "--quiet", &tree.display().to_string(), sha])
}

/// One check at a time in a tree: a file beside it holding the pid that has it.
struct TreeLock(PathBuf);

impl TreeLock {
    fn take(path: PathBuf, wait: Duration) -> Result<TreeLock, String> {
        let start = Instant::now();
        loop {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{} {}", std::process::id(), util::now_iso());
                    return Ok(TreeLock(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::dead(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() > wait {
                        return Err(format!("the tree has been in use for {}s", wait.as_secs()));
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
                Err(e) => return Err(format!("{}: {e}", util::contract(&path))),
            }
        }
    }

    /// Whether the pid in the lock is gone, which is the only reason to break one.
    fn dead(path: &Path) -> bool {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let Some(pid) = text.split_whitespace().next().filter(|p| p.chars().all(|c| c.is_ascii_digit())) else {
            // Being written this instant, or garbage: garbage ages out.
            return std::fs::metadata(path).and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(60)).unwrap_or(false);
        };
        !Command::new("kill").args(["-0", pid]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(true)
    }
}

impl Drop for TreeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---- running ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Done {
    Passed,
    Failed { exit: Option<i32>, timed_out: bool },
    /// An earlier step failed, so this one was not run.
    Skipped,
}

#[derive(Clone, Debug)]
pub(crate) struct Ran {
    pub step: Step,
    pub done: Done,
    pub took: Duration,
    pub tail: String,
}

/// The environment a check's commands get: the caller's, less wsp's own.
fn clean_env(cmd: &mut Command) {
    for (k, _) in std::env::vars() {
        if k.starts_with("WSP_") || k.starts_with("HERDR_") || k == "GIT_INDEX_FILE" {
            cmd.env_remove(k);
        }
    }
}

/// Run one step in `dir`, its output to `log`, killed with its process group
/// at the limit.
pub(crate) fn run_step(dir: &Path, step: &Step, log: &Path) -> Ran {
    use std::os::unix::process::CommandExt;
    let start = Instant::now();
    let failed = |why: String| Ran {
        step: step.clone(),
        done: Done::Failed { exit: None, timed_out: false },
        took: start.elapsed(),
        tail: why,
    };
    let Ok(out) = std::fs::File::create(log) else { return failed(format!("could not write {}", util::contract(log))) };
    let Ok(err) = out.try_clone() else { return failed("could not share the log".into()) };
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&step.command).current_dir(dir).stdin(Stdio::null()).stdout(out).stderr(err).process_group(0);
    clean_env(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failed(format!("sh: {e}")),
    };
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() > step.limit => {
                timed_out = true;
                let _ = Command::new("kill").args(["-s", "KILL", "--", &format!("-{}", child.id())]).status();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    let ok = !timed_out && status.is_some_and(|s| s.success());
    let done = if ok {
        Done::Passed
    } else {
        Done::Failed { exit: status.and_then(|s| s.code()), timed_out }
    };
    Ran { step: step.clone(), done, took: start.elapsed(), tail: tail_of(log) }
}

/// The last lines of a log, each cut to a width a row can hold.
fn tail_of(log: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(log) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(16 * 1024)));
    let mut bytes = Vec::new();
    let _ = f.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..]
        .iter()
        .map(|l| util::truncate(l, TAIL_COLS))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run `steps` in order, stopping at the first that fails.
pub(crate) fn run_all(dir: &Path, steps: &[Step], log: &Path, mut each: impl FnMut(&Ran)) -> Vec<Ran> {
    let mut out: Vec<Ran> = Vec::new();
    for s in steps {
        let stop = out.last().is_some_and(|r| matches!(r.done, Done::Failed { .. }) || r.done == Done::Skipped);
        let ran = if stop {
            Ran { step: s.clone(), done: Done::Skipped, took: Duration::ZERO, tail: String::new() }
        } else {
            run_step(dir, s, log)
        };
        each(&ran);
        out.push(ran);
    }
    out
}

pub(crate) fn passed(ran: &[Ran]) -> bool {
    ran.iter().all(|r| r.done == Done::Passed)
}

fn secs(d: Duration) -> u64 {
    d.as_secs().max(u64::from(d > Duration::ZERO))
}

fn why_failed(r: &Ran) -> String {
    match &r.done {
        Done::Failed { timed_out: true, .. } => format!("timed out after {}s", r.step.limit.as_secs()),
        Done::Failed { exit: Some(n), .. } => format!("exit {n}"),
        Done::Failed { exit: None, .. } => "no exit status".into(),
        _ => String::new(),
    }
}

/// The step lines an entry carries.
pub(crate) fn account(ran: &[Ran]) -> String {
    let mut out = Vec::new();
    for r in ran {
        let (mark, rest) = match &r.done {
            Done::Passed => ("✓", format!("{}s", secs(r.took))),
            Done::Failed { .. } => ("✗", format!("{}s · {}", secs(r.took), why_failed(r))),
            Done::Skipped => ("–", "skipped".to_string()),
        };
        let of = if r.step.of == Of::Task { " · the task's" } else { "" };
        out.push(format!("{mark} {} · {} · {rest}{of}", r.step.name, r.step.command.lines().next().unwrap_or_default()));
        if matches!(r.done, Done::Failed { .. }) {
            out.extend(r.tail.lines().map(|l| format!("  | {l}")));
        }
    }
    out.join("\n")
}

/// What a failed run owes, for the row's log and the pane: the first failing
/// step, its command and the end of its output.
pub(crate) fn owed(ran: &[Ran], sha: &str) -> String {
    let Some(r) = ran.iter().find(|r| matches!(r.done, Done::Failed { .. })) else { return String::new() };
    let tail: Vec<&str> = r.tail.lines().collect();
    let tail = tail[tail.len().saturating_sub(12)..].join("\n");
    format!(
        "wsp check failed at {}: `{}` ({}) — {}, {}s. Run it yourself and fix what it reports; the whole run is under ## Verification.\n\n{tail}",
        &sha[..sha.len().min(12)],
        r.step.name,
        r.step.command.lines().next().unwrap_or_default(),
        why_failed(r),
        secs(r.took),
    )
}

// ---- the record --------------------------------------------------------------

/// How long a `running` entry is believed: its steps' limits, the wait for the
/// tree, and a margin.
fn stale_after(steps: &[Step]) -> i64 {
    (steps.iter().map(|s| s.limit).sum::<Duration>() + TREE_WAIT + Duration::from_secs(300)).as_secs() as i64
}

/// Write a `running` entry for the landing, unless one is owed to nobody.
///
/// `Some(at)` is an entry written and waiting for its run. `None` is a run
/// already under way or finished for this commit — unless `again`, which a
/// person asks for and which ignores a finished one.
pub(crate) fn begin(store: &Store, id: &str, sha: &str, stale: i64, again: bool) -> Option<String> {
    let read: String = sha.chars().take(12).collect();
    let made = store.locked(|| {
        let mut t = store.find_task(id)?;
        let mut runs = v::runs(&t);
        let mut mine = runs.iter().enumerate().rev().filter(|(_, r)| r.read.as_deref().is_some_and(|x| v::same_commit(x, sha)));
        if let Some((i, r)) = mine.next() {
            let fresh = util::epoch_secs() - util::epoch_of(&r.at) <= stale;
            match r.state {
                RunState::Running if fresh => return None,
                RunState::Running => {
                    runs.remove(i);
                }
                _ if !again => return None,
                _ => {}
            }
        }
        let at = util::now_iso();
        runs.push(Run { at: at.clone(), state: RunState::Running, read: Some(read), took: None, text: String::new() });
        v::write_runs(&mut t, &runs);
        store.save_task(&t).ok()?;
        Some(at)
    })?;
    store.git_commit(&format!("wsp: checking {id}"));
    Some(made)
}

/// Take an entry back when its run could not start: nothing was run, so
/// nothing is recorded, and the next trigger starts it again.
fn abandon(store: &Store, id: &str, at: &str) {
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return };
        let runs: Vec<Run> = v::runs(&t).into_iter().filter(|r| r.at != at).collect();
        v::write_runs(&mut t, &runs);
        let _ = store.save_task(&t);
    });
}

/// What [`settle`] did to the row.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Settled {
    Passed,
    /// Failed, and the row was at review: it is back at `doing`.
    SentBack,
    /// Failed, and the row was not at review: recorded, nobody moved.
    Failed,
    /// The row or its entry is gone.
    Lost,
}

/// Write the outcome over its `running` entry — and, when it failed on a row at
/// review, send the row back in the same write, so no reader ever sees a failed
/// landing as finished.
pub(crate) fn settle(store: &Store, id: &str, at: &str, ran: &[Ran], took: Duration) -> Settled {
    let state = if passed(ran) { RunState::Passed } else { RunState::Failed };
    let settled = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return Settled::Lost };
        let mut runs = v::runs(&t);
        let Some(run) = runs.iter_mut().find(|r| r.at == at) else { return Settled::Lost };
        run.state = state;
        run.took = Some(secs(took));
        run.text = account(ran);
        let read = run.read.clone();
        v::write_runs(&mut t, &runs);
        let mut out = if state == RunState::Passed { Settled::Passed } else { Settled::Failed };
        if state == RunState::Failed && t.status() == Status::Review {
            let owes = owed(ran, read.as_deref().unwrap_or_default());
            v::send_back(&mut t, util::now_iso(), "wsp check failed", read.as_deref(), &owes, Some(CHECK_HEAD));
            out = Settled::SentBack;
        }
        if store.save_task(&t).is_err() {
            return Settled::Lost;
        }
        out
    });
    let word = state.as_str();
    store.log_event("task-checked", serde_json::json!({ "id": id, "state": word, "steps": ran.len(), "took": took.as_secs() }));
    store.git_commit(&format!("wsp: checked {id} {word}"));
    settled
}

const CHECK_HEAD: &str = "`wsp check` failed on this row's landing. What is owed:\n\n";

// ---- the verb -----------------------------------------------------------------

pub fn check(store: &Store, args: &Args) -> i32 {
    const USAGE: &str = "wsp check <task> [--plan] [--at <commit>] [--json]";
    let p = Paint::new();
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let auto = args.has("auto");
    let t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let steps = declared(store, &t);
    if steps.is_empty() {
        if !auto {
            println!(
                "{} {}",
                p.dim("nothing to run —"),
                p.dim(&format!("project {} declares no ## Checks and {} has none", project_of(&t).unwrap_or_default(), t.id))
            );
        }
        return 0;
    }
    let at = args.get("at");
    let tg = match target(store, &t, at.as_deref()) {
        Ok(tg) => tg,
        Err(why) => {
            eprintln!("wsp: {why}");
            return 1;
        }
    };
    if args.has("plan") || args.has("dry-run") {
        println!("{} at {}", p.bold(&t.id), &tg.sha[..12]);
        for s in &steps {
            println!("  {} · {} · up to {}s{}", s.name, s.command, s.limit.as_secs(), if s.of == Of::Task { " · the task's" } else { "" });
        }
        println!("  {}", p.dim(&format!("in {}", util::contract(&dir_of(store, &tg).join("tree")))));
        return 0;
    }

    // A named commit is a look at another commit — a replay — and writes
    // nothing on the row. The landing is the one that is recorded.
    let record = at.is_none();
    let entry = match (record, args.get("run")) {
        (true, Some(at)) => Some(at),
        (true, None) => match begin(store, &t.id, &tg.sha, stale_after(&steps), true) {
            Some(at) => Some(at),
            None => {
                eprintln!("wsp: a check of {} at {} is already running", t.id, &tg.sha[..12]);
                return 1;
            }
        },
        (false, _) => None,
    };

    let dir = dir_of(store, &tg);
    let tree = dir.join("tree");
    let started = Instant::now();
    let lock = match TreeLock::take(dir.join("lock"), TREE_WAIT) {
        Ok(l) => l,
        Err(why) => return gave_up(store, &t.id, entry.as_deref(), &why),
    };
    if let Err(why) = checkout(&tg.repo, &tree, &tg.sha) {
        drop(lock);
        return gave_up(store, &t.id, entry.as_deref(), &format!("could not put {} in {}: {why}", &tg.sha[..12], util::contract(&tree)));
    }
    if !auto && !args.json() {
        println!("{} {} at {} in {}", p.dim("checking"), p.bold(&t.id), &tg.sha[..12], util::contract(&tree));
    }
    let ran = run_all(&tree, &steps, &dir.join("last.log"), |r| {
        if !auto && !args.json() {
            let (mark, tail) = match &r.done {
                Done::Passed => (p.green("✓"), String::new()),
                Done::Failed { .. } => (p.red("✗"), format!(" · {}", why_failed(r))),
                Done::Skipped => (p.dim("–"), " · skipped".to_string()),
            };
            println!("  {mark} {} · {}s{tail}", r.step.name, secs(r.took));
        }
    });
    drop(lock);
    let took = started.elapsed();
    let ok = passed(&ran);

    let settled = match &entry {
        Some(at) => settle(store, &t.id, at, &ran, took),
        None => Settled::Lost,
    };
    if args.json() {
        println!(
            "{}",
            serde_json::json!({ "task": t.id, "commit": tg.sha, "passed": ok, "took": took.as_secs(), "steps": account(&ran) })
        );
    } else if !auto {
        if !ok {
            for l in owed(&ran, &tg.sha).lines().skip(2) {
                println!("    {}", p.dim(l));
            }
        }
        let tail = match (&settled, record) {
            (Settled::SentBack, _) => format!(" · {} is back at doing with this as what is owed", t.id),
            (_, false) => " · nothing written on the row".to_string(),
            _ => String::new(),
        };
        println!(
            "{} {} {}  {}s{tail}",
            if ok { p.green("✓") } else { p.red("✗") },
            p.bold(&t.id),
            if ok { "passed" } else { "failed" },
            took.as_secs()
        );
    }
    if settled == Settled::SentBack {
        if let Some(t) = store.find_task(&t.id) {
            crate::cmd_task::deliver_reason(store, &t.id, &owed(&ran, &tg.sha));
        }
    }
    if record {
        crate::cycle::poke_task(store, &t.id, "checked");
    }
    i32::from(!ok)
}

/// A run that could not start: its entry is taken back and the reason said
/// where somebody looks.
fn gave_up(store: &Store, id: &str, entry: Option<&str>, why: &str) -> i32 {
    if let Some(at) = entry {
        abandon(store, id, at);
    }
    crate::cycle::log_line(store, &format!("check {id}: not run — {why}"));
    eprintln!("wsp: check {id}: not run — {why}");
    1
}

// ---- the triggers ---------------------------------------------------------------

/// Whether the verifier may start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    Clear,
    /// A run is owed or under way for this landing.
    Wait,
}

/// The gate in front of a member's verifier, starting the run it is waiting on
/// when none is.
///
/// Clear for a member with nothing declared, no landing, or no root to run in —
/// none of those is a reason to hold work — and for a landing whose run has
/// finished, either way: a pass opens the way, and a failure has already sent
/// the member back, so one at review again was answered by a person.
pub(crate) fn gate(store: &Store, member: &Task) -> Gate {
    let steps = declared(store, member);
    let Some(sha) = crate::repair::landed(member) else { return Gate::Clear };
    if steps.is_empty() {
        return Gate::Clear;
    }
    if target(store, member, None).is_err() {
        return Gate::Clear;
    }
    let mine = v::runs(member).into_iter().rev().find(|r| r.read.as_deref().is_some_and(|x| v::same_commit(x, &sha)));
    if mine.as_ref().is_some_and(|r| r.state != RunState::Running) {
        return Gate::Clear;
    }
    if let Some(at) = begin(store, &member.id, &sha, stale_after(&steps), false) {
        crate::cycle::launch_out(store, &["check", &member.id, "--auto", "--run", &at]);
    }
    Gate::Wait
}

/// After `wsp land`: start the run for a row that no list will gate.
pub(crate) fn after_land(store: &Store, id: &str) {
    if let Some(t) = store.find_task(id) {
        let _ = gate(store, &t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(text: &str) -> Step {
        parse_steps(text, Of::Project).remove(0)
    }

    #[test]
    fn a_step_is_a_name_a_limit_and_a_command() {
        let s = step("- tests (20m): cargo test --release");
        assert_eq!((s.name.as_str(), s.command.as_str(), s.limit), ("tests", "cargo test --release", Duration::from_secs(1200)));
        let s = step("- build: cargo build");
        assert_eq!((s.name.as_str(), s.limit), ("build", DEFAULT_LIMIT));
        let s = step("- quick (90s): ./check.sh");
        assert_eq!(s.limit, Duration::from_secs(90));
    }

    #[test]
    fn a_bare_command_is_a_step_named_for_its_program_and_a_colon_inside_it_is_not_a_name() {
        let s = step("- `test -f docs/numbers.md`");
        assert_eq!((s.name.as_str(), s.command.as_str()), ("test", "test -f docs/numbers.md"));
        let s = step(r#"- grep -q "a: b" notes.txt"#);
        assert_eq!(s.command, r#"grep -q "a: b" notes.txt"#, "the text before `: ` is not a name");
        assert_eq!(s.name, "grep");
        let s = step("- scripts/lanes.sh gate");
        assert_eq!(s.name, "lanes.sh");
    }

    #[test]
    fn prose_and_empty_items_declare_nothing() {
        let text = "Run these in order.\n\n- tests: cargo test\n-\n- \nnot a list item\n* lint (1m): cargo clippy";
        let names: Vec<_> = parse_steps(text, Of::Task).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["tests", "lint"]);
    }

    #[test]
    fn a_limit_that_is_not_one_makes_the_head_part_of_the_command() {
        let s = step("- slow (soon): run it");
        assert_eq!(s.command, "slow (soon): run it");
    }

    fn scratch_log(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("wsp-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("log"))
    }

    fn sh(command: &str, limit: u64) -> Step {
        Step { name: "s".into(), command: command.into(), limit: Duration::from_secs(limit), of: Of::Project }
    }

    #[test]
    fn exit_zero_passes_and_a_failure_keeps_the_end_of_its_output() {
        let (dir, log) = scratch_log("exit");
        let ok = run_step(&dir, &sh("echo fine", 10), &log);
        assert_eq!(ok.done, Done::Passed);
        let bad = run_step(&dir, &sh("echo one; echo two >&2; exit 3", 10), &log);
        assert_eq!(bad.done, Done::Failed { exit: Some(3), timed_out: false });
        assert!(bad.tail.contains("one") && bad.tail.contains("two"), "both streams: {}", bad.tail);
    }

    #[test]
    fn a_step_past_its_limit_is_killed_with_everything_it_started() {
        let (dir, log) = scratch_log("limit");
        let started = Instant::now();
        let r = run_step(&dir, &sh("sleep 30 & sleep 30", 1), &log);
        assert!(started.elapsed() < Duration::from_secs(10), "waited on the sleep");
        assert_eq!(r.done, Done::Failed { exit: None, timed_out: true });
        assert!(why_failed(&r).contains("timed out after 1s"));
    }

    #[test]
    fn steps_stop_at_the_first_failure_and_say_which_were_not_run() {
        let (dir, log) = scratch_log("stop");
        let steps = vec![sh("true", 10), sh("exit 1", 10), sh("true", 10)];
        let ran = run_all(&dir, &steps, &log, |_| {});
        let done: Vec<_> = ran.iter().map(|r| r.done.clone()).collect();
        assert_eq!(done[0], Done::Passed);
        assert!(matches!(done[1], Done::Failed { .. }));
        assert_eq!(done[2], Done::Skipped);
        assert!(!passed(&ran));
        let text = account(&ran);
        assert!(text.contains("✓ s") && text.contains("✗ s") && text.contains("– s · true · skipped"), "{text}");
    }

    #[test]
    fn a_check_runs_without_wsps_own_environment() {
        let (dir, log) = scratch_log("env");
        std::env::set_var("WSP_CHECK_PROBE", "set");
        let r = run_step(&dir, &sh(r#"test -z "$WSP_CHECK_PROBE""#, 10), &log);
        std::env::remove_var("WSP_CHECK_PROBE");
        assert_eq!(r.done, Done::Passed, "WSP_* reached the command");
    }

    // ---- the row ----

    use crate::model::Project;

    fn git_run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A store with a project rooted at a repository whose one commit holds
    /// `check.sh`, and a task landed on that commit.
    fn world(name: &str, project_checks: &str, script: &str) -> (util::Isolated, Store, String) {
        let env = util::isolated(&format!("check-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_run(&repo, &["init", "--quiet", "-b", "master"]);
        std::fs::write(repo.join("check.sh"), script).unwrap();
        git_run(&repo, &["add", "check.sh"]);
        git_run(&repo, &["commit", "--quiet", "-m", "first"]);
        let sha = git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let store = Store::open();
        let mut p = Project::new("pj");
        p.roots = vec![repo.display().to_string()];
        p.body = format!("## Checks\n{project_checks}\n");
        store.save_project(&p).unwrap();
        let mut t = Task::new("a row", "pj-001");
        t.project = Some("pj".into());
        t.set_status(Status::Review);
        t.log(&format!("{} {sha} on pj-001", crate::repair::LANDED));
        store.save_task(&t).unwrap();
        (env, store, sha)
    }

    #[test]
    fn a_run_is_recorded_beside_the_verifiers_passes_and_leaves_them_alone() {
        let (_env, store, sha) = world("beside", "- ok: sh check.sh", "exit 0\n");
        v::record(&store, "pj-001", v::State::Holds, "it holds", None, Some("held")).unwrap();
        let t = store.find_task("pj-001").unwrap();
        let tg = target(&store, &t, None).unwrap();
        let at = begin(&store, "pj-001", &tg.sha, 3600, false).unwrap();
        let ran = vec![Ran { step: sh("sh check.sh", 10), done: Done::Passed, took: Duration::from_secs(4), tail: String::new() }];
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::from_secs(4)), Settled::Passed);

        let t = store.find_task("pj-001").unwrap();
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].state, runs[0].took), (RunState::Passed, Some(4)));
        assert!(v::same_commit(runs[0].read.as_deref().unwrap(), &sha));
        let passes = v::passes(&t);
        assert_eq!(passes.len(), 1, "{}", t.body);
        assert_eq!(passes[0].text, "it holds", "the run's lines are not read as the pass's verdict");
        // A write of the passes keeps the run, and a write of the run keeps the pass.
        let mut t2 = t.clone();
        v::write(&mut t2, &passes);
        assert_eq!(v::runs(&t2).len(), 1);
        assert!(v::verified(&[t2], "pj-001"), "a passing run is not a verdict, and does not unseat one");
    }

    #[test]
    fn a_run_is_started_once_per_landing_and_again_only_when_asked() {
        let (_env, store, sha) = world("once", "- ok: true", "exit 0\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        assert!(begin(&store, "pj-001", &sha, 3600, false).is_none(), "a second trigger found the first one running");
        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new() }];
        settle(&store, "pj-001", &at, &ran, Duration::ZERO);
        assert!(begin(&store, "pj-001", &sha, 3600, false).is_none(), "a landing already checked is not checked again");
        assert!(begin(&store, "pj-001", &sha, 3600, true).is_some(), "but a person can ask");
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 2);
    }

    #[test]
    fn a_running_entry_nobody_finished_is_taken_again_after_its_budget() {
        let (_env, store, sha) = world("stale", "- ok: true", "exit 0\n");
        let mut t = store.find_task("pj-001").unwrap();
        v::write_runs(
            &mut t,
            &[Run { at: "2026-01-01T00:00:00Z".into(), state: RunState::Running, read: Some(sha[..12].into()), took: None, text: String::new() }],
        );
        store.save_task(&t).unwrap();
        assert!(begin(&store, "pj-001", &sha, 3600, false).is_some());
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 1, "the dead entry was replaced, not kept");
    }

    #[test]
    fn a_failure_on_a_row_at_review_sends_it_back_in_the_same_write_with_the_step_as_what_is_owed() {
        let (_env, store, sha) = world("sendback", "- tests: sh check.sh", "echo boom >&2; exit 7\n");
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = vec![Ran {
            step: Step { name: "tests".into(), command: "sh check.sh".into(), limit: DEFAULT_LIMIT, of: Of::Project },
            done: Done::Failed { exit: Some(7), timed_out: false },
            took: Duration::from_secs(2),
            tail: "boom".into(),
        }];
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::from_secs(2)), Settled::SentBack);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Doing);
        let log = t.section("Log").unwrap();
        assert!(log.contains("sent back: wsp check failed at") && log.contains("`tests`") && log.contains("exit 7"), "{log}");
        let run = &v::runs(&t)[0];
        assert_eq!(run.state, RunState::Failed);
        assert!(run.text.contains("✗ tests · sh check.sh · 2s · exit 7") && run.text.contains("  | boom"), "{}", run.text);
    }

    #[test]
    fn a_failure_on_a_row_still_at_doing_is_recorded_and_nobody_is_moved() {
        let (_env, store, sha) = world("doing", "- t: false", "exit 1\n");
        let mut t = store.find_task("pj-001").unwrap();
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        let at = begin(&store, "pj-001", &sha, 3600, false).unwrap();
        let ran = vec![Ran { step: sh("false", 10), done: Done::Failed { exit: Some(1), timed_out: false }, took: Duration::ZERO, tail: String::new() }];
        assert_eq!(settle(&store, "pj-001", &at, &ran, Duration::ZERO), Settled::Failed);
        assert_eq!(store.find_task("pj-001").unwrap().status(), Status::Doing);
    }

    #[test]
    fn the_gate_holds_a_landed_member_with_checks_until_the_run_has_passed() {
        let (_env, store, sha) = world("gate", "- ok: true", "exit 0\n");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(gate(&store, &t), Gate::Wait, "no run for this landing yet");
        let t = store.find_task("pj-001").unwrap();
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1, "the gate wrote the entry that keeps a second trigger from starting another");
        assert_eq!(runs[0].state, RunState::Running);
        assert_eq!(gate(&store, &t), Gate::Wait, "and it is still running");
        assert_eq!(v::runs(&store.find_task("pj-001").unwrap()).len(), 1);

        let ran = vec![Ran { step: sh("true", 10), done: Done::Passed, took: Duration::ZERO, tail: String::new() }];
        settle(&store, "pj-001", &runs[0].at, &ran, Duration::ZERO);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(gate(&store, &t), Gate::Clear, "passed for {sha}");
    }

    #[test]
    fn the_gate_is_clear_where_nothing_is_declared_nothing_landed_or_nothing_can_run() {
        let (_env, store, _) = world("clear", "", "exit 0\n");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(gate(&store, &t), Gate::Clear, "a project that declares nothing");

        let mut t = store.find_task("pj-001").unwrap();
        t.body = "## Checks\n- ok: true\n".into();
        store.save_task(&t).unwrap();
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(declared(&store, &t).len(), 1, "a task's own checks count on their own");
        assert_eq!(gate(&store, &t), Gate::Clear, "no landing recorded");
    }

    #[test]
    fn a_task_adds_its_checks_after_its_projects() {
        let (_env, store, _) = world("both", "- build: true", "exit 0\n");
        let mut t = store.find_task("pj-001").unwrap();
        t.body.push_str("\n## Checks\n- `test -f check.sh`\n");
        let steps = declared(&store, &t);
        assert_eq!(steps.iter().map(|s| (s.name.as_str(), s.of)).collect::<Vec<_>>(), [("build", Of::Project), ("test", Of::Task)]);
    }

    #[test]
    fn the_verb_runs_at_the_landing_in_a_tree_of_its_own_and_records_it() {
        let (env, store, sha) = world("verb", "- file: test -f check.sh\n- script: sh check.sh", "exit 0\n");
        let code = check(&store, &Args::synth("check", &["pj-001"], &[]));
        assert_eq!(code, 0);
        let t = store.find_task("pj-001").unwrap();
        let runs = v::runs(&t);
        assert_eq!(runs.len(), 1, "{}", t.body);
        assert_eq!(runs[0].state, RunState::Passed, "{}", runs[0].text);
        assert!(runs[0].text.contains("✓ file") && runs[0].text.contains("✓ script"), "{}", runs[0].text);
        let tree = env.state().join("checks/pj-repo/tree");
        assert!(tree.join("check.sh").exists(), "the warm tree is under the state directory");
        assert_eq!(git(&tree, &["rev-parse", "HEAD"]).unwrap().trim(), sha);
        assert_eq!(t.status(), Status::Review, "a pass moves nobody");
    }

    #[test]
    fn the_verb_at_a_named_commit_runs_and_writes_nothing_on_the_row() {
        let (_env, store, sha) = world("replay", "- script: sh check.sh", "exit 1\n");
        let before = store.find_task("pj-001").unwrap().body;
        let code = check(&store, &Args::synth("check", &["pj-001"], &[("at", &sha)]));
        assert_eq!(code, 1, "the failure is the exit status");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.body, before);
        assert_eq!(t.status(), Status::Review, "a replay moves nobody");
    }

    #[test]
    fn the_verb_on_a_failing_landing_sends_the_row_back() {
        let (_env, store, _) = world("fails", "- script: sh check.sh", "echo broken >&2; exit 4\n");
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[])), 1);
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.status(), Status::Doing);
        assert!(v::runs(&t)[0].text.contains("| broken"), "{}", v::runs(&t)[0].text);
    }

    #[test]
    fn the_plan_names_the_steps_and_runs_none() {
        let (env, store, _) = world("plan", "- script: sh check.sh", "exit 0\n");
        assert_eq!(check(&store, &Args::synth("check", &["pj-001"], &[("plan", "true")])), 0);
        assert!(v::runs(&store.find_task("pj-001").unwrap()).is_empty());
        assert!(!env.state().join("checks").exists(), "no tree was made");
    }
}
