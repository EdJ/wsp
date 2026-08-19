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
//! It also holds the one act in this design that *destroys* anything —
//! [`sweep`], which removes the working trees of the members a passed group
//! leaves behind. That lives here rather than beside the sweep it extends
//! because the licence and the barrier are one fact: the evidence that opens a
//! barrier is the evidence that clears the trees behind it, and two
//! computations under one `rm -rf` can disagree. The argument for it is on
//! [`sweep`], not repeated here.
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use crate::cmd_checkout;
use crate::cmd_verify::{git, toplevel};
use crate::model::{Group, Status, Task, Worklist};
use crate::resolve::Index;
use crate::store::Store;
use crate::util;

// The `#[allow(dead_code)]` markers this module was built under are all gone:
// the attribute was the convention's marker for "its caller has not landed
// yet", taken off one item at a time with the `use` that reads it rather than
// as a blanket allow over the module that would go on hiding something
// genuinely dead. The last of them came off with `wsp worklist next` and `go`,
// which are what the whole module was written for.

/// Which question is being asked of a member, because the two cost different
/// amounts and are allowed to give different answers. See the module docs.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub id: String,
    pub settlement: Settlement,
    /// `None` under [`Reading::Settled`], and for a member the store has never
    /// heard of — there is no branch to ask about and no project to ask it in.
    pub landing: Option<Landing>,
}

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

/// A member of a group behind the position, and which group that was.
///
/// The ordinal is a *position* and not an id — it is rewritten whenever a group
/// is inserted above it, which is why a log line names members rather than
/// numbers — so it is carried on an answer computed now and never written down.
/// It is here for one reason: see [`sweep`] and what it costs `--keep`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behind {
    pub group: usize,
    pub member: Standing,
}

