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
//! robustness-010. Naming explicit paths does not help when the file is genuinely
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
//! ever run deleted its own caller's cwd out from under it (robustness-047), and
//! that failure is now unreachable rather than handled. Removing is `checkout
//! --rm`, run when a task is genuinely finished, which is a moment somebody
//! decides rather than a side effect of a verb they run all day. And because a
//! command somebody has to remember is a command that gets forgotten — which is
//! the lesson of every leak recorded on robustness-010 — there is [`sweep`], the
//! same shape as `wsp archive` for tasks and `wsp reconcile --reap` for claims,
//! and it exists for the same reason both of those do.
//!
//! [`stale`] names three kinds of finished-with tree and [`sweep`] acts on two
//! of them, and which two is the whole of the judgement. A tree whose *task is
//! closed* goes, because somebody decided the work was over. A tree that is
//! merely clean and level with the trunk — [`Why::Idle`] — is reported forever
//! and never swept: it is indistinguishable from the tree made thirty seconds
//! ago for an agent who has not typed yet.
//!
//! # And the leak the leak leaves
//!
//! Everything above is about *directories*, because [`stale`] reads
//! `.worktrees`. Taking a tree away does not always take its branch — [`remove`]
//! keeps one git refuses to delete, which is what makes the removal recoverable
//! — so these verbs have been creating litter of their own that no reader wsp
//! has could see. Seven refs in `~/claude/wsp` on 2026-08-20, against eight live
//! trees.
//!
//! [`strays`] is that half, and it splits the same way. A branch whose commits
//! are all on the trunk is a name and nothing else, and [`sweep`] takes it with
//! `git branch -d` — git's own refusal is the guard, the same one [`remove`]
//! leans on. A branch still holding commits is **work**, and the only thing
//! wrong with it was that nothing said so: the sweep that made it named it once
//! on stdout and nothing has mentioned it since. Now `doctor` does, which is
//! the difference between a fact and a fact somebody reads.
//!
//! # The third reason, and it is evidence rather than a relaxed rule
//!
//! Eighteen worktrees and nineteen orphaned workspaces accumulated in a single
//! night, because everything an agent works on here sits at `review` by design
//! — `done` is Ed's — so the sweep's one reason never arrives and every one of
//! those trees reads as `Idle`. The refusal above is still right; what was
//! missing was a fact.
//!
//! A **worklist** has it. A member of a group a worklist has declared finished
//! is not a tree nobody has typed in: something put an agent on it, and its
//! work is on the trunk. So [`Why::Landed`] is `Idle` **plus that evidence** —
//! a strict subset of it, narrower and not broader — and it is the third thing
//! [`sweep`] acts on. The evidence arrives as a closure, the way `closed` does,
//! because it is not git's to answer: [`crate::worklist::passed_by_running`]
//! for a directory walk, and [`crate::worklist::sweep`] at a barrier, where it
//! is the same fact that opened the barrier and so costs nothing twice.
//!
//! That is the only place in this arrangement where directories are destroyed
//! on a predicate rather than on somebody's decision, so the refusals are the
//! point: uncommitted work stops it, and a member still holding its claim is
//! named with the `despawn` to run rather than swept out from under an agent
//! that is still in the room.
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
//! The third question robustness-010 carried. [`crate::resolve::Index::project_for_cwd`]
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

use std::collections::BTreeSet;
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

/// A branch left behind by a renumbering, still holding work.
///
/// Named and never taken. [`ensure`] cuts the tree it was asked for regardless;
/// this is the sentence that goes with it.
struct Orphan {
    /// A former id of the task, which is the branch its work is on.
    branch: String,
    /// How many commits it has that the trunk has not. Never zero — a branch
    /// with nothing outstanding is not worth a reader's attention, and saying
    /// so about every renumbered task would train them past the line that
    /// matters.
    commits: usize,
}

/// What making a tree found.
///
/// Two answers rather than one because the second is a *finding* and not a
/// result: the tree was made either way, and [`Orphan`] is what nobody was
/// looking at.
struct Made {
    /// Whether the tree was new.
    fresh: bool,
    orphan: Option<Orphan>,
}

/// The commits a former id's branch holds that the trunk has not, if there is
/// one and it holds any.
///
/// The looking half of `worklist-027`, and the whole of it: **adopting work is
/// asked for, never automatic** — the symmetric rule to `robustness-090` d1's
/// *destroying a record is asked for, never automatic*, and settled on this
/// task by two seats independently. A former id's branch may be work somebody
/// deliberately abandoned, and a task refiled because its first attempt was
/// wrong is a *common* reason to renumber, so reviving it under the new id
/// would put commits nobody chose to carry forward into a diff nobody expects.
/// A lost branch is discovered; a silently adopted one is discovered later.
///
/// So the answer is a sentence and never an action. [`checkout_dir`] resolves
/// a renumbered task's *directory* through the store and can be silent about
/// it because it cannot be wrong — there is one tree, it is where the work is,
/// and reusing it loses nothing. A branch is ambiguous in exactly the way a
/// directory is not, and nothing in the store records which kind it is.
///
/// Only paid for when a tree is being made, and only past the first line for a
/// task that has actually been renumbered: [`former_ids`] is empty for every
/// task in a store that has never renamed anything, and that store has no file
/// at all.
fn orphan(repo: &Path, onto: &str, task: &str) -> Option<Orphan> {
    former_ids(task).into_iter().find_map(|branch| {
        // Asked before it is compared, because `ahead()` on a branch that does
        // not exist answers the same empty list as one that has landed — the
        // conflation `worklist-002` named and `worklist-025` chased through
        // four call sites. Here it would silently drop the report.
        git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])?;
        let commits = ahead(repo, onto, &branch).len();
        (commits > 0).then_some(Orphan { branch, commits })
    })
}

/// Make this task's tree if it has none.
///
/// The branch is created at the trunk's tip on first use and reused after, so
/// an agent that comes back to a task after a `wsp release` finds its own
/// commits rather than a fresh start.
///
/// A renumbered task whose tree was swept is the one case where that promise
/// fails, because the commits are on a branch of the id it had then and this
/// cuts a fresh one at the trunk tip. It still does — see [`orphan`] for why
/// adopting the old branch is the worse answer — but it no longer does it
/// silently, and the finding rides out on [`Made`] rather than being printed
/// from here, because two callers want it in two shapes.
fn ensure(repo: &Path, dir: &Path, task: &str, from: &str) -> Result<Made, String> {
    // Before the early return, because the guard is a property of the
    // repository rather than of this tree: a second agent arriving at a tree
    // that already exists is exactly who it is for. See `crate::guard` for why
    // it is installed here and not asked for by a verb.
    crate::guard::ensure(repo);
    if dir.join(".git").exists() {
        return Ok(Made { fresh: false, orphan: None });
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
    // Looked for on both arms, and not only where a fresh branch is cut. A
    // task renumbered after some of its work had already landed under the new
    // id has both branches, and the old one is no less invisible for that.
    Ok(Made { fresh: true, orphan: orphan(repo, from, task) })
}

/// The directory a workspace being opened on `task` should stand in.
///
/// The one seam `spawn` uses, and the reason any of this is worth building. A
/// rule an agent has to remember is a rule that gets skipped — that is written
/// down three times over on robustness-010, about naming paths, about announcing
/// first, and about the isolation build that became `wsp verify`. So the tree
/// is not something an agent asks for; it is where the pane is opened.
///
/// `root` is a project root out of the store and comes back the same shape, `~`
/// and all, because expanding paths is the backend's job and not this one's.
/// `None` is the honest answer to "nothing to isolate here": a root that is not
/// a git repository, or a repository on a detached HEAD with no trunk to branch
/// from. The caller opens the root itself and says so.
///
/// The two lines this prints are [`ensure`]'s finding rather than this
/// function's business, and they are printed **here** rather than returned
/// because this is the seam every tree that is not a `wsp checkout` is made
/// through, and a finding each caller has to remember to report is a finding
/// that goes unreported — robustness-010's lesson about rules an agent has to
/// remember, applied to a caller. See [`orphan`] for what is being said and
/// why nothing is done about it.
///
/// Where it lands, stated rather than assumed: `spawn` runs this in front of
/// its own output, so whoever asked for the agent reads it. `wsp resume` calls
/// it while building its picker and then clears the screen to draw, so the
/// line is written and not seen — that path reaches this only as a *fallback*
/// for a claim with no recorded cwd, and the same sentence is waiting on the
/// next `wsp checkout` of the task, which is where somebody is actually
/// deciding what to do with the work.
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
    let made = ensure(&repo, &dir, task, &branch).ok()?;
    if let Some(o) = &made.orphan {
        eprintln!(
            "wsp: {task} was renumbered — branch {} still holds {} that {branch} has not, and this tree is not on it",
            o.branch,
            n_commits(o.commits)
        );
        eprintln!("wsp: nothing was taken from it — `git -C {} merge {}` if that work carries forward", util::contract(&dir), o.branch);
    }
    Some(util::contract(&dir))
}

/// `n commits`, singular at one.
///
/// One reader-facing phrase in one place: every line that reports outstanding
/// work counts the same thing and should count it the same way.
fn n_commits(n: usize) -> String {
    match n {
        1 => "1 commit".to_string(),
        n => format!("{n} commits"),
    }
}

