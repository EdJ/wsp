//! Where a worklist is up to, and the two readings of "finished".
//!
//! Nothing here is stored. A worklist's file holds one piece of state —
//! `status`, `draft | running | held | done` — because that is a *decision*
//! somebody takes: to start it, to stop starting things, to be finished with
//! it. **Where the run is up to is derived and never written**: the position is
//! the first group whose barrier nobody has passed, and what is holding that
//! group is computed from the tasks and from git every time it is asked for.
//! The evidence is the `batch` handbook, whose written table of where the night
//! had got to disagreed with what was actually happening inside the hour while
//! the membership stayed true. A status you can compute cannot go stale, and
//! this module is the computation.
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
//! # The two answers git has that the predicate as designed did not
//!
//! `cmd_checkout::ahead()` answers "what does this branch hold that the trunk
//! has not", and it answers **nothing** in three different situations: the
//! branch landed, there is no such branch, and the branch is there holding
//! nothing. The first is the only one that is finished, and the other two both
//! open a barrier on work that has not happened — the one failure a barrier
//! exists to prevent (`wsp-088` d4). So neither is left as a silent zero:
//! the branch is asked about before it is compared ([`Landing::NoBranch`]),
//! and an empty comparison is [`Landing::Nothing`], which is a reading and not
//! a verdict.
//!
//! The empty branch is not an edge case, it is **every branch between `wsp
//! spawn` and the first commit**: `wsp checkout` cuts the branch at the trunk
//! tip, so a whole group reads as landed from the moment its agents are given
//! their trees. Driven against a store and a repository of their own on
//! 2026-08-19, five spawned agents with nothing committed anywhere produced
//! `group 1 of 3 finished`, and four spawned agents produced a group of one
//! with the other four silently gone from it. Fixtures never saw it because a
//! fixture builds branches that already have commits on them.
//!
//! Both answers are genuinely ambiguous, and **the store settles both the same
//! way**. A branch is deleted with `git branch -d`, which refuses unless the
//! work is merged, so a *missing* branch is either work that landed and was
//! swept or work that never began; a branch holding nothing is either that
//! same landed work before the sweep took it, or a tree somebody is standing in
//! this minute. A landed branch and a branch cut from the trunk tip are the
//! same object, and no question put to git separates them. So a member is
//! finished only if it is also settled — nobody has finished a task whose
//! branch holds nothing and whose status is still `doing`.
//!
//! That is **one reading either side of the sweep** rather than two, which is
//! worth more than the defect it closes: a member that landed without reaching
//! `review` used to read finished while its branch stood and never-started the
//! moment the sweep deleted it. See [`Position::slipped`], which exists to
//! survive that flip.
//!
//! What it costs is that a member whose work is on the trunk holds the barrier
//! until somebody says `wsp review`. That is the narrow half of "a fact, not a
//! status anybody must remember to set": the fact still decides wherever git
//! has one, and this is exactly the state where git has none. The alternative
//! — a branch with no commits is not landed — is worse and was rejected:
//! `worklist-008` produces prose in the store and not one line of code, so its
//! branch will never hold a commit, and a predicate demanding one stalls every
//! design-only member for ever. [`Landing::NoRepo`] exists because design-only
//! work is legitimate, and this must not take that back.
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
    /// What the store says about a task already in hand.
    ///
    /// [`member`] asks the same question by id, and this is the half of it that
    /// is pure. A caller holding the whole task list — the watch's level read
    /// sweeps it once a tick — must not go back to the store per member, and
    /// must not spell *settled* out a second time: `review` is where worklist
    /// work stops, and a second copy of that sentence is how the two come to
    /// disagree.
    pub fn of(t: &Task) -> Settlement {
        match t.status() {
            Status::Review => Settlement::Review,
            Status::Done => Settlement::Closed,
            other => Settlement::Open(other),
        }
    }

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
    /// The branch is there and holds nothing the trunk has not got. **Landed
    /// work and a branch `wsp checkout` cut at the trunk tip are the same
    /// object**, so this is a reading and not a verdict: [`Standing`] settles
    /// it against the store exactly as it does [`NoBranch`], and for the same
    /// reason. See the module docs.
    Nothing,
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
    /// **Only one answer decides on git alone**, and it is the negative one:
    /// commits the trunk has not got are a fact, and nothing in the store
    /// overrides them. Every other answer falls back on the store, which is
    /// the judgement the module docs argue for — a branch that is missing, or
    /// is there holding nothing, is landed work when the task is settled and
    /// work that never began when it is not.
    pub fn finished(&self) -> bool {
        match &self.landing {
            None => self.settlement.settled(),
            Some(Landing::Ahead { .. }) => false,
            Some(Landing::Nothing | Landing::NoBranch | Landing::NoRepo) => {
                self.settlement.settled()
            }
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
            // Both halves of the ambiguity, in that order, because the reader
            // at a barrier is nearly always looking at the first one: a tree
            // handed out minutes ago with nothing committed in it yet. The
            // second half is why the sentence does not simply say "not
            // started" — it may be work that is on the trunk and never reached
            // `review`, and telling somebody to go and spawn that is the
            // failure this predicate exists to prevent.
            Some(Landing::Nothing) if !self.settlement.settled() => {
                "nothing committed on its branch — or landed and never marked".into()
            }
            // Both halves here too, and for the reason `Nothing` gives above:
            // a branch is deleted with `-d`, which succeeds *because* the work
            // is merged, so a member closed by `wsp despawn` after it landed
            // arrives here with its task still open. Saying only "nothing has
            // run for it" sends a reader to spawn work that is on the trunk,
            // which is the very failure the sentence next door was written to
            // avoid, and it disagreed with `NoBranch`'s own doc.
            Some(Landing::NoBranch) if !self.settlement.settled() => {
                "no branch — nothing has run for it, or it landed and was swept".into()
            }
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
    /// The first group whose barrier has not been passed, or `None` when every
    /// one of them has.
    ///
    /// **Not the first group whose members are finished.** Those are two
    /// different events and the distance between them is where a reading goes;
    /// see [`position`], and [`at_barrier`](Position::at_barrier) for the state
    /// that used to be drawn as a group already behind the run.
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
    /// the branch is on the trunk **and** somebody wrote a verdict on the group
    /// — so one walk has to answer both, or the destructive half is standing
    /// on a second computation that can disagree with the first.
    ///
    /// **The second half of that sentence used to be "and its members read as
    /// finished", which is not a barrier being passed and is not anybody's
    /// decision.** Driven on 2026-08-20: a two-group run whose first group had
    /// landed and whose barrier nobody had read, and `wsp checkout --sweep`
    /// took both of its trees and both of its branches with no `go` ever run —
    /// the licence for it manufactured by the walk itself. Nothing committed
    /// was lost, because every refusal below it held, and the trees a person
    /// was standing at that barrier to read were gone anyway. Groups below the
    /// floor are the whole of this list now.
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
    /// died, a status somebody forgot — reads as unfinished, and goes on
    /// reading that way after the sweep has taken the branch that was the only
    /// argument against it. Without the floor the position slips back onto a
    /// group already passed, and `wsp worklist next` offers to start members
    /// whose work is on the trunk: a second agent spawned onto landed work,
    /// which is the exact failure the barrier exists to prevent, caused by the
    /// barrier's own cleanup.
    ///
    /// It used to be worse and it is worth recording which half was fixed
    /// where. That member read *finished* while its branch stood and flipped to
    /// never-started when the sweep deleted it, so the barrier opened on the
    /// strength of a reading the sweep then destroyed. Settling `Nothing`
    /// against the store made the two sides of the sweep agree — the barrier no
    /// longer opens there at all — which leaves this floor holding one case
    /// rather than two: a group whose verdict is already written.
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
    /// Every barrier passed. Not the same as the worklist being `done`, which
    /// is somebody saying there is nothing left to want from it — and not the
    /// same as every group's *work* being over, which is [`at_barrier`] on the
    /// last group there is.
    ///
    /// That last distinction is the one that cost something. This used to
    /// answer true the moment the final group's members finished, so a list
    /// whose last barrier nobody had ever read reported itself complete — and
    /// a list that says *every one of 4 finished* is a list nobody goes back
    /// to, which makes the final group's verdict the one most likely to be
    /// skipped and the one a person is most likely to read later as the
    /// summary of the whole run.
    ///
    /// [`at_barrier`]: Position::at_barrier
    pub fn finished(&self) -> bool {
        self.at.is_none()
    }

    /// The members holding the current group, in the order the group names
    /// them. What a barrier reports when it will not open.
    pub fn holding(&self) -> Vec<&Standing> {
        self.members.iter().filter(|s| !s.finished()).collect()
    }

    /// The group at the position has nothing left holding it, so what the run
    /// is waiting for is the barrier and not the work.
    ///
    /// **The third state, and the one no surface could draw.** `at` says which
    /// group, `holding` says what is still outstanding in it, and between them
    /// there was no way to tell a group whose work was done and whose verdict
    /// nobody had written from a group that had been passed — so every surface
    /// drew it as passed. It is not a fourth thing to compute: the members are
    /// already read and carried, and this reads them.
    ///
    /// **It answers under the reading it was asked with**, which matters here
    /// more than anywhere else in the module. Under [`Reading::Settled`] it
    /// says the store has nothing outstanding, which arrives before the commit
    /// is on the trunk; only [`Reading::Landed`] may be spoken of as a barrier
    /// about to open, and a caller on the free reading owes its sentence that
    /// caveat.
    ///
    /// A group with no members at all is at its barrier from the moment the
    /// run reaches it, which is the honest answer: there is nothing in it to
    /// wait for and somebody still has to say so.
    pub fn at_barrier(&self) -> bool {
        self.at.is_some() && self.holding().is_empty()
    }
}

/// Where the run is up to: the first group whose barrier has not been passed,
/// under `reading`.
///
/// # A barrier is crossed by `go`, and the members finishing is not that event
///
/// This walked past a group the moment its members read as finished, and that
/// is `worklist-049`. Two different facts were being used as one: *the work
/// landed*, which git and the store answer, and *a barrier was crossed*, which
/// only a person does and which the [`crate::model::Group::verdict`] is the
/// record of. The whole design exists to put a reading between them — so a
/// group whose work is done and whose verdict nobody has written is not behind
/// the run. It is where the run is standing, and [`Position::at_barrier`] is
/// what says so.
///
/// Driven on 2026-08-20 against a two-group list. `wsp worklist show` reported
/// `at group 2 of 2` and drew `✓` on the group `wsp worklist next` was at that
/// same minute asking for a sentence about. With the second group landed as
/// well it said *every one of 2 finished*, and the `go` that followed wrote a
/// verdict on the last group only: the first barrier's stop condition was
/// never put to anybody and that group carries no verdict to this day. The
/// barrier was not merely mis-drawn, it was skipped.
///
/// # So the position is the floor, and that is the whole of it
///
/// `floor` is the group after the last one carrying a verdict. Because it is
/// the *last* such group, nothing above it has one, and "the first group not
/// finished, of those still in front of somebody" and "the first group whose
/// barrier is shut" stop being two questions. Collapsing them is the fix.
///
/// It is still derived and still never written. What it is derived from is the
/// record of which barriers a person crossed rather than a status somebody has
/// to remember to set, which is the same argument `status` makes one field up.
/// Where the *work* has got to is the members carried back on the answer, read
/// every time and written down nowhere — the `batch` handbook's failure is
/// closed where it was always closed.
///
/// # What it costs
///
/// Groups ahead of the position are not asked about at all, so the expensive
/// reading is one git process per member of the group being waited on and of
/// the ones behind it. That is what it always was and it is now strictly less:
/// the walk stops at the floor instead of running on through every group whose
/// members happened to be finished.
///
/// A worklist with no groups is finished, which is the honest answer: there is
/// nothing left in it to start.
///
/// # Below the floor
///
/// [`Position::slipped`] is unchanged and is still needed. A group *below* the
/// floor whose members do not read as finished is a barrier somebody crossed
/// over work that is not there — the run does not go back for it, and it is
/// carried out to be printed rather than swallowed.
///
/// # The one list where the two questions do not diverge
///
/// **A list nobody has started has no barrier behind any of its groups.** The
/// only barrier a draft has is the one in front of group 1, which is the list's
/// own `## Overview` — see `Gate::Start`. So there is nothing there for the
/// walk to stop at, and the honest answer for a plan is the one somebody
/// reading it wants: the first group not finished, which is where the run will
/// begin. `wsp worklist go` stamps everything before that as having been
/// finished before the list existed, and from the moment it does, this is the
/// barrier walk for the rest of the run.
pub fn position(store: &Store, w: &Worklist, reading: Reading) -> Position {
    let groups = w.groups();
    let draft = w.status() == crate::model::WorklistStatus::Draft;
    let floor = groups.iter().rposition(|g| !g.verdict.trim().is_empty()).map_or(0, |i| i + 1);
    let mut repos = Repos::new(store);
    let mut passed: Vec<Behind> = Vec::new();
    let mut slipped: Vec<Standing> = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let members = group(store, &mut repos, g, reading);
        // The first group nobody has written a verdict on — or, on a plan with
        // no barriers in it yet, the first one that is not finished. Whether
        // the group being stopped at is finished is a different answer, and it
        // is on the record this returns rather than in this condition.
        let stop = match draft {
            true => !members.iter().all(Standing::finished),
            false => i + 1 > floor,
        };
        if stop {
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
///
/// The read is [`Store::task_now`] and not [`Store::task`], and this is the
/// site that argues for it hardest: `Settlement::Gone` is *settled*, so a
/// member the store could not find stopped holding its group, the run walked
/// on past work still sitting on its branch, and `go` wrote "no task answers
/// to <old id>" into the log where it reads later as evidence somebody
/// archived it. Reproduced 2026-08-19 on `worklist-015`.
///
/// `Standing::id` is the id the task answers to **now**, which is not always
/// the id the worklist names it by. It is the current one because that is the
/// identity everything downstream is keyed on: a tree is named after the id it
/// was cut under, and [`passed_by_running`] expands the current id *backwards*
/// through `ids.json` to reach the older names a directory walk sees. Handing
/// it the older name gives it nothing to expand and loses the newer one.
pub fn member(store: &Store, repos: &mut Repos, id: &str, reading: Reading) -> Standing {
    let task = store.task_now(&repos.renamed, id);
    let settlement = match &task {
        None => Settlement::Gone,
        Some(t) => Settlement::of(t),
    };
    let landing = match (reading, &task) {
        (Reading::Settled, _) => None,
        (Reading::Landed, None) => None,
        (Reading::Landed, Some(t)) => Some(landing(repos, t)),
    };
    let id = task.map_or_else(|| id.to_string(), |t| t.id);
    Standing { id, settlement, landing }
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
    // Read once for the whole list rather than per member, as
    // `passed_by_running` reads it: a list is a handful of groups of a handful
    // of ids, and this is asked on a surface that redraws.
    let renamed = store.renamed_ids();
    for g in w.groups() {
        for m in g.members {
            if store.task_now(&renamed, &m).is_none() && !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
}

/// Take away the trees of the members behind this barrier, and of the group
/// whose barrier is being crossed.
///
/// **The group being crossed is the one the position is standing at**, not the
/// one before it: `at` is the first group nobody has written a verdict on, so
/// the group whose trees this barrier releases is on [`Position::members`] and
/// not yet on [`Position::passed`]. `worklist-049` moved that line, and reading
/// the sweep off `passed` alone would now leave every crossed group's trees
/// standing where the last thing anybody wanted was more of them.
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
    // Every group behind the position, **and the group at it**. `at` is the
    // first group nobody has written a verdict on, so the group whose barrier
    // this call is crossing is the one the position is standing at rather than
    // the one before it — and it is on `members`, not on `passed`. Taken only
    // when nothing is holding it: a group still being worked is not a group
    // whose barrier anybody is crossing, and this is the end of the design
    // that removes directories.
    let crossing = p.at_barrier().then(|| p.members.iter()).into_iter().flatten();
    for m in p.passed.iter().map(|b| &b.member).chain(crossing) {
        // `m.id` is already the id the task answers to now — `member` resolved
        // it — but the read goes through `Store::task_now` anyway so that
        // nothing here depends on that having been done upstream.
        let Some(project) = store.task_now(&repos.renamed, &m.id).and_then(|t| t.project) else {
            continue;
        };
        let Some(trunk) = repos.of(&project) else { continue };
        members.push(cmd_checkout::Passed {
            task: m.id.clone(),
            trunk: trunk.dir,
            trunk_branch: trunk.branch,
        });
    }
    let closed = cmd_checkout::finished(store);
    let occupied = cmd_checkout::Occupied::now(store);
    let swept = cmd_checkout::sweep_passed(&members, &closed, &|t, d| occupied.of(t, d), dry);

    // The group this barrier is the far side of, which is the one the position
    // is standing at — and when there is no position left, the last group
    // there is. Its own members are on `p.members`, so the filter below never
    // finds them on `passed` and never calls them earlier work.
    let crossed = p.at.unwrap_or(p.of);
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
/// barrier walks members.
///
/// **Passed means a verdict was written on the group**, and this is the caller
/// where that matters most, because it is the one that hands a licence to a
/// command run by somebody who is not at the barrier at all. It used to mean
/// *the members read as finished*, and a `wsp checkout --sweep` typed in the
/// repository then took the trees and the branches of a group whose stop
/// condition nobody had answered, with no `go` ever run. Reproduced
/// 2026-08-20; see [`Position::passed`].
///
/// It deliberately does **not** include the group at the position, however
/// finished its work reads. That group's trees go when its barrier is crossed
/// and [`sweep`] takes them, which is the moment somebody is looking.
///
/// Former ids are in it because a tree made before its task was renumbered is
/// still named after the id it had then, and a directory walk only ever sees
/// that name.
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

    // And this is the state `ahead()` alone does not have either: an empty
    // answer on a branch that exists is landed work and is equally a branch cut
    // at the trunk tip and never committed to, which is every member of a group
    // between `wsp spawn` and its first commit. Neither this nor `NoBranch` is
    // a verdict; `Standing::finished` settles both against the store.
    match cmd_checkout::ahead(&trunk.dir, &trunk.branch, &branch).len() {
        0 => Landing::Nothing,
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
/// composed, and it is read off the trunk's reflog rather than out of the
/// trees or off the branches, so it answers just as well after the sweep as
/// before it — and after the despawn that ends a member, which is the state it
/// was silently blind to until `worklist-024`.
///
/// One `git reflog` per repository — a group's members are routinely in two or
/// three — and one `git diff --name-only` per *land*, which is one per member
/// for all but the member that landed twice. That is the cost the design
/// priced, and it did not move much when the arity did.
pub fn overlaps(store: &Store, members: &[String]) -> Overlap {
    let mut repos = Repos::new(store);
    let mut logs: HashMap<PathBuf, cmd_checkout::Landings> = HashMap::new();
    let mut touched: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut out = Overlap::default();

    for id in members {
        // Through `Store::task_now`, because these are the raw member ids out
        // of the worklist's own text and a renumbered one is not at the path
        // it names. It is also what makes the `branches` call below work: that
        // expands the *current* id backwards into the names a tree may have
        // been cut under, so asking it with the worklist's older name gives it
        // nothing to expand and misses the branch under the newer one.
        //
        // No project, no root, or no repository is not a member that could not
        // be read: it is design-only work with nothing to look in, and it is
        // passed over exactly as the sweep passes over it.
        let Some(task) = store.task_now(&repos.renamed, id) else { continue };
        let Some(trunk) = task.project.as_deref().and_then(|p| repos.of(p)) else { continue };
        let log = logs
            .entry(trunk.dir.clone())
            .or_insert_with(|| cmd_checkout::Landings::read(&trunk.dir, &trunk.branch));
        // The branch is asked for under every name the task has had, for the
        // reason `Repos::branches` gives: a tree made before a renumbering is
        // on a branch of the old id, and that is the ref the reflog holds.
        //
        // **Every name, not the first one that answers.** A task renumbered
        // between two lands has one land under each name, and stopping at the
        // first reads the newer and drops the older — the same "one is all
        // there is" mistake `Landings::files` had within a single name, one
        // level up. A name that never landed answers `None` here legitimately,
        // because it is a name and not a claim, so it is *no* name answering
        // that means nobody could place the member.
        let mut files: BTreeSet<String> = BTreeSet::new();
        let mut placed = false;
        for b in repos.branches(&task.id) {
            if let Some(f) = log.files(&trunk.dir, &b) {
                placed = true;
                files.extend(f);
            }
        }
        if !placed {
            out.unread.push(id.clone());
        }
        for f in files {
            touched.entry(f).or_default().push(id.clone());
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

    /// A tree on a branch of the task's name with **nothing committed on it**,
    /// which is what `wsp checkout` leaves and what every member of a group
    /// looks like between its spawn and its first commit.
    ///
    /// The one state a fixture does not build by accident, and the reason this
    /// helper exists next to `committed`: `worklist-007` drove all four
    /// landings twice against fixtures whose branches all had commits on them,
    /// and the defect that opened every barrier early was invisible in both
    /// passes.
    fn spawned(repo: &Path, id: &str) -> PathBuf {
        let dir = repo.join(cmd_checkout::WORKTREES).join(id);
        let from = cmd_checkout::trunk_branch(repo).expect("the repository is on a branch");
        git_run(repo, &["worktree", "add", "--quiet", "-b", id, &dir.display().to_string(), &from]);
        dir
    }

    fn land(repo: &Path, id: &str) {
        git_run(repo, &["merge", "--ff-only", "--quiet", id]);
    }

    /// A list that is **running**, because that is what the barrier walk is a
    /// walk over. A draft has no barrier behind any of its groups and takes the
    /// other arm of [`position`] entirely, so the tests that are about a plan
    /// rather than a run say `Draft` out loud.
    fn list(groups: &str) -> Worklist {
        let mut w = Worklist::new("batch", "Overnight batch");
        w.body = format!("## Groups\n{groups}");
        w.set_status(crate::model::WorklistStatus::Running);
        w
    }

    /// A barrier somebody passed, written the way `wsp worklist go` writes it.
    /// The one written fact the position reads, so a test that wants the run to
    /// have got past a group has to say that a person took it past.
    fn passed(w: &mut Worklist, n: usize, said: &str) {
        let mut groups = w.groups();
        groups[n - 1].verdict = format!("2026-08-20T09:00:00Z {said}");
        w.set_groups(&groups);
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

    /// The same root, in the report rather than in the predicate. A branch that
    /// landed nothing is sitting on the trunk value it was cut from, so a
    /// search on the tip alone finds it in the reflog and hands it the diff of
    /// whatever really landed there — the trunk's own commit, reported as the
    /// work of members who had committed nothing at all.
    ///
    /// It is the same evidence and the same mistake as the barrier's, so it is
    /// fixed with it rather than separately: the entry has to *name* the branch.
    #[test]
    fn a_member_that_landed_nothing_is_not_handed_the_trunks_own_commit() {
        let (_env, store, repo) = scratch("empty-overlap");
        // Two entries in the trunk's reflog, so there is something below the
        // tip for the diff to reach for.
        std::fs::write(repo.join("theirs.txt"), "not ours\n").unwrap();
        git_run(&repo, &["add", "theirs.txt"]);
        git_run(&repo, &["commit", "--quiet", "-m", "theirs"]);

        for id in ["wsp-1", "wsp-2"] {
            task(&store, id, "review");
            spawned(&repo, id);
        }

        let o = overlaps(&store, &["wsp-1".into(), "wsp-2".into()]);
        assert!(o.shared.is_empty(), "neither of them touched a file, let alone the same one");
        assert_eq!(o.unread, ["wsp-1", "wsp-2"], "and not knowing is said, not smoothed over");
    }

    /// The state the report was written for, and the one it could not read: a
    /// member whose tree was taken away after it landed.
    ///
    /// `wsp despawn` ends an agent by removing its tree, and `remove` deletes
    /// the branch with `git branch -d`, which succeeds *because* the work is
    /// merged. So the ordinary close of a member's work is exactly the state
    /// where a branch lookup answers nothing — and the barrier reported the
    /// one member it could still see as sharing a file with nobody.
    ///
    /// The reflog is untouched by any of that, and since `worklist-013` the
    /// entry names the branch, so the name is the whole lookup.
    #[test]
    fn a_member_whose_tree_was_taken_away_after_it_landed_is_still_read() {
        let (_env, store, repo) = scratch("swept");
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
            git_run(&dir, &["rebase", "--quiet", "master"]);
            land(&repo, id);
        }
        // What `despawn` leaves behind, in the order `remove` does it.
        let dir = repo.join(cmd_checkout::WORKTREES).join("wsp-1");
        git_run(&repo, &["worktree", "remove", "--force", &dir.display().to_string()]);
        git_run(&repo, &["branch", "-d", "wsp-1"]);

        let o = overlaps(&store, &["wsp-1".into(), "wsp-2".into()]);
        assert_eq!(
            o.shared,
            vec![("shared.txt".to_string(), vec!["wsp-1".to_string(), "wsp-2".to_string()])],
            "the branch is gone and the reflog still names what it landed"
        );
        assert!(o.unread.is_empty(), "and nothing about it is unreadable");
    }

    /// Land, review, land again is the ordinary shape of a member's work, and
    /// every land is a separate entry in the trunk's reflog. Reading one of
    /// them reports a fraction of what the member touched — and the fraction
    /// that is missing is where the overlap was, so a real collision prints as
    /// `none`, which is the clean bill of health this report exists to refuse.
    #[test]
    fn a_member_that_landed_twice_is_read_on_both_of_its_lands() {
        let (_env, store, repo) = scratch("twice");
        task(&store, "wsp-1", "review");
        task(&store, "wsp-2", "review");

        let first = committed(&repo, "wsp-1"); // wsp-1.txt
        std::fs::write(first.join("early.txt"), "mine\n").unwrap();
        git_run(&first, &["add", "."]);
        git_run(&first, &["commit", "--quiet", "-m", "early"]);
        land(&repo, "wsp-1");
        // The second land, with a file the companion never touches: the newest
        // entry alone says these two members share nothing.
        std::fs::write(first.join("late.txt"), "mine\n").unwrap();
        git_run(&first, &["add", "."]);
        git_run(&first, &["commit", "--quiet", "-m", "late"]);
        land(&repo, "wsp-1");

        let second = committed(&repo, "wsp-2");
        git_run(&second, &["rebase", "--quiet", "master"]);
        std::fs::write(second.join("early.txt"), "theirs\n").unwrap();
        git_run(&second, &["commit", "--quiet", "--all", "--message", "early too"]);
        land(&repo, "wsp-2");

        let o = overlaps(&store, &["wsp-1".into(), "wsp-2".into()]);
        assert_eq!(
            o.shared,
            vec![("early.txt".to_string(), vec!["wsp-1".to_string(), "wsp-2".to_string()])],
            "the file they shared is on wsp-1's first land, not its last"
        );
    }

    /// The same arity fault one level up, where the branch is asked for under
    /// every name the task has had. A member that lands, gets renumbered, and
    /// lands again from a fresh tree has one entry under each name, and taking
    /// the first name that answers drops the other land entirely.
    #[test]
    fn a_member_renumbered_between_two_lands_is_read_under_both_of_its_names() {
        let (_env, store, repo) = scratch("renamed");
        task(&store, "wsp-1", "review");
        task(&store, "wsp-2", "review");

        // The first land, under the id the tree was cut with.
        let old = committed(&repo, "wsp-9"); // wsp-9.txt
        std::fs::write(old.join("early.txt"), "mine\n").unwrap();
        git_run(&old, &["add", "."]);
        git_run(&old, &["commit", "--quiet", "-m", "early"]);
        land(&repo, "wsp-9");
        store.rename_tasks(&BTreeMap::from([("wsp-9".to_string(), "wsp-1".to_string())])).unwrap();

        // And a second, from a tree cut under the new one — which is what
        // `ensure` does for a task whose first tree is gone.
        let new = committed(&repo, "wsp-1"); // wsp-1.txt, off the trunk
        land(&repo, "wsp-1");

        let second = committed(&repo, "wsp-2");
        git_run(&second, &["rebase", "--quiet", "master"]);
        std::fs::write(second.join("early.txt"), "theirs\n").unwrap();
        git_run(&second, &["commit", "--quiet", "--all", "--message", "early too"]);
        land(&repo, "wsp-2");
        assert!(new.is_dir(), "both trees stood the whole time");

        let o = overlaps(&store, &["wsp-1".into(), "wsp-2".into()]);
        assert_eq!(
            o.shared,
            vec![("early.txt".to_string(), vec!["wsp-1".to_string(), "wsp-2".to_string()])],
            "the shared file is on the land the member made under its old id"
        );
    }

    /// The whole of what "derived" means, asserted as arithmetic — and the line
    /// between the two things that used to be one number.
    ///
    /// **Where the run is** moves when somebody passes a barrier. **What is
    /// holding the group in front of it** moves because the tasks moved. Both
    /// are computed every time and neither is written down, which is the
    /// `batch` handbook's failure made impossible rather than warned about.
    /// Running them together is `worklist-049`: the run walked on past a
    /// barrier nobody had read the moment its members went quiet.
    #[test]
    fn the_position_is_the_first_barrier_nobody_has_passed() {
        let (_env, store, _repo) = scratch("derived");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "todo");
        }
        let mut w = list("- 1  wsp-1\n- 2  wsp-2  wsp-3\n");

        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(1), "nothing is finished, so it is at the front");
        assert_eq!(p.of, 2);
        assert_eq!(p.members.len(), 1, "and it carries back the group being waited on");
        assert!(!p.at_barrier(), "what it is waiting on is the work");

        task(&store, "wsp-1", "review");
        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(1), "the members finishing is not a barrier being crossed");
        assert!(p.at_barrier(), "and what it waits on now is the barrier, not the work");
        assert!(p.passed.is_empty(), "nothing is behind a barrier nobody has passed");

        passed(&mut w, 1, "the two of them agreed");
        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(2), "somebody passed it, and that is what moves a run");
        assert!(!p.at_barrier());

        task(&store, "wsp-2", "done");
        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(2), "one member of two is not the group");
        assert_eq!(p.holding().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["wsp-3"]);

        task(&store, "wsp-3", "review");
        let p = position(&store, &w, Reading::Settled);
        assert!(p.at_barrier(), "the work is over");
        assert!(
            !p.finished(),
            "and a list whose last barrier nobody has read is not a list that has finished"
        );

        passed(&mut w, 2, "the record reads right");
        let p = position(&store, &w, Reading::Settled);
        assert!(p.finished(), "every barrier passed, and that is the whole of it");
        assert_eq!(p.at, None);
    }

    /// The other arm, and the one the fix nearly took away: **a plan has no
    /// barriers behind its groups**, so a draft's position is the first group
    /// not finished — which is where the run will begin.
    ///
    /// A list is routinely composed over work that is already done: design-only
    /// members reach `review` before anything is spawned, and the `batch` was
    /// written around 26 tasks in flight. Reading such a plan as standing at
    /// group 1 would tell somebody the wrong place, and `wsp worklist next`
    /// would name a group of finished members as what may start. It is `go`
    /// that turns those groups into barriers somebody passed, and from then on
    /// the walk is the barrier walk.
    #[test]
    fn a_plan_has_no_barriers_so_its_position_is_where_the_run_would_begin() {
        let (_env, store, _repo) = scratch("plan");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "todo");
        }
        task(&store, "wsp-1", "review");
        let mut w = list("- 1  wsp-1\n- 2  wsp-2  wsp-3\n");
        w.set_status(crate::model::WorklistStatus::Draft);

        let p = position(&store, &w, Reading::Settled);
        assert_eq!(p.at, Some(2), "group 1 was finished before the list was written");
        assert_eq!(
            p.members.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["wsp-2", "wsp-3"],
            "and the members carried back are the ones that would start"
        );

        // Started, with group 1 marked the way `go` marks it. From here the two
        // are the same walk, because there is now a verdict to walk over.
        w.set_status(crate::model::WorklistStatus::Running);
        passed(&mut w, 1, "already finished when the list started");
        assert_eq!(position(&store, &w, Reading::Settled).at, Some(2), "and it did not move");
    }

    /// The disagreement the design asks for, in one assertion: the same group,
    /// read two ways, at the same moment. `settled` says the work is done with
    /// and `landed` says the commit is still on a branch — which is exactly the
    /// "land it" read as "finish it" that cost the `batch` its most expensive
    /// hour, surfaced while somebody can still go and look.
    ///
    /// The two readings answer the same `at`, because a barrier nobody passed
    /// is a barrier nobody passed however the work reads. Where they part is
    /// [`Position::at_barrier`], which is the whole of what each reading is
    /// entitled to say.
    #[test]
    fn a_member_at_review_is_settled_and_the_barrier_still_will_not_open() {
        let (_env, store, repo) = scratch("disagree");
        task(&store, "wsp-1", "review");
        task(&store, "wsp-2", "todo");
        committed(&repo, "wsp-1");
        let w = list("- 1  wsp-1\n- 2  wsp-2\n");

        let free = position(&store, &w, Reading::Settled);
        assert_eq!(free.at, Some(1), "no verdict, so neither reading has gone anywhere");
        assert!(free.at_barrier(), "and the free reading has nothing left holding group 1");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "nor has the expensive one");
        assert!(!p.at_barrier(), "which says the work is not done, where the free one said it was");
        let held = p.holding();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].settlement, Settlement::Review, "both answers are on the one member");
        assert_eq!(held[0].note(), "1 commit not on master", "and the note says where the work is");

        land(&repo, "wsp-1");
        let p = position(&store, &w, Reading::Landed);
        assert!(p.at_barrier(), "landing it leaves the barrier and nothing else");
        assert_eq!(p.at, Some(1), "which is still a barrier, and still nobody has passed it");
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
        assert_eq!(p.members[0].note(), "no branch — nothing has run for it, or it landed and was swept");

        // The other half of the ambiguity: a branch is deleted with `-d`, which
        // refuses unless the work is merged, so a missing branch on a settled
        // task is work that landed and was swept.
        committed(&repo, "wsp-1");
        land(&repo, "wsp-1");
        git_run(&repo, &["worktree", "remove", "--force", &repo.join(cmd_checkout::WORKTREES).join("wsp-1").display().to_string()]);
        git_run(&repo, &["branch", "-d", "wsp-1"]);

        // Still `todo`, which is the state `wsp despawn` leaves behind when an
        // agent lands and ends without marking its task — and the state the
        // note used to describe as work that had never run.
        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.members[0].landing, Some(Landing::NoBranch), "the same reading either way");
        assert!(
            p.members[0].note().contains("landed and was swept"),
            "an open task whose branch was swept must not be reported as never started"
        );

        task(&store, "wsp-1", "review");
        let p = position(&store, &w, Reading::Landed);
        assert!(p.at_barrier(), "a swept tree on a settled task is finished, not started");
    }

    /// The same state at the other end, and the one that was actually costing
    /// us barriers: the branch is *there* and holds nothing, because
    /// `wsp checkout` cuts it at the trunk tip. Every member of every group
    /// looks like this from the moment it is spawned until its first commit.
    #[test]
    fn a_branch_with_nothing_on_it_does_not_read_as_landed() {
        let (_env, store, repo) = scratch("empty-branch");
        task(&store, "wsp-1", "doing");
        spawned(&repo, "wsp-1");
        let w = list("- 1  wsp-1\n- 2  wsp-2\n");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "a tree handed out this minute has not finished the group");
        assert_eq!(p.members[0].landing, Some(Landing::Nothing), "git has nothing to say either way");
        assert_eq!(p.members[0].note(), "nothing committed on its branch — or landed and never marked");

        // And the constraint the fix had to satisfy. `worklist-008` produces
        // prose in the store and not one line of code, so its branch will never
        // hold a commit: a predicate that demanded one would stall every
        // design-only member for ever, which is the failure the naive repair
        // makes and this one must not.
        task(&store, "wsp-1", "review");
        assert!(
            position(&store, &w, Reading::Landed).at_barrier(),
            "design-only work is finished when the store says so, branch or no branch"
        );
    }

    /// The reproduction, at the size it was found at: a whole group spawned,
    /// nothing committed anywhere, and a barrier that used to say `finished`.
    ///
    /// `wsp-088` d4's named failure is a barrier opening early — it does not
    /// error, it starts the next group on a trunk missing this one's work — so
    /// this asserts the members are *named* as well as counted. An earlier
    /// reading dropped four of five spawned agents out of the group silently,
    /// and nothing in the one line a governor reads said they had ever been
    /// there.
    #[test]
    fn a_group_that_has_only_been_spawned_holds_the_barrier_and_names_who() {
        let (_env, store, repo) = scratch("spawned-group");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "doing");
            spawned(&repo, id);
        }
        let w = list("- 1  wsp-1  wsp-2  wsp-3\n- 2  wsp-4\n");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "the barrier does not open on three trees and no commits");
        assert_eq!(
            p.holding().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["wsp-1", "wsp-2", "wsp-3"],
            "and every one of them is on the line that says what is holding it"
        );
        assert!(p.passed.is_empty(), "nothing behind it, so nothing to sweep");
    }

    /// One reading either side of the sweep, which is what settling `Nothing`
    /// against the store buys beyond the defect it closes.
    ///
    /// A member that landed and never reached `review` used to read *finished*
    /// while its branch stood and *never started* the moment the sweep deleted
    /// that branch — the barrier opening on evidence its own cleanup then
    /// destroyed. Both sides now say the same thing, and [`Position::slipped`]
    /// is left holding one case instead of two.
    #[test]
    fn a_landed_branch_reads_the_same_before_and_after_its_tree_is_swept() {
        let (_env, store, repo) = scratch("either-side");
        task(&store, "wsp-1", "doing");
        committed(&repo, "wsp-1");
        land(&repo, "wsp-1");
        let w = list("- 1  wsp-1\n");

        assert_eq!(position(&store, &w, Reading::Landed).at, Some(1), "landed, and the status never came");

        git_run(&repo, &["worktree", "remove", "--force", &repo.join(cmd_checkout::WORKTREES).join("wsp-1").display().to_string()]);
        git_run(&repo, &["branch", "-d", "wsp-1"]);
        assert_eq!(
            position(&store, &w, Reading::Landed).at,
            Some(1),
            "and the sweep taking the branch changes nothing about the answer"
        );
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
        assert!(p.at_barrier(), "a deleted task does not hold the barrier");
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
        assert!(position(&store, &w, Reading::Landed).at_barrier());
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
        assert!(
            position(&store, &w, Reading::Landed).at_barrier(),
            "and landing that branch finishes it"
        );
    }

    /// The store's half of the same fact, and it fails in the dangerous
    /// direction where git's fails in the safe one.
    ///
    /// A renumbering that reached everything except `worklists/` left the list
    /// naming a member by an id nothing answered to. The raw path read came
    /// back nothing, that read as `Gone`, and `Gone` is *settled* — so the
    /// member stopped holding the barrier and the run walked on past work still
    /// on its branch, while `go` wrote "no task answers to it" into the log
    /// where it reads later as evidence somebody archived it. The neighbouring
    /// test is the git half of this: there the same blindness merely *stalls* a
    /// barrier, which is why it was found first and this was not.
    ///
    /// The list here still names the old id on purpose. Every list written
    /// before the migration walk learned about `worklists/` is in exactly that
    /// state, and the store rewriting new ones does nothing for them.
    #[test]
    fn a_member_renumbered_out_from_under_a_list_still_holds_its_barrier() {
        let (_env, store, repo) = scratch("renumbered-member");
        task(&store, "wsp-9", "doing");
        committed(&repo, "wsp-9");
        std::fs::write(store.ids_path(), r#"{"old-3":"wsp-9"}"#).unwrap();
        let w = list("- 1  old-3\n");

        assert!(dangling(&store, &w).is_empty(), "a task answers to it perfectly well");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "the barrier is held by work that is still on its branch");
        assert_eq!(p.members[0].settlement, Settlement::Open(Status::Doing), "not gone: doing");
        assert_eq!(p.members[0].note(), "1 commit not on master");
        assert_eq!(
            p.members[0].id, "wsp-9",
            "named by the id it answers to now, not the one the list still uses — \
             that is the identity a tree is cut under and what `passed_by_running` \
             expands backwards from"
        );

        // `review` as well as the land, and that is not this test being made
        // to pass: a branch holding nothing is a branch that landed and one
        // `wsp checkout` cut at the trunk tip, and the store is what separates
        // them (`Landing::Nothing`). So the sequence here is the one an agent
        // actually performs, and the assertion still turns on the renumbering
        // — the id the list names is not the id either half is read under.
        land(&repo, "wsp-9");
        task(&store, "wsp-9", "review");
        assert!(
            position(&store, &w, Reading::Landed).at_barrier(),
            "and it opens on the landing, through the renumbering"
        );
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
        assert!(position(&store, &w, Reading::Landed).at_barrier());
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
        assert!(p.at_barrier());
        assert_eq!(
            member(&store, &mut Repos::new(&store), "wsp-1", Reading::Settled).landing,
            None,
            "no branch question was asked"
        );
    }

    /// The sweep a barrier licenses, end to end, and the two halves of the
    /// predicate pulled apart. The group whose barrier is being crossed goes,
    /// and everything behind it goes; the group **ahead** of that barrier is
    /// not touched, however landed a member of it happens to be.
    ///
    /// The group being crossed is the one the position is standing **at**, not
    /// the one before it — `worklist-049`. That is the half of the fix that had
    /// to be made in the same change as the position itself: read the sweep off
    /// `passed` alone, as it was, and a barrier would now leave every tree it
    /// crosses standing.
    #[test]
    fn passing_a_barrier_sweeps_the_group_it_crosses_and_not_the_one_ahead() {
        let (_env, store, repo) = scratch("sweep");
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            task(&store, id, "review");
        }
        // Group 1 is done with and on the trunk. Group 2 has one member landed
        // and one still holding commits, so its own barrier stays shut.
        committed(&repo, "wsp-1");
        land(&repo, "wsp-1");
        committed(&repo, "wsp-2");
        land(&repo, "wsp-2");
        committed(&repo, "wsp-3");
        let mut w = list("- 1  wsp-1\n- 2  wsp-2  wsp-3\n");

        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "the run has not gone past a barrier nobody has read");
        assert!(p.at_barrier(), "and the work of the group it is standing at is over");
        assert!(p.passed.is_empty(), "nothing is behind it yet");

        let out = sweep(&store, &p, false).expect("the landed reading licenses it");
        assert_eq!(out.swept.removed, ["wsp-1"], "the group whose barrier is being crossed");
        assert!(out.earlier.is_empty(), "the group just crossed is not an earlier one");
        assert!(!repo.join(cmd_checkout::WORKTREES).join("wsp-1").exists(), "the passed tree is still here");
        assert!(
            repo.join(cmd_checkout::WORKTREES).join("wsp-2").join(".git").exists(),
            "a member of the group ahead of the barrier was swept"
        );

        // And with the verdict written, group 1 is behind the run rather than
        // under it, which is the state `passed` is for.
        passed(&mut w, 1, "clean");
        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(2), "the verdict is what moved it");
        assert_eq!(p.passed.iter().map(|b| b.member.id.as_str()).collect::<Vec<_>>(), ["wsp-1"]);
        assert_eq!(p.passed[0].group, 1, "and it remembers which group it was in");
        assert!(!p.at_barrier(), "wsp-3 has not landed, so this barrier is not one to cross");
        assert!(
            sweep(&store, &p, false).expect("still licensed").swept.removed.is_empty(),
            "a group still being worked is not swept because the run is standing at it"
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
        let mut w = list("- 1  wsp-1\n- 2  wsp-2\n- 3  wsp-3\n");
        let worktrees = repo.join(cmd_checkout::WORKTREES);
        // All three trees up front, because a member with no branch at all is a
        // member nothing has run for — worklist-002's own distinction — and the
        // barrier would settle on the store rather than on git.
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            committed(&repo, id);
        }

        // Barrier 1, passed with `--keep`, which is the caller writing the
        // verdict and not calling us at all — so nothing happens, which is
        // exactly what the flag promises, at this barrier.
        land(&repo, "wsp-1");
        let p = position(&store, &w, Reading::Landed);
        assert_eq!(p.at, Some(1), "group 1 has landed and nobody has read its barrier");
        assert!(p.at_barrier());
        passed(&mut w, 1, "kept, to go and look at it");
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
        assert_eq!(p.at, Some(2), "the verdict moved it on, and this is the barrier behind group 2");
        assert!(p.at_barrier());
        let out = sweep(&store, &p, false).expect("the landed reading licenses it");
        assert_eq!(out.swept.removed, ["wsp-1", "wsp-2"], "the deferred tree did not go with this one");
        assert_eq!(out.earlier, ["wsp-1"], "and the caller cannot say which one it kept last time");
        assert!(worktrees.join("wsp-3").join(".git").exists(), "the group ahead of the barrier was swept");
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
        assert_eq!(p.at, Some(1), "nobody has passed a barrier, on either reading");
        assert!(
            p.at_barrier(),
            "and on the free reading the group looks done, which is what makes this the dangerous one"
        );
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
        w.set_status(crate::model::WorklistStatus::Draft);
        store.save_worklist(&w).unwrap();
        assert!(passed_by_running(&store).is_empty(), "a draft plan licensed a removal");

        w.set_status(crate::model::WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
        assert!(
            passed_by_running(&store).is_empty(),
            "group 1 has landed and nobody has read its barrier — that is not a licence, \
             and `wsp checkout --sweep` took both of its trees on it (worklist-049)"
        );

        passed(&mut w, 1, "clean");
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
        w.set_status(crate::model::WorklistStatus::Draft);
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
        assert!(!at.at_barrier());

        task(&store, "wsp-1", "review");
        let at = running_position(&store, "batch").unwrap();
        assert_eq!(at.at, Some(1), "the work moving does not take the run past a barrier");
        assert!(at.at_barrier(), "and what changed is what the group is waiting for");

        passed(&mut w, 1, "clean");
        store.save_worklist(&w).unwrap();
        assert_eq!(running_position(&store, "batch").unwrap().at, Some(2), "the verdict moves it");

        w.set_status(crate::model::WorklistStatus::Draft);
        store.save_worklist(&w).unwrap();
        assert_eq!(running_position(&store, "batch"), None, "a plan is not a run");
    }
}
