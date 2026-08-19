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
//! tree only has to be *somebody's* and an agent is a workspace rather than any
//! one of its panes. An edit tree is a different thing keyed on a different
//! fact: what
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
//! # Landing leaves the tree standing
//!
//! `land` used to remove the tree it had landed, which read as tidiness and was
//! not. Ed's call, 2026-08-17: a task can be reopened, and a tree destroyed on
//! every land has to be rebuilt every time work resumes — a second checkout, a
//! cold `target/` and the minutes of both, paid on the routine act to save them
//! on the rare one.
//!
//! So the tree outlives the landing, and three things follow. Landing is
//! exactly what its name says — put my commits on the trunk — which makes it
//! safe to run repeatedly and safe to run from inside the tree: the first `land`
//! ever run deleted its own caller's cwd out from under it (t-260817-018), and
//! that failure is now unreachable rather than handled. Removing is `checkout
//! --rm`, run when a task is genuinely finished, which is a moment somebody
//! decides rather than a side effect of a verb they run all day. And because a
//! command somebody has to remember is a command that gets forgotten — which is
//! the lesson of every leak recorded on t-260815-022 — there is [`sweep`], the
//! same shape as `wsp archive` for tasks and `wsp reconcile --reap` for claims,
//! and it exists for the same reason both of those do.
//!
//! The sweep removes trees whose *task is closed* and nothing else. [`stale`]
//! also reports the tree that is merely clean and level with the trunk, and that
//! one is reported forever rather than swept: it is indistinguishable from the
//! tree made thirty seconds ago for an agent that has not typed yet.
//!
//! # Looked for where the work lives, not only where you are standing
//!
//! Both verbs used to compute exactly one path — the repository the caller's
//! cwd is in, and the task id under its `.worktrees` — and answer for that path
//! alone. Right for an agent, which is standing in the tree it is asking about.
//! Wrong for everybody else, and in particular for a governor, which sits in
//! its project's seat and cleans up after tasks in lanes rooted in other
//! repositories. Run from the `wsp` seat, `wsp checkout fork-009 --rm` looked
//! under `~/claude/wsp`, found nothing, printed `no tree for fork-009` and
//! exited 0 — while the tree stood in `~/claude/herdr/.worktrees/fork-009`.
//! Two survived that way on 2026-08-18 and were found by a `git worktree list`
//! run for an unrelated reason; both happened to be clean.
//!
//! So the search takes two candidates in order: the repository the caller is
//! standing in, and the repository the store says the task's project is worked
//! in — [`crate::resolve::Index::root_of`], which is the same answer
//! [`crate::cmd_spawn`] hands [`tree_for`] when it makes the tree. Where wsp
//! put it is now a place wsp looks.
//!
//! Only the *looking* widens. A tree is still made in the first candidate, so
//! `checkout` in a repository puts the tree in that repository, because a task
//! can genuinely be worked in two at once — `fork-006` had one tree in herdr for
//! its UI and one in wsp for the panel, on the day this was written — and where
//! you are standing is the only statement of which you meant.
//!
//! When neither candidate has a tree, the answer names both. `no tree for X` on
//! its own is indistinguishable from "there was nothing to do", which is the
//! whole of why nobody looked again.
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
    let base = trunk.join(WORKTREES);
    let dir = base.join(task);
    if dir.join(".git").exists() {
        return dir;
    }
    // A tree made before the task was renumbered is named after the id it had
    // then, and its commits are on a branch of that name. Making a second tree
    // beside it would leave that work on a branch `land` no longer looks for —
    // which is why the migration deliberately does not rename trees somebody
    // may be standing in, and why the name is resolved here instead.
    for former in former_ids(task) {
        let old = base.join(&former);
        if old.join(".git").exists() {
            return old;
        }
    }
    dir
}

/// Ids this task used to have, newest first.
///
/// Read from the store rather than passed in, because every one of the twenty
/// callers of [`checkout_dir`] would otherwise have to carry a store it does
/// not need. It is one small file, read at most once per checkout, and a store
/// that has never renamed anything has no file at all.
fn former_ids(task: &str) -> Vec<String> {
    crate::store::Store::open()
        .renamed_ids()
        .into_iter()
        .filter(|(_, to)| to == task)
        .map(|(from, _)| from)
        .rev()
        .collect()
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

/// The directory a workspace being opened on `task` should stand in.
///
/// The one seam `spawn` uses, and the reason any of this is worth building. A
/// rule an agent has to remember is a rule that gets skipped — that is written
/// down three times over on t-260815-022, about naming paths, about announcing
/// first, and about the isolation build that became `wsp verify`. So the tree
/// is not something an agent asks for; it is where the pane is opened.
///
/// `root` is a project root out of the store and comes back the same shape, `~`
/// and all, because expanding paths is the backend's job and not this one's.
/// `None` is the honest answer to "nothing to isolate here": a root that is not
/// a git repository, or a repository on a detached HEAD with no trunk to branch
/// from. The caller opens the root itself and says so.
pub(crate) fn tree_for(root: &str, task: &str) -> Option<String> {
    let root = util::expand(root);
    let repo = toplevel(&root)?;
    let trunk = trunk(&repo)?;
    let branch = trunk_branch(&trunk)?;
    let dir = checkout_dir(&trunk, task);
    // A branch already checked out in another worktree makes `add` refuse, which
    // is git declining to put two agents on one task — the same thing the claim
    // guard declines a moment later, and a good reason to fall back to the root
    // rather than to fail the spawn.
    ensure(&repo, &dir, task, &branch).ok()?;
    Some(util::contract(&dir))
}

/// Why a tree is finished with.
///
/// A distinction rather than a sentence, because the two are not equally sure
/// and [`sweep`] acts on only one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Why {
    /// The task is closed. Somebody decided the work was over, which is the
    /// only fact here strong enough to remove a directory on — and it holds
    /// however much is in the tree, because that is a fact about the work.
    Closed,
    /// Nothing uncommitted, and nothing the trunk has not got. Worth naming and
    /// never worth sweeping: it is exactly what the tree made thirty seconds ago
    /// for an agent that has not typed yet looks like.
    Idle,
}

