//! Where a worklist is up to, and the two readings of "finished".
//!
//! Nothing here is stored. A worklist's file holds one piece of state —
//! `status`, `draft | running | held | done` — because that is a *decision*
//! somebody takes: to start it, to stop starting things, to be finished with
//! it. **Where the run is up to is derived and never written**: the position is
//! the first group not finished, computed from the tasks and from git every
//! time it is asked for. The evidence is the `batch` handbook, whose written
//! table of where the night had got to disagreed with what was actually
//! happening inside the hour while the membership stayed true. A status you can
//! compute cannot go stale, and this module is the computation.
//!
//! # Two readings, and they are deliberately allowed to disagree
//!
//! | | reads | costs | asked by |
//! |---|---|---|---|
//! | [`Reading::Settled`] | the task's status is `review` or closed | the store | the panel, `worklist show` |
//! | [`Reading::Landed`] | the branch is on the trunk | git, per member | the barrier |
//!
//! Both of the cheap signals a barrier could have rested on are wrong, and the
//! code should say why rather than leave the next reader to find out. **`done`
//! never arrives**: everything here sits at `review` by design, because `done`
//! is Ed's, so a barrier on `done` never opens. **`review` arrives too early**:
//! the `batch`'s costliest failure was an order saying "land it" read as
//! "finish it", leaving the commit on the task's branch where the next agent's
//! tree never saw it (`wsp-084` d1), and a barrier on `review` starts the next
//! group on a trunk missing the previous group's work — that same defect with a
//! barrier's authority behind it.
//!
//! So both readings are kept and **the disagreement is surfaced rather than
//! resolved**. "5 of 7 at review" beside a barrier that will not open *is* that
//! misreading, found by the free view in time to go and look instead of at 4am.
//! A [`Standing`] carries both answers for one member, which is where the
//! disagreement is legible; a caller drawing at 250ms asks for `Settled` and
//! never starts a process.
//!
//! # The third answer git has, which the predicate as designed did not
//!
//! `cmd_checkout::ahead()` answers "what does this branch hold that the trunk
//! has not", and on a branch that does not exist it answers "nothing" — which
//! reads as landed. That is the barrier opening on a group **nothing was ever
//! spawned for**, which is the one failure it exists to prevent. So the branch
//! is asked about before it is compared, and [`Landing::NoBranch`] is its own
//! answer rather than a silent zero.
//!
//! It is genuinely ambiguous and the store settles it: a branch is deleted with
//! `git branch -d`, which refuses unless the work is merged, so a *missing*
//! branch is either work that landed and was swept or work that never began.
//! A member with no branch is therefore finished only if it is also settled —
//! nobody has finished a task whose branch never existed and whose status is
//! still `todo`. The cost is one extra process per member, and only where the
//! branch is missing, since the comparison is skipped when there is nothing to
//! compare.
//!
//! # Members that are not there
//!
//! The sweep's own judgement, reused: **absence is somebody's decision and an
//! unlanded branch is a fact.** A task that is closed is settled, because one
//! somebody finished by hand overnight must not stall the night. One moved to
//! another project changes nothing — that is the whole point of a worklist
//! referencing its members rather than owning them, and the project is looked
//! up fresh here every time so the move is simply read. One that has been
//! archived or deleted is settled *and* is named by [`dangling`], for a caller
//! to print where it cannot be missed and to write into the worklist's log.
//! It is never removed from the membership: a machine silently editing a plan
//! is the stale-plan failure with nobody left to notice it.
//!
//! # Why a module and not a method
//!
//! Free functions over `&Store` and a `&Worklist`, beside the record rather
//! than on it. A position is not an entity: it is computation over the store
//! and over git, it is derived and never written, and hung off `Worklist` it
//! would look exactly like a field somebody could set.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::cmd_checkout;
use crate::cmd_verify::{git, toplevel};
use crate::model::{Group, Status, Task, Worklist};
use crate::resolve::Index;
use crate::store::Store;
use crate::util;

// Every public item below carries `#[allow(dead_code)]`, on the convention the
// record was built under: the attribute is the marker for "its caller has not
// landed yet", and `worklist next` and the barrier are group 4. It comes off
// one item at a time with the `use` that reads it, rather than as a blanket
// allow over the module that would go on hiding something genuinely dead.