/// Where a worklist is up to.
///
/// The ordinal is 1-based because it is the number written in the file and
/// typed at the command line, and it is a *position* rather than an id: it is
/// rewritten on every write, which is why a log line names the members and not
/// the number.
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
    /// Every member of every group *behind* the position, read.
    ///
    /// Carried back for a reason rather than for tidiness: the sweep a passed
    /// group licenses stands on exactly the fact that opened the barrier —
    /// the branch is on the trunk **and** a worklist says this group is behind
    /// it — so one walk has to answer both, or the destructive half is standing
    /// on a second computation that can disagree with the first.
    ///
    /// Every passed group and not only the one immediately behind, because the
    /// licence is word for word the same for all of them: a run where `--keep`
    /// was used once, or one that began before any of this existed, would
    /// otherwise leave trees that nothing ever comes back for. It costs nothing
    /// — these members were read on the way past.
    ///
    /// Each one says which group it was in, and that ordinal exists for exactly
    /// one reason: it is what lets [`sweep`] tell a caller that it is about to
    /// take trees an earlier `--keep` spared. See that function.
    pub passed: Vec<Behind>,
    /// Members of a group the run has already passed that do not read as
    /// finished. Empty in every ordinary run.
    ///
    /// This is the [`crate::model::Group::verdict`] floor's receipt, and it is
    /// carried out rather than swallowed. **Passing a barrier sweeps the trees
    /// behind it, and sweeping a tree deletes its branch** — `git branch -d`,
    /// which refuses unless the work is merged — so the evidence that opened
    /// the barrier is gone by design, and [`Standing::finished`] falls back on
    /// the store for a member with no branch: settled means it landed and was
    /// swept, unsettled means nothing ever ran for it.
    ///
    /// That fallback is right and it has one hole. A member whose branch landed
    /// but whose task never reached `review` — an agent that landed and then
    /// died, a status somebody forgot — reads as finished while its branch is
    /// there and as *never started* the moment the sweep takes it. Without the
    /// floor the position slips back onto a group already passed, and `wsp
    /// worklist next` offers to start members whose work is on the trunk: a
    /// second agent spawned onto landed work, which is the exact failure the
    /// barrier exists to prevent, caused by the barrier's own cleanup.
    ///
    /// So the walk does not stop below the floor — and it says who made it
    /// stop trying to. A member here is the disagreement this module exists to
    /// surface rather than resolve, and a caller that prints nothing about it
    /// is the floor quietly covering the thing it was put in to survive.
    pub slipped: Vec<Standing>,
    /// Which of the two questions was asked.
    ///
    /// Carried so that [`sweep`] can refuse the free answer. `Settled` is a
    /// task's status, which arrives before the commit is on the trunk — that is
    /// the `batch`'s costliest failure, and it must not be what a directory is
    /// removed on.
    pub reading: Reading,
}

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
///
/// # The one written thing it reads, and why that is not a contradiction
///
/// It never stops **below the floor**: the last group carrying a
/// [`crate::model::Group::verdict`], which is a barrier somebody passed. That
/// is a written fact on a record whose whole design is that its position is
/// derived — and it belongs here for the reason `status` does. Passing a
/// barrier is a *decision*, and a decision does not un-happen because a task's
/// status is missing. What is derived is where the work has got to; what is
/// written is which barriers a person has crossed, and the position is the
/// first group not finished **of those still in front of somebody**.
///
/// Without it the run goes backwards, and the mechanism is the barrier's own
/// cleanup — see [`Position::slipped`], which is where the members that caused
/// it are carried out to be printed.
///
/// It costs nothing in the ordinary case: no verdicts is a floor of zero and
/// the walk is exactly what it was.
pub fn position(store: &Store, w: &Worklist, reading: Reading) -> Position {
    let groups = w.groups();
    let floor = groups.iter().rposition(|g| !g.verdict.trim().is_empty()).map_or(0, |i| i + 1);
    let mut repos = Repos::new(store);
    let mut passed: Vec<Behind> = Vec::new();
    let mut slipped: Vec<Standing> = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let members = group(store, &mut repos, g, reading);
        let done = members.iter().all(Standing::finished);
        if !done && i + 1 > floor {
            return Position { at: Some(i + 1), of: groups.len(), members, passed, slipped, reading };
        }
        // Below the floor and not finished: walked past, and named. A group
        // whose barrier was passed goes on the `passed` list whatever its
        // members now say — `cmd_checkout::sweep_passed` judges every tree
        // again before it removes it, so a member that is genuinely not landed
        // is kept there and named rather than taken on this list's word.
        slipped.extend(members.iter().filter(|s| !s.finished()).cloned());
        passed.extend(members.into_iter().map(|member| Behind { group: i + 1, member }));
    }
    Position { at: None, of: groups.len(), members: Vec::new(), passed, slipped, reading }
}

/// Every member of one group, in the order the group names them.
pub fn group(store: &Store, repos: &mut Repos, g: &Group, reading: Reading) -> Vec<Standing> {
    g.members.iter().map(|m| member(store, repos, m, reading)).collect()
}

/// How one member stands.
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