impl Why {
    /// What to do about it — which is a different command for each, and the
    /// reason a caller reporting these has to know which one it is holding.
    ///
    /// `seated` overrides both, and it is the whole of robustness-076 in one
    /// line: while an agent is still in a seat on this task, `--rm` and
    /// `--sweep` take the tree and leave the agent, the claim and the workspace
    /// standing. Naming them there is how the ending came to be half-done every
    /// time. [`crate::cmd_spawn::despawn`] is the verb that finishes it, and it
    /// removes the tree on the way.
    pub(crate) fn fix(&self, task: &str, seated: bool) -> String {
        match (self, seated) {
            (_, true) => format!("`wsp despawn {task}`"),
            (Why::Closed, false) => "`wsp checkout --sweep`".into(),
            (Why::Idle, false) => format!("`wsp checkout {task} --rm`"),
        }
    }
}

/// A tree that is finished with: which task's, why, and how to say so.
pub(crate) struct Stale {
    pub task: String,
    pub why: Why,
    /// The reason as a reader wants it, since only this function knows the name
    /// of the trunk branch it just compared against.
    pub note: String,
}

/// The trees under `root` that are finished with, and the reason each is.
///
/// The looking half: it reads and never touches anything, and [`sweep`] acts on
/// the part of what it finds that is sure enough to act on. This is what `wsp
/// doctor` reports, rather than a cleanup step at the end of a piece of work,
/// because that is the shape the evidence asks for: `git worktree list` held
/// four abandoned trees on 2026-08-16 and three more on the day before, ~132M of
/// them, and nobody was careless — a procedure run by hand at the end of a long
/// piece of work is a procedure that gets abandoned halfway when something more
/// interesting happens.
///
/// `closed` is asked of the store by the caller rather than read here, because
/// the two reasons are different in kind and only one of them is git's: a tree
/// whose task is done is finished even if it has uncommitted work in it, and
/// that is a fact about the work rather than about the checkout.
pub(crate) fn stale(root: &Path, closed: &dyn Fn(&str) -> bool) -> Vec<Stale> {
    let Some(trunk) = trunk(root) else { return Vec::new() };
    let Some(branch) = trunk_branch(&trunk) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(trunk.join(WORKTREES)) else { return Vec::new() };

    let mut out: Vec<Stale> = Vec::new();
    for e in entries.flatten() {
        let dir = e.path();
        if !dir.join(".git").exists() {
            continue;
        }
        let Some(task) = dir.file_name().and_then(|s| s.to_str()).map(str::to_string) else {
            continue;
        };
        if closed(&task) {
            out.push(Stale { task, why: Why::Closed, note: "the task is closed".into() });
        } else if !dirty(&dir) && ahead(root, &branch, &task).is_empty() {
            let note = format!("nothing uncommitted and nothing {branch} has not got");
            out.push(Stale { task, why: Why::Idle, note });
        }
    }
    out.sort_by(|a, b| a.task.cmp(&b.task));
    out
}

/// Take a tree away, leaving the branch if it still holds work.
///
/// The one place a checkout is destroyed, so that `--rm` and [`sweep`] cannot
/// come to disagree about what removing one means. `--force` on the `worktree
/// remove` and the `remove_dir_all` after it are there because the caller has
/// already decided — whoever calls this has looked at [`dirty`] and either
/// found nothing or been told to go ahead anyway.
///
/// `-d` and not `-D` on the branch: a branch git will not delete is one holding
/// commits the trunk has not got, and that is work rather than litter. Left
/// behind, `checkout` finds it again and rebuilds the tree from it, which is
/// what makes removing a tree recoverable and reopening a task cheap.
fn remove(repo: &Path, dir: &Path, task: &str) -> Removed {
    let existed = dir.exists();
    let _ = git(repo, &["worktree", "remove", "--force", &dir.display().to_string()]);
    let _ = std::fs::remove_dir_all(dir);
    let _ = git(repo, &["worktree", "prune"]);
    let branch_kept = git(repo, &["branch", "-d", task]).is_none();
    Removed { existed, branch_kept }
}

struct Removed {
    existed: bool,
    branch_kept: bool,
}

/// What one sweep did, and what it declined to do.
#[derive(Default)]
pub(crate) struct Swept {
    /// Tasks whose trees went.
    pub removed: Vec<String>,
    /// Tasks whose branch outlived the tree because it still holds commits the
    /// trunk has not got.
    pub branches: Vec<String>,
    /// Tasks left alone, and why. Reported rather than silent: a sweep that
    /// quietly skips things is one nobody can tell from a sweep that found
    /// nothing.
    pub kept: Vec<(String, String)>,
}