/// Why a tree is finished with.
///
/// A distinction rather than a sentence, because the three are not equally sure
/// and [`sweep`] acts on only two of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Why {
    /// The task is closed. Somebody decided the work was over, which is the
    /// only fact here strong enough to remove a directory on — and it holds
    /// however much is in the tree, because that is a fact about the work.
    Closed,
    /// The branch is on the trunk **and** a running worklist has passed the
    /// group this task was in.
    ///
    /// **Read as a relaxation of [`Why::Idle`] this is wrong, and the next
    /// person to widen it will widen the wrong half.** The git question is
    /// identical and it has not moved an inch: nothing uncommitted, nothing the
    /// trunk has not got. What is added is *evidence*, and the whole of this
    /// variant is that evidence — a member of a group a worklist has declared
    /// finished is **not** the tree made thirty seconds ago for an agent that
    /// has not typed yet, because something put an agent on it and its work is
    /// on the trunk. So this is a strict **subset** of `Idle`, narrower than it
    /// rather than broader, and the only honest way to widen it is to widen
    /// what counts as a worklist having passed a group.
    ///
    /// The evidence arrives as a closure for the reason `closed` does: it is
    /// the store's fact rather than git's, and asking it here would put a
    /// second reading of "finished" beside [`crate::worklist`]'s, which is the
    /// duplication the whole worklist design exists to stop.
    Landed,
    /// Nothing uncommitted, and nothing the trunk has not got. Worth naming and
    /// never worth sweeping: it is exactly what the tree made thirty seconds ago
    /// for an agent that has not typed yet looks like.
    Idle,
}

impl Why {
    /// What to do about it — which is a different command for each, and the
    /// reason a caller reporting these has to know which one it is holding.
    ///
    /// `seated` overrides all three, and it is the whole of robustness-076 in one
    /// line: while an agent is still in a seat on this task, `--rm` and
    /// `--sweep` take the tree and leave the agent, the claim and the workspace
    /// standing. Naming them there is how the ending came to be half-done every
    /// time. [`crate::cmd_spawn::despawn`] is the verb that finishes it, and it
    /// removes the tree on the way.
    pub(crate) fn fix(&self, task: &str, seated: bool) -> String {
        match (self, seated) {
            (_, true) => despawn_hint(task),
            (Why::Closed | Why::Landed, false) => "`wsp checkout --sweep`".into(),
            (Why::Idle, false) => format!("`wsp checkout {task} --rm`"),
        }
    }
}

/// The verb that ends a piece of work, spelled out in one place.
///
/// Both the advice on a stale tree and the sweep's refusal point at it, and
/// they have to point at the same thing: while an agent is still in a seat on
/// this task, taking the tree leaves the agent, the claim and the workspace
/// standing, which is how the ending came to be half-done every time.
pub(crate) fn despawn_hint(task: &str) -> String {
    format!("`wsp despawn {task}`")
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
/// `closed` and `passed` are both asked of the store by the caller rather than
/// read here, because neither is git's: a tree whose task is done is finished
/// even if it has uncommitted work in it, which is a fact about the work; and
/// whether a worklist has passed the group a task was in is a fact about a
/// plan. See [`Why::Landed`] for what the second one is and is not.
pub(crate) fn stale(
    root: &Path,
    closed: &dyn Fn(&str) -> bool,
    passed: &dyn Fn(&str) -> bool,
) -> Vec<Stale> {
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
        if let Ok(s) = judge(root, &branch, &task, &dir, closed(&task), passed(&task)) {
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.task.cmp(&b.task));
    out
}

/// A branch with no working tree on it, and what it is holding.
///
/// The other half of the question [`stale`] answers. `stale` reads `.worktrees`
/// entries, so everything it can say is about a **directory**; what [`remove`]
/// leaves behind when it declines to delete a branch is a **ref**, and until
/// this nothing ever looked at one. Seven had collected in `~/claude/wsp` by
/// 2026-08-20 against eight live trees.
///
/// The cheap half of that is litter — a name whose commits are all on the trunk
/// — and the expensive half is *work*: [`remove`] keeps a branch precisely when
/// git refuses to delete it, which is precisely when it still holds commits.
/// The sweep that made it says so once, on stdout, and nothing mentions it
/// again.
pub(crate) struct Stray {
    /// The branch. A branch name and never assumed to be a task id, for the
    /// reason [`judge`] gives about reading it off a tree.
    pub branch: String,
    /// What the store makes of the name, which is the whole of the advice.
    pub whose: Whose,
    /// The commits it has that the trunk has not. Zero is litter: the work is
    /// on the trunk and the ref is all that is left of it.
    pub commits: usize,
}

/// What the store makes of a branch's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Whose {
    /// A task the store still knows by this name. Nothing here is lost, only
    /// unattended: [`ensure`] reuses `refs/heads/<task>` when it is there, so
    /// `wsp checkout <name>` builds the tree again from this very branch.
    Task,
    /// An id a task used to have, carrying the id it has now.
    ///
    /// The expensive one, and the case [`remove`]'s own docs name and say they
    /// cannot repair: nobody types the old id any more, and `wsp checkout`
    /// under the new one cuts a fresh branch at the trunk tip, so commits left
    /// here are unreachable through wsp however long they sit there. Naming the
    /// current id is what makes the report actionable — it is the id the reader
    /// knows the work by.
    Former(String),
    /// Not wsp's, and left entirely alone. A person's own branch that happens
    /// to be merged is not litter because a tool did not make it, and this is
    /// the only thing standing between [`sweep`] and somebody's topic branch.
    Stranger,
}