/// Take away the trees of the members this worklist has passed.
///
/// **This is the one destructive thing in the whole worklist design, and the
/// only part of this module that is not a question.** A sentence belongs in the
/// module header saying so; it is here rather than there because two agents
/// were in this file at once on 2026-08-19 and neither was to edit line 1.
///
/// The evidence is exactly the evidence that opened the barrier, which is why
/// [`Position`] carries the members it passed on its way through rather than
/// sending the sweep to ask again: two computations under one act is two
/// answers that can disagree, and the one holding the `rm -rf` is the wrong one
/// to be second.
///
/// **The one place in this design that destroys directories on a predicate
/// without being asked**, so read [`cmd_checkout::Why::Landed`] before touching
/// the predicate: it is `Why::Idle` — nothing uncommitted, nothing the trunk
/// has not got — plus one piece of evidence, and it is narrower than `Idle`
/// rather than broader. What the worklist adds is that a member of a group it
/// has declared finished is not the tree made thirty seconds ago for an agent
/// who has not typed yet: something put an agent on it, and its work is on the
/// trunk.
///
/// The evidence has to be the expensive reading and the argument is not
/// symmetrical. `Settled` says a task is at `review`, which arrives *before*
/// the commit is on the trunk — that is the `batch`'s costliest failure, and it
/// is not a thing to remove a directory on. So a `Settled` position is refused
/// here rather than quietly re-read, because re-reading it would put a second
/// computation under the destructive half that can disagree with the first.
///
/// # `--keep` defers, it does not opt out, and the caller must say so
///
/// The caller's `--keep` is simply not calling this. That reads as an opt-out
/// and it is not one, because this sweeps **every** group behind the position
/// rather than only the one just crossed. So a barrier passed with `--keep`
/// spares its trees, and the *next* `go` takes them, because by then that group
/// is one more group behind and the licence for it is word for word the same:
///
/// ```text
/// barrier 1, --keep   nothing swept
/// barrier 2, normal   group 1 and group 2 both go
/// ```
///
/// That is a real cost and it is chosen rather than overlooked. Sweeping only
/// the group just crossed would leave a `--keep`ed group's trees with nothing
/// that ever comes back for them — the 18-worktree finding, reintroduced by the
/// flag meant to be careful. Making `--keep` *sticky* would need a written mark
/// on a group saying "kept", which is a field that can go stale on a record
/// whose whole design is that its position is derived and never written, and it
/// would still leak the trees at the end.
///
/// What is not acceptable is doing it quietly: somebody who kept a tree in
/// order to go and look at it loses it one barrier later without being told,
/// which is the failure `--keep` exists to prevent, delayed by one group. So
/// [`Sweep::earlier`] names exactly those trees and **the caller is obliged to
/// print them** — "sweeping 6 trees, 2 of them from groups passed earlier" is
/// the whole of the fix, and it arrives where the decision is being made. The
/// argument for sweeping automatically is that a manual procedure gets
/// abandoned; the answer to that is not to make the automatic one quieter.
///
/// What `--keep` still buys is real, and it is worth being clear that no work is
/// at stake either way: a removed tree comes back with `wsp checkout <id>`,
/// because `remove` deletes the branch with `-d` and git refuses that unless
/// the work is merged, and uncommitted work stops the removal outright. `--keep`
/// buys the convenience of looking, for one barrier.
///
/// Why this is automatic at all, since it is the question somebody will ask:
/// it was manual for two nights and happened zero times in them, leaving 18
/// worktrees and 19 orphaned workspaces in one of them. A procedure run by hand
/// at the end of a long piece of work is a procedure abandoned halfway when
/// something more interesting happens, which is the finding `stale`'s own
/// header already records. The barrier is also the only moment in a run when
/// nothing is in flight, which is why cleanup belongs here and could not have
/// belonged anywhere under the lanes this replaces. The caller opts out with
/// `--keep`, and the two refusals that are not optional — uncommitted work, and
/// a member still holding its claim — are [`cmd_checkout::sweep_passed`]'s.
///
/// Members the store has never heard of, and members with no project to look
/// in, are passed over: there is no repository to find a tree in. They are
/// still named by [`dangling`], which is a different question and a different
/// moment.
pub fn sweep(store: &Store, p: &Position, dry: bool) -> Result<Sweep, String> {
    if p.reading != Reading::Landed {
        return Err("a tree is removed on the landed reading and never on the settled one".into());
    }
    let mut repos = Repos::new(store);
    let mut members = Vec::new();
    for b in &p.passed {
        let Some(project) = store.task(&b.member.id).and_then(|t| t.project) else { continue };
        let Some(trunk) = repos.of(&project) else { continue };
        members.push(cmd_checkout::Passed {
            task: b.member.id.clone(),
            trunk: trunk.dir,
            trunk_branch: trunk.branch,
        });
    }
    let closed = cmd_checkout::finished(store);
    let occupied = cmd_checkout::Occupied::now(store);
    let swept = cmd_checkout::sweep_passed(&members, &closed, &|t, d| occupied.of(t, d), dry);

    // The group this barrier is the far side of. `at` is the first group not
    // finished, so the one just crossed is the one before it — and when nothing
    // is left, it is the last group there is.
    let crossed = p.at.map(|a| a - 1).unwrap_or(p.of);
    let earlier = swept
        .removed
        .iter()
        .filter(|t| p.passed.iter().any(|b| &&b.member.id == t && b.group < crossed))
        .cloned()
        .collect();
    Ok(Sweep { swept, earlier })
}