/// Remove the trees whose task is closed, and say what was left standing.
///
/// The acting half of [`stale`], and deliberately narrower than it: only
/// [`Why::Closed`] is swept, because a closed task is somebody's decision and
/// an idle tree is a guess about a directory. Three further refusals, all in the
/// direction of leaving a tree that could have gone rather than removing one
/// that could not:
///
/// - `busy` is the world outside git — a claim still held, a pane standing in
///   the tree, the caller's own cwd — and comes in as a closure so the rule can
///   be tested without a store or a live herdr, the same bargain [`stale`]
///   makes with `closed`.
/// - Uncommitted work stops the removal even though a closed task means the
///   tree is finished with. What a branch holds comes back with `wsp checkout`;
///   what was never committed does not come back at all, and this is the one
///   thing here that is not recoverable.
/// - A tree whose task the store has never heard of is not swept, because the
///   sweep would then read a store pointed somewhere else as a repository full
///   of litter. The caller decides what "closed" means, and the answer for an
///   unknown id is no.
pub(crate) fn sweep(
    root: &Path,
    closed: &dyn Fn(&str) -> bool,
    busy: &dyn Fn(&str) -> Option<String>,
    dry: bool,
) -> Swept {
    let mut out = Swept::default();
    let Some(trunk) = trunk(root) else { return out };
    for s in stale(root, closed) {
        if s.why != Why::Closed {
            continue;
        }
        if let Some(why) = busy(&s.task) {
            out.kept.push((s.task, why));
            continue;
        }
        let dir = checkout_dir(&trunk, &s.task);
        if dirty(&dir) {
            out.kept.push((s.task, "uncommitted work in it".into()));
            continue;
        }
        if !dry {
            if remove(root, &dir, &s.task).branch_kept {
                out.branches.push(s.task.clone());
            }
        }
        out.removed.push(s.task);
    }
    out
}

/// What ending a piece of work did to its tree.
///
/// Three answers rather than a boolean, because the reader of a `despawn` has
/// to be able to tell the tree that was never there from the one that is still
/// standing — and the second is the only one that needs them to do something.
/// Reporting them the same way is the fault this whole verb was written
/// against: a step that says it finished when it did nothing.
pub(crate) enum Tree {
    /// No tree for this task in any repository it could be in. The ordinary
    /// answer for work that never took a checkout.
    Absent,
    /// Gone, and whether the branch outlived it.
    Removed { path: String, branch_kept: bool },
    /// Left standing, and why — always a reason the caller can act on.
    Kept { path: String, why: String },
}

/// Take away the tree for `task`, if there is one and nothing says not to.
///
/// The other end of [`tree_for`]: `checkout` puts the tree there when work is
/// placed on a task, and this takes it away when the work is ended. Both are
/// called by `spawn` and `despawn` rather than by the agent, for the reason
/// [`tree_for`] gives — a cleanup step an agent has to remember is one that is
/// skipped, and the evidence on robustness-076 is eighteen trees left standing
/// after a single overnight batch.
///
/// The refusals are `--rm`'s, and deliberately not the sweep's. Uncommitted
/// work is the one thing removing a tree destroys for good, so it stops this;
/// commits the trunk has not got do *not*, because [`remove`] leaves the branch
/// behind and `wsp checkout` builds the tree again from it. Nothing here is
/// overridable — a caller who wants a dirty tree gone can say so where saying
/// it is the whole point of the command, `wsp checkout <id> --rm --force`.
///
/// `standing` is the world outside git — the caller's own cwd, a pane in the
/// tree — and comes in as a closure for the reason [`sweep`]'s `busy` does: the
/// rule is worth testing and a live herdr is not worth needing in order to test
/// it.
pub(crate) fn discard(
    repos: Vec<PathBuf>,
    task: &str,
    standing: &dyn Fn(&Path) -> Option<String>,
) -> Tree {
    // No usable repository is no tree, not an error: `pick` only fails when
    // there was nowhere to look, and nowhere to look is nothing to remove.
    let Ok(w) = pick(repos, task) else { return Tree::Absent };
    if !w.dir.join(".git").exists() {
        return Tree::Absent;
    }
    let path = util::contract(&w.dir);
    if let Some(why) = standing(&w.dir) {
        return Tree::Kept { path, why };
    }
    if dirty(&w.dir) {
        return Tree::Kept {
            path,
            why: format!("uncommitted work in it — `wsp checkout {task} --rm --force` to lose it"),
        };
    }
    Tree::Removed { path, branch_kept: remove(&w.repo, &w.dir, task).branch_kept }
}