/// Which question is being asked of a member, because the two cost different
/// amounts and are allowed to give different answers. See the module docs.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// The store alone: a task at `review` or closed is finished with. Free,
    /// and what a surface redrawing four times a second can afford.
    Settled,
    /// Git as well: the member's branch is on the trunk. One process per
    /// member, and the only reading a barrier may open on.
    Landed,
}

/// What the store says about a member. The free half, and always computed —
/// it is what settles the ambiguous cases in the expensive half.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// Still somebody's to finish, and the word the store holds for it. A
    /// `blocked` or `parked` member reads here rather than being special-cased
    /// into finished: it does hold the barrier, and being named on the line
    /// that says what is holding it is how somebody comes to remove it.
    Open(Status),
    /// At `review` — the status everything in a worklist actually reaches,
    /// since `done` is Ed's.
    Review,
    /// `done`. Somebody decided the work was over.
    Closed,
    /// The store has never heard of the id: archived, or deleted. See
    /// [`dangling`].
    Gone,
}

#[allow(dead_code)]
impl Settlement {
    pub fn settled(&self) -> bool {
        matches!(self, Settlement::Review | Settlement::Closed | Settlement::Gone)
    }

    /// The word for a column, which is the status where there is one and the
    /// fact of absence where there is not.
    pub fn word(&self) -> &str {
        match self {
            Settlement::Open(s) => s.as_str(),
            Settlement::Review => Status::Review.as_str(),
            Settlement::Closed => Status::Done.as_str(),
            Settlement::Gone => "gone",
        }
    }
}

/// What git says about a member's branch.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landing {
    /// The trunk has everything the branch holds. The only answer a barrier
    /// opens on by itself.
    Landed,
    /// Commits the trunk has not got, and the name of the trunk branch they
    /// are not on — read from the repository rather than assumed to be
    /// `master`, because that is a fact about the repository and not about wsp.
    Ahead { commits: usize, trunk: String },
    /// No branch of this name, under this id or any it used to have. Either
    /// nothing has run for it, or it landed and was swept; [`Standing`] settles
    /// which against the store.
    NoBranch,
    /// Nowhere to ask: the member has no project, its project has no root,
    /// that root is not a git repository, or the trunk is on a detached HEAD
    /// with no branch to compare against. Design-only work legitimately lives
    /// here, which is why this is not an error.
    NoRepo,
}

/// One member, read. Both answers where both were asked, so the disagreement
/// between them is a thing a caller can print rather than a thing it has to
/// go and reconstruct.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub id: String,
    pub settlement: Settlement,
    /// `None` under [`Reading::Settled`], and for a member the store has never
    /// heard of — there is no branch to ask about and no project to ask it in.
    pub landing: Option<Landing>,
}

#[allow(dead_code)]
impl Standing {
    /// Whether this member is finished, under the reading it was read with.
    ///
    /// The two branch-shaped answers fall back on the store, and that is the
    /// judgement the module docs argue for: a missing branch is landed work
    /// that was swept when the task is settled, and work that never began when
    /// it is not.
    pub fn finished(&self) -> bool {
        match &self.landing {
            None => self.settlement.settled(),
            Some(Landing::Landed) => true,
            Some(Landing::Ahead { .. }) => false,
            Some(Landing::NoBranch | Landing::NoRepo) => self.settlement.settled(),
        }
    }

    /// Why it is not finished, in the words a reader can act on, and empty
    /// where it is finished or where the status word already says it.
    ///
    /// Nothing about the agent: who is standing on it is the claim's to say,
    /// and this module never asks herdr.
    pub fn note(&self) -> String {
        match &self.landing {
            Some(Landing::Ahead { commits, trunk }) => {
                let n = if *commits == 1 { "1 commit".to_string() } else { format!("{commits} commits") };
                format!("{n} not on {trunk}")
            }
            Some(Landing::NoBranch) if !self.settlement.settled() => "no branch — nothing has run for it".into(),
            Some(Landing::NoRepo) if !self.settlement.settled() => "no repository to look in".into(),
            _ => String::new(),
        }
    }
}

/// Where a worklist is up to.
///
/// The ordinal is 1-based because it is the number written in the file and
/// typed at the command line, and it is a *position* rather than an id: it is
/// rewritten on every write, which is why a log line names the members and not
/// the number.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    /// The first group not finished, or `None` when every group is.
    pub at: Option<usize>,
    /// How many groups there are.
    pub of: usize,
    /// The members of the group at `at`, read — empty when there is no such
    /// group. Carried back rather than left to be asked for again, because
    /// under [`Reading::Landed`] asking again is another process per member,
    /// and because "waiting on 2" and the two lines under it are one answer.
    pub members: Vec<Standing>,
}