/// What one barrier's sweep did, and the part of it the caller has to say out
/// loud.
pub struct Sweep {
    /// What was removed, what branch outlived its tree, and what was left
    /// standing and why — [`cmd_checkout::sweep_passed`]'s own answer.
    pub swept: cmd_checkout::Swept,
    /// The removed trees that belonged to a group passed *before* the barrier
    /// just crossed. Named separately because these are the ones an earlier
    /// `--keep` spared, and the caller owes the reader a sentence about them.
    pub earlier: Vec<String>,
}

/// Every task a *running* worklist has passed, and the ids those tasks used to
/// have.
///
/// The same licence [`sweep`] acts on, in the shape `wsp checkout --sweep` and
/// `wsp doctor` want it: they walk directories and ask about names, where the
/// barrier walks members. Former ids are in it because a tree made before its
/// task was renumbered is still named after the id it had then, and a directory
/// walk only ever sees that name.
///
/// **Free when nothing is running**, which is nearly always: the store is read,
/// no worklist comes back `running`, and not one git process is started. That
/// is what makes it affordable on a command that is otherwise store-only.
pub fn passed_by_running(store: &Store) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let running: Vec<Worklist> = store.worklists().into_iter().filter(|w| w.status().is_running()).collect();
    if running.is_empty() {
        return out;
    }
    let renamed = store.renamed_ids();
    for w in &running {
        for s in position(store, w, Reading::Landed).passed {
            let id = s.member.id;
            out.extend(renamed.iter().filter(|(_, to)| *to == id.as_str()).map(|(from, _)| from.clone()));
            out.insert(id);
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

// ---- where a task sits in a run -----------------------------------------
//
// Membership rather than progress, and the routing step in front of
// `cmd_govern::seat_for`. Appended as one block: `worklist-005` and
// `worklist-006` are both adding to this file in the same group, and two
// blocks at the end of it merge where two sets of interleaved edits do not.

/// Where a task sits in a running worklist: which list, and which of its
/// groups.
///
/// **Membership, not progress.** `group` is the ordinal of the group this task
/// is a member of, which is what `group 2 of 4` on a task reads off, where
/// [`position`] answers the different question of which group the *run* has
/// got to. The two are the same number only while the front of the queue is
/// what is being worked, and a reader looking at one task wants the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placing {
    /// The slug, which is also the scope its seat is keyed on — see
    /// [`crate::cmd_govern::seat_for`], whose first step is this.
    pub list: String,
    /// 1-based, because it is the number printed and the number typed.
    pub group: usize,
    pub of: usize,
}

/// Every task in a *running* worklist, and where each one sits.
///
/// One read of `worklists/`, asked per task afterwards. Routing wants it once
/// per raised hand and `wsp flag` once per row, and a lookup that opened the
/// directory again for every question would read the same handful of files a
/// hundred times to answer `None` a hundred times.
///
/// **The ordinary state is empty and costs a failed `read_dir`.** That is the
/// same bargain the governors map already makes — no seats anywhere is one
/// missing file and every behaviour unchanged — and it is what lets the step
/// this feeds sit in front of routing without being paid for by a tree that
/// has never made a worklist.
///
/// **Only running lists are in it.** A draft or held list is a plan, and a task
/// may be in as many plans as somebody cares to write; it is the *run* that
/// gives one of them a claim on where a raised hand goes. A task that somehow
/// appears in two running lists answers with the first in slug order rather
/// than with neither — the verbs are what stop that happening, and a routing
/// question asked at 3am wants an answer, not a diagnosis.
///
/// [`Default`] is the ordinary state spelled out: nothing running, every
/// answer `None`. It is what a caller composing against a fixture wants and
/// what a store with no `worklists/` reads as.
#[derive(Default)]
pub struct Running {
    at: BTreeMap<String, Placing>,
}

impl Running {
    pub fn read(store: &Store) -> Running {
        let mut at: BTreeMap<String, Placing> = BTreeMap::new();
        for w in store.worklists().iter().filter(|w| w.status().is_running()) {
            let groups = w.groups();
            for (i, g) in groups.iter().enumerate() {
                for m in &g.members {
                    at.entry(m.clone()).or_insert(Placing {
                        list: w.id.clone(),
                        group: i + 1,
                        of: groups.len(),
                    });
                }
            }
        }
        Running { at }
    }

    pub fn of(&self, task: &str) -> Option<&Placing> {
        self.at.get(task)
    }

    /// The list a task is in, which is the only half routing needs.
    pub fn list_of(&self, task: &str) -> Option<&str> {
        self.at.get(task).map(|p| p.list.as_str())
    }
}

/// Where the run is up to, for a caller that has a slug rather than a record —
/// the seat line's half of [`Running`].
///
/// `None` where the slug is not a worklist, and where it is one that is not
/// running: a seat may be taken on a list before it starts, and a list that has
/// not started is not anywhere yet.
///
/// [`Reading::Settled`] because this is a *reading* verb's line. The landed
/// reading is a git process per member and belongs to the barrier, which is the
/// one caller whose answer has to be a fact rather than a view.
pub fn running_position(store: &Store, list: &str) -> Option<Position> {
    let w = store.worklist(list).filter(|w| w.status().is_running())?;
    Some(position(store, &w, Reading::Settled))
}

// ---- what a group that has just landed touched ---------------------------

/// Which members of a group put their hands on the same file.
///
/// Empty is the ordinary answer and it is the one worth having: it is the
/// composition rule holding, said as an observation rather than as an absence.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Overlap {
    /// One entry per file more than one member changed, and who changed it.
    /// Sorted by file, so two runs of the same group read the same.
    pub shared: Vec<(String, Vec<String>)>,
    /// Members whose change could not be read back at all — see
    /// [`cmd_checkout::Landings`], which distinguishes "changed nothing" from
    /// "nobody knows".
    ///
    /// **Named rather than dropped**, and that is the whole reason this field
    /// exists. A member nobody could read contributes no overlaps, so silently
    /// skipping it turns "we could not look" into "we looked and it was clean"
    /// — which is the `batch`'s absence-as-evidence, produced by the very
    /// report written to replace it.
    pub unread: Vec<String>,
}