/// Whether a tree has anything uncommitted, tracked or not.
fn dirty(dir: &Path) -> bool {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// The commits on `branch` that the trunk does not have, newest first.
///
/// `pub(crate)` for the worklist barrier, which asks this same question of
/// every member of a group and opens on the answer. Nothing about it changed
/// to leave the module — but note what it cannot tell a caller from here: a
/// branch that does not exist is not an error to `git log`, so the answer for
/// one is the same empty list as for one that has landed. `land` and [`stale`]
/// both hold a tree, so a branch is a given for them;
/// [`crate::worklist::Landing`] does not and asks first.
pub(crate) fn ahead(repo: &Path, trunk_branch: &str, branch: &str) -> Vec<String> {
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
    /// Every trunk a tree for this task was looked for under, in the order
    /// tried. Carried on the answer rather than dropped because the message it
    /// is wanted for is the one printed when the search found nothing.
    looked: Vec<PathBuf>,
}

impl Where {
    /// Where wsp looked, as a reader wants to read it.
    fn searched(&self) -> String {
        self.looked.iter().map(|t| util::contract(t)).collect::<Vec<_>>().join(" or ")
    }
}

/// The repositories a tree for `task` could be in, in the order to try them.
///
/// Two facts, and they disagree exactly when it matters. Where the caller is
/// standing is what an agent means: it is inside the tree it is asking about.
/// Where the store says the task's project is worked is what everybody else
/// means, and a governor cleaning up after a lane is never standing in it.
///
/// Deduplicated through [`util::real`], because the overwhelmingly common case
/// is the two agreeing and neither the search nor the "where I looked" message
/// should say the same repository twice.
pub(crate) fn candidates(store: &Store, cwd: &Path, task: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |repo: Option<PathBuf>| {
        let Some(repo) = repo else { return };
        let real = util::real(&repo.display().to_string());
        if !out.iter().any(|x| util::real(&x.display().to_string()) == real) {
            out.push(repo);
        }
    };
    add(toplevel(cwd));
    let root = store
        .task(task)
        .and_then(|t| t.project)
        .and_then(|p| crate::resolve::Index::new(store.projects()).root_of(&p))
        .map(|r| util::expand(&r));
    add(root.as_deref().and_then(toplevel));
    out
}

/// The first candidate that already holds this task's tree — or, when none
/// does, the first that could hold one.
///
/// The two halves of that sentence are the whole design. Searching every
/// candidate is what stops `--rm` reporting success on its own arithmetic while
/// a tree stands in the next repository along. Falling back to the *first* is
/// what keeps `checkout` making the tree where you are standing, which is the
/// only way to say which repository you meant when a task is worked in two.
///
/// A candidate that is not a usable repository is skipped rather than fatal,
/// and its reason is kept for the case where no candidate works out: a stale
/// project root should not stop a caller who is standing in the right place.
fn pick(repos: Vec<PathBuf>, task: &str) -> Result<Where, String> {
    let mut looked: Vec<PathBuf> = Vec::new();
    let mut fallback: Option<Where> = None;
    let mut refused: Option<String> = None;

    for repo in repos {
        let Some(trunk) = trunk(&repo) else {
            refused.get_or_insert_with(|| {
                format!("cannot find the main working tree of {}", util::contract(&repo))
            });
            continue;
        };
        let Some(branch) = trunk_branch(&trunk) else {
            refused.get_or_insert_with(|| {
                format!("{} is on a detached HEAD — there is no trunk to branch from", util::contract(&trunk))
            });
            continue;
        };
        looked.push(trunk.clone());
        let dir = checkout_dir(&trunk, task);
        let found = dir.join(".git").exists();
        let w = Where { repo, trunk, branch, task: task.to_string(), dir, looked: Vec::new() };
        if found {
            return Ok(Where { looked, ..w });
        }
        fallback.get_or_insert(w);
    }

    match fallback {
        Some(w) => Ok(Where { looked, ..w }),
        None => Err(refused.unwrap_or_else(|| "no repository to look in".to_string())),
    }
}

fn locate(store: &Store, args: &Args) -> Result<Where, String> {
    let cwd = std::env::current_dir().map_err(|_| "cannot read the current directory".to_string())?;
    // The task first, because it is what says which repositories to consider.
    let task = task_of(store, args)?;
    let repos = candidates(store, &cwd, &task);
    if repos.is_empty() {
        return Err(format!(
            "{} is not in a git repository, and no project root says where {task} is worked",
            util::contract(&cwd)
        ));
    }
    pick(repos, &task)
}

pub fn checkout(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    // Before `locate`, which wants a task: the sweep is about every tree in the
    // repository and is exactly the command you reach for holding none.
    if args.has("sweep") {
        return swept(store, args);
    }
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
        // Uncommitted work is the one thing removing a tree destroys for good:
        // the branch carries every commit back when the task is picked up
        // again, and carries nothing that was never committed. This mattered
        // less when `land` did the removing, because `land` refused to run on a
        // dirty tree at all; now that `--rm` is the only way a tree goes, the
        // refusal has to live here.
        if dirty(&w.dir) && !args.has("force") {
            eprintln!(
                "wsp: {} has uncommitted work — commit it, or `--force` to lose it",
                util::contract(&w.dir)
            );
            return 2;
        }
        let r = remove(&w.repo, &w.dir, &w.task);
        if args.json() {
            println!(
                "{}",
                json!({
                    "removed": r.existed,
                    "branch_kept": r.branch_kept,
                    "path": util::contract(&w.dir),
                    "looked": w.looked.iter().map(|t| util::contract(t)).collect::<Vec<_>>(),
                })
            );
        } else if !r.existed {
            // Two lines for the answer nobody acts on, because the one line it
            // used to be read as "there was nothing to do" and the reader had
            // no reason to look again. Naming the repositories searched turns
            // it into a fact somebody can check, and the hint points at the one
            // place that knows about a tree wsp does not: git itself.
            println!("no tree for {} under {}", w.task, w.searched());
            println!(
                "{}",
                p.dim("nothing removed — if it was worked in another repository, `git worktree list` there names it")
            );
        } else {
            println!("removed {}", util::contract(&w.dir));
            if r.branch_kept {
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

/// `wsp checkout --sweep` — the trees nobody ran `--rm` on.
///
/// The repository the caller is standing in, rather than every declared project
/// root: this removes directories, and the blast radius of a command that does
/// that should be the tree you can see. `wsp doctor` is the half that looks
/// everywhere and only reports.
///
/// `-n` prints the same list and touches nothing, which is what makes the first
/// run of a removing command something a person can bring themselves to type.
fn swept(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let dry = args.has("dry-run");
    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("wsp: cannot read the current directory");
        return 2;
    };
    let Some(repo) = toplevel(&cwd) else {
        eprintln!("wsp: {} is not in a git repository", util::contract(&cwd));
        return 2;
    };
    let Some(trunk) = trunk(&repo) else {
        eprintln!("wsp: cannot find the repository's main working tree");
        return 2;
    };

    let closed = finished(store);
    let here = util::real(&cwd.display().to_string());
    let claims = store.claims();
    // Where the panes are standing is herdr's to answer, and a herdr that is not
    // there answers nothing — so the sweep falls back to the two facts it can
    // read without one, its own cwd and the claims. Both refuse in the safe
    // direction, and neither can be made wrong by a socket being down.
    let panes = if crate::herdr::available() { crate::herdr::panes().unwrap_or_default() } else { Vec::new() };
    let busy = |task: &str| -> Option<String> {
        let dir = util::real(&checkout_dir(&trunk, task).display().to_string());
        if here.starts_with(&dir) {
            return Some("you are standing in it".into());
        }
        if let Some(pane) = panes.iter().find(|x| util::real(&x.cwd).starts_with(&dir)) {
            return Some(format!("{} is standing in it", pane.pane_id));
        }
        // A claim on a closed task is unusual and it is still somebody: the
        // agent that finished the work and has not let go of it yet.
        claims.contains_key(task).then(|| "still claimed".to_string())
    };

    let out = sweep(&repo, &closed, &busy, dry);

    if args.json() {
        println!(
            "{}",
            json!({
                "removed": out.removed,
                "branches_kept": out.branches,
                "kept": out.kept.iter().map(|(t, w)| json!({"task": t, "why": w})).collect::<Vec<_>>(),
                "dry_run": dry,
            })
        );
        return 0;
    }

    for task in &out.removed {
        let path = util::contract(&checkout_dir(&trunk, task));
        println!("{} {path}", if dry { p.dim("would remove") } else { p.dim("removed") });
    }
    for task in &out.branches {
        println!("{}", p.yellow(&format!("branch {task} kept — it has commits the trunk has not")));
    }
    for (task, why) in &out.kept {
        println!("{} {task} — {why}", p.dim("kept"));
    }
    if out.removed.is_empty() && out.kept.is_empty() {
        println!("{}", p.dim("no tree here belongs to a finished task"));
    }
    0
}

/// Whether the store considers a task finished — done, or archived out of the
/// store entirely.
///
/// One definition, because `doctor` reporting a tree as litter and the sweep
/// removing it have to be answering the same question. Archived counts: the
/// task file has moved to `archive/tasks/`, so a tree named after it would
/// otherwise be invisible to both — a leak that grows precisely with how long
/// the arrangement has been running.
///
/// An id the store has never heard of is *not* finished. That is the answer
/// that keeps a store pointed somewhere else — `WSP_HOME` in a sandbox, a fresh
/// checkout with no store at all — from reading as a repository full of litter.
pub(crate) fn finished(store: &Store) -> impl Fn(&str) -> bool {
    let tasks = store.tasks();
    let archived = store.archived_ids();
    move |id: &str| {
        tasks
            .iter()
            .find(|t| t.id == id)
            .map(|t| !t.status().is_open())
            .unwrap_or_else(|| archived.iter().any(|a| a == id))
    }
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
        eprintln!("wsp: no tree for {} under {} — nothing to land", w.task, w.searched());
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

    // Nothing is removed here — see the module docs. The tree is left exactly
    // where it was, on its own branch, level with the trunk it just landed on,
    // ready for the next commit or for the same task being picked up again next
    // week. `wsp checkout --rm` ends it when the work is genuinely over.

    if args.json() {
        println!(
            "{}",
            json!({
                "task": w.task,
                "into": w.branch,
                "commits": landed,
                "files": files,
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
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A directory to stand a repository in, and an empty store beside it:
    /// [`checkout_dir`] resolves a task's former ids out of the *ambient*
    /// store, so every test here reads whatever the developer has renamed
    /// today unless it is pointed somewhere else. See
    /// [`crate::util::isolated`], and note that one test takes one — a
    /// second on the same thread waits for the first.
    fn scratch(name: &str) -> (util::Isolated, PathBuf) {
        let env = util::isolated(&format!("co-{name}"));
        let dir = env.path("repo");
        std::fs::create_dir_all(&dir).unwrap();
        (env, dir)
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
        let (_env, dir) = scratch("land");
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
    }

    /// A tree is a checkout, so the trunk's uncommitted work is not in it. This
    /// is the sweep made impossible rather than merely warned about: `git add`
    /// in one tree cannot see the other tree's files at all.
    #[test]
    fn another_agents_uncommitted_work_is_not_in_this_tree() {
        let (_env, dir) = scratch("isolated");
        repo(&dir);
        std::fs::write(dir.join("theirs.txt"), "theirs\n").unwrap();

        let wt = checkout_dir(&dir, "t-2");
        ensure(&dir, &wt, "t-2", "master").unwrap();
        assert!(!wt.join("theirs.txt").exists(), "another agent's file was in this tree");
        assert!(!dirty(&wt), "a fresh tree was already dirty");
    }

    /// Coming back to a task finds the commits it left, rather than a fresh
    /// branch — an agent releases a task and picks it up again all the time.
    #[test]
    fn coming_back_to_a_task_finds_the_branch_it_left() {
        let (_env, dir) = scratch("resume");
        repo(&dir);
        let wt = checkout_dir(&dir, "t-3");
        ensure(&dir, &wt, "t-3", "master").unwrap();
        std::fs::write(wt.join("wip.txt"), "wip\n").unwrap();
        run(&wt, &["add", "wip.txt"]);
        run(&wt, &["commit", "--quiet", "-m", "wip"]);

        git_ok(&dir, &["worktree", "remove", &wt.display().to_string()]).unwrap();
        assert!(ensure(&dir, &wt, "t-3", "master").unwrap(), "the tree was not remade");
        assert!(wt.join("wip.txt").exists(), "the work left on the branch did not come back");
    }

    /// The leak this arrangement adds, found by something that runs anyway.
    ///
    /// Every version of the per-worktree rule so far has leaked trees, and none
    /// of the leaks was carelessness — a cleanup step at the end of a long
    /// piece of work is a step that gets abandoned when something more
    /// interesting happens. So the tell is a tree with nothing in it that the
    /// trunk has not already got, and a tree with real work in it is left
    /// alone however long it has stood there.
    #[test]
    fn a_tree_with_nothing_left_in_it_is_reported_and_one_with_work_is_not() {
        let (_env, dir) = scratch("stale");
        repo(&dir);
        let never_closed = |_: &str| false;

        let idle = checkout_dir(&dir, "t-idle");
        ensure(&dir, &idle, "t-idle", "master").unwrap();
        let busy = checkout_dir(&dir, "t-busy");
        ensure(&dir, &busy, "t-busy", "master").unwrap();
        std::fs::write(busy.join("wip.txt"), "wip\n").unwrap();
        run(&busy, &["add", "wip.txt"]);
        run(&busy, &["commit", "--quiet", "-m", "wip"]);

        let found = stale(&dir, &never_closed);
        assert_eq!(found.len(), 1, "wrong trees named: {:?}", names(&found));
        assert_eq!(found[0].task, "t-idle");
        assert_eq!(found[0].why, Why::Idle);

        // Uncommitted work counts as work: a tree is not litter because git has
        // not been told about what is in it yet.
        std::fs::write(idle.join("scratch.txt"), "not yet\n").unwrap();
        assert!(stale(&dir, &never_closed).is_empty(), "a tree with unsaved work was called finished");

        // A closed task ends its tree whatever is in it — the work is over, and
        // the tree outliving it is the leak.
        let closed = stale(&dir, &|id: &str| id == "t-busy");
        assert_eq!(closed[0].task, "t-busy");
        assert_eq!(closed[0].why, Why::Closed, "a closed task's tree was named for the wrong reason");
    }

    /// A repository with a project rooted at it, and a task in that project.
    fn lane(env: &util::Isolated, name: &str, task: &str) -> PathBuf {
        let dir = env.path(name);
        std::fs::create_dir_all(&dir).unwrap();
        repo(&dir);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        let mut p = crate::model::Project::new(name);
        p.roots = vec![dir.display().to_string()];
        store.save_project(&p).unwrap();
        let mut t = crate::model::Task::new("a task", task);
        t.project = Some(name.to_string());
        store.save_task(&t).unwrap();
        dir
    }

    /// The failure this whole search exists for: the caller is standing in one
    /// repository and the tree is in the one the task's project is worked in.
    ///
    /// A governor is always in this position — it sits in its project's seat and
    /// cleans up after lanes rooted elsewhere — so the old answer, computed from
    /// cwd alone, was `no tree` for a tree that was plainly on disk. Twice on
    /// 2026-08-18, and silently, because that sentence is what a repository with
    /// nothing to clean up says too.
    #[test]
    fn a_tree_in_the_tasks_own_repository_is_found_from_a_different_one() {
        let (env, seat) = scratch("cross-repo");
        repo(&seat);
        let lane = lane(&env, "lane", "lane-1");
        let store = Store::open();

        // Where spawn would have put it: under the project's root, not the seat.
        ensure(&lane, &checkout_dir(&lane, "lane-1"), "lane-1", "master").unwrap();

        let repos = candidates(&store, &seat, "lane-1");
        assert_eq!(repos.len(), 2, "the task's own repository was not a candidate: {repos:?}");

        let w = pick(repos, "lane-1").expect("a tree that is on disk is findable");
        let want = util::real(&checkout_dir(&lane, "lane-1").display().to_string());
        assert_eq!(util::real(&w.dir.display().to_string()), want, "the search answered for the wrong repository");
        assert_eq!(
            util::real(&w.repo.display().to_string()),
            util::real(&lane.display().to_string()),
            "the tree was found but the repository it lives in was not"
        );
        assert_eq!(w.looked.len(), 2, "the seat was not searched first");
    }

    /// Widening the search must not move where a tree is made, because a task
    /// can be worked in two repositories at once — `fork-006` had a tree in each
    /// on the day this was written — and standing in one is the only way to say
    /// which is meant. So the tree you are standing next to wins, and when
    /// neither has one the first candidate is still where `checkout` will make
    /// it.
    #[test]
    fn standing_in_a_repository_still_decides_which_one_gets_the_tree() {
        let (env, seat) = scratch("cwd-wins");
        repo(&seat);
        let lane = lane(&env, "lane", "lane-2");
        let store = Store::open();

        // Neither has a tree yet: the answer is the repository the caller is in,
        // which is the one `checkout` is about to make it in.
        let repos = candidates(&store, &seat, "lane-2");
        let w = pick(repos.clone(), "lane-2").expect("somewhere to make it");
        assert_eq!(
            util::real(&w.repo.display().to_string()),
            util::real(&seat.display().to_string()),
            "a tree with no home was placed away from the caller"
        );
        assert_eq!(w.searched().matches(" or ").count(), 1, "the failure message names one place, not both");

        // And with a tree in both, still the one the caller is standing in.
        ensure(&seat, &checkout_dir(&seat, "lane-2"), "lane-2", "master").unwrap();
        ensure(&lane, &checkout_dir(&lane, "lane-2"), "lane-2", "master").unwrap();
        let w = pick(repos, "lane-2").expect("a tree in both");
        assert_eq!(
            util::real(&w.dir.display().to_string()),
            util::real(&checkout_dir(&seat, "lane-2").display().to_string()),
            "the caller's own tree lost to the project root's"
        );
    }

    fn names(found: &[Stale]) -> Vec<&str> {
        found.iter().map(|s| s.task.as_str()).collect()
    }

    /// Landing puts commits on the trunk and does nothing else to the tree it
    /// took them from.
    ///
    /// It used to remove it, which deleted its own caller's cwd the first time
    /// anybody ran it (t-260817-018) and cost a fresh checkout and a cold
    /// `target/` every time work resumed on a task. So the tree is still there
    /// afterwards, still on its branch, and landing again from it is an ordinary
    /// thing to do rather than a rebuild.
    #[test]
    fn a_landed_tree_is_still_standing_and_lands_again() {
        let (_env, dir) = scratch("relanding");
        repo(&dir);
        let wt = checkout_dir(&dir, "t-6");
        ensure(&dir, &wt, "t-6", "master").unwrap();

        for n in ["one", "two"] {
            std::fs::write(wt.join(format!("{n}.txt")), n).unwrap();
            run(&wt, &["add", "."]);
            run(&wt, &["commit", "--quiet", "-m", n]);
            // What `land` does, and now the whole of it.
            git_ok(&wt, &["rebase", "master"]).unwrap();
            git_ok(&dir, &["merge", "--ff-only", "--quiet", "t-6"]).unwrap();

            assert!(wt.join(".git").exists(), "the tree did not survive landing {n}");
            assert!(!dirty(&wt), "landing left the tree it landed from dirty");
            assert!(ahead(&dir, "master", "t-6").is_empty(), "the branch is still ahead after landing");
            assert!(dir.join(format!("{n}.txt")).exists(), "{n} did not reach the trunk");
        }
    }

    /// The sweep is the acting half of the report, and it is narrower on
    /// purpose: only a closed task's tree goes, and only when nothing and
    /// nobody in it would be lost with it.
    #[test]
    fn the_sweep_takes_a_closed_tasks_tree_and_leaves_everything_it_cannot_prove() {
        let (_env, dir) = scratch("sweep");
        repo(&dir);

        // Four trees, one reason each to be here.
        for t in ["t-done", "t-open", "t-held", "t-messy"] {
            ensure(&dir, &checkout_dir(&dir, t), t, "master").unwrap();
        }
        std::fs::write(checkout_dir(&dir, "t-messy").join("draft.txt"), "not committed\n").unwrap();

        let closed = |id: &str| id != "t-open";
        let busy = |id: &str| (id == "t-held").then(|| "you are standing in it".to_string());

        // `-n` says the same thing and touches nothing, which is what makes the
        // first run of a removing command typeable.
        let looked = sweep(&dir, &closed, &busy, true);
        assert_eq!(looked.removed, ["t-done"], "the dry run named the wrong trees");
        assert!(checkout_dir(&dir, "t-done").join(".git").exists(), "-n removed a tree");

        let out = sweep(&dir, &closed, &busy, false);
        assert_eq!(out.removed, ["t-done"]);
        assert!(!checkout_dir(&dir, "t-done").exists(), "the finished tree is still here");
        for t in ["t-open", "t-held", "t-messy"] {
            assert!(checkout_dir(&dir, t).join(".git").exists(), "{t} was swept and should not have been");
        }
        let kept: Vec<&str> = out.kept.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(kept, ["t-held", "t-messy"], "a skipped tree went unreported: {:?}", out.kept);
        assert!(out.kept[1].1.contains("uncommitted"), "the reason did not name the work at risk");
    }

    /// A swept tree is recoverable and an unswept one has to be: the branch
    /// outlives the directory, so a task reopened after its tree went finds its
    /// commits again — and the branch is only kept when it holds some.
    #[test]
    fn the_sweep_keeps_the_branch_when_it_still_holds_work() {
        let (_env, dir) = scratch("sweep-branch");
        repo(&dir);
        let wt = checkout_dir(&dir, "t-7");
        ensure(&dir, &wt, "t-7", "master").unwrap();
        std::fs::write(wt.join("unlanded.txt"), "mine\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", "never landed"]);

        let out = sweep(&dir, &|_| true, &|_| None, false);
        assert_eq!(out.removed, ["t-7"]);
        assert_eq!(out.branches, ["t-7"], "the branch went with the tree, and the commit with it");

        ensure(&dir, &wt, "t-7", "master").unwrap();
        assert!(wt.join("unlanded.txt").exists(), "reopening the task did not get the work back");
    }

    /// The other end of that seam, and the whole of robustness-076: ending the
    /// work takes the tree away, and says so in a way that can be told from
    /// having done nothing.
    ///
    /// Three answers rather than a boolean, because the leak was never a
    /// missing removal — it was three commands where only the first was a verb
    /// anybody ran, and steps that reported success they had not achieved. A
    /// `despawn` that prints "removed" over a tree still on disk is worse than
    /// one that never touched it.
    #[test]
    fn ending_the_work_takes_the_tree_and_says_which_of_the_three_it_did() {
        let (_env, dir) = scratch("discard");
        repo(&dir);
        let nobody = |_: &Path| None;

        // Nothing to remove is its own answer, and it is the common one: most
        // endings are of work that never took a checkout.
        assert!(matches!(discard(vec![dir.clone()], "t-none", &nobody), Tree::Absent));

        let wt = checkout_dir(&dir, "t-1");
        ensure(&dir, &wt, "t-1", "master").unwrap();
        match discard(vec![dir.clone()], "t-1", &nobody) {
            Tree::Removed { branch_kept, .. } => assert!(!branch_kept, "a branch level with the trunk was kept"),
            other => panic!("an idle tree survived the ending: {}", named(&other)),
        }
        assert!(!wt.exists(), "the directory is still there after a Removed");
    }

    /// The two refusals, and they are `--rm`'s rather than the sweep's.
    ///
    /// Uncommitted work is the one thing removing a tree destroys for good, so
    /// it stops this and the reason names the command that overrides it.
    /// Somebody standing in the tree stops it too — two agents in one tree is
    /// ordinary here, and the second one must not have the floor taken out from
    /// under it because the first was despawned.
    #[test]
    fn a_tree_with_work_in_it_or_somebody_in_it_is_kept_and_the_reason_says_which() {
        let (_env, dir) = scratch("discard-kept");
        repo(&dir);

        let wt = checkout_dir(&dir, "t-2");
        ensure(&dir, &wt, "t-2", "master").unwrap();
        std::fs::write(wt.join("half-done.txt"), "not committed\n").unwrap();
        match discard(vec![dir.clone()], "t-2", &|_| None) {
            Tree::Kept { why, .. } => {
                assert!(why.contains("uncommitted"), "the reason did not name the work at risk: {why}");
                assert!(why.contains("--force"), "and gave no way through: {why}");
            }
            other => panic!("uncommitted work was thrown away: {}", named(&other)),
        }
        assert!(wt.join("half-done.txt").exists(), "the file went anyway");

        // Committed, so nothing is at risk — and it still stays, because
        // somebody is in it. The closure is asked about the tree that was
        // found, which is what lets the caller answer for a path it never
        // computed itself.
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", "done"]);
        let occupied = |d: &Path| {
            assert!(d.ends_with("t-2"), "asked about the wrong tree: {d:?}");
            Some("w9:p1 is standing in it".into())
        };
        match discard(vec![dir.clone()], "t-2", &occupied) {
            Tree::Kept { why, .. } => assert!(why.contains("w9:p1"), "the reason did not name who: {why}"),
            other => panic!("a tree was removed from under somebody: {}", named(&other)),
        }
        assert!(wt.exists());
    }

    /// A tree removed with commits the trunk has not got is recoverable, and the
    /// caller is told so — the branch outlives the directory, which is what
    /// makes ending a piece of work cheap to undo. Same guarantee [`sweep`]
    /// gives, asserted here because `despawn` reaches it by a different route.
    #[test]
    fn ending_the_work_keeps_the_branch_when_it_still_holds_commits() {
        let (_env, dir) = scratch("discard-branch");
        repo(&dir);
        let wt = checkout_dir(&dir, "t-3");
        ensure(&dir, &wt, "t-3", "master").unwrap();
        std::fs::write(wt.join("unlanded.txt"), "mine\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", "never landed"]);

        match discard(vec![dir.clone()], "t-3", &|_| None) {
            Tree::Removed { branch_kept, .. } => assert!(branch_kept, "the commits were not recoverable"),
            other => panic!("{}", named(&other)),
        }
        ensure(&dir, &wt, "t-3", "master").unwrap();
        assert!(wt.join("unlanded.txt").exists(), "reopening the task did not get the work back");
    }

    /// Which of the three came back, for a panic message that names it.
    fn named(t: &Tree) -> String {
        match t {
            Tree::Absent => "Absent — no tree was found at all".into(),
            Tree::Removed { path, .. } => format!("Removed {path}"),
            Tree::Kept { path, why } => format!("Kept {path} — {why}"),
        }
    }

    /// The seam `spawn` opens a workspace through. A task gets a tree; a root
    /// that is not a repository gets nothing and the caller opens the root, so
    /// spawning into a project that is not under git goes on working.
    #[test]
    fn a_spawn_onto_a_task_is_given_a_tree_and_a_spawn_into_nothing_is_not() {
        let (env, dir) = scratch("spawn");
        repo(&dir);
        let root = dir.display().to_string();

        let placed = tree_for(&root, "t-5").expect("a task in a repository gets a tree");
        assert!(placed.ends_with(".worktrees/t-5"), "opened somewhere else: {placed}");
        assert!(checkout_dir(&dir, "t-5").join(".git").exists(), "the path is not a checkout");

        let bare = env.path("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(tree_for(&bare.display().to_string(), "t-5"), None, "a directory git does not know");
    }

    /// `overlap` asks this about every pane herdr reports, so it has to be a
    /// path rule and it has to get the trunk case right: a worktree nested under
    /// the root is *not* inside the trunk, however much the prefix says so.
    #[test]
    fn a_worktree_under_the_root_is_a_tree_of_its_own() {
        // Paths only, and it still needs a store of its own: `checkout_dir`
        // asks one which ids `t-1` used to have.
        let _env = util::isolated("co-worktree-of");
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
        let (_env, dir) = scratch("trunk");
        repo(&dir);
        let wt = checkout_dir(&dir, "t-4");
        ensure(&dir, &wt, "t-4", "master").unwrap();
        assert_eq!(trunk(&wt).map(|p| util::real(&p.display().to_string())), Some(util::real(&dir.display().to_string())));
        assert_eq!(trunk_branch(&dir).as_deref(), Some("master"));
        assert_eq!(trunk_branch(&wt).as_deref(), Some("t-4"), "a tree is on its own branch");
    }
}