#[allow(dead_code)]
impl Position {
    /// Every group finished. Not the same as the worklist being `done`, which
    /// is somebody saying there is nothing left to want from it.
    pub fn finished(&self) -> bool {
        self.at.is_none()
    }

    /// The members holding the current group, in the order the group names
    /// them. What a barrier reports when it will not open.
    pub fn holding(&self) -> Vec<&Standing> {
        self.members.iter().filter(|s| !s.finished()).collect()
    }
}

/// Where the run is up to: the first group not finished, under `reading`.
///
/// Walks from the front and **stops at the first group that is not finished**,
/// which is what makes the expensive reading affordable at a barrier: the
/// groups ahead of the position are not asked about at all, and a git process
/// is spent only on the members of the group actually being waited on and the
/// ones behind it that have already passed.
///
/// A worklist with no groups is finished, which is the honest answer: there is
/// nothing left in it to start.
#[allow(dead_code)]
pub fn position(store: &Store, w: &Worklist, reading: Reading) -> Position {
    let groups = w.groups();
    let mut repos = Repos::new(store);
    for (i, g) in groups.iter().enumerate() {
        let members = group(store, &mut repos, g, reading);
        if !members.iter().all(Standing::finished) {
            return Position { at: Some(i + 1), of: groups.len(), members };
        }
    }
    Position { at: None, of: groups.len(), members: Vec::new() }
}

/// Every member of one group, in the order the group names them.
#[allow(dead_code)]
pub fn group(store: &Store, repos: &mut Repos, g: &Group, reading: Reading) -> Vec<Standing> {
    g.members.iter().map(|m| member(store, repos, m, reading)).collect()
}

/// How one member stands.
#[allow(dead_code)]
pub fn member(store: &Store, repos: &mut Repos, id: &str, reading: Reading) -> Standing {
    let task = store.task(id);
    let settlement = match &task {
        None => Settlement::Gone,
        Some(t) => match t.status() {
            Status::Review => Settlement::Review,
            Status::Done => Settlement::Closed,
            other => Settlement::Open(other),
        },
    };
    let landing = match (reading, &task) {
        (Reading::Settled, _) => None,
        (Reading::Landed, None) => None,
        (Reading::Landed, Some(t)) => Some(landing(repos, t)),
    };
    Standing { id: id.to_string(), settlement, landing }
}

