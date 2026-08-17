//! `wsp checkout` — a working tree of this agent's own, and `wsp land` to put
//! it back on the trunk.
//!
//! The other half of the isolation `wsp verify` started. `verify` gives every
//! agent its own tree to *build* in, which was the half that bit hardest; this
//! is the half with no cheaper version, because the failure it answers is not a
//! build at all. Six times on 2026-08-15 and three more on 2026-08-17, one
//! agent's uncommitted work landed in another agent's commit, or was reverted
//! out from under it. None of the nine was carelessness and each got past the
//! defence built after the last one, because they all assumed something untrue:
//! that a checkout belongs to whoever is looking at it.
//!
//! Everything short of a tree each has been tried and is recorded on
//! t-260815-022. Naming explicit paths does not help when the file is genuinely
//! being changed by both of you. Hunk-level staging only works if you are the
//! one committing. Announcing first does not help — a sweep happened twenty
//! minutes after the announcement, because the other agent committed *to give a
//! clean base* and took the edit with it. A private `GIT_INDEX_FILE` fixes the
//! index and leaves the tree. The common factor is that a working tree is
//! shared mutable state with no owner, and git stages by file.
//!
//! # One tree per task, not per agent
//!
//! [`crate::cmd_verify`] keys its build tree on the workspace, because a build
//! tree only has to be *somebody's* and pane ids are reissued across herdr
//! restarts. An edit tree is a different thing keyed on a different fact: what
//! lands is a task's work, the branch is named for the task, and `spawn` knows
//! the task before the workspace it is about to open exists. An agent holds one
//! task at a time, so per-task is per-agent in practice — and when the agent
//! moves task it moves tree, which is right for a branch that is about to land.
//!
//! No task, no tree. A bare shell and the coordination seat stay in the trunk,
//! which is the tree a whole-tree review reads.
//!
//! # Short-lived branches, because review stays whole-tree
//!
//! Ed's call, 2026-08-17: we are not building per-agent review, because review
//! time goes up sharply once a reviewer has to visit each agent's corner. That
//! settles the branch question rather than leaving it open. If the reviewer
//! reads the trunk, then a long-lived per-task branch leaves them reading a tree
//! none of the work is in — so the branch has to be short and land early rather
//! than isolate for long.
//!
//! Git will not put two worktrees on one branch, so "they all share master" was
//! never available; the real choice was short-lived branches against
//! detached-HEAD plumbing, and the first is ordinary where the second is clever.
//!
//! [`land`] rebases onto the trunk and fast-forwards the trunk onto it. The
//! rebase is where two agents editing one file finally meet, and it is the point
//! of the whole arrangement: the collision that used to be a silent sweep is now
//! a conflict with both sides named, in your own tree, yours to resolve.
//!
//! # Under the root, gitignored
//!
//! The third question t-260815-022 carried. [`crate::resolve::Index::project_for_cwd`]
//! and `overlap`'s `root_for` are longest-prefix matches against declared
//! project roots, so a worktree outside the root resolves to no project at all:
//! `wsp where` goes blank, the panel cannot place the pane, and `overlap` reads
//! two agents in two worktrees of one repository as `Elsewhere` — silencing the
//! warning in exactly the arrangement this creates. Nesting under the root makes
//! all of that work without touching resolution. Teaching resolution about
//! worktrees instead means asking git where a path belongs once per pane, in
//! `wsp brief`, which a session-start hook runs.
//!
//! What nesting costs is one line of `.gitignore` and one correction in
//! `overlap`, which would otherwise read a worktree under the trunk as *inside*
//! it and warn about an agent that can no longer reach your files. That is the
//! inverse of the failure above and just as expensive: a warning that is wrong
//! in the reassuring direction gets ignored, and one that is wrong in the
//! alarming direction gets ignored too.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::cmd_verify::{git, git_ok, toplevel};
use crate::store::Store;
use crate::util;
use crate::Args;

/// The directory under a repository root that holds the per-task worktrees.
///
/// Dotted so it is out of the way of an `ls`, and one directory rather than one
/// per task at the root so a single `.gitignore` line covers every tree that
/// will ever exist here.
pub(crate) const WORKTREES: &str = ".worktrees";