/// The branches this repository has a working tree on.
///
/// From `git worktree list` rather than from the `.worktrees` directory,
/// because the question is whether **any** tree holds the branch: a tree
/// somebody made by hand elsewhere is still somebody working on it, and reading
/// our own directory would call that branch abandoned and take it. Detached
/// trees contribute nothing, which is right — they hold no branch.
fn worktree_branches(repo: &Path) -> BTreeSet<String> {
    git(repo, &["worktree", "list", "--porcelain"])
        .map(|out| {
            out.lines()
                .filter_map(|l| l.strip_prefix("branch refs/heads/"))
                .map(|b| b.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The branches under `repo` that no tree is on, and what each still holds.
///
/// The looking half again, and it reads and never touches anything — the same
/// division [`stale`] and [`sweep`] are built on. `whose` is the store's to
/// answer and comes in as a closure for the reason `closed` does: the rule is
/// worth testing and a store is not worth needing in order to test it.
///
/// The trunk's own branch drops out without a special case, because the trunk
/// is a working tree and [`worktree_branches`] sees it.
pub(crate) fn strays(repo: &Path, whose: &dyn Fn(&str) -> Whose) -> Vec<Stray> {
    let Some(trunk) = trunk(repo) else { return Vec::new() };
    let Some(onto) = trunk_branch(&trunk) else { return Vec::new() };
    let held = worktree_branches(repo);
    let Some(all) = git(repo, &["branch", "--format=%(refname:short)"]) else { return Vec::new() };

    let mut out: Vec<Stray> = Vec::new();
    for branch in all.lines().map(str::trim).filter(|b| !b.is_empty()) {
        if held.contains(branch) {
            continue;
        }
        let whose = whose(branch);
        if whose == Whose::Stranger {
            continue;
        }
        let commits = ahead(repo, &onto, branch).len();
        out.push(Stray { branch: branch.to_string(), whose, commits });
    }
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// Take a branch that has nothing left on it.
///
/// `-d` and never `-D`, which is [`remove`]'s guard reused rather than
/// restated: git refuses to delete a branch holding commits the trunk has not
/// got, so the worst this can do is lose a name whose every commit is on the
/// trunk. Asked of git rather than decided from [`Stray::commits`], because the
/// count was taken a moment ago and a ref is what is about to be destroyed.
///
/// **Run from the trunk**, and that is not incidental: `-d` compares against
/// whatever HEAD the directory it runs in is on. `--sweep` is run by an agent
/// standing in its own tree, so from there `-d` would be asking "is it merged
/// into *my task branch*" — which refuses in the safe direction for a branch
/// the trunk has and this one has not, and in the unsafe direction for one this
/// tree merged and the trunk never saw.
fn prune(trunk: &Path, branch: &str) -> bool {
    git(trunk, &["branch", "-d", branch]).is_some()
}

/// Whether one tree is finished with, and why — or, in the `Err`, why it is
/// not, in the words a caller that left it standing wants to print.
///
/// One definition, asked two ways: of every tree under a repository by
/// [`stale`], and of one named member of a group a worklist has passed by
/// [`sweep_passed`]. They have to agree for the same reason [`finished`] gives
/// — a barrier removing a tree and `doctor` calling it litter must be answering
/// the same question — and this is the only place the answer is worked out.
///
/// The order of the refusals is the order of what they cost to be wrong about.
/// `closed` comes first and outranks everything, including uncommitted work,
/// because somebody deciding the work is over is a fact about the work rather
/// than about the checkout; [`sweep`] then refuses on `dirty` separately, so
/// the strong reason still cannot take unsaved work with it.
///
/// **The branch is read off the tree rather than assumed to be the task's
/// name.** A tree made before its task was renumbered is on a branch of the id
/// it had then, and `ahead()` asked about a branch that does not exist answers
/// the same empty list as one that has landed — so guessing the name here would
/// read work that is still on a branch as work that is on the trunk, with a
/// directory removal on the end of it. That is the one conflation this whole
/// predicate is written to avoid, and a tree on no branch at all is refused
/// rather than guessed at for the same reason.
fn judge(repo: &Path, onto: &str, task: &str, dir: &Path, closed: bool, passed: bool) -> Result<Stale, String> {
    if closed {
        return Ok(Stale { task: task.to_string(), why: Why::Closed, note: "the task is closed".into() });
    }
    if dirty(dir) {
        return Err("uncommitted work in it".into());
    }
    // [`trunk_branch`] asked of a linked worktree is the branch that tree is
    // on, which is exactly what this wants: the tree names its own branch and
    // nothing here has to reconstruct it from the id. `onto` is the other one —
    // the trunk's branch, which is what landing puts the work on.
    let Some(branch) = trunk_branch(dir) else {
        return Err("on a detached HEAD — no branch to compare with the trunk".into());
    };
    let commits = ahead(repo, onto, &branch);
    if !commits.is_empty() {
        return Err(format!("{} on {branch} that {onto} has not got", n_commits(commits.len())));
    }
    let why = if passed { Why::Landed } else { Why::Idle };
    let note = match why {
        Why::Landed => format!("a worklist has passed the group it was in, and nothing {onto} has not got"),
        _ => format!("nothing uncommitted and nothing {onto} has not got"),
    };
    Ok(Stale { task: task.to_string(), why, note })
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
///
/// **The branch is read off the tree, before the tree goes, rather than taken
/// from the task's name** — [`judge`]'s rule, arriving here late, and read in
/// this one function rather than asked of each of its four callers. A tree made
/// before its task was renumbered is on a branch of the id it had then, so
/// `branch -d <task>` named a branch that does not exist: it deleted nothing
/// and reported *kept* — "it has commits the trunk has not" — over a branch
/// that had landed everything it held, on every removal of a renumbered task's
/// tree.
///
/// What that leaves behind is worse than the false line, and reading the name
/// here does not repair it. [`checkout_dir`] finds an old id only while its
/// *directory* stands, so once the tree is gone `wsp checkout` under the new id
/// cuts a fresh branch at the trunk tip and unlanded commits are unreachable
/// through wsp — which is exactly the recovery [`discard`] declines to refuse on
/// the strength of. The name carried out to the caller is half the answer: it
/// makes the line that says where the work went name a branch somebody can
/// check out. The other half is [`strays`], which finds that branch again on
/// every `doctor` afterwards, because a name printed once is a name that has
/// scrolled away by the time anybody wants it.
fn remove(repo: &Path, dir: &Path) -> Removed {
    let existed = dir.exists();
    // Before the removal: a tree that has gone cannot be asked what it was on.
    // `None` is a detached HEAD, where there is no branch to keep and nothing
    // to delete — [`judge`] refuses such a tree, so only `--rm` reaches here
    // with one.
    let branch = trunk_branch(dir);
    let _ = git(repo, &["worktree", "remove", "--force", &dir.display().to_string()]);
    let _ = std::fs::remove_dir_all(dir);
    let _ = git(repo, &["worktree", "prune"]);
    // From the trunk, not from `repo`: `-d` compares against the HEAD of the
    // directory it runs in, and `repo` is whatever the caller was standing in —
    // for `--sweep` and `--rm` that is routinely an agent's own task tree. See
    // [`prune`], which is the same one-line rule from the other direction.
    let at = trunk(repo).unwrap_or_else(|| repo.to_path_buf());
    let kept = branch.filter(|b| git(&at, &["branch", "-d", b]).is_none());
    Removed { existed, kept }
}

struct Removed {
    existed: bool,
    /// The branch that outlived the tree, named rather than flagged: it is what
    /// a reader has to type to get the work back, and it is not always the
    /// task's id. `None` is a branch that went with the tree, having nothing
    /// the trunk had not got.
    kept: Option<String>,
}

/// What one sweep did, and what it declined to do.
#[derive(Default)]
pub(crate) struct Swept {
    /// Tasks whose trees went.
    pub removed: Vec<String>,
    /// The branches that outlived their tree because they still hold commits
    /// the trunk has not got. Branch names and not task ids: a renumbered task
    /// works on a branch of the id it had then, and this is the name somebody
    /// getting the work back has to type.
    pub branches: Vec<String>,
    /// Tasks left alone, and why. Reported rather than silent: a sweep that
    /// quietly skips things is one nobody can tell from a sweep that found
    /// nothing.
    pub kept: Vec<(String, String)>,
    /// Branches whose tree had already gone before this sweep ran, taken
    /// because their work is on the trunk. Separate from `branches` on
    /// provenance rather than on kind: these are what an *earlier* removal left
    /// behind, which is why nothing has mentioned them since.
    pub pruned: Vec<String>,
    /// Branches with no tree that still hold work. Named and never taken, for
    /// the same reason [`remove`] left them there in the first place.
    pub stranded: Vec<Stray>,
}

/// Remove the trees there is evidence about, and say what was left standing.
///
/// The acting half of [`stale`], and deliberately narrower than it:
/// [`Why::Closed`] and [`Why::Landed`] are swept and [`Why::Idle`] is not,
/// because a closed task is somebody's decision and a landed member of a passed
/// group is a worklist's, while an idle tree is a guess about a directory.
/// Three further refusals, all in the direction of leaving a tree that could
/// have gone rather than removing one that could not:
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
///
/// # And the branches, which are the same leak one layer down
///
/// Removing a tree does not always remove its branch — [`remove`] keeps one git
/// refuses to delete — so this verb has been *creating* litter [`stale`] cannot
/// see for as long as it has existed, seven refs' worth in `~/claude/wsp`. The
/// branch half is [`strays`], and the two refusals it makes are the two above
/// in different clothes: a branch still holding work is named and never taken,
/// and a branch that is not wsp's is not touched at all.
pub(crate) fn sweep(
    root: &Path,
    closed: &dyn Fn(&str) -> bool,
    passed: &dyn Fn(&str) -> bool,
    whose: &dyn Fn(&str) -> Whose,
    busy: &dyn Fn(&str) -> Option<String>,
    dry: bool,
) -> Swept {
    let mut out = Swept::default();
    let Some(trunk) = trunk(root) else { return out };
    // Before the trees go, so the two halves cannot report the same branch
    // twice: what this run is about to leave behind comes back on `branches`,
    // and `strays` is what some earlier run left and nothing has said since.
    for s in strays(root, whose) {
        if s.commits > 0 {
            out.stranded.push(s);
            continue;
        }
        // No `busy` question, because there is nothing to be busy in. A tree is
        // a place an agent can be standing; a ref whose every commit is on the
        // trunk is a name, and `wsp checkout` cuts an identical branch for
        // whoever comes back to the task.
        if dry || prune(&trunk, &s.branch) {
            out.pruned.push(s.branch);
        }
    }
    for s in stale(root, closed, passed) {
        if s.why == Why::Idle {
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
            if let Some(branch) = remove(root, &dir).kept {
                out.branches.push(branch);
            }
        }
        out.removed.push(s.task);
    }
    out
}

/// One member of a group a worklist has passed, and where its work lands.
///
/// The trunk and its branch are resolved by the caller rather than looked up
/// here, because the caller had to resolve both already: they are what it
/// compared the member's branch against to decide the group was finished at
/// all. That is [`crate::worklist::Repos`], which holds one answer per project
/// across a whole position walk.
///
/// Marked dead until `wsp worklist go` lands, which is the convention
/// [`crate::worklist`] states: the attribute is the marker for "its caller has
/// not landed yet", not for something nobody wants.
pub(crate) struct Passed {
    pub task: String,
    pub trunk: PathBuf,
    pub trunk_branch: String,
}

/// Remove the trees of the members of a group a worklist has passed.
///
/// The other shape of [`sweep`], and the reason there are two: `sweep` walks
/// one repository's `.worktrees` and asks about everything in it, and a group's
/// members are named, few, and routinely in different repositories — a worklist
/// references tasks across projects, which is the whole point of it. Walking
/// every repository any member lives in would sweep trees belonging to work
/// nobody at this barrier has anything to do with, which is a blast radius a
/// governor did not ask for.
///
/// Every refusal `sweep` makes is made here, and one of them matters more than
/// the rest. **A member still holding a claim is named rather than swept** —
/// `busy` answers for it, and the answer carries the [`despawn_hint`] to run,
/// because an agent still holding its claim is an agent still in the room and
/// taking its tree leaves it standing in a directory that is gone.
/// **Uncommitted work stops the removal**, unchanged and not overridable from
/// here: a branch brings every commit back and brings nothing that was never
/// committed, and this is the one thing in the whole arrangement that does not
/// come back.
///
/// And the licence is checked per tree rather than taken on the caller's word.
/// The caller has just established that the group is finished, so it would be
/// reasonable to trust it — but between that walk and this one an agent can
/// commit, and this is the end of the design that destroys directories. So each
/// tree is asked [`judge`] with `passed` true, and a tree that is somehow ahead
/// of the trunk comes back [`Why::Idle`]'s way: kept, and named with what it is
/// holding.
///
/// A member with no tree is not a finding. Most of them will have none: a tree
/// swept at an earlier barrier, or work that never took a checkout at all.
pub(crate) fn sweep_passed(
    members: &[Passed],
    closed: &dyn Fn(&str) -> bool,
    busy: &dyn Fn(&str, &Path) -> Option<String>,
    dry: bool,
) -> Swept {
    let mut out = Swept::default();
    for m in members {
        let dir = checkout_dir(&m.trunk, &m.task);
        if !dir.join(".git").exists() {
            continue;
        }
        if let Some(why) = busy(&m.task, &dir) {
            out.kept.push((m.task.clone(), why));
            continue;
        }
        match judge(&m.trunk, &m.trunk_branch, &m.task, &dir, closed(&m.task), true) {
            Err(why) => out.kept.push((m.task.clone(), why)),
            Ok(_) => {
                if let Some(branch) = (!dry).then(|| remove(&m.trunk, &dir).kept).flatten() {
                    out.branches.push(branch);
                }
                out.removed.push(m.task.clone());
            }
        }
    }
    out
}

/// Who is still in a tree, which is everything about a sweep that is not git's
/// to answer.
///
/// One rule, read once and asked of many trees, because `--sweep` and the
/// barrier are looking at the same directories and two answers to "is anybody
/// in there" is how a tree comes to be taken out from under somebody. Three
/// facts in the order they are sure: the caller's own cwd, a pane herdr reports
/// standing in the tree, and a claim still held.
///
/// **A herdr that is not there answers nothing**, so the middle fact is simply
/// absent when the socket is down, and the other two still hold. All three
/// refuse in the safe direction — they keep a tree that could have gone — and
/// none of them can be made wrong by a socket being down.
pub(crate) struct Occupied {
    here: PathBuf,
    panes: Vec<crate::herdr::Pane>,
    claimed: Vec<String>,
}

impl Occupied {
    pub(crate) fn now(store: &Store) -> Occupied {
        let here = std::env::current_dir().map(|c| util::real(&c.display().to_string())).unwrap_or_default();
        let panes =
            if crate::herdr::available() { crate::herdr::panes().unwrap_or_default() } else { Vec::new() };
        Occupied { here, panes, claimed: store.claims().keys().cloned().collect() }
    }

    /// Why `task`'s tree at `dir` has to be left standing, or `None`.
    pub(crate) fn of(&self, task: &str, dir: &Path) -> Option<String> {
        let dir = util::real(&dir.display().to_string());
        if self.here.starts_with(&dir) {
            return Some("you are standing in it".into());
        }
        if let Some(pane) = self.panes.iter().find(|x| util::real(&x.cwd).starts_with(&dir)) {
            return Some(format!("{} is standing in it", pane.pane_id));
        }
        // A claim on a tree that is finished with is unusual and it is still
        // somebody: the agent that did the work and has not let go of it yet.
        // Naming the verb rather than the fact is the difference between a
        // report and something a governor can act on without going to look.
        self.claimed
            .iter()
            .any(|t| t == task)
            .then(|| format!("still claimed — {} ends it", despawn_hint(task)))
    }
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
    /// Gone, and the branch that outlived it, if one did — named, because it
    /// is what somebody has to type to get the work back and it is not always
    /// the task's id.
    Removed { path: String, kept: Option<String> },
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
    Tree::Removed { path, kept: remove(&w.repo, &w.dir).kept }
}

/// Whether a tree has anything uncommitted, tracked or not.
fn dirty(dir: &Path) -> bool {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// Whether a tree's whole quarrel with HEAD is in its **index**, with every
/// file on disk matching HEAD exactly.
///
/// This is the state `wsp commit-help` leaves behind by design and says
/// nothing about at the moment it bites. Step 1 mandates a private
/// `GIT_INDEX_FILE`, correctly — `git add` otherwise writes to one
/// `.git/index` for everybody standing in the tree. Commit against it and HEAD
/// moves while the shared index still holds the tree from *before*, so
/// `git status` reports `MM` and git reports, truthfully about itself and
/// misleadingly to a reader, *"Your local changes to the following files would
/// be overwritten by merge"*.
///
/// **There are no local changes.** `git diff HEAD` is empty and the working
/// tree is byte-for-byte HEAD; "local changes" means a working tree to the
/// person reading it, and on 2026-08-19 that sentence sent two agents hunting
/// a change that did not exist — `worklist-020` reached three wrong
/// conclusions in a row from believing it, and `worklist-013` reached the same
/// wrong one independently. So [`land`] asks this before it repeats git's
/// wording, and names `git reset`, which clears the index and touches no file.
///
/// # What this deliberately does not do
///
/// It does not reset anything. The index is *shared*, so a reset here would
/// discard whatever another agent had staged — no file content, but somebody's
/// intent, and this design's rule is that destroying a record is asked for and
/// never automatic (`robustness-090` d1). Diagnosing costs two `git` calls on
/// a path that has already failed; deciding costs somebody their staging.
///
/// Untracked files are not consulted, because they are not what a fast-forward
/// refuses over — git names those separately, in a different sentence. The one
/// caller that *does* need them excluded checks for them itself, where the
/// difference between "your files" and "your index" is the message.
fn index_only(dir: &Path) -> bool {
    // `git` strips `GIT_INDEX_FILE`, which is exactly what is wanted: the index
    // in question is the shared `.git/index`, never the private one a caller
    // partway through the commit procedure has exported. See
    // [`crate::cmd_verify::git`], and [`crate::cmd_agent::doctor`], which asks
    // a neighbouring question of every declared root for the same reason.
    git(dir, &["diff", "--quiet", "HEAD"]).is_some()
        && git(dir, &["diff", "--cached", "--quiet", "HEAD"]).is_none()
}

/// Whether anything in `dir` is untracked and not ignored.
fn untracked(dir: &Path) -> bool {
    git(dir, &["ls-files", "--others", "--exclude-standard"])
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

/// What each landed branch put on the trunk, read back off the trunk's reflog.
///
/// The worklist barrier's one piece of feedback about how a group was composed:
/// *do not put two tasks that touch the same file in one group* is advice
/// nothing enforces, and the `batch`'s evidence for it was an **absence** — no
/// land-time conflicts across fifteen agents — which cannot be acted on. This
/// is an observation instead, and it arrives at the barrier, which is exactly
/// when the next group is being composed.
///
/// # Why the reflog and not the branch
///
/// After [`land`] the question has no answer left in the graph. `land` rebases
/// the branch onto the trunk and fast-forwards the trunk onto it, so by the
/// time a barrier asks, the branch's commits **are** the trunk's history: the
/// merge base of the two is the branch tip, `trunk..branch` is empty, and
/// `merge-base --fork-point` answers with the tip itself. Nothing about a
/// landed branch says where it began.
///
/// The trunk's reflog does, exactly and cheaply. Every land is one
/// fast-forward, so the entry that **names this branch** has the trunk's
/// previous value in the entry below it, and the diff between the two is
/// precisely what this branch added — whoever else landed in between, and
/// however many times it was rebased on the way. Measured against a repository
/// with an unrelated commit landing between two members and all three rebased:
/// each member came back with its own files and no one else's.
///
/// And the branch itself need not still be there, which is the other half of
/// why this is not read off a ref: a member is closed by removing its tree,
/// and [`remove`] then deletes the branch with `git branch -d`, which succeeds
/// *because* the work is merged. The reflog outlives all of it. See
/// [`Landings::files`] for the lookup that follows from that.
///
/// The reflog is read **once for the repository** and one `git diff
/// --name-only` is spent per land — usually one per member, and the live trunk
/// has no member above five — which is what makes this affordable to do for a
/// whole group.
///
/// # What it cannot answer, and why that is said out loud
///
/// A reflog is local, is not in the repository anybody clones, and expires.
/// None of that bites at a barrier crossed minutes after the land — but a
/// branch landed by hand some other way, or a trunk reset, leaves a member
/// this cannot place, and [`Landings::files`] answers `None` for it rather than
/// an empty list. **The two must not be confused by the caller**: an empty list
/// means the member changed nothing, and `None` means nobody knows what it
/// changed. Reporting the second as the first is a clean bill of health nothing
/// checked, which is the `batch`'s absence-as-evidence all over again.
pub(crate) struct Landings {
    /// The trunk's values, newest first, each with the reflog's own account of
    /// how the trunk came to hold it.
    values: Vec<(String, String)>,
}

impl Landings {
    pub(crate) fn read(repo: &Path, trunk_branch: &str) -> Landings {
        let values = git(repo, &["reflog", "show", "--format=%H %gs", trunk_branch])
            .map(|s| {
                s.lines()
                    .filter_map(|l| l.trim().split_once(' '))
                    .map(|(h, why)| (h.to_string(), why.trim().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Landings { values }
    }

    /// The files `branch` put on the trunk, or `None` where the reflog does not
    /// place it. See the type's docs for why those are two different answers.
    ///
    /// **The name is the whole lookup, and every entry carrying it counts.**
    /// [`land`] fast-forwards the trunk onto the branch and `merge <branch>:`
    /// is what git writes for that, so the name is written down at every land
    /// and is the only key needed. It replaces a hash comparison against the
    /// branch's current tip that was wrong in both of its halves:
    ///
    /// - **A landed branch need not exist.** `wsp despawn` ends a member by
    ///   removing its tree, and [`remove`] then deletes the branch — see the
    ///   type's docs. So the ordinary close of a member's work was answering
    ///   `None`, "nobody knows what it changed", for the one state this report
    ///   was built to survive.
    /// - **A member lands more than once.** Land, review, land again is the
    ///   ordinary shape of a member's work — the live trunk holds `merge
    ///   worklist-011:` three times running — and only the newest entry can
    ///   hold the branch's current tip. One entry is a fraction of what the
    ///   member touched, and an overlap sitting in any earlier land printed as
    ///   `none`: a clean bill of health nothing checked, which is the failure
    ///   this whole type exists to refuse.
    ///
    /// The hash was never the key, but dropping it is not merely a widening —
    /// matching on it *invents* evidence. A branch `wsp checkout` cut at the
    /// trunk tip sits on a trunk value it did not put there, so the tip alone
    /// hands it the diff of whatever really landed at that point: driven on
    /// 2026-08-19 against five spawned agents with nothing committed, all five
    /// came back as having touched a file none of them had opened. A branch
    /// that reached the trunk some other way names nothing and answers `None`,
    /// which is what this type already promises for a trunk that was reset or
    /// a land done by hand.
    ///
    /// The union is a **set**: a file touched by two lands of one member is
    /// one file, and listing it twice would have that member overlapping
    /// itself. And a land whose base is off the bottom of the reflog takes the
    /// whole answer to `None` rather than reporting the lands above it — a
    /// partial account of what a member touched is a partial `none`, which is
    /// the one confusion this type is here to keep out.
    pub(crate) fn files(&self, repo: &Path, branch: &str) -> Option<Vec<String>> {
        let named = format!("merge {branch}:");
        let mut out: BTreeSet<String> = BTreeSet::new();
        let mut landed = false;
        for (at, (tip, _)) in
            self.values.iter().enumerate().filter(|(_, (_, why))| why.starts_with(&named))
        {
            landed = true;
            let (base, _) = self.values.get(at + 1)?;
            let diff = git(repo, &["diff", "--name-only", base, tip])?;
            out.extend(diff.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()));
        }
        landed.then(|| out.into_iter().collect())
    }
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

    /// The branch the work is on: **read off the tree, never assumed to be the
    /// task's name**, and `None` for a tree that is detached or not there yet.
    ///
    /// [`checkout_dir`] resolves the *directory* through a renumbering — a tree
    /// made before the id changed keeps the old name — and the branch in it is
    /// of that old name too. Both verbs here were asking [`ahead`] about
    /// `task`, and `ahead` on a branch that does not exist answers the same
    /// empty list as one that has landed: `land` printed `nothing to land`,
    /// exited 0 and left the work on the branch, and `checkout` dropped the
    /// `ahead N commit(s) to land` line that is how an agent knows to type it.
    /// [`judge`] has read the branch this way since `worklist-002`, calling the
    /// conflation the one thing that predicate is written to avoid; these are
    /// the callers nobody told.
    ///
    /// Asked of git each time rather than resolved in [`pick`], because the
    /// answer changes underneath a `Where`: `checkout` calls [`ensure`], which
    /// is what creates the branch this then reads.
    fn on(&self) -> Option<String> {
        trunk_branch(&self.dir)
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
        let r = remove(&w.repo, &w.dir);
        if args.json() {
            println!(
                "{}",
                json!({
                    "removed": r.existed,
                    "branch_kept": r.kept.is_some(),
                    "branch": r.kept,
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
            if let Some(branch) = &r.kept {
                println!("{}", p.yellow(&format!("branch {branch} kept — it has commits the trunk has not")));
            }
        }
        return 0;
    }

    let head = git(&w.trunk, &["rev-parse", "--short", &w.branch]).map(|s| s.trim().to_string()).unwrap_or_default();
    let made = match ensure(&w.repo, &w.dir, &w.task, &w.branch) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };
    let fresh = made.fresh;
    // After [`ensure`], so a tree it has just made names its own new branch.
    let on = w.on();
    let commits = on.as_deref().map(|b| ahead(&w.repo, &w.branch, b)).unwrap_or_default();

    if args.json() {
        println!(
            "{}",
            json!({
                "task": w.task,
                "path": util::contract(&w.dir),
                "branch": on,
                "from": w.branch,
                "new": fresh,
                "ahead": commits.len(),
                "former": made.orphan.as_ref().map(|o| json!({ "branch": o.branch, "commits": o.commits })),
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
    // Named and not taken — see [`orphan`]. Two lines because they are two
    // different things: the first is a fact the reader did not have, the second
    // is the one command that acts on it, and printing the command in the same
    // breath as the fact is what makes this a visible choice rather than a
    // warning. `--rm` already names the branch it keeps when a tree goes; this
    // is the other end of that gap, at the moment the task is picked up again,
    // which is the only moment somebody is guaranteed to be looking.
    if let Some(o) = &made.orphan {
        println!(
            "{} {}",
            p.dim("former"),
            p.yellow(&format!("{} holds {} that {} has not — this tree is not on it", o.branch, n_commits(o.commits), w.branch))
        );
        println!(
            "       {}",
            p.dim(&format!("the id this task had before it was renumbered — `git merge {}` in the tree above takes it", o.branch))
        );
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
    // The same evidence the barrier stands on, asked of the store here rather
    // than recomputed: a tree whose group a running worklist has passed is one
    // this command can now clear, and `--sweep` finding it where `wsp worklist
    // go` has not been run is how the trees that accumulated before any of this
    // existed finally go. Free when nothing is running, which is most of the
    // time — see [`crate::worklist::passed_by_running`].
    let passed = crate::worklist::passed_by_running(store);
    let occupied = Occupied::now(store);
    let busy = |task: &str| -> Option<String> { occupied.of(task, &checkout_dir(&trunk, task)) };

    let whose = naming(store);
    let out = sweep(&repo, &closed, &|t| passed.contains(t), &whose, &busy, dry);

    if args.json() {
        println!(
            "{}",
            json!({
                "removed": out.removed,
                "branches_kept": out.branches,
                "branches_pruned": out.pruned,
                "stranded": out.stranded.iter().map(|s| json!({
                    "branch": s.branch,
                    "commits": s.commits,
                    "now": match &s.whose { Whose::Former(now) => Some(now), _ => None },
                })).collect::<Vec<_>>(),
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
    // Dim, and one line however many there are: a name whose commits are all on
    // the trunk is the least interesting thing this command does, and the seven
    // that had collected here would otherwise be seven lines of nothing.
    if !out.pruned.is_empty() {
        println!(
            "{}",
            p.dim(&format!(
                "{} {} whose tree had gone and whose work is on the trunk: {}",
                if dry { "would take" } else { "took" },
                n_branches(out.pruned.len()),
                out.pruned.join(", ")
            ))
        );
    }
    for s in &out.stranded {
        println!("{}", p.yellow(&stranded_line(s)));
    }
    if out.removed.is_empty() && out.kept.is_empty() && out.pruned.is_empty() && out.stranded.is_empty() {
        println!("{}", p.dim("no tree or branch here belongs to a finished task"));
    }
    0
}

/// `n branches`, singular at one — [`n_commits`]'s neighbour and there for the
/// same reason: `doctor` and `--sweep` count the same refs and say so with the
/// same words.
pub(crate) fn n_branches(n: usize) -> String {
    match n {
        1 => "1 branch".to_string(),
        n => format!("{n} branches"),
    }
}

/// A branch holding work with no tree to find it from, in one sentence.
///
/// Written once and printed by both callers, because `--sweep` naming a branch
/// as litter-that-is-not and `doctor` reporting the same branch have to be
/// saying the same thing — the disagreement between a report and a removal is
/// how eighteen worktrees came to be sitting in one repository with nobody able
/// to say which were safe to take.
///
/// The two halves are the two readings of [`Whose`], and only one of them is
/// urgent: a branch still under its own task's name is one `wsp checkout` away,
/// and a former id is not reachable through wsp at all — so that one names the
/// git command, which is the only thing that gets the work back.
pub(crate) fn stranded_line(s: &Stray) -> String {
    let held = format!("branch {} holds {} and has no tree", s.branch, n_commits(s.commits));
    match &s.whose {
        Whose::Former(now) => format!(
            "{held} — it is {now} now, and `wsp checkout {now}` cuts a fresh branch rather than finding this one; `git switch {}` does",
            s.branch
        ),
        _ => format!("{held} — `wsp checkout {}` builds it again", s.branch),
    }
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

/// What the store makes of a branch name, read once and asked of many.
///
/// [`finished`]'s counterpart for [`strays`], and the same shape: read the
/// store once here, ask a closure many times there. It answers with the
/// *current* id for a former one rather than with a boolean, because that is
/// the id the reader knows the work by and the one they have to be told is no
/// longer the way back to it.
///
/// An id the store has never heard of is [`Whose::Stranger`] — the same answer
/// [`finished`] gives for the same reason, and here it carries more weight: it
/// is what keeps a sweep run against a store pointed somewhere else from
/// reading every branch in the repository as litter.
pub(crate) fn naming(store: &Store) -> impl Fn(&str) -> crate::cmd_checkout::Whose {
    let known: BTreeSet<String> =
        store.tasks().into_iter().map(|t| t.id).chain(store.archived_ids()).collect();
    let renamed = store.renamed_ids();
    move |branch: &str| {
        if known.contains(branch) {
            return Whose::Task;
        }
        // One hop and not a walk: [`Store::rename_tasks`] collapses chains as
        // it writes, so no entry's value is another entry's key and an id
        // renamed twice already points at where it ended up.
        match renamed.get(branch).filter(|now| known.contains(*now)) {
            Some(now) => Whose::Former(now.clone()),
            None => Whose::Stranger,
        }
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
        // …but say which "it". A tree whose files match HEAD has nothing to
        // commit, and telling its agent to commit sends it looking for work it
        // has already done. See [`index_only`]: the disagreement is the shared
        // index left behind a commit made through a private one, and `git
        // reset` is the whole of the fix. Untracked files are excluded because
        // then there genuinely is something here to commit.
        if index_only(&w.dir) && !untracked(&w.dir) {
            let d = util::contract(&w.dir);
            eprintln!("wsp: {d} has nothing uncommitted — its files match HEAD exactly");
            eprintln!(
                "wsp: what `git status` shows is the index, a commit behind after a commit made through a private GIT_INDEX_FILE — `git -C {d} reset` clears it and touches no file"
            );
            return 2;
        }
        eprintln!("wsp: {} has uncommitted work — commit it first", util::contract(&w.dir));
        return 2;
    }
    // The branch is read off the tree, which is the whole of `worklist-025`:
    // `w.task` is the *current* id and the tree may be on a branch of the id it
    // had before a `wsp mv` renumbered it. See [`Where::on`] — asking `ahead`
    // for a branch that does not exist got the answer for one that had landed,
    // and this printed success over work still sitting on a branch. A detached
    // tree is refused rather than guessed at, [`judge`]'s rule for the same
    // reason: there is no branch here to compare with the trunk.
    let Some(on) = w.on() else {
        eprintln!(
            "wsp: {} is on a detached HEAD — nothing here names a branch to land",
            util::contract(&w.dir)
        );
        return 2;
    };
    if ahead(&w.repo, &w.branch, &on).is_empty() {
        println!("{} {}", p.dim("nothing to land —"), p.dim(&format!("{on} is already in {}", w.branch)));
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
        let landed = ahead(&w.repo, &w.branch, &on);
        // `--ff-only` in the trunk rather than a push: the trunk is a checked-out
        // working tree, and this is the one form of update that refuses rather
        // than silently overwriting a file somebody has open there.
        git_ok(&w.trunk, &["merge", "--ff-only", "--quiet", &on]).map_err(|e| {
            let t = util::contract(&w.trunk);
            let mut msg = format!("{t} would not fast-forward: {e}");
            // git's refusal names *local changes*, and the commonest reason
            // there are none is the one the commit procedure creates. Appended
            // rather than substituted: git is not wrong about itself, and the
            // reader is owed both what it said and what it meant. [`index_only`]
            // carries the incident and why this does not simply reset.
            if index_only(&w.trunk) {
                msg.push_str(&format!(
                    "\nwsp: there are no local changes there — the files in {t} match its HEAD exactly, and the whole disagreement is its index, \
                     left a commit behind by a commit made through a private GIT_INDEX_FILE\n\
                     wsp: `git -C {t} reset` clears it and touches no file, then `wsp land` again"
                ));
            }
            msg
        })?;
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

    /// Every branch in the repository is a task the store still knows by that
    /// name — the ordinary case, and the one that leaves [`sweep`]'s tree rules
    /// exactly as they were.
    fn all_ours(_: &str) -> Whose {
        Whose::Task
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
        assert!(ensure(&dir, &wt, "t-1", "master").unwrap().fresh, "the tree was not new");
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
        assert!(ensure(&dir, &wt, "t-3", "master").unwrap().fresh, "the tree was not remade");
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
        let none_passed = |_: &str| false;

        let idle = checkout_dir(&dir, "t-idle");
        ensure(&dir, &idle, "t-idle", "master").unwrap();
        let busy = checkout_dir(&dir, "t-busy");
        ensure(&dir, &busy, "t-busy", "master").unwrap();
        std::fs::write(busy.join("wip.txt"), "wip\n").unwrap();
        run(&busy, &["add", "wip.txt"]);
        run(&busy, &["commit", "--quiet", "-m", "wip"]);

        let found = stale(&dir, &never_closed, &none_passed);
        assert_eq!(found.len(), 1, "wrong trees named: {:?}", names(&found));
        assert_eq!(found[0].task, "t-idle");
        assert_eq!(found[0].why, Why::Idle);

        // Uncommitted work counts as work: a tree is not litter because git has
        // not been told about what is in it yet.
        std::fs::write(idle.join("scratch.txt"), "not yet\n").unwrap();
        assert!(stale(&dir, &never_closed, &none_passed).is_empty(), "a tree with unsaved work was called finished");

        // A closed task ends its tree whatever is in it — the work is over, and
        // the tree outliving it is the leak.
        let closed = stale(&dir, &|id: &str| id == "t-busy", &none_passed);
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

    /// A tree with one commit in it, taken away again the way a person does it
    /// — leaving exactly what `remove` leaves and what nothing has ever looked
    /// at: the branch, with no directory.
    fn abandoned(dir: &Path, task: &str, land: bool) {
        let wt = checkout_dir(dir, task);
        ensure(dir, &wt, task, "master").unwrap();
        std::fs::write(wt.join(format!("{task}.txt")), "work\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", task]);
        if land {
            run(dir, &["merge", "--ff-only", "--quiet", task]);
        }
        run(dir, &["worktree", "remove", "--force", &wt.display().to_string()]);
    }

    fn stray_named<'a>(found: &'a [Stray], branch: &str) -> &'a Stray {
        found.iter().find(|s| s.branch == branch).unwrap_or_else(|| {
            panic!("{branch} was not among {:?}", found.iter().map(|s| &s.branch).collect::<Vec<_>>())
        })
    }

    /// The half of the leak `stale` cannot see, because it reads directories
    /// and this is a ref.
    ///
    /// `remove` keeps a branch git refuses to delete — that is the design, and
    /// it is what makes taking a tree recoverable — so `--sweep` and `--rm`
    /// have been *creating* this state since they existed, saying so once on
    /// stdout and never again. Seven had collected in `~/claude/wsp` by
    /// 2026-08-20 against eight live trees.
    #[test]
    fn a_branch_whose_tree_has_gone_is_seen_and_one_still_being_worked_in_is_not() {
        let (_env, dir) = scratch("strays");
        repo(&dir);

        let live = checkout_dir(&dir, "t-live");
        ensure(&dir, &live, "t-live", "master").unwrap();
        abandoned(&dir, "t-landed", true);
        abandoned(&dir, "t-holding", false);

        let found = strays(&dir, &all_ours);
        let seen: Vec<&str> = found.iter().map(|s| s.branch.as_str()).collect();
        assert_eq!(seen, vec!["t-holding", "t-landed"], "wrong branches named");
        // The trunk drops out by being a working tree, not by a special case.
        assert!(!seen.contains(&"master"), "the trunk's own branch was called abandoned");
        assert_eq!(stray_named(&found, "t-landed").commits, 0, "a landed branch is holding something");
        assert_eq!(stray_named(&found, "t-holding").commits, 1, "the unlanded work was not counted");
    }

    /// The two answers a sweep can give about a branch, and they are the two it
    /// already gives about a tree: take what the trunk has got, name what it
    /// has not.
    ///
    /// The second is the whole point of looking. A branch with commits on it
    /// and no tree is *work*, and until it was reported nothing in wsp
    /// mentioned it after the sweep that made it scrolled off the screen.
    #[test]
    fn the_sweep_takes_a_landed_branch_with_no_tree_and_names_one_still_holding_work() {
        let (_env, dir) = scratch("sweep-strays");
        repo(&dir);
        abandoned(&dir, "t-landed", true);
        abandoned(&dir, "t-holding", false);

        let out = sweep(&dir, &|_| false, &|_| false, &all_ours, &|_| None, false);
        assert_eq!(out.pruned, vec!["t-landed"], "the wrong branch was taken");
        assert_eq!(out.stranded.len(), 1, "the work was not named");
        assert_eq!(out.stranded[0].branch, "t-holding");
        assert_eq!(out.stranded[0].commits, 1);

        let left = git(&dir, &["branch", "--format=%(refname:short)"]).unwrap();
        assert!(!left.contains("t-landed"), "the landed branch is still here: {left}");
        assert!(left.contains("t-holding"), "the unlanded work was taken: {left}");
        assert!(
            git(&dir, &["cat-file", "-e", "t-holding^{commit}"]).is_some(),
            "the commits went with the ref"
        );
    }

    /// `-n` on a removing command has to be free, and this one destroys refs
    /// rather than directories — which is easier to do by accident and just as
    /// hard to undo without the name.
    #[test]
    fn a_dry_sweep_names_the_branch_it_would_take_and_leaves_it() {
        let (_env, dir) = scratch("sweep-strays-dry");
        repo(&dir);
        abandoned(&dir, "t-landed", true);

        let out = sweep(&dir, &|_| false, &|_| false, &all_ours, &|_| None, true);
        assert_eq!(out.pruned, vec!["t-landed"]);
        assert!(
            git(&dir, &["branch", "--format=%(refname:short)"]).unwrap().contains("t-landed"),
            "a dry run took the branch"
        );
    }

    /// The one thing standing between a removing verb and somebody's own work.
    ///
    /// A merged topic branch is indistinguishable from wsp's litter by every
    /// git question there is; the only difference is that wsp did not make it.
    /// Same refusal `finished` makes for an id the store has never heard of,
    /// and it matters more here: a sweep run against a store pointed somewhere
    /// else would otherwise read every branch in the repository as litter.
    #[test]
    fn a_branch_wsp_did_not_make_is_not_litter_however_landed_it_is() {
        let (_env, dir) = scratch("stranger");
        repo(&dir);
        abandoned(&dir, "t-landed", true);
        run(&dir, &["branch", "eds-experiment"]);

        let ours = |b: &str| match b == "t-landed" {
            true => Whose::Task,
            false => Whose::Stranger,
        };
        let out = sweep(&dir, &|_| false, &|_| false, &ours, &|_| None, false);
        assert_eq!(out.pruned, vec!["t-landed"]);
        assert!(
            git(&dir, &["branch", "--format=%(refname:short)"]).unwrap().contains("eds-experiment"),
            "a branch wsp did not make was swept"
        );
    }

    /// The expensive reading, and the reason the report carries a name rather
    /// than a count.
    ///
    /// `remove`'s own docs name this case and say they cannot repair it: a tree
    /// made before its task was renumbered is on a branch of the id it had
    /// then, and once the tree is gone `wsp checkout` under the *new* id cuts a
    /// fresh branch at the trunk tip — so telling a reader to run it is telling
    /// them to walk past their work. The line has to name the git command, and
    /// it has to name the id they know the task by, because nobody remembers
    /// the old one.
    #[test]
    fn a_branch_of_an_id_a_task_no_longer_has_says_which_task_it_is_and_how_to_reach_it() {
        let (_env, dir) = scratch("stray-renumbered");
        repo(&dir);
        abandoned(&dir, "old-3", false);

        let renamed = |b: &str| match b == "old-3" {
            true => Whose::Former("wsp-9".into()),
            false => Whose::Stranger,
        };
        let found = strays(&dir, &renamed);
        assert_eq!(found.len(), 1);
        let line = stranded_line(&found[0]);
        assert!(line.contains("old-3") && line.contains("wsp-9"), "both ids have to be in it: {line}");
        assert!(line.contains("git switch old-3"), "the only command that gets the work back: {line}");
        assert!(!line.contains("`wsp checkout old-3`"), "told to run the verb that cannot find it: {line}");

        // And it is not taken: the whole point is that these commits are the
        // ones nothing else can reach.
        let out = sweep(&dir, &|_| false, &|_| false, &renamed, &|_| None, false);
        assert!(out.pruned.is_empty(), "a branch holding unreachable work was taken");
        assert_eq!(out.stranded.len(), 1);
    }

    /// What the store makes of a branch name, which is where the three readings
    /// come from.
    #[test]
    fn the_store_tells_a_task_from_an_id_it_used_to_have_from_a_name_it_never_knew() {
        let env = util::isolated("co-naming");
        let store = Store::open();
        store.ensure_dirs().unwrap();
        store.save_task(&crate::model::Task::new("live", "wsp-9")).unwrap();
        std::fs::write(store.ids_path(), r#"{"old-3":"wsp-9"}"#).unwrap();

        let whose = naming(&store);
        assert_eq!(whose("wsp-9"), Whose::Task);
        assert_eq!(whose("old-3"), Whose::Former("wsp-9".into()));
        assert_eq!(whose("eds-experiment"), Whose::Stranger);
        // A renumbering onto an id the store no longer holds is a stranger too,
        // rather than advice pointing at a task that is not there.
        std::fs::write(store.ids_path(), r#"{"old-3":"gone-1"}"#).unwrap();
        assert_eq!(naming(&store)("old-3"), Whose::Stranger);
        drop(env);
    }

    /// `git branch -d` compares against the HEAD of the directory it runs in,
    /// and `--sweep` is run by an agent standing in its own tree.
    ///
    /// So the question git was being asked was "is it merged into *my task
    /// branch*", which is the wrong branch in both directions: it refuses a
    /// branch the trunk has and this tree has not — safe, merely confusing —
    /// and it *deletes* one this tree merged and the trunk never saw. `remove`
    /// runs it from the trunk, so the ref that is destroyed and the commits
    /// that are kept are measured against the same branch the tree was judged
    /// against.
    #[test]
    fn a_branch_the_trunk_has_not_got_survives_a_removal_run_from_another_tree() {
        let (_env, dir) = scratch("remove-from-tree");
        repo(&dir);

        // One agent's work, committed and never landed.
        let theirs = checkout_dir(&dir, "t-theirs");
        ensure(&dir, &theirs, "t-theirs", "master").unwrap();
        std::fs::write(theirs.join("theirs.txt"), "theirs\n").unwrap();
        run(&theirs, &["add", "."]);
        run(&theirs, &["commit", "--quiet", "-m", "theirs"]);

        // Another agent, standing in its own tree, which has taken that work in.
        let mine = checkout_dir(&dir, "t-mine");
        ensure(&dir, &mine, "t-mine", "master").unwrap();
        run(&mine, &["merge", "--quiet", "--no-edit", "t-theirs"]);

        let out = remove(&mine, &theirs);
        assert_eq!(
            out.kept.as_deref(),
            Some("t-theirs"),
            "the branch was deleted against the wrong HEAD — the trunk has not got it"
        );
        assert!(
            git(&dir, &["rev-parse", "--verify", "--quiet", "refs/heads/t-theirs"]).is_some(),
            "the ref is gone and master never had the commit"
        );
    }

    /// Landing puts commits on the trunk and does nothing else to the tree it
    /// took them from.
    ///
    /// It used to remove it, which deleted its own caller's cwd the first time
    /// anybody ran it (robustness-047) and cost a fresh checkout and a cold
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
        let none_passed = |_: &str| false;
        let busy = |id: &str| (id == "t-held").then(|| "you are standing in it".to_string());

        // `-n` says the same thing and touches nothing, which is what makes the
        // first run of a removing command typeable.
        let looked = sweep(&dir, &closed, &none_passed, &all_ours, &busy, true);
        assert_eq!(looked.removed, ["t-done"], "the dry run named the wrong trees");
        assert!(checkout_dir(&dir, "t-done").join(".git").exists(), "-n removed a tree");

        let out = sweep(&dir, &closed, &none_passed, &all_ours, &busy, false);
        assert_eq!(out.removed, ["t-done"]);
        assert!(!checkout_dir(&dir, "t-done").exists(), "the finished tree is still here");
        for t in ["t-open", "t-held", "t-messy"] {
            assert!(checkout_dir(&dir, t).join(".git").exists(), "{t} was swept and should not have been");
        }
        let kept: Vec<&str> = out.kept.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(kept, ["t-held", "t-messy"], "a skipped tree went unreported: {:?}", out.kept);
        assert!(out.kept[1].1.contains("uncommitted"), "the reason did not name the work at risk");
    }

    /// The whole of `Why::Landed`, as the one difference between two trees
    /// nothing else can tell apart. Both are clean, both are level with the
    /// trunk, and the git question gives the same answer about each; the only
    /// thing between them is that a worklist has passed the group one of them
    /// was in. The named one goes and the other is reported forever, which is
    /// what "narrower than `Idle`, not broader" means when it is arithmetic.
    #[test]
    fn a_tree_a_worklist_has_passed_is_swept_and_the_one_beside_it_is_not() {
        let (_env, dir) = scratch("landed");
        repo(&dir);
        for t in ["t-passed", "t-fresh"] {
            ensure(&dir, &checkout_dir(&dir, t), t, "master").unwrap();
        }
        let never_closed = |_: &str| false;
        let passed = |id: &str| id == "t-passed";

        let found = stale(&dir, &never_closed, &passed);
        assert_eq!(names(&found), ["t-fresh", "t-passed"], "both trees are finished with");
        assert_eq!(found[0].why, Why::Idle, "the tree nothing knows anything about");
        assert_eq!(found[1].why, Why::Landed, "and the one a worklist has evidence about");
        assert!(found[1].note.contains("passed the group"), "the reason did not say what the evidence is");

        let out = sweep(&dir, &never_closed, &passed, &all_ours, &|_| None, false);
        assert_eq!(out.removed, ["t-passed"]);
        assert!(checkout_dir(&dir, "t-fresh").join(".git").exists(), "an idle tree was swept on a guess");
    }

    /// The conflation the whole predicate is written against, with the
    /// destructive end pointed at it. `ahead()` asked about a branch that does
    /// not exist answers the same empty list as one that has landed, so a sweep
    /// that guessed the branch from the task id would take the tree of a task
    /// renumbered mid-run and leave its commits on a branch nobody looks for.
    /// The tree names its own branch instead, so the work holds the tree.
    #[test]
    fn a_passed_member_whose_work_is_not_on_the_trunk_keeps_its_tree() {
        let (_env, dir) = scratch("landed-ahead");
        repo(&dir);
        // The tree an agent left after committing under the id it had then.
        let wt = checkout_dir(&dir, "old-3");
        ensure(&dir, &wt, "old-3", "master").unwrap();
        std::fs::write(wt.join("mine.txt"), "mine\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", "unlanded"]);

        // The worklist names the task by the id it has now, and the barrier has
        // passed the group. Nothing about that makes the work landed.
        let members = [Passed {
            task: "wsp-9".into(),
            trunk: dir.clone(),
            trunk_branch: "master".into(),
        }];
        std::fs::write(Store::open().ids_path(), r#"{"old-3":"wsp-9"}"#).unwrap();

        let out = sweep_passed(&members, &|_| false, &|_, _| None, false);
        assert!(out.removed.is_empty(), "a tree holding unlanded work was swept");
        assert_eq!(out.kept.len(), 1);
        assert!(out.kept[0].1.contains("that master has not got"), "the reason did not name the work: {:?}", out.kept);
        assert!(wt.join("mine.txt").exists(), "the work is gone");
    }

    /// The same conflation, with `land` on the end of it rather than a sweep —
    /// `worklist-025`, found by driving it: an inbox task takes a tree, commits,
    /// is filed with `wsp mv` and renumbered, and `wsp land` printed `nothing to
    /// land`, exited 0 and left the work on the branch.
    ///
    /// Asserted as the two answers themselves, because the difference between
    /// them is the whole defect: the id says landed and the tree says one
    /// commit outstanding, about the same work at the same moment.
    #[test]
    fn landing_a_renumbered_task_reads_the_branch_off_the_tree_and_not_off_the_id() {
        let (_env, dir) = scratch("land-renumbered");
        repo(&dir);
        // The tree an agent took and committed in under the id it had then.
        let wt = checkout_dir(&dir, "inbox-001");
        ensure(&dir, &wt, "inbox-001", "master").unwrap();
        std::fs::write(wt.join("work.txt"), "mine\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", "the work"]);
        std::fs::write(Store::open().ids_path(), r#"{"inbox-001":"proj-001"}"#).unwrap();

        // `land` resolves the tree by the new id and finds the old one, which is
        // the half that already worked.
        let w = pick(vec![dir.clone()], "proj-001").expect("the renumbered tree");
        assert!(w.dir.ends_with("inbox-001"), "the tree was not resolved through the renaming: {:?}", w.dir);

        assert!(
            ahead(&w.repo, &w.branch, &w.task).is_empty(),
            "the id names a branch that exists, so this test proves nothing"
        );
        let on = w.on().expect("the tree names its own branch");
        assert_eq!(on, "inbox-001");
        assert_eq!(ahead(&w.repo, &w.branch, &on).len(), 1, "the work the id reported as landed");

        // And the rest of what `land` does, driven with that name: the trunk
        // fast-forwards onto the branch the tree is really on.
        git_ok(&wt, &["rebase", &w.branch]).unwrap();
        git_ok(&w.trunk, &["merge", "--ff-only", "--quiet", &on]).unwrap();
        assert!(dir.join("work.txt").exists(), "the work still did not reach the trunk");
    }

    /// The other end of the same fault, and the more expensive one: `remove`
    /// deleted `git branch -d <task>`, which for a renumbered task is a name no
    /// branch has. So a tree went, the branch it was on stayed under the old
    /// name with nothing pointing at it — [`checkout_dir`] only finds an old id
    /// while its *directory* stands — and the next `wsp checkout` cut a fresh
    /// branch at the trunk tip. The commits were still in the repository and
    /// unreachable through wsp, which is exactly what [`discard`] declines to
    /// refuse on the strength of the branch bringing them back.
    #[test]
    fn ending_a_renumbered_tasks_work_names_the_branch_the_tree_was_really_on() {
        let (_env, dir) = scratch("discard-renumbered");
        repo(&dir);
        let wt = checkout_dir(&dir, "inbox-002");
        ensure(&dir, &wt, "inbox-002", "master").unwrap();
        std::fs::write(wt.join("unlanded.txt"), "mine\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", "never landed"]);
        std::fs::write(Store::open().ids_path(), r#"{"inbox-002":"proj-002"}"#).unwrap();

        match discard(vec![dir.clone()], "proj-002", &|_| None) {
            Tree::Removed { kept, .. } => assert_eq!(
                kept.as_deref(),
                Some("inbox-002"),
                "the branch holding the work was not named, so nobody can get it back"
            ),
            other => panic!("{}", named(&other)),
        }
        assert!(
            git(&dir, &["rev-parse", "--verify", "--quiet", "refs/heads/inbox-002"]).is_some(),
            "the branch went and took the only copy of the work with it"
        );

        // And the same tree once its work is on the trunk: the branch goes with
        // the directory, rather than being reported as kept forever under a
        // name that never existed.
        ensure(&dir, &wt, "inbox-002", "master").unwrap();
        git_ok(&dir, &["merge", "--ff-only", "--quiet", "inbox-002"]).unwrap();
        match discard(vec![dir.clone()], "proj-002", &|_| None) {
            Tree::Removed { kept, .. } => assert_eq!(kept, None, "a landed branch was kept: {kept:?}"),
            other => panic!("{}", named(&other)),
        }
        assert!(
            git(&dir, &["rev-parse", "--verify", "--quiet", "refs/heads/inbox-002"]).is_none(),
            "the branch outlived work that is on the trunk"
        );
    }

    /// `worklist-027`, the third and last of the renumbering conflations, and
    /// the only one that is not a bug being fixed. A task takes a tree under
    /// one id, commits, is renumbered, and the tree is swept: the commits are
    /// on a branch of the old id, [`checkout_dir`] finds an old id only while
    /// its *directory* stands, and reopening the task cuts a fresh branch at
    /// the trunk tip. Both halves are asserted here because both are chosen —
    /// **the branch is not adopted**, and it is **not left unmentioned**. See
    /// [`orphan`] for why, and for the two seats that decided it independently.
    #[test]
    fn reopening_a_renumbered_task_whose_tree_went_names_the_branch_holding_its_work() {
        let (_env, dir) = scratch("orphan-branch");
        repo(&dir);
        let wt = checkout_dir(&dir, "inbox-003");
        ensure(&dir, &wt, "inbox-003", "master").unwrap();
        std::fs::write(wt.join("unlanded.txt"), "mine\n").unwrap();
        run(&wt, &["add", "."]);
        run(&wt, &["commit", "--quiet", "-m", "never landed"]);
        std::fs::write(Store::open().ids_path(), r#"{"inbox-003":"proj-003"}"#).unwrap();

        // The tree goes and the branch stays, which is `worklist-025`'s half.
        assert_eq!(remove(&dir, &wt).kept.as_deref(), Some("inbox-003"), "the branch went with the tree");

        // Reopening under the new id. A fresh branch at the trunk tip, and the
        // work not in it: this is the loss, and it is deliberate — a task
        // refiled because its first attempt was wrong is a common reason to
        // renumber, so the old branch is quite likely work somebody abandoned.
        let fresh = checkout_dir(&dir, "proj-003");
        let made = ensure(&dir, &fresh, "proj-003", "master").unwrap();
        assert!(made.fresh, "the tree was not new");
        assert!(!fresh.join("unlanded.txt").exists(), "the abandoned branch was adopted");
        assert_eq!(trunk_branch(&fresh).as_deref(), Some("proj-003"), "the tree is not on the new id's branch");

        // And the finding that turns an invisible loss into a visible choice.
        let o = made.orphan.expect("the branch holding the only copy of the work was not named");
        assert_eq!(o.branch, "inbox-003");
        assert_eq!(o.commits, 1);

        // Nothing to say once that work is on the trunk. A line printed for
        // every renumbered task is a line a reader learns to skip, including
        // on the checkouts where it is the whole point.
        let _ = remove(&dir, &fresh);
        git_ok(&dir, &["merge", "--ff-only", "--quiet", "inbox-003"]).unwrap();
        let made = ensure(&dir, &checkout_dir(&dir, "proj-003"), "proj-003", "master").unwrap();
        assert!(made.orphan.is_none(), "a landed branch was reported as work nobody is looking at");
    }

    /// The two refusals a barrier may not talk its way past, and the reason
    /// each is there. Uncommitted work is the one thing here that does not come
    /// back, so it stops the removal however good the evidence is. An agent
    /// still holding its claim is an agent still in the room, so its tree is
    /// **named with the verb that ends the work** rather than taken out from
    /// under it — a half-done ending is what this whole seam exists to stop.
    #[test]
    fn the_sweep_a_passed_group_licenses_still_refuses_work_and_still_refuses_a_claim() {
        let (_env, dir) = scratch("landed-refusals");
        repo(&dir);
        for t in ["t-messy", "t-claimed", "t-clear"] {
            ensure(&dir, &checkout_dir(&dir, t), t, "master").unwrap();
        }
        std::fs::write(checkout_dir(&dir, "t-messy").join("draft.txt"), "not committed\n").unwrap();

        let members: Vec<Passed> = ["t-messy", "t-claimed", "t-clear"]
            .iter()
            .map(|t| Passed { task: (*t).into(), trunk: dir.clone(), trunk_branch: "master".into() })
            .collect();
        let busy = |task: &str, _: &Path| {
            (task == "t-claimed").then(|| format!("still claimed — {} ends it", despawn_hint(task)))
        };

        let out = sweep_passed(&members, &|_| false, &busy, false);
        assert_eq!(out.removed, ["t-clear"], "the wrong trees went: {:?}", out.removed);
        let kept: Vec<&str> = out.kept.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(kept, ["t-messy", "t-claimed"]);
        assert!(out.kept[0].1.contains("uncommitted"), "the work at risk was not named");
        assert!(out.kept[1].1.contains("`wsp despawn t-claimed`"), "the ending to run was not named: {:?}", out.kept[1]);
        assert!(checkout_dir(&dir, "t-messy").join("draft.txt").exists(), "unsaved work was destroyed");
    }

    /// A member with no tree is the ordinary case and not a finding: most of a
    /// passed group took a checkout that an earlier barrier already swept, or
    /// never took one at all.
    #[test]
    fn a_passed_member_with_no_tree_is_silent_rather_than_reported() {
        let (_env, dir) = scratch("landed-absent");
        repo(&dir);
        let members = [Passed { task: "t-never".into(), trunk: dir.clone(), trunk_branch: "master".into() }];
        let out = sweep_passed(&members, &|_| false, &|_, _| None, false);
        assert!(out.removed.is_empty() && out.kept.is_empty(), "a member that never had a tree was reported");
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

        let out = sweep(&dir, &|_| true, &|_| false, &all_ours, &|_| None, false);
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
            Tree::Removed { kept, .. } => assert!(kept.is_none(), "a branch level with the trunk was kept: {kept:?}"),
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
            Tree::Removed { kept, .. } => assert_eq!(kept.as_deref(), Some("t-3"), "the commits were not recoverable"),
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

    /// Commit through a private index, the way `wsp commit-help` step 1
    /// mandates, and leave the shared one behind — the state this whole
    /// diagnosis exists for.
    fn commit_behind_the_shared_index(dir: &Path, file: &str, body: &str, msg: &str) {
        let idx = dir.join("private-index");
        std::fs::write(dir.join(file), body).unwrap();
        for args in [
            vec!["read-tree", "HEAD"],
            vec!["add", file],
            vec!["commit", "--quiet", "-m", msg],
        ] {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .env("GIT_INDEX_FILE", &idx)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        std::fs::remove_file(&idx).unwrap();
    }

    /// The sentence that misled two agents in one hour, asserted as a fact
    /// about git rather than as a claim about wsp: the tree is dirty, and
    /// there is nothing in it to commit.
    #[test]
    fn a_tree_a_commit_left_behind_its_index_reads_as_dirty_with_nothing_to_commit() {
        let (_env, dir) = scratch("stale-index");
        repo(&dir);
        commit_behind_the_shared_index(&dir, "kept.txt", "two\n", "second");

        assert!(dirty(&dir), "`git status` is expected to show this — it shows MM");
        assert!(index_only(&dir), "the files match HEAD and only the index does not");
        assert!(!untracked(&dir), "nothing untracked, so nothing here is work to commit");

        // And the fix named in the message, which is the claim that has to
        // hold: it clears the index and touches no file.
        let before = std::fs::read_to_string(dir.join("kept.txt")).unwrap();
        run(&dir, &["reset", "--quiet"]);
        assert_eq!(std::fs::read_to_string(dir.join("kept.txt")).unwrap(), before);
        assert!(!dirty(&dir), "a bare reset did not settle it");
    }

    /// The other half of the same predicate: ordinary uncommitted work must go
    /// on reading as uncommitted work, or the diagnosis would swallow the
    /// refusal it was added beside.
    #[test]
    fn ordinary_uncommitted_work_is_not_mistaken_for_a_stale_index() {
        let (_env, dir) = scratch("real-dirt");
        repo(&dir);

        std::fs::write(dir.join("kept.txt"), "edited\n").unwrap();
        assert!(dirty(&dir));
        assert!(!index_only(&dir), "an edited file is a change to the working tree");

        std::fs::write(dir.join("kept.txt"), "one\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        assert!(dirty(&dir));
        assert!(untracked(&dir), "a new file is work somebody has to commit");
        assert!(!index_only(&dir), "and the index has nothing to do with it");

        // Both at once is the case the two questions are asked separately for:
        // the index is a commit behind *and* there is a file to commit, so
        // `land` owes the ordinary refusal and not the diagnosis.
        commit_behind_the_shared_index(&dir, "kept.txt", "two\n", "second");
        assert!(index_only(&dir), "an untracked file is not a change to a tracked one");
        assert!(untracked(&dir), "…and it is still sitting there");
    }

    /// The refusal itself, end to end: the trunk's index alone stops the
    /// fast-forward, and the working tree it names is untouched throughout.
    #[test]
    fn a_stale_trunk_index_is_the_whole_of_what_refuses_the_fast_forward() {
        let (_env, dir) = scratch("ff-index");
        repo(&dir);
        commit_behind_the_shared_index(&dir, "kept.txt", "two\n", "second");

        let wt = checkout_dir(&dir, "t-5");
        ensure(&dir, &wt, "t-5", "master").unwrap();
        std::fs::write(wt.join("kept.txt"), "three\n").unwrap();
        run(&wt, &["add", "kept.txt"]);
        run(&wt, &["commit", "--quiet", "-m", "third"]);

        let refused = git_ok(&dir, &["merge", "--ff-only", "--quiet", "t-5"]).unwrap_err();
        assert!(
            refused.contains("local changes"),
            "git's wording is what the added line answers, and it changed: {refused}"
        );
        assert!(index_only(&dir), "…and there are none — this is the index");

        run(&dir, &["reset", "--quiet"]);
        git_ok(&dir, &["merge", "--ff-only", "--quiet", "t-5"]).expect("a bare reset was not the fix");
        assert_eq!(std::fs::read_to_string(dir.join("kept.txt")).unwrap(), "three\n");
    }
}