/// The members no task answers to, across the whole list rather than only the
/// group being waited on.
///
/// Separate from [`position`] because the two are wanted at different moments:
/// a dangling id in group 4 is worth knowing at the barrier after group 1,
/// while there is still time to put something back. The caller prints it and
/// writes it to the worklist's log — this only finds it, because removing it is
/// the one thing nothing here is allowed to do.
#[allow(dead_code)]
pub fn dangling(store: &Store, w: &Worklist) -> Vec<String> {
    let mut out = Vec::new();
    for g in w.groups() {
        for m in g.members {
            if store.task(&m).is_none() && !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
}

/// What the landing question needs and what does not change between members:
/// the project tree, the repository each project's work lands in, and the
/// record of what was renumbered into what.
///
/// A group of seven members is usually seven tasks in two or three projects,
/// and resolving one project's trunk is a root walk and two git processes. Held
/// across a group, that is three resolutions instead of twenty-one; held across
/// a whole position walk it is three for the list. Nothing in it is a cache of
/// something that changes under it inside one barrier check.
#[allow(dead_code)]
pub struct Repos {
    index: Index,
    renamed: BTreeMap<String, String>,
    seen: HashMap<String, Option<Trunk>>,
}

#[derive(Debug, Clone)]
struct Trunk {
    dir: PathBuf,
    branch: String,
}

#[allow(dead_code)]
impl Repos {
    pub fn new(store: &Store) -> Repos {
        Repos { index: Index::new(store.projects()), renamed: store.renamed_ids(), seen: HashMap::new() }
    }

    /// The trunk a project's work lands on, resolved once. `None` where there
    /// is nothing to ask: no root, or a root that is not a repository.
    fn of(&mut self, project: &str) -> Option<Trunk> {
        if let Some(known) = self.seen.get(project) {
            return known.clone();
        }
        let found = self
            .index
            .root_of(project)
            .map(|r| util::expand(&r))
            .as_deref()
            .and_then(toplevel)
            .and_then(|repo| {
                let dir = cmd_checkout::trunk(&repo)?;
                let branch = cmd_checkout::trunk_branch(&dir)?;
                Some(Trunk { dir, branch })
            });
        self.seen.insert(project.to_string(), found.clone());
        found
    }

    /// The branch names this task's work could be on, current id first.
    ///
    /// A tree made before a task was renumbered is on a branch of the old
    /// name — the same fact `cmd_checkout::checkout_dir` resolves for the
    /// directory, for the same reason it does not rename trees somebody may be
    /// standing in. Without this a renumbered member reads as having no branch,
    /// and a barrier stalls on work that is already on the trunk.
    fn branches(&self, task: &str) -> Vec<String> {
        let mut out = vec![task.to_string()];
        out.extend(self.renamed.iter().filter(|(_, to)| *to == task).map(|(from, _)| from.clone()));
        out
    }
}

/// Whether a member's work is on the trunk.
fn landing(repos: &mut Repos, t: &Task) -> Landing {
    let Some(project) = t.project.as_deref() else { return Landing::NoRepo };
    let Some(trunk) = repos.of(project) else { return Landing::NoRepo };

    // Asked before it is compared, and this is the state `ahead()` alone does
    // not have: on a branch that does not exist it reports nothing outstanding,
    // which is indistinguishable from landed. The form is `ensure`'s, which is
    // what creates the branch in the first place.
    let Some(branch) = repos.branches(&t.id).into_iter().find(|b| {
        git(&trunk.dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_some()
    }) else {
        return Landing::NoBranch;
    };

    match cmd_checkout::ahead(&trunk.dir, &trunk.branch, &branch).len() {
        0 => Landing::Landed,
        commits => Landing::Ahead { commits, trunk: trunk.branch },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Project, Worklist};
    use std::path::Path;
    use std::process::Command;

    /// A store of its own, a repository beside it, and the project pointing at
    /// the second — which is what the landing question walks. The process is
    /// pointed at the store as well: `Repos` reads `ids.json` through it and
    /// `checkout_dir`'s renumbering rule reads the ambient one. See
    /// [`crate::util::isolated`].
    fn scratch(tag: &str) -> (util::Isolated, Store, PathBuf) {
        let env = util::isolated(&format!("wl-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_run(&repo, &["init", "--quiet", "-b", "master"]);
        std::fs::write(repo.join("kept.txt"), "one\n").unwrap();
        git_run(&repo, &["add", "kept.txt"]);
        git_run(&repo, &["commit", "--quiet", "-m", "first"]);

        let mut p = Project::new("wsp");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();

        (env, store, repo)
    }

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

    fn task(store: &Store, id: &str, status: &str) {
        let mut t = Task::new(id, id);
        t.project = Some("wsp".into());
        t.status_raw = status.into();
        store.save_task(&t).unwrap();
    }

    /// A tree on a branch of the task's name with one commit in it, which is
    /// what an agent that has committed and not landed leaves behind.
    fn committed(repo: &Path, id: &str) -> PathBuf {
        let dir = repo.join(cmd_checkout::WORKTREES).join(id);
        let from = cmd_checkout::trunk_branch(repo).expect("the repository is on a branch");
        git_run(repo, &["worktree", "add", "--quiet", "-b", id, &dir.display().to_string(), &from]);
        std::fs::write(dir.join(format!("{id}.txt")), "mine\n").unwrap();
        git_run(&dir, &["add", "."]);
        git_run(&dir, &["commit", "--quiet", "-m", id]);
        dir
    }

    fn land(repo: &Path, id: &str) {
        git_run(repo, &["merge", "--ff-only", "--quiet", id]);
    }

    fn list(groups: &str) -> Worklist {
        let mut w = Worklist::new("batch", "Overnight batch");
        w.body = format!("## Groups\n{groups}");
        w
    }

    /// The whole of what "derived" means, asserted as arithmetic: the position
    /// moves because the *tasks* moved, and nothing wrote it down. This is the
    /// `batch` handbook's failure made impossible rather than warned about —
    /// there is no second copy to disagree with the first.
    #[test]
    fn the_position_is_the_first_group_not_finished_and_moves_on_its_own() {
        let (_env, store, _repo) = scratch("derived");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "todo");
        }
        let w = list("- 1  wsp-1\n- 2  wsp-2  wsp-3\n");

        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(1), "nothing is finished, so it is at the front");
        assert_eq!(p.of, 2);
        assert_eq!(p.members.len(), 1, "and it carries back the group being waited on");

        task(&store, "wsp-1", "review");
        assert_eq!(position(&store, &w, Reading::Settled).at, Some(2), "the group behind it settled");

        task(&store, "wsp-2", "done");
        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(2), "one member of two is not the group");
        assert_eq!(p.holding().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["wsp-3"]);

        task(&store, "wsp-3", "review");
        let p = position(&store, &w, Reading::Settled);
        assert!(p.finished(), "every group is finished");
        assert_eq!(p.at, None);
    }

    /// The disagreement the design asks for, in one assertion: the same group,
    /// read two ways, at the same moment. `settled` says the work is done with
    /// and `landed` says the commit is still on a branch — which is exactly the
    /// "land it" read as "finish it" that cost the `batch` its most expensive
    /// hour, surfaced while somebody can still go and look.
    #[test]
    fn a_member_at_review_is_settled_and_the_barrier_still_will_not_open() {
        let (_env, store, repo) = scratch("disagree");
        task(&store, "wsp-1", "review");
        task(&store, "wsp-2", "todo");
        committed(&repo, "wsp-1");
        let w = list("- 1  wsp-1\n- 2  wsp-2\n");

        assert_eq!(position(&store, &w, Reading::Settled).at, Some(2), "the free reading has moved on");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "and the barrier has not");
        let held = p.holding();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].settlement, Settlement::Review, "both answers are on the one member");
        assert_eq!(held[0].note(), "1 commit not on master", "and the note says where the work is");

        land(&repo, "wsp-1");
        assert_eq!(position(&store, &w, Reading::Landed).at, Some(2), "landing it opens the barrier");
    }

    /// The state `ahead()` alone does not have. A group nobody has spawned for
    /// has no branches, and every one of its members would otherwise read as
    /// having nothing outstanding — the barrier opening on work that has not
    /// begun, which is the single failure it exists to prevent.
    #[test]
    fn a_member_that_has_never_run_does_not_read_as_landed() {
        let (_env, store, repo) = scratch("no-branch");
        task(&store, "wsp-1", "todo");
        let w = list("- 1  wsp-1\n");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "no branch and no work done is not finished");
        assert_eq!(p.members[0].landing, Some(Landing::NoBranch));
        assert_eq!(p.members[0].note(), "no branch — nothing has run for it");

        // The other half of the ambiguity: a branch is deleted with `-d`, which
        // refuses unless the work is merged, so a missing branch on a settled
        // task is work that landed and was swept.
        committed(&repo, "wsp-1");
        land(&repo, "wsp-1");
        git_run(&repo, &["worktree", "remove", "--force", &repo.join(cmd_checkout::WORKTREES).join("wsp-1").display().to_string()]);
        git_run(&repo, &["branch", "-d", "wsp-1"]);
        task(&store, "wsp-1", "review");

        let p = position(&store, &w, Reading::Landed);
        assert!(p.finished(), "a swept tree on a settled task is finished, not started");
    }

    /// Absence is somebody's decision and it must not stall a night. The id is
    /// still named, because the one thing nothing here may do is quietly edit
    /// the membership — that is the stale-plan failure with nobody left to
    /// notice it.
    #[test]
    fn a_member_the_store_has_never_heard_of_is_settled_and_still_named() {
        let (_env, store, _repo) = scratch("dangling");
        task(&store, "wsp-1", "review");
        let w = list("- 1  wsp-1  wsp-gone\n- 2  wsp-also-gone\n");

        let p = position(&store, &w, Reading::Landed);
        assert!(p.finished(), "a deleted task does not hold the barrier");
        assert_eq!(dangling(&store, &w), ["wsp-gone", "wsp-also-gone"], "and both are reported, ahead of the position");
        assert_eq!(
            member(&store, &mut Repos::new(&store), "wsp-gone", Reading::Landed).settlement,
            Settlement::Gone
        );
    }

    /// The point of referencing rather than owning, as a test: the `batch` moved
    /// 26 tasks into a project and back out again for want of this. A member
    /// that changes project is looked up fresh, so it lands in the repository it
    /// lives in now.
    #[test]
    fn a_member_moved_to_another_project_is_read_where_it_now_lives() {
        let (env, store, repo) = scratch("moved");
        let other = env.path("other");
        std::fs::create_dir_all(&other).unwrap();
        git_run(&other, &["init", "--quiet", "-b", "trunk"]);
        std::fs::write(other.join("kept.txt"), "one\n").unwrap();
        git_run(&other, &["add", "kept.txt"]);
        git_run(&other, &["commit", "--quiet", "-m", "first"]);
        let mut p = Project::new("herdr");
        p.roots = vec![other.display().to_string()];
        store.save_project(&p).unwrap();

        task(&store, "wsp-1", "review");
        committed(&repo, "wsp-1");
        let w = list("- 1  wsp-1\n");
        assert_eq!(position(&store, &w, Reading::Landed).at, Some(1), "unlanded where it was");

        let mut t = store.task("wsp-1").unwrap();
        t.project = Some("herdr".into());
        store.save_task(&t).unwrap();
        committed(&other, "wsp-1");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "and unlanded where it now is");
        assert_eq!(
            p.members[0].note(),
            "1 commit not on trunk",
            "the trunk branch is read from the repository, not assumed"
        );
        land(&other, "wsp-1");
        assert!(position(&store, &w, Reading::Landed).finished());
    }

    /// A task renumbered mid-run keeps its work on a branch of the name it had
    /// then — the same fact `checkout_dir` resolves for the directory. Without
    /// this the member reads as never having run and the barrier stalls on work
    /// that is already on the trunk.
    #[test]
    fn work_on_the_branch_a_task_used_to_be_called_still_counts() {
        let (_env, store, repo) = scratch("renamed");
        task(&store, "wsp-9", "review");
        committed(&repo, "old-3");
        std::fs::write(store.ids_path(), r#"{"old-3":"wsp-9"}"#).unwrap();
        let w = list("- 1  wsp-9\n");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "the work is on the old branch and it is not on the trunk");
        assert_eq!(p.members[0].note(), "1 commit not on master");

        land(&repo, "old-3");
        assert!(position(&store, &w, Reading::Landed).finished(), "and landing that branch finishes it");
    }

    /// Design-only work has no branch and never will, and there is nothing
    /// wrong with it — `wsp-095` sits in a group of phase one and produces
    /// prose. It finishes on the store, like anything else with nowhere to ask.
    #[test]
    fn a_member_with_no_repository_to_look_in_finishes_on_the_store() {
        let (_env, store, _repo) = scratch("no-repo");
        let mut t = Task::new("wsp-1", "wsp-1");
        t.status_raw = "todo".into();
        store.save_task(&t).unwrap(); // no project at all
        let w = list("- 1  wsp-1\n");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.members[0].landing, Some(Landing::NoRepo));
        assert_eq!(p.at, Some(1), "open work is not finished just because git cannot be asked");
        assert_eq!(p.members[0].note(), "no repository to look in");

        let mut t = store.task("wsp-1").unwrap();
        t.status_raw = "review".into();
        store.save_task(&t).unwrap();
        assert!(position(&store, &w, Reading::Landed).finished());
    }

    /// The free reading is free: the panel redraws four times a second and a
    /// git process per member is not in that budget. Asserted as the absence of
    /// the expensive answer rather than as a stopwatch, which is the fact the
    /// budget actually rests on.
    #[test]
    fn the_settled_reading_never_asks_git() {
        let (_env, store, repo) = scratch("free");
        task(&store, "wsp-1", "review");
        committed(&repo, "wsp-1");
        let w = list("- 1  wsp-1\n");

        let p = position(&store, &w, Reading::Settled);
        assert!(p.finished());
        assert_eq!(
            member(&store, &mut Repos::new(&store), "wsp-1", Reading::Settled).landing,
            None,
            "no branch question was asked"
        );
    }

    /// A worklist nobody has put a group in is not waiting on anything. The
    /// alternative — a position of 1 in a list of 0 — is a barrier with nothing
    /// behind it, and `next` would have to special-case it anyway.
    #[test]
    fn a_worklist_with_no_groups_is_finished_rather_than_stuck_at_the_front() {
        let (_env, store, _repo) = scratch("empty");
        let p = position(&store, &Worklist::new("batch", "Overnight batch"), Reading::Landed);
        assert!(p.finished());
        assert_eq!(p.of, 0);
    }
}