/// The working tree a path is in, when it is one of ours.
///
/// A pure path rule on purpose: `overlap` calls it for every pane herdr reports,
/// on every `wsp brief`, and asking git which worktree owns a path costs a
/// process each time. The layout is ours to define, so the answer is already in
/// the path — everything up to and including the component after `.worktrees`.
///
/// `None` means the trunk, or anywhere else at all. The caller wants the
/// distinction rather than a boolean: two paths are in the same working tree
/// only when this agrees about both, and "both in no worktree" is as much an
/// agreement as "both in that one".
pub(crate) fn worktree_of(p: &Path) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();
    let mut parts = p.components();
    while let Some(c) = parts.next() {
        if c.as_os_str() == WORKTREES {
            let name = parts.next()?;
            prefix.push(c);
            prefix.push(name);
            return Some(prefix);
        }
        prefix.push(c);
    }
    None
}

/// The repository's main working tree — the trunk that gets reviewed.
///
/// From `git worktree list`, whose first entry is the main one, rather than by
/// walking up from `.git`: inside a linked worktree `.git` is a file pointing
/// somewhere under the main repository's `.git/worktrees`, and reconstructing
/// the main tree's path from that is arithmetic on git's private layout.
pub(crate) fn trunk(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["worktree", "list", "--porcelain"])?;
    let line = out.lines().find_map(|l| l.strip_prefix("worktree "))?;
    Some(PathBuf::from(line.trim()))
}