/// What the members of a group that has just landed touched, and where two of
/// them touched the same file.
///
/// **Feedback on how the group was composed, not a check on it.** Mutual
/// exclusion is deliberately not machinery in this design — it is the rule
/// *do not put two tasks that touch the same file in one group*, which is
/// advice to whoever composes the next group. This is the one thing that stops
/// that advice being taken on faith: the `batch` ran fifteen agents with zero
/// land-time conflicts, which is evidence of nothing, because an absence cannot
/// be acted on. A named pair of members and the file they shared can be.
///
/// It arrives at the barrier because that is when the next group is being
/// composed, and it is read off the trunk rather than out of the trees, so it
/// still answers after the trees are gone. Called **before** the sweep all the
/// same: [`cmd_checkout::Landings::files`] finds a member by its branch tip,
/// and the sweep deletes the branch.
///
/// One `git reflog` per repository — a group's members are routinely in two or
/// three — and one `git diff --name-only` per member, which is the cost the
/// design priced.
pub fn overlaps(store: &Store, members: &[String]) -> Overlap {
    let mut repos = Repos::new(store);
    let mut logs: HashMap<PathBuf, cmd_checkout::Landings> = HashMap::new();
    let mut touched: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut out = Overlap::default();

    for id in members {
        // No project, no root, or no repository is not a member that could not
        // be read: it is design-only work with nothing to look in, and it is
        // passed over exactly as the sweep passes over it.
        let Some(trunk) = store.task(id).and_then(|t| t.project).and_then(|p| repos.of(&p)) else {
            continue;
        };
        let log = logs
            .entry(trunk.dir.clone())
            .or_insert_with(|| cmd_checkout::Landings::read(&trunk.dir, &trunk.branch));
        // The branch is asked for under every name the task has had, for the
        // reason `Repos::branches` gives: a tree made before a renumbering is
        // on a branch of the old id, and that is the ref the reflog holds.
        match repos.branches(id).iter().find_map(|b| log.files(&trunk.dir, b)) {
            None => out.unread.push(id.clone()),
            Some(files) => {
                for f in files {
                    touched.entry(f).or_default().push(id.clone());
                }
            }
        }
    }

    out.shared = touched.into_iter().filter(|(_, who)| who.len() > 1).collect();
    out
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

    /// The one piece of feedback the composition rule ever gets, proved against
    /// real branches that have already landed — which is the only state it is
    /// ever asked in, and the state where the graph no longer holds the answer.
    ///
    /// Three members: two that share a file and one that does not, with an
    /// unrelated commit landing on the trunk in the middle and every branch
    /// rebased on the way in. That is the arrangement the reflog reading exists
    /// for, and the arrangement a merge base cannot answer.
    #[test]
    fn a_group_that_has_landed_says_which_of_its_members_put_hands_on_one_file() {
        let (_env, store, repo) = scratch("overlap");
        // Room in the file, so two members can share it and still rebase clean.
        // A conflict is the other outcome of the same fact and it stops the
        // land rather than reaching this.
        std::fs::write(repo.join("shared.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        git_run(&repo, &["add", "shared.txt"]);
        git_run(&repo, &["commit", "--quiet", "-m", "shared"]);

        for (id, line) in [("wsp-1", 0usize), ("wsp-2", 9)] {
            task(&store, id, "review");
            let dir = committed(&repo, id);
            let mut lines: Vec<String> =
                std::fs::read_to_string(dir.join("shared.txt")).unwrap().lines().map(String::from).collect();
            lines[line] = id.to_string();
            std::fs::write(dir.join("shared.txt"), lines.join("\n") + "\n").unwrap();
            git_run(&dir, &["commit", "--quiet", "--all", "--message", "shared"]);
        }
        task(&store, "wsp-3", "review");
        committed(&repo, "wsp-3");
        // Somebody else's work, landing between two of the group's members.
        std::fs::write(repo.join("theirs.txt"), "not ours\n").unwrap();
        git_run(&repo, &["add", "theirs.txt"]);
        git_run(&repo, &["commit", "--quiet", "-m", "theirs"]);
        for id in ["wsp-1", "wsp-3", "wsp-2"] {
            git_run(&repo.join(cmd_checkout::WORKTREES).join(id), &["rebase", "--quiet", "master"]);
            land(&repo, id);
        }
        // A member with a repository to look in and no branch in it: nothing
        // ran for it, and what it changed is unknown rather than nothing.
        task(&store, "wsp-4", "review");

        let o = overlaps(&store, &["wsp-1".into(), "wsp-2".into(), "wsp-3".into(), "wsp-4".into()]);
        assert_eq!(
            o.shared,
            vec![("shared.txt".to_string(), vec!["wsp-1".to_string(), "wsp-2".to_string()])],
            "the file two of them shared, and only that file"
        );
        assert_eq!(o.unread, ["wsp-4"], "and the one nobody could place is named, not dropped");
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

    /// The sweep a passed group licenses, end to end, and the two halves of the
    /// predicate pulled apart. The group behind the barrier is swept; the group
    /// the barrier is waiting on is not touched, however landed a member of it
    /// happens to be — being behind the position is half the evidence and there
    /// is no such thing as three quarters of it.
    #[test]
    fn passing_a_barrier_sweeps_the_group_behind_it_and_not_the_one_it_waits_on() {
        let (_env, store, repo) = scratch("sweep");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "review");
        }
        // Group 1 is done with and on the trunk. Group 2 has one member landed
        // and one still holding commits, so the barrier stays shut.
        committed(&repo, "wsp-1");
        land(&repo, "wsp-1");
        committed(&repo, "wsp-2");
        land(&repo, "wsp-2");
        committed(&repo, "wsp-3");
        let w = list("- 1  wsp-1\n- 2  wsp-2  wsp-3\n");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(2), "the barrier is where it should be");
        assert_eq!(p.passed.iter().map(|b| b.member.id.as_str()).collect::<Vec<_>>(), ["wsp-1"]);
        assert_eq!(p.passed[0].group, 1, "and it remembers which group it was in");

        let out = sweep(&store, &p, false).expect("the landed reading licenses it");
        assert_eq!(out.swept.removed, ["wsp-1"]);
        assert!(out.earlier.is_empty(), "the group just crossed is not an earlier one");
        assert!(!repo.join(cmd_checkout::WORKTREES).join("wsp-1").exists(), "the passed tree is still here");
        assert!(
            repo.join(cmd_checkout::WORKTREES).join("wsp-2").join(".git").exists(),
            "a member of the group being waited on was swept"
        );
    }

    /// What `--keep` actually buys, asserted rather than assumed, because it is
    /// the half of the sweep that is easy to believe wrongly. Keeping a group's
    /// trees at one barrier does not keep them: the next barrier takes them,
    /// since by then that group is one further behind and the licence is word
    /// for word the same. So the flag **defers**, and the only thing that makes
    /// that honest is `earlier` naming the trees a caller has to mention —
    /// somebody who kept a tree to go and look at it must not lose it in
    /// silence one group later.
    #[test]
    fn keeping_a_groups_trees_at_one_barrier_defers_them_to_the_next() {
        let (_env, store, repo) = scratch("keep");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "review");
        }
        let w = list("- 1  wsp-1\n- 2  wsp-2\n- 3  wsp-3\n");
        let worktrees = repo.join(cmd_checkout::WORKTREES);
        // All three trees up front, because a member with no branch at all is a
        // member nothing has run for — worklist-002's own distinction — and the
        // barrier would settle on the store rather than on git.
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            committed(&repo, id);
        }

        // Barrier 1. `--keep` is the caller not calling us at all, so nothing
        // happens — which is exactly what the flag promises, at this barrier.
        land(&repo, "wsp-1");
        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(2), "group 1 has landed and group 2 has not");
        assert!(worktrees.join("wsp-1").join(".git").exists(), "nobody swept anything");

        // Barrier 2, passed without it. Group 1's tree goes too, because by now
        // it is one more group behind and the licence for it is the same — and
        // it is named apart from group 2's, which is the sentence the caller
        // owes whoever kept it in order to go and look at it.
        //
        // `wsp land` rebases before it fast-forwards, and so does this: the
        // trunk moved when group 1 landed.
        git_run(&worktrees.join("wsp-2"), &["rebase", "--quiet", "master"]);
        land(&repo, "wsp-2");
        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(3), "the barrier moved on");
        let out = sweep(&store, &p, false).expect("the landed reading licenses it");
        assert_eq!(out.swept.removed, ["wsp-1", "wsp-2"], "the deferred tree did not go with this one");
        assert_eq!(out.earlier, ["wsp-1"], "and the caller cannot say which one it kept last time");
        assert!(worktrees.join("wsp-3").join(".git").exists(), "the group being waited on was swept");
    }

    /// The one refusal that is about the *reading* rather than the tree, and it
    /// is the reason [`Position`] carries which question it answered. `Settled`
    /// says a task reached `review`, which arrives before the commit is on the
    /// trunk — the `batch`'s costliest failure — and a directory is not removed
    /// on that.
    #[test]
    fn a_tree_is_never_removed_on_the_free_reading() {
        let (_env, store, repo) = scratch("sweep-reading");
        task(&store, "wsp-1", "review");
        task(&store, "wsp-2", "todo");
        committed(&repo, "wsp-1");
        let w = list("- 1  wsp-1\n- 2  wsp-2\n");

        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(2), "the free reading has moved past a member that has not landed");
        assert!(sweep(&store, &p, false).is_err(), "a tree was removed on a status");
        assert!(repo.join(cmd_checkout::WORKTREES).join("wsp-1").join(".git").exists());
    }

    /// The same licence in the shape a directory walk wants it, and the reason
    /// it is affordable: a store with nothing running answers without starting
    /// one git process, which is what lets `wsp checkout --sweep` and `doctor`
    /// ask the question at all.
    #[test]
    fn only_a_running_worklist_licenses_a_sweep_and_a_quiet_store_costs_nothing() {
        let (_env, store, repo) = scratch("licence");
        task(&store, "wsp-1", "review");
        committed(&repo, "wsp-1");
        land(&repo, "wsp-1");
        task(&store, "wsp-2", "todo");

        let mut w = list("- 1  wsp-1\n- 2  wsp-2\n");
        store.save_worklist(&w).unwrap();
        assert!(passed_by_running(&store).is_empty(), "a draft plan licensed a removal");

        w.set_status(crate::model::WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
        assert_eq!(passed_by_running(&store).into_iter().collect::<Vec<_>>(), ["wsp-1"]);

        // The id the task used to have is in it too: a tree made before the
        // renumbering is named after that one, and a directory walk sees the
        // name and nothing else.
        std::fs::write(store.ids_path(), r#"{"old-3":"wsp-1"}"#).unwrap();
        assert_eq!(
            passed_by_running(&store).into_iter().collect::<Vec<_>>(),
            ["old-3", "wsp-1"],
            "a tree left under an old id is invisible to the sweep that should take it"
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

    /// Membership, read the way routing reads it: which list a task is in and
    /// which group of it — and **only while that list is running**.
    ///
    /// The status half is the load-bearing one. A draft list is a plan, and
    /// somebody planning tomorrow night into a second list must not move where
    /// tonight's raised hands are delivered; the run is what gives one list a
    /// claim on a task. `held` is the same rule read from the other end: a run
    /// somebody stopped has stopped answering for its members too.
    #[test]
    fn only_a_running_list_says_where_a_task_sits() {
        let (_env, store, _repo) = scratch("running");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "todo");
        }
        let mut w = list("- 1  wsp-1\n- 2  wsp-2  wsp-3\n");

        // Drafted and not started: a plan, and no claim on anything in it.
        store.save_worklist(&w).unwrap();
        assert_eq!(Running::read(&store).of("wsp-1"), None, "a draft list is not a run");

        w.set_status(crate::model::WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
        let r = Running::read(&store);
        assert_eq!(
            r.of("wsp-2"),
            Some(&Placing { list: "batch".into(), group: 2, of: 2 }),
            "the group it is a member of, not the group the run is at"
        );
        assert_eq!(r.list_of("wsp-1").as_deref(), Some("batch"));
        assert_eq!(r.of("wsp-9"), None, "a task nobody has listed");

        // Stopped. The members are still written down and the run is not
        // answering for them any more.
        w.set_status(crate::model::WorklistStatus::Held);
        store.save_worklist(&w).unwrap();
        assert_eq!(Running::read(&store).of("wsp-1"), None, "a held run has stopped answering");
    }

    /// The ordinary state, which is the one that has to cost nothing: a store
    /// that has never made a worklist reads as an empty map rather than as an
    /// error, and every question of it is `None`.
    #[test]
    fn a_store_with_no_worklists_is_simply_nothing_running() {
        let (_env, store, _repo) = scratch("none");
        task(&store, "wsp-1", "todo");
        assert_eq!(Running::read(&store).of("wsp-1"), None);
        assert_eq!(running_position(&store, "batch"), None, "and no list is anywhere");
    }

    /// Where the *run* is, for a caller holding a slug — and only for a list
    /// that is running. A seat may be taken on a list before it starts, which
    /// is how somebody comes to be sitting there to start it, and a list that
    /// has not started is not anywhere yet.
    #[test]
    fn a_run_has_a_position_and_a_plan_does_not() {
        let (_env, store, _repo) = scratch("run-position");
        for id in ["wsp-1", "wsp-2"] {
            task(&store, id, "todo");
        }
        let mut w = list("- 1  wsp-1\n- 2  wsp-2\n");
        w.set_status(crate::model::WorklistStatus::Running);
        store.save_worklist(&w).unwrap();

        let at = running_position(&store, "batch").expect("it is running");
        assert_eq!((at.at, at.of), (Some(1), 2));

        task(&store, "wsp-1", "review");
        assert_eq!(running_position(&store, "batch").unwrap().at, Some(2), "and it moves on its own");

        w.set_status(crate::model::WorklistStatus::Draft);
        store.save_worklist(&w).unwrap();
        assert_eq!(running_position(&store, "batch"), None, "a plan is not a run");
    }
}