/// The branch the trunk has checked out, or `None` if it is detached.
///
/// What `land` fast-forwards, and what a new task branch starts from. Read from
/// the trunk rather than assumed to be `master`, because the name of the trunk
/// branch is a fact about the repository and not about wsp.
pub(crate) fn trunk_branch(trunk: &Path) -> Option<String> {
    let out = git(trunk, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let name = out.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Where a task's tree lives. The task id is the directory name and the branch
/// name both: it is already unique, and a worktree left behind names the task
/// that abandoned it.
pub(crate) fn checkout_dir(trunk: &Path, task: &str) -> PathBuf {
    trunk.join(WORKTREES).join(task)
}

/// Make this task's tree if it has none. Returns whether it was new.
///
/// The branch is created at the trunk's tip on first use and reused after, so
/// an agent that comes back to a task after a `wsp release` finds its own
/// commits rather than a fresh start.
fn ensure(repo: &Path, dir: &Path, task: &str, from: &str) -> Result<bool, String> {
    if dir.join(".git").exists() {
        return Ok(false);
    }
    // A directory left by a removed worktree, or a registration whose directory
    // is gone: both make `add` refuse, and both are what a cleanup step nobody
    // ran leaves behind.
    let _ = std::fs::remove_dir_all(dir);
    let _ = git(repo, &["worktree", "prune"]);
    std::fs::create_dir_all(dir.parent().unwrap_or(dir)).map_err(|e| e.to_string())?;

    let path = dir.display().to_string();
    let existing = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{task}")]);
    if existing.is_some() {
        git_ok(repo, &["worktree", "add", "--quiet", &path, task])?;
    } else {
        git_ok(repo, &["worktree", "add", "--quiet", "-b", task, &path, from])?;
    }
    Ok(true)
}

/// Whether a tree has anything uncommitted, tracked or not.
fn dirty(dir: &Path) -> bool {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// The commits on `branch` that the trunk does not have, newest first.
fn ahead(repo: &Path, trunk_branch: &str, branch: &str) -> Vec<String> {
    git(repo, &["log", "--format=%h %s", &format!("{trunk_branch}..{branch}")])
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// Serialise the moment two worktrees meet.
///
/// Everything else here is isolated by construction and needs no lock; landing
/// is the one act that writes a ref two agents both want, and it takes about a
/// second. So the judgement is the opposite of [`crate::store::Store::locked`],
/// which gives up after two seconds and carries on: a lost store update is
/// recoverable and a `wsp claim` that never returns is not, but a *half* land —
/// rebased onto a trunk that moved underneath, or fast-forwarded to a branch
/// that is being rewritten — is neither. So this waits longer and then refuses,
/// rather than proceeding without it.
fn landing<T>(store: &Store, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use std::time::{Duration, Instant};
    const PATIENCE: Duration = Duration::from_secs(90);
    // Longer than any land takes and shorter than a person's attention: a
    // process killed mid-land leaves this behind, and nobody is going to think
    // to look for it.
    const STALE: Duration = Duration::from_secs(300);

    let _ = std::fs::create_dir_all(&store.state);
    let path = store.state.join("landing.lock");
    let start = Instant::now();
    loop {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let held = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().unwrap_or_default())
                    .unwrap_or_default();
                if held > STALE {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if start.elapsed() > PATIENCE {
                    return Err(format!(
                        "another agent has been landing for {}s — {} is the lock",
                        held.as_secs(),
                        util::contract(&path)
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("cannot take the landing lock: {e}")),
        }
    }
    let out = f();
    let _ = std::fs::remove_file(&path);
    out
}

/// The task whose tree this is: named on the command line, or the one this pane
/// holds. A pane holding nothing gets told so rather than given a tree named
/// after nothing.
fn task_of(store: &Store, args: &Args) -> Result<String, String> {
    if let Some(id) = args.rest.first() {
        return Ok(id.clone());
    }
    let w = crate::cmd_agent::Whereabouts::live(store);
    match crate::cmd_agent::locate(&w).task {
        Some(t) => Ok(t.id),
        None => Err("no task in hand — `wsp claim <id>` first, or name one".into()),
    }
}

/// Everything both verbs need to know about where they are.
struct Where {
    repo: PathBuf,
    trunk: PathBuf,
    branch: String,
    task: String,
    dir: PathBuf,
}

fn locate(store: &Store, args: &Args) -> Result<Where, String> {
    let cwd = std::env::current_dir().map_err(|_| "cannot read the current directory".to_string())?;
    let repo = toplevel(&cwd).ok_or_else(|| format!("{} is not in a git repository", util::contract(&cwd)))?;
    let trunk = trunk(&repo).ok_or_else(|| "cannot find the repository's main working tree".to_string())?;
    let branch = trunk_branch(&trunk).ok_or_else(|| {
        format!("{} is on a detached HEAD — there is no trunk to branch from", util::contract(&trunk))
    })?;
    let task = task_of(store, args)?;
    let dir = checkout_dir(&trunk, &task);
    Ok(Where { repo, trunk, branch, task, dir })
}

pub fn checkout(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let w = match locate(store, args) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    if args.has("rm") {
        let here = std::env::current_dir().unwrap_or_default();
        if here.starts_with(&w.dir) {
            eprintln!("wsp: you are standing in {} — cd out of it first", util::contract(&w.dir));
            return 2;
        }
        let existed = w.dir.exists();
        let _ = git(&w.repo, &["worktree", "remove", "--force", &w.dir.display().to_string()]);
        let _ = std::fs::remove_dir_all(&w.dir);
        let _ = git(&w.repo, &["worktree", "prune"]);
        // `-d` and not `-D`: a branch git will not delete is one holding commits
        // the trunk has not got, and that is work rather than litter.
        let kept = git(&w.repo, &["branch", "-d", &w.task]).is_none();
        if args.json() {
            println!("{}", json!({"removed": existed, "branch_kept": kept, "path": util::contract(&w.dir)}));
        } else if !existed {
            println!("no tree for {}", w.task);
        } else {
            println!("removed {}", util::contract(&w.dir));
            if kept {
                println!("{}", p.yellow(&format!("branch {} kept — it has commits the trunk has not", w.task)));
            }
        }
        return 0;
    }

    let head = git(&w.trunk, &["rev-parse", "--short", &w.branch]).map(|s| s.trim().to_string()).unwrap_or_default();
    let fresh = match ensure(&w.repo, &w.dir, &w.task, &w.branch) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };
    let commits = ahead(&w.repo, &w.branch, &w.task);

    if args.json() {
        println!(
            "{}",
            json!({
                "task": w.task,
                "path": util::contract(&w.dir),
                "branch": w.task,
                "from": w.branch,
                "new": fresh,
                "ahead": commits.len(),
            })
        );
        return 0;
    }

    println!(
        "{} {}{}",
        p.dim("tree"),
        util::contract(&w.dir),
        if fresh { p.dim(&format!("  new, from {} {head}", w.branch)) } else { String::new() }
    );
    if !commits.is_empty() {
        println!("{} {}", p.dim("ahead"), p.bold(&format!("{} commit(s) to land", commits.len())));
    }
    println!("{} {}", p.dim("cd"), util::contract(&w.dir));
    0
}

pub fn land(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let w = match locate(store, args) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    if !w.dir.join(".git").exists() {
        eprintln!("wsp: no tree for {} — nothing to land", w.task);
        return 2;
    }
    // Committing is the agent's act and stays the agent's act: what is
    // uncommitted here is a decision nobody else can make, and landing around
    // it would be this command doing the one thing the whole arrangement exists
    // to stop — moving somebody's work without them.
    if dirty(&w.dir) {
        eprintln!("wsp: {} has uncommitted work — commit it first", util::contract(&w.dir));
        return 2;
    }
    if ahead(&w.repo, &w.branch, &w.task).is_empty() {
        println!("{} {}", p.dim("nothing to land —"), p.dim(&format!("{} is already in {}", w.task, w.branch)));
        return 0;
    }

    let result = landing(store, || {
        // Rebase first, so what reaches the trunk is a fast-forward and the
        // trunk can never gain a merge commit from here. This is also where two
        // agents who edited one file meet, which is the arrangement working
        // rather than failing.
        if let Err(e) = git_ok(&w.dir, &["rebase", &w.branch]) {
            return Err(format!(
                "the rebase onto {} stopped: {e}\n\
                 wsp: it is your tree and your conflict — resolve it in {}, `git rebase --continue`, then `wsp land` again",
                w.branch,
                util::contract(&w.dir)
            ));
        }
        let landed = ahead(&w.repo, &w.branch, &w.task);
        // `--ff-only` in the trunk rather than a push: the trunk is a checked-out
        // working tree, and this is the one form of update that refuses rather
        // than silently overwriting a file somebody has open there.
        git_ok(&w.trunk, &["merge", "--ff-only", "--quiet", &w.task])
            .map_err(|e| format!("{} would not fast-forward: {e}", util::contract(&w.trunk)))?;
        Ok(landed)
    });

    let landed = match result {
        Ok(l) => l,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };

    // The file list, printed after the fact against the trunk. This is step 4
    // of the commit procedure, and it is the check that has caught a wrong
    // commit twice — a fifth file riding along, invisible to `git diff --cached`
    // because that compares against HEAD as it was when you staged.
    let files: Vec<String> = git(&w.trunk, &["show", "--stat", "--format=", &format!("{}~{}..{}", w.branch, landed.len(), w.branch)])
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default();

    let kept = args.has("keep");
    if !kept {
        let _ = git(&w.repo, &["worktree", "remove", &w.dir.display().to_string()]);
        let _ = std::fs::remove_dir_all(&w.dir);
        let _ = git(&w.repo, &["worktree", "prune"]);
        let _ = git(&w.repo, &["branch", "-d", &w.task]);
    } else {
        // Landed and still standing in it: put the branch back on the trunk so
        // the next piece of work starts from what everyone else can see.
        let _ = git_ok(&w.dir, &["reset", "--hard", &w.branch, "--quiet"]);
    }

    if args.json() {
        println!(
            "{}",
            json!({
                "task": w.task,
                "into": w.branch,
                "commits": landed,
                "files": files,
                "tree_kept": kept,
            })
        );
        return 0;
    }

    println!("{} {} into {}", p.green("✓"), p.bold(&format!("{} commit(s)", landed.len())), w.branch);
    for c in landed.iter().rev() {
        println!("  {c}");
    }
    for f in &files {
        println!("  {}", p.dim(f));
    }
    if !kept {
        println!("{}", p.dim("tree removed — `wsp checkout` for the next one"));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-co-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(dir: &Path, args: &[&str]) {
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

    fn repo(dir: &Path) {
        run(dir, &["init", "--quiet", "-b", "master"]);
        std::fs::write(dir.join("kept.txt"), "one\n").unwrap();
        run(dir, &["add", "kept.txt"]);
        run(dir, &["commit", "--quiet", "-m", "first"]);
    }

    /// The whole point, asserted end to end: work committed in one agent's tree
    /// reaches the trunk, and nothing else does.
    #[test]
    fn work_committed_in_a_tree_of_its_own_lands_on_the_trunk() {
        let dir = scratch("land");
        repo(&dir);

        let wt = checkout_dir(&dir, "t-1");
        assert!(ensure(&dir, &wt, "t-1", "master").unwrap(), "the tree was not new");
        std::fs::write(wt.join("mine.txt"), "mine\n").unwrap();
        run(&wt, &["add", "mine.txt"]);
        run(&wt, &["commit", "--quiet", "-m", "mine"]);

        // Another agent's edit, uncommitted, in the trunk. It must survive.
        std::fs::write(dir.join("theirs.txt"), "theirs\n").unwrap();

        assert_eq!(ahead(&dir, "master", "t-1").len(), 1);
        git_ok(&wt, &["rebase", "master"]).unwrap();
        git_ok(&dir, &["merge", "--ff-only", "--quiet", "t-1"]).unwrap();

        assert!(dir.join("mine.txt").exists(), "the work did not reach the trunk");
        assert_eq!(std::fs::read_to_string(dir.join("theirs.txt")).unwrap(), "theirs\n");
        assert!(
            git(&dir, &["log", "--format=%s", "-1"]).unwrap().contains("mine"),
            "the trunk is not at the landed commit"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tree is a checkout, so the trunk's uncommitted work is not in it. This
    /// is the sweep made impossible rather than merely warned about: `git add`
    /// in one tree cannot see the other tree's files at all.
    #[test]
    fn another_agents_uncommitted_work_is_not_in_this_tree() {
        let dir = scratch("isolated");
        repo(&dir);
        std::fs::write(dir.join("theirs.txt"), "theirs\n").unwrap();

        let wt = checkout_dir(&dir, "t-2");
        ensure(&dir, &wt, "t-2", "master").unwrap();
        assert!(!wt.join("theirs.txt").exists(), "another agent's file was in this tree");
        assert!(!dirty(&wt), "a fresh tree was already dirty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Coming back to a task finds the commits it left, rather than a fresh
    /// branch — an agent releases a task and picks it up again all the time.
    #[test]
    fn coming_back_to_a_task_finds_the_branch_it_left() {
        let dir = scratch("resume");
        repo(&dir);
        let wt = checkout_dir(&dir, "t-3");
        ensure(&dir, &wt, "t-3", "master").unwrap();
        std::fs::write(wt.join("wip.txt"), "wip\n").unwrap();
        run(&wt, &["add", "wip.txt"]);
        run(&wt, &["commit", "--quiet", "-m", "wip"]);

        git_ok(&dir, &["worktree", "remove", &wt.display().to_string()]).unwrap();
        assert!(ensure(&dir, &wt, "t-3", "master").unwrap(), "the tree was not remade");
        assert!(wt.join("wip.txt").exists(), "the work left on the branch did not come back");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `overlap` asks this about every pane herdr reports, so it has to be a
    /// path rule and it has to get the trunk case right: a worktree nested under
    /// the root is *not* inside the trunk, however much the prefix says so.
    #[test]
    fn a_worktree_under_the_root_is_a_tree_of_its_own() {
        let root = Path::new("/Users/x/claude/wsp");
        assert_eq!(worktree_of(root), None, "the trunk is not a worktree");
        assert_eq!(worktree_of(&root.join("src/main.rs")), None);
        assert_eq!(
            worktree_of(&checkout_dir(root, "t-1").join("src/main.rs")),
            Some(checkout_dir(root, "t-1")),
            "a path inside a tree did not resolve to it"
        );
        assert_ne!(
            worktree_of(&checkout_dir(root, "t-1")),
            worktree_of(&checkout_dir(root, "t-2")),
            "two agents' trees read as one"
        );
        // `.worktrees` with nothing under it is the directory itself, and
        // belongs to no tree.
        assert_eq!(worktree_of(&root.join(WORKTREES)), None);
    }

    /// The trunk is where the review reads, so both verbs have to find it from
    /// inside a worktree as readily as from the trunk itself.
    #[test]
    fn the_trunk_is_found_from_inside_a_worktree() {
        let dir = scratch("trunk");
        repo(&dir);
        let wt = checkout_dir(&dir, "t-4");
        ensure(&dir, &wt, "t-4", "master").unwrap();
        assert_eq!(trunk(&wt).map(|p| util::real(&p.display().to_string())), Some(util::real(&dir.display().to_string())));
        assert_eq!(trunk_branch(&dir).as_deref(), Some("master"));
        assert_eq!(trunk_branch(&wt).as_deref(), Some("t-4"), "a tree is on its own branch");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
