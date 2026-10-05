//! What the daemon's tick repairs when no verb announced it. `wsp-147`.
//!
//! [`crate::cycle`] moves a run only on a verb, because every event it waits on
//! *is* a verb somebody ran: a member's `review` and `land`, a verifier's
//! `review` or `block`, the barrier agent's `go` or `hold`. Ed, 2026-10-04:
//! **wsp owns the state machine.** So the daemon runs [`crate::cycle::step`] for
//! every running list on every tick as well, and this module is the other half
//! of that — the states a verb never reaches, because nothing in them ever
//! wrote one.
//!
//! # The four, and the record each is keyed on
//!
//! Every run step is keyed on a record written *before* the spawn, inside the
//! store lock, so two passes racing find the record and start nothing twice.
//! These are the same shape, and it is the whole design:
//!
//! | state | how it is recognised | the record it keys on |
//! |---|---|---|
//! | the agent is gone | the seat reads [`State::Gone`], or `Idle` past [`STALLED_AFTER`] with the row still `doing` | a marker line in the member's `## Log` |
//! | the start never claimed | `doing`, unclaimed, wsp's own start its last word | the `started by wsp:` line `cycle` already takes on |
//! | the landing was never recorded | settled, branch level with the trunk, no landing on the row | a landing line, which `wsp land` now writes too |
//! | `advance` skipped a member | settled, and nothing was started for it | none, and it is stamped on every pass for that reason |
//!
//! **A log line and not a state file, and that is the load-bearing choice.** A
//! separate `reconciled.json` would have to be kept in step with the task files
//! by every verb that can move either — `wsp mv`, `release`, a `despawn` — and
//! the day one of them forgot is the day the reconciler respawns an agent that
//! is working. The task's own `## Log` already survives all of those, and
//! reading it back costs nothing.
//!
//! # Two places, never one
//!
//! `cycle.log`, which is how a governor reads what the run has been doing, and
//! the seat, through [`crate::cycle::tell`]. **A repair that logged without
//! telling would be invisible for the hour between a governor's polls, and one
//! that told without logging would leave `cycle.log` — the file the next agent
//! reads to find out what happened — lying by omission.** `wsp-142` is what the
//! second failure costs: the member went to `review`, `advance` ran, started
//! nothing and wrote nothing, `worklist next` read `somewhere it did not
//! record`, and the governor went looking for the cause in the wrong record.
//!
//! # What it does not do
//!
//! It decides nothing. It does not pass a barrier, read a verdict, or judge
//! work — a barrier agent does that, and this only makes sure one exists. It
//! leaves a group its list runs by hand alone throughout: a governor reading
//! its own reviews by eye is not a wedged run, and a tick that respawned its
//! members would be a second governor nobody asked for.
//!
//! # The fifth, which is a seat rather than a member
//!
//! [`seat_kept`] is the one repair here that is not about a group's position, and
//! it is in this module because everything that makes it safe is here: the
//! [`Seats`] port, the [`EVERY`] clock, and the record-first discipline at the
//! top of the file. It fills a governor seat that reads `Empty`/`Gone` for
//! [`EMPTY_TICKS`] passes on a list that is running, and it seats one for a list
//! that has none — "governors are wsp's to allocate, not an agent's to remember".
//!
//! **It says it in one place rather than two, and that is the exception.** The
//! rule above is that a repair which logged without telling would be invisible
//! for the hour between a governor's polls. Here there is no governor to tell:
//! the whole finding is that nobody is in the seat. The second place is the
//! successor's own work order — [`crate::cmd_spawn::unheld`] names the seat, why
//! it was empty and how much was held for it — which is the moment somebody can
//! read it, and reading it twice is the cost `skipped` argues about.

use crate::model::{Status, Task, Worklist, WorklistStatus};
use crate::cycle::Seats;
use crate::place::State;
use crate::cmd_govern;
use crate::store::Store;
use crate::util;
use crate::worklist::{self, Position, Reading};

/// How long an agent may sit at a prompt with its row still at `doing` before
/// the row stops being taken at face value.
///
/// Ten minutes, and it is the same number as `cycle`'s `WEDGED_AFTER` on
/// purpose: one threshold for "a start that has not got going", so a reader
/// asking why a member is being repaired does not have to learn two. An agent
/// that has *stopped* is not slow, and nothing downstream notices — its row
/// says `doing` and its claim says held, so every reading of the run agrees it
/// is going.
const STALLED_AFTER: i64 = 10 * 60;

/// The marker in a member's `## Log` saying the run has noticed its agent is
/// not working on it.
///
/// **Read back by prefix, and written at most once per member per stall.** The
/// repair has to tell three states apart — never noticed, noticed and told,
/// and past the point of telling again — and only a record that survives a
/// daemon restart can. It is written under the store lock and re-read inside
/// the same lock that decides whether to write it, so two ticks racing cannot
/// both tell.
const NOTICED: &str = "wsp: this member's seat reads";

/// The marker in a member's `## Log` saying its work is on the trunk, and the
/// commit it is on.
///
/// Written by [`crate::cmd_checkout::land`] and by [`unrecorded_landing`], and
/// read by both to answer "has this member's work changed since?". Its being
/// the key for the re-verify is the whole of `wsp-136` item 1: `updated` moves
/// on any write at all, so a governor's `wsp note` used to buy a fresh verifier
/// on code nobody had touched.
pub(crate) const LANDED: &str = "wsp: landed";

/// The same marker on a **verifier's** row, naming the commit it was asked to
/// read. A different word and the same shape, and the shape is what matters:
/// [`landed`] reads both, because the question a reader is asking — "what is
/// this row's commit?" — has one answer whichever of the two rows carries it.
pub(crate) const READING: &str = "wsp: reading";

/// How long between passes. A minute, and the same interval as
/// [`crate::attention`]'s for a reason worth stating rather than inheriting:
/// every threshold this module compares against is minutes long, so a shorter
/// pass would re-read every seat in every running group several times per
/// threshold and change nothing, and a longer one would make the reconciler
/// slower than the thing it is repairing.
const EVERY: i64 = 60;

/// How many passes must read a governor seat `Empty` or `Gone` before wsp seats
/// a successor into it. Two, and the second one is the whole of the delay.
///
/// **Not zero, and one is the tempting number.** A seat reads `Empty` for a
/// moment every time its agent is cleared and restarted, and `Starting` covers
/// only the window where a backend says so — `place_compound` reads a pane with
/// a shell in it and no named agent as `Empty`, which is the ordinary state of a
/// pane between two agents. Seating on that reading gives a run two governors
/// for one question, which is `wsp-114`'s shape arrived at by a different road.
///
/// **Two ticks is two minutes and it is counted, not timed.** The count lives on
/// the governor record ([`crate::cmd_govern::Vacancy`]) rather than in
/// [`Pass`], because the daemon `exec`s itself when an install lands underneath
/// it — which is exactly when a person is most likely to be installing, mid-run,
/// with a governor dead.
const EMPTY_TICKS: u64 = 2;

/// The pass's own clock, held between ticks.
///
/// **Separate from [`crate::attention::Pass`], and not a second reading of the
/// same one.** That pass exists to notice changes in the store and is woken by
/// them; this one exists for conditions that raise no event and write no
/// record, which is precisely the case a wake-driven gate never fires on.
/// Sharing one gate would tie "somebody woke the daemon" to "a member's agent
/// exited", which is the defect `wsp-147` exists to remove.
pub(crate) struct Pass {
    last: Option<i64>,
}

impl Pass {
    pub(crate) fn new() -> Pass {
        Pass { last: None }
    }

    /// Whether it is time. `None` is due at once: a daemon started by hand is
    /// normally started because something is already wrong, and the states this
    /// repairs are exactly the ones nothing else will ever report.
    pub(crate) fn due(&self, at: i64) -> bool {
        self.last.is_none_or(|last| at - last >= EVERY)
    }
}


/// One tick: the run's own steps, then these repairs, for every running list.
///
/// Idempotent by the property at the top of the module, which is what lets this
/// run on a timer. A tick while the last one is still working finds the record
/// and starts nothing twice.
pub(crate) fn tick(store: &Store, seats: &dyn Seats, pass: &mut Pass) {
    pass.last = Some(util::epoch_secs());
    say_frozen_screens(store, seats);
    for w in store.worklists().into_iter().filter(|w| w.status().is_running()) {
        let _ = crate::cycle::step(store, &w, seats);
        let pos = worklist::position(store, &w, Reading::Landed);
        let Some(at) = pos.at else { continue };
        let groups = w.groups();
        let Some(g) = groups.get(at - 1) else { continue };
        // Hand-run: its governor reads its own reviews and decides when to
        // spawn, so there is nothing here to repair. See the module docs.
        if g.policy().is_none() {
            continue;
        }
        gone_member(store, seats, &w, at, &pos);
        unrecorded_landing(store, &w, at, &pos);
        skipped(store, &w, at, &pos);
    }
    // **After** the loop, and for a different reason to every repair above: this
    // one is about a run that has *finished*, so there is no group at a
    // position to read it from. `wsp-136` item 3 — the last barrier's check was
    // the one agent `end_behind` could never reach, because ending it at the
    // pass is ending the turn in which it reviews its own row, and there is no
    // next pass to reach it from.
    //
    // **Running *and* held**, and the `held` half is here because the first
    // version of this loop filtered to running lists alone — so the predicate
    // below was widened and then never reached. Found by installing and looking
    // at `wsp wip` for twenty minutes afterwards: `wsp-process` is held, its
    // finished barrier check was still standing, and the reading that should
    // have found it was never asked about that list at all.
    for w in store
        .worklists()
        .into_iter()
        .filter(|w| matches!(w.status(), WorklistStatus::Running | WorklistStatus::Held))
    {
        crate::cycle::end_all(store, crate::cycle::last_barrier_left_behind(store, &w));
    }
    // And every verifier whose verdict is recorded, whichever group it read.
    // `wsp-158` — a verifier's turn ends with its own verdict and it was then
    // left sitting in its seat holding a claim for the rest of the night.
    let verdicts = crate::cycle::verdicts_recorded(store);
    if !verdicts.is_empty() {
        stamp(store, &format!(
            "{} recorded a verdict and {} nothing left to do — ending {}",
            verdicts.len(),
            if verdicts.len() == 1 { "has" } else { "have" },
            verdicts.join(" ")
        ));
    }
    crate::cycle::end_all(store, verdicts);
    // **Collected first and then visited once each**, which is the whole of the
    // second trigger and the reason it is here rather than in the loop above.
    // A scope on a running list and a scope with a backlog are the same seat, and
    // running the loop above and this one over the same scope in the same pass
    // counted it twice and seated on the first tick — the two ticks `wsp-148`
    // asks for were spent by one pass. Found by running it, in a sandbox, and the
    // two log lines carried the same second on them.
    for (scope, trigger) in seat_scopes(store) {
        seat_vacant(store, seats, &scope, &trigger);
    }
}

/// Every scope this pass will look at a seat for, each one once.
///
/// **Two sources and one list.** The first is a running list's own seat — see
/// [`seat_kept`], which is where the chain is walked. The second is a scope
/// that owes somebody an answer: a list that has finished, and a project that
/// was never a list, both hold their backlog for ever without it, because
/// nothing but a governor clears a spool and the only thing that put a governor
/// anywhere was the first source. `wsp watch --status` would go on reading
/// `unseated · 2 held · reseating` for exactly those scopes, which says a
/// governor is on its way and is a sentence about a governor nothing was sent to
/// seat.
///
/// **A scope with no governor record is not in the second list.** `wake` reads it
/// as `no seat on this scope`, which is true and is a person's decision —
/// somebody addressed a project that has no governor, and seating one for every
/// scope anybody has ever sent a message to is a different feature. What belongs
/// here is a post that exists and is empty, which is the same vacancy as a dead
/// governor either way.
///
/// **Ordered, and deduped as it is built** — a set in insertion order, so the
/// seat a running list governs is examined as that list's own and the backlog
/// contributes nothing new for it.
fn seat_scopes(store: &Store) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let add = |scope: String, trigger: String, out: &mut Vec<(String, String)>| {
        // First one wins, so the seat a running list governs is reported as that
        // list's — the more specific of the two reasons.
        if !out.iter().any(|(s, _)| *s == scope) {
            out.push((scope, trigger));
        }
    };
    for w in store.worklists().into_iter().filter(|w| w.status().is_running()) {
        add(
            crate::cycle::governing_post(store, &w).unwrap_or_else(|| w.id.clone()),
            RUNNING.to_string(),
            &mut out,
        );
    }
    let governors = store.governors();
    // **`scopes_owed`, which is `wsp-178`'s question** — held is not owed:
    // `wsp-166` withholds a governor's non-decisions from its seat, and a scope
    // whose whole spool is withheld owes nothing while `depth() > 0` says it is
    // holding something. `tokenhub-spec-sync` is the live case — three entries,
    // all `edge: left`, reseated every twenty minutes for hours.
    for (scope, owed) in crate::wake::scopes_owed(store) {
        if governors.contains_key(&scope) {
            // The count in the sentence is the count this walk counted, so the
            // line cannot claim a scope owes an answer without a number behind it.
            add(scope, format!("it owes {owed} and nothing is answering it"), &mut out);
        }
    }
    out
}

/// **Why this pass is looking at this seat — two words, not a new field.** The
/// same sentence served both triggers, and `wsp-174` caught it on the trunk:
/// `tokenhub-spec-sync` is a `done` list and `cycle.log` said *this list is
/// running* on 149 lines about it. Its own note on this — "worth a word on the
/// third reader rather than a new field" — is the right call: a trigger belongs
/// in the sentence that reports it, and a second field on the governor record
/// would be one more thing for every writer that replaces the whole value to
/// remember.
/// **`RUNNING` is a constant and the other trigger is not**, because only one of
/// them can be asserted from what the walk knows. "This list is running" is a
/// fact about a worklist the walk has in hand. "It owes an answer" is a fact
/// about a count, and `wsp-178`'s third item is that the sentence must stop
/// claiming it for a scope that owes none — so the sentence carries the number
/// the count was read at, and there is no way to say it without one.
const RUNNING: &str = "this list is running";

/// One dated line in `cycle.log`, through the daemon's own file handle.
///
/// **The store is a parameter and not taken from here**, because this runs inside
/// the daemon where the store already exists, and because a repair's lines and
/// the run steps' have to land in the *same file*: the reconciler's stdout is
/// launchd's `daemon.log`, so "through stdout" would have put them in a file
/// nobody reads for a run. See [`crate::cycle::log_line`].
fn stamp(store: &Store, line: &str) {
    crate::cycle::log_line(store, line);
}

/// Every seat whose screen this pass overruled, said once each.
///
/// **Said here rather than inside the reading that overrules it.** [`Fleet`] is
/// what turns a frozen opencode screen into `Idle`, and it is deliberately a
/// cheap function on a port that a panel calls four times a second — it has no
/// store, no file and no place to put a sentence. This has all three, and a
/// governor reading `cycle.log` after a seat has been frozen for an hour wants
/// to see it said for that hour rather than once, which is the difference
/// between this and a note in the seat's own record.
///
/// **A cross-check on the reading rather than a duplicate of it.** The decision
/// is `crate::overrule`, called once in both places; this re-asks the same
/// question so the *report* exists, and a disagreement between them would be a
/// bug worth seeing rather than a silent one.
fn say_frozen_screens(store: &Store, seats: &dyn Seats) {
    let compound = crate::place_compound::Compound::new();
    let tasks = store.tasks();
    for t in tasks {
        if !matches!(t.status(), Status::Review | Status::Done) {
            continue;
        }
        for seat in store.panes_for_task(&t.id) {
            if seats.state(&seat) != Some(State::Working) {
                continue;
            }
            let session = compound.session_of(&crate::place::Seat::new(&seat));
            let Some(finished) = (!session.is_empty())
                .then(|| crate::agent_commands::opencode_finished_at(&session))
                .flatten()
            else {
                continue;
            };
            if crate::cycle::overrule(State::Working, Some(finished), util::epoch_secs()) != State::Working {
                stamp(
                    store,
                    &format!(
                        "{seat}: the screen says working and opencode finished this session at {finished} — read as idle"
                    ),
                );
            }
        }
    }
}

// ---- a governor seat nobody is in -----------------------------------------

/// A run whose governor seat is standing empty, and whether it is time to fill
/// it.
///
/// **The scope is the one the run's seat was *meant* to be on**, not the first
/// scope that happens to answer — [`crate::cycle::governing_post`] rather than
/// `governing_scope`, and the difference is the whole of `wsp-148`'s second
/// half. A governor whose pane dies is vacated by `reconcile` before this pass
/// runs, so the *filled* chain answers `None` and filling that would hand a run
/// a brand-new seat on the list while the project's seat it was actually using
/// sits empty one step up. Two governors, or one governor and a project nobody
/// is watching; both are worse than the vacancy.
///
/// **Another machine's seat is left alone**, which is the one refusal here and
/// the reason [`crate::cmd_govern::host_of`] is asked rather than
/// `seat_of_scope`: a seat on another host reads as no seat to everything local,
/// and a reconciler that filled those would seat a second governor for a run
/// that has one — on the machine that cannot see it.
///
/// **A post nobody has ever filled is seated at once, and a seat that reads
/// empty has to read it [`EMPTY_TICKS`] times.** The two cases have opposite
/// urgencies and the same shape: the first has no pane that could be read empty
/// and no agent that could still be coming up, so there is nothing to wait for
/// and a run that starts tonight starts with somebody; the second has a pane,
/// and a pane is `Empty` between agents.
fn seat_vacant(store: &Store, seats: &dyn Seats, scope: &str, trigger: &str) {
    let governors = store.governors();
    let scope = scope.to_string();
    // **A seat a person stood down stays down, and this is the whole reason the
    // decision is recorded rather than inferred.** `vacate` was `reconcile`'s as
    // well as `--clear`'s, and the two left the same record, so a governor that
    // had died and a position somebody had deliberately left empty read the
    // same. On 2026-10-05 that put `cpd-275` back into a `tooling` seat Ed had
    // closed by hand, two ticks after the close — and `--clear` could not close a
    // seat at all, which is the more serious half: a person had no way to say
    // "nobody governs this" and the reconciler kept spending money to disagree.
    //
    // **Before everything else, including the count.** A seat that is not going
    // to be filled must not be counted towards a threshold that will not be
    // crossed, and must not stamp a line into `cycle.log` claiming a governor is
    // on its way. Silence is the correct report for a decision somebody made.
    if cmd_govern::stood_at(&governors, &scope).is_some() {
        return;
    }
    // Somebody else's machine is somebody else's seat. Absent record is ours.
    if governors.contains_key(&scope) && cmd_govern::host_of(&governors, &scope) != util::hostname() {
        return;
    }
    let never_filled = !governors.contains_key(&scope);
    // `None` is every reading that means somebody is there, and the one that
    // means nobody can be asked; see `how_it_reads`.
    let Some(word) = how_it_reads(store, seats, &scope) else { return };
    // This pass's own count, and the record's reading of it, from one write.
    let here = cmd_govern::count_unseated(store, &scope);
    if !never_filled && !here.overdue(EMPTY_TICKS) {
        // Said once, on the way out — the pass that noticed is the interesting
        // one, and the next one is already counting.
        if here.unseated == 1 {
            stamp(store, &format!("{}: the seat reads {word} and {trigger}", scope));
        }
        return;
    }
    // The claim is the duplicate guard and it is taken *before* the process
    // starts, because the process is where the seat is written and a tick that
    // arrives in between would find an empty post and start a second one.
    if !cmd_govern::claim_seat(store, &scope) {
        return;
    }
    stamp(store, &format!("{scope}: the seat reads {word} and {trigger} — seating a successor"));
    if cfg!(test) {
        #[cfg(test)]
        crate::cycle::tests::RESEATED.with(|s| s.borrow_mut().push(scope.clone()));
    }
    crate::cycle::launch_out(store, &["govern", &scope, "--reseat"]);
}

/// What is in a scope's seat: somebody, nobody, or a reading nobody can give.
///
/// Named `Occupancy` rather than `Reading` because `worklist::Reading` is
/// already the name of a different question in this file — and two readings of
/// two records, one of which had already been spelled wrongly once, is not a
/// thing to make easier.
///
/// **`Vacant` carries the word the reconciler logs** because the two readers that
/// need this question need it for different sentences — one writes to
/// `cycle.log`, the other decides whether to open a pane — and one answer with
/// two renderings is one thing to keep right. See [`reading`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Occupancy {
    /// A pane with an agent in it. Somebody is here and nothing is to be done.
    Occupied,
    /// Nobody — never filled, no pane recorded, a pane with nothing in it, or a
    /// pane that has exited or closed.
    Vacant(&'static str),
    /// No backend can be asked. **Never `Vacant`**, and never read as it: this is
    /// a machine that cannot be seen, and a repair that acted on it would fire
    /// hardest where it knows least.
    Unreadable,
}

/// Whether this scope's seat has somebody in it right now — the one question the
/// reconciler and `wsp govern --reseat` have to answer identically.
///
/// **They did not, and the disagreement was permanent rather than intermittent.**
/// The guard asked whether the *record* names a pane and the reconciler asked
/// whether anything is *in* it. A record naming a pane that has since died
/// answers the first and not the second — which is the original shape of this
/// whole row — so the daemon counted the vacancy, launched the verb, and was
/// refused with *"the seat has somebody in it"*, once a minute, for ever. Live on
/// `tokenhub-spec-sync` at 12:35, whose count reached 54 without the seat ever
/// being filled.
///
/// **The guard refuses exactly what the reconciler would not have asked about**,
/// which is the invariant worth having: `Vacant` is the only answer that lets a
/// pane be opened, so a scope where the two ever part company strands the post
/// rather than double-seating it. `how_it_reads` is this same answer read for the
/// log line, not a second opinion about it.
pub(crate) fn reading(store: &Store, seats: &dyn Seats, scope: &str) -> Occupancy {
    // `seat_held` rather than `seat_of_scope(..).or(last_seat(..))`: a record
    // with a workspace and an empty `pane` is a slot nobody is in, and it was
    // `tooling`'s live shape on 2026-10-05.
    let Some(seat) = cmd_govern::seat_held(scope, &store.governors()) else {
        return Occupancy::Vacant("unseated");
    };
    match seats.state(&seat.pane) {
        // **`None` is one answer and this is where the two live.** A backend that
        // could not be reached answers `None`, and a backend that has never heard
        // of the pane answers `None` too — `Refusal::NoSeat` is its "not mine".
        // The second is the case this whole row is about: a governor pane that
        // was closed answers it for ever, and a repair that refused to act on it
        // would never have found the one dead seat it exists to replace.
        // [`crate::cycle::Seats::absent`] is the question that tells them apart.
        None if !seats.absent(&seat.pane) => Occupancy::Unreadable,
        None => Occupancy::Vacant("gone"),
        Some(State::Empty) => Occupancy::Vacant("empty"),
        Some(State::Gone) => Occupancy::Vacant("gone"),
        Some(_) => Occupancy::Occupied,
    }
}

/// How a seat reads, when the answer is one this pass acts on — `None` for every
/// state that means somebody is there, and for the one reading that means nobody
/// can ask.
///
/// **A projection of [`reading`], and the reason it is one rather than a second
/// implementation is the failure this row has now produced twice**: two functions
/// answering "is this seat empty" from one record, disagreeing, and the
/// disagreement costing either a governor that was never replaced or a seat that
/// could not be closed. `Idle` is somebody at a prompt and is not this repair's
/// business.
///
/// **A live seat clears the count and nothing else.** Not the claim — a claim is
/// held by a reseat that is running, and the pass that finds the seat filled is
/// the only evidence there will be that it worked. Clearing it would be clearing
/// the receipt and would let a second successor into a slot that has just proved
/// it can hold one.
fn how_it_reads(store: &Store, seats: &dyn Seats, scope: &str) -> Option<&'static str> {
    match reading(store, seats, scope) {
        Occupancy::Vacant(word) => Some(word),
        Occupancy::Occupied => {
            cmd_govern::forget_unseated(store, scope);
            None
        }
        Occupancy::Unreadable => None,
    }
}

// ---- a member whose agent is not working on it ----------------------------

/// The claim is held and the seat is not going to finish the work: the agent
/// has exited, or it has sat at a prompt past [`STALLED_AFTER`] with the row
/// still at `doing`.
///
/// **Tell first, then respawn, and only after the threshold.** The order is the
/// whole of it: an agent that exited mid-turn takes its uncommitted thinking
/// with it, and a respawn hands the next one a fresh context rather than the
/// one halfway through the work. `wsp tell` is the repair for a stopped turn
/// and costs nothing, so it goes first and the respawn is the fallback for
/// when the tell did not take — which on a dead seat is immediately, and on a
/// stalled one is a threshold later.
///
/// **The respawn is on the same branch and the same tree, and gets there by not
/// touching either.** The branch is named after the task and the tree is at
/// `.worktrees/<task>`, so [`crate::cycle::step`] taking the member again opens
/// the seat it opened last time. What has to happen first is ending the dead
/// one, and that is [`crate::cycle::despawn`] — the agent, the claim and the
/// tree, with the tree kept when it has uncommitted work in it, which is said
/// in `cycle.log` and never left silent.
fn gone_member(store: &Store, seats: &dyn Seats, w: &Worklist, at: usize, pos: &Position) {
    for s in pos.members.iter().filter(|s| !s.finished()) {
        let Some(t) = store.find_task(&s.id) else { continue };
        if t.status() != Status::Doing {
            continue;
        }
        // A claim is the precondition and the seat is not: only a member
        // somebody is holding can have lost the agent that was holding it.
        if !store.claims().contains_key(&t.id) {
            continue;
        }
        let Some(seat) = store.panes_for_task(&t.id).first().cloned() else {
            // Held, and unbound. A claim whose pane is gone with no
            // `wsp reconcile` to rebuild the binding — reported and never
            // respawned on, because there is no seat to end and the tree may be
            // somebody's.
            stamp(store, &format!(
                "{} group {at}: {} is held but no seat is bound to it — `wsp reconcile` rebuilds that from the claim",
                w.id, t.id
            ));
            continue;
        };
        let state = match seats.state(&seat) {
            Some(s) => s,
            None => continue,
        };
        let gone = match state {
            State::Gone => true,
            State::Idle => match noticed(&t) {
                // Never told: at a prompt, on the clock, nothing said yet.
                None => true,
                Some(since) => since + STALLED_AFTER <= util::epoch_secs(),
            },
            // Working is a turn in flight and `Starting` is an agent still
            // coming up. Both are somebody else's business.
            _ => false,
        };
        if !gone {
            continue;
        }

        let word = state.as_str();
        let already = noticed(&t);
        if already.is_some_and(|since| since + STALLED_AFTER <= util::epoch_secs()) {
            // Told once, still not working. End it, put the row back where a
            // start can take it, and let the run start it again — the same
            // branch, the same tree, a fresh agent.
            //
            // **The status moves back to `todo` and not left at `doing`,** and
            // that is not tidiness. `cycle::take_member` may take a `doing` row
            // again only while `started by wsp:` is its last word, and the
            // notice line this repair wrote is now the last word — so leaving it
            // at `doing` is a row nothing can start, and the reconciler has
            // replaced a stalled agent with a stalled row. It is the same move
            // `cycle::spawn` makes for a start that failed.
            stamp(store, &format!(
                "{} group {at}: {} was told to finish or say why and its seat still reads {word} — ending it, and the run starts it again",
                w.id, t.id
            ));
            put_back(store, &t.id);
            crate::cycle::despawn(&t.id);
            return;
        }

        // The record first, under the lock and re-read inside it, so two ticks
        // racing cannot both tell.
        let id = t.id.clone();
        let wrote = store.locked(|| {
            let Some(mut t) = store.find_task(&id) else { return false };
            if noticed(&t).is_some() {
                return false;
            }
            t.log(&format!(
                "{NOTICED} {word} with this row still at doing — told it to finish, land and review, or say what is left"
            ));
            t.touch();
            store.save_task(&t).is_ok()
        });
        if !wrote {
            continue;
        }
        stamp(store, &format!("{} group {at}: {} reads {word} ({seat}) with nothing working on it — telling it", w.id, id));
        tell_member(store, &id, &format!(
            "wsp noticed that {id} is at doing with nothing working on it. If your work is finished, \
             land it and run `wsp review {id}`; if it is not, say what is left. Nobody else is \
             waiting on this row."
        ));
        crate::cycle::tell(store, w, &format!(
            "{} in the {} run is at doing with nothing working on it — its seat reads {word} and it \
             has been told to finish, land and review, or say what is left. `wsp show {}` has its row.",
            id, w.id, id
        ));
    }
}

/// Put a member back where a start can take it, and say why on its row.
///
/// **The same move [`crate::cycle::spawn`] makes when a start fails**, and for
/// the same reason: `take_member` recognises its own start by the last word of
/// the log, and anything written after it — a notice, a note, a governor's
/// correction — means the next advance reads the row as somebody else's.
fn put_back(store: &Store, id: &str) {
    let id = id.to_string();
    store.locked(|| {
        let Some(mut t) = store.find_task(&id) else { return false };
        if t.status() == Status::Todo {
            return false;
        }
        t.set_status(Status::Todo);
        t.log("wsp put this back: the agent it was started on is gone, and the run will start it again");
        t.touch();
        store.save_task(&t).is_ok()
    });
}

/// When this member was last told its agent was not working on it, read back
/// off its own `## Log`.
///
/// `None` for a member never told, which is what makes the tell fire once. The
/// date is the log line's own stamp — `- <iso> wsp: …` — and `split_whitespace`
/// on a body written by [`crate::model::append_dated`] is reading a format this
/// module also writes through.
fn noticed(t: &Task) -> Option<i64> {
    t.section("Log")?
        .lines()
        .filter(|l| l.contains(NOTICED))
        .last()
        .and_then(|l| l.trim_start_matches("- ").split_whitespace().next())
        .map(util::epoch_of)
        .filter(|at| *at > 0)
}

/// `wsp tell <id> -`, run as its own process so the seat's own delivery path is
/// the thing that happens rather than something reimplemented here.
///
/// **A refusal is stamped, never retried.** A member whose agent has exited is
/// the case this exists for and `wsp tell` refuses it for exactly that reason;
/// the respawn is the fallback, on a later tick.
fn tell_member(store: &Store, id: &str, text: &str) {
    if cfg!(test) {
        #[cfg(test)]
        return crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().push((id.to_string(), text.to_string())));
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let child = std::process::Command::new(exe)
        .args(["tell", id, "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            stamp(store, &format!("tell {id}: could not run `wsp tell` ({e})"));
            return;
        }
    };
    if let Some(mut to) = child.stdin.take() {
        use std::io::Write;
        let _ = writeln!(to, "{text}");
    }
    match child.wait_with_output() {
        Ok(o) if o.status.success() => stamp(store, &format!("told {id}: it has the sentence")),
        Ok(o) => stamp(store, &format!(
            "told {id}: not delivered — {}",
            util::truncate(String::from_utf8_lossy(&o.stderr).trim(), 120)
        )),
        Err(e) => stamp(store, &format!("told {id}: not delivered — {e}")),
    }
}

// ---- a landing nobody recorded -------------------------------------------

/// A member whose work is on the trunk and whose row says nothing about it.
///
/// tokenhub-022, 2026-10-04: an agent fast-forwarded the trunk itself, `wsp
/// land` was never run, and `worklist next` read the member as unread — the
/// branch level with the trunk and no record anywhere of how it got there.
///
/// **`wsp land` now writes [`LANDED`], and this is what that record buys.** The
/// absence of the line is the signal, and it is safe to act on because
/// [`worklist::Standing::finished`] has already settled the underlying question:
/// a member at `review` or `done` whose branch holds nothing the trunk has not
/// got has landed, whichever route it took. This writes down what that reading
/// concluded, so the re-verify in [`crate::cycle::open_verifier`] has a commit
/// to key on that is not `updated`, and so a person reading the row can see it.
fn unrecorded_landing(store: &Store, w: &Worklist, at: usize, pos: &Position) {
    for s in pos.members.iter().filter(|s| s.finished()) {
        if !matches!(s.settlement, worklist::Settlement::Review | worklist::Settlement::Closed) {
            continue;
        }
        let Some(t) = store.find_task(&s.id) else { continue };
        if landed(&t).is_some() {
            continue;
        }
        let Some((sha, trunk)) = worklist::landed_at_of(store, &s.id) else { continue };
        let id = t.id.clone();
        let wrote = store.locked(|| {
            let Some(mut t) = store.find_task(&id) else { return false };
            if landed(&t).is_some() {
                return false;
            }
            t.log(&format!(
                "{LANDED} {sha} on {trunk} — recorded by wsp: the branch reached the trunk without \
                 `wsp land` saying so"
            ));
            t.touch();
            store.save_task(&t).is_ok()
        });
        if !wrote {
            continue;
        }
        store.git_commit(&format!("wsp: recorded a landing of {id}"));
        stamp(store, &format!(
            "{} group {at}: {id} is on {trunk} at {sha} and no land was recorded — recording it, and the verifier starts on it",
            w.id
        ));
        crate::cycle::tell(store, w, &format!(
            "{id} in the {} run reached {trunk} at {sha} without `wsp land` recording it. wsp recorded \
             the landing, so its verifier reads that commit.",
            w.id
        ));
    }
}

/// The commit this row's own `## Log` records as the work it is about, newest
/// first — the key a re-verify is compared against.
///
/// **Reads [`LANDED`] and [`READING`] both**, which is one reader for two rows:
/// a member records the commit it landed and a verifier records the commit it
/// was asked to read, and the comparison is between them. A reader that only
/// knew the first would fall through to `updated` for every verifier and
/// reinstate the defect `wsp-136` item 1 is about, silently, on the one row
/// where it matters most.
///
/// **`updated` cannot be this key, and the reason is `wsp-136` item 1.** Any
/// write moves it: a governor's `wsp note`, a `wsp mv`, an unrelated edit to
/// the overview. Keying a re-verify on it started a fresh verifier — a whole
/// agent, a whole read of the tree — on code that had not changed, every time
/// somebody said anything about the row.
pub(crate) fn landed(t: &Task) -> Option<String> {
    t.section("Log")?
        .lines()
        .filter(|l| l.contains(LANDED) || l.contains(READING))
        .last()
        .and_then(|l| {
            let marker = if l.contains(LANDED) { LANDED } else { READING };
            let after = l.split(marker).nth(1)?.trim();
            let sha = after.split_whitespace().next()?;
            (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha.to_string())
        })
}

// ---- a member `advance` skipped ------------------------------------------

/// Tell the seat about every member of the current group that is settled and
/// got nothing started for it.
///
/// `wsp-142`: the member went to `review`, `advance` ran, spawned no verifier
/// and **said nothing**, so `worklist next` read `somewhere it did not record`
/// and the governor went looking for the cause in the wrong record. The stall
/// was real; the silence was the defect.
///
/// **The log line is not written here.** [`crate::cycle::step`] already stamps
/// it — `unlanded`, with the same reading and the same clock — and a second
/// writer for one fact doubles a line per member per minute in the file a
/// governor reads to find out what the run is doing. That was a finding on
/// `wsp-167` and it is right: the log half of this repair existed before it,
/// and only the **seat** half is new. The barrier's own no-op below has no such
/// twin in `step`, so it does log.
///
/// **Told once per reason, and the reason is on the member's row.** A governor
/// told "it has not landed" ten minutes ago and told nothing since has learned
/// that something changed; a governor told the same sentence every minute has
/// learned nothing and paid a context read for it.
///
/// **Stamped every pass, told once per reason.** The two differ deliberately.
/// `cycle.log` is how a governor reads what the run has been doing, and a
/// member still waiting twenty minutes later has not become less waiting — but
/// the seat's spool is a queue an agent reads with a whole context behind it,
/// and the same sentence arriving every minute is a cost paid on every request
/// of every session. So the sentence carries a marker on the member holding the
/// reason, and a seat hears it when the reason changes, which is when the
/// sentence has stopped being the same sentence.
fn skipped(store: &Store, w: &Worklist, at: usize, pos: &Position) {
    let tasks = store.tasks();
    for s in pos.members.iter().filter(|s| s.settlement.settled() && !s.finished()) {
        let note = s.note();
        let why = if note.is_empty() { "its branch is not on the trunk".to_string() } else { note };
        stamp(store, &format!(
            "{} group {at}: {} is at {} and nothing was started for it — {why}. `wsp land {}` puts it on \
             the trunk, and its verifier starts on that.",
            w.id,
            s.id,
            s.settlement.word(),
            s.id
        ));
        let said = format!("{WHY_NOT} {} is at {} and not on the trunk: {why}.", s.id, s.settlement.word());
        if told_once(store, &s.id, &said) {
            crate::cycle::tell(store, w, &format!(
                "{} in the {} run is at {} with nothing started for it — {why}, so no verifier can run \
                 on it. `wsp land {}` puts it on the trunk and its verifier starts on that.",
                s.id,
                w.id,
                s.settlement.word(),
                s.id
            ));
        }
    }
    // The barrier's own no-op, one level up and the same silence: every member
    // settled, the run did not move, and nothing said which verdict was
    // missing. The list rather than the count, because the count is the
    // question a reader has.
    if pos.at_barrier() {
        let waiting: Vec<&str> = pos
            .members
            .iter()
            .filter(|s| !crate::cycle::verified(&tasks, &s.id))
            .map(|s| s.id.as_str())
            .collect();
        if !waiting.is_empty() {
            stamp(store, &format!(
                "{} group {at}: every member has landed and the barrier still waits on {} — no verifier has recorded a verdict.",
                w.id,
                waiting.join(" ")
            ));
        }
    }
}

/// The marker in a member's `## Log` for "the seat has been told this member is
/// waiting, and here is why".
const WHY_NOT: &str = "wsp: this member is waiting because";

/// Whether the seat still has to be told this, and records that it has.
///
/// **A marker holding the reason, not a bare flag.** `false` only when the last
/// line says this exact sentence — so a member that lands and then waits on
/// something else is told again, because a governor told "it has not landed"
/// ten minutes ago and now told nothing has learned that something changed.
///
/// **The comparison reads the sentence and not the line, and the timestamp is the
/// reason that is worth saying.** `## Log` entries are dated by `append_dated`,
/// and this used to build the same `- <now> <said>` to compare against, so "the
/// seat has already been told this" was true only when the two passes fell in the
/// same wall-clock second. Two passes a minute apart — which is what
/// [`EVERY`] means — never are, so the marker never matched and this always
/// returned `true`. Nothing showed, because the wake spool dedupes the sentence
/// and the seat heard it once either way; the function was inert and its own test
/// was a clock. A test that asserts a sentence is told once passed about one run
/// in four, which is the rate the two ticks happen to straddle a second.
fn told_once(store: &Store, id: &str, said: &str) -> bool {
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        let last = t
            .section("Log")
            .and_then(|l| l.lines().last().map(|x| x.trim().to_string()))
            .unwrap_or_default();
        if undated(&last) == said {
            return false;
        }
        t.log(said);
        t.touch();
        store.save_task(&t).is_ok()
    })
}

/// One `## Log` line with its date taken off, which is what a reader compares.
///
/// **The date is decoration and the sentence is the content**, so a comparison
/// that kept the date would be asking whether it is the same *instant* rather than
/// the same *sentence*. A stamp that is not a stamp is left alone rather than
/// stripped blindly, so a hand-written line beginning with a word that happens to
/// look like a date cannot have its first word eaten.
fn undated(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("- ") else { return line };
    match rest.split_once(' ') {
        Some((stamp, said)) if util::is_stamp(stamp) => said,
        _ => rest,
    }
}

/// One dated line in `cycle.log`.
///
/// Through [`crate::cycle::stamp`], so a repair's lines and the run steps' are
/// in one file in one order — and **recorded in memory under `cfg(test)`**,
/// which is the only reason the tests below can assert on what was said. A
/// repair whose whole contract is "says why, in a file" cannot be tested by
/// reading a file the code path deliberately does not write when there is no
/// terminal to write it to.


#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cycle::tests::{ENDED, MEMBER_TOLD, RESEATED, SPAWNED, TOLD};
    use crate::model::{Group, WorklistStatus};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The seats a fake answers for. `None` in the map is a seat nobody can say
    /// anything about, which is deliberately not the same as one reading `Gone`.
    struct Fake(std::collections::BTreeMap<String, Option<State>>);

    impl Fake {
        fn new(pairs: &[(&str, Option<State>)]) -> Fake {
            Fake(pairs.iter().map(|(s, v)| (s.to_string(), *v)).collect())
        }

        fn empty() -> Fake {
            Fake(Default::default())
        }
    }

    impl Seats for Fake {
        fn state(&self, seat: &str) -> Option<State> {
            self.0.get(seat).copied().flatten()
        }

        /// **The `Fake`'s own reading is that an unlisted seat is *not* absent.**
        /// `state` answers `None` for every seat it was not scripted for, which is
        /// also what it answers for a machine that could not be asked, and the
        /// default keeps that meaning. A test that wants the other answer names
        /// the seat in [`absent`], which is the point of it being separate.
        fn absent(&self, _seat: &str) -> bool {
            false
        }
    }

    /// A [`Fake`] that also says which seats are **positively** not there — every
    /// backend has answered and none has them.
    ///
    /// The state a closed governor pane reads: `state` says nothing about it and
    /// `absent` says yes. Without this the fixture could not express the case
    /// `wsp-148` is about, which is why the defect it found was found by running
    /// wsp rather than by testing it.
    /// The seat a fake can answer about *and* report gone: `state` is `None` and
/// `absent` is true, which is what a closed pane is.
#[derive(Default)]
struct Absent(Vec<String>);

impl Seats for Absent {
    fn state(&self, _seat: &str) -> Option<State> {
        None
    }
    fn absent(&self, seat: &str) -> bool {
        self.0.iter().any(|s| s == seat)
    }
}

    /// What `cycle.log` was told, on either path.
    ///
    /// **One record and not two.** `cycle::stamp` is the only writer — the
    /// reconciler logs through it rather than opening the file itself — so a
    /// repair's lines and the run steps' lines come out of one list in one
    /// order, and a test reading "what did the run say" reads both.
    fn stamped() -> Vec<String> {
        crate::cycle::tests::SAID.with(|s| s.borrow_mut().drain(..).collect())
    }

    fn member_told() -> Vec<(String, String)> {
        MEMBER_TOLD.with(|t| t.borrow_mut().drain(..).collect())
    }

    fn governed() -> Vec<String> {
        TOLD.with(|t| t.borrow_mut().drain(..).collect())
    }

    fn ended() -> Vec<String> {
        ENDED.with(|e| e.borrow_mut().drain(..).collect())
    }

    fn spawned() -> Vec<(String, String)> {
        SPAWNED.with(|s| s.borrow_mut().drain(..).collect())
    }

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("repair-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    /// A running list of one group, and the only kind of group the reconciler
    /// touches: one carrying a policy.
    fn list(store: &Store, members: &[&str], agent: &str) -> Worklist {
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group {
            members: members.iter().map(|m| m.to_string()).collect(),
            agent: agent.into(),
            ..Group::default()
        }]);
        store.save_worklist(&w).unwrap();
        w
    }

    fn member(store: &Store, id: &str, status: Status) {
        let mut t = Task::new(id, id);
        t.set_status(status);
        store.save_task(&t).unwrap();
    }

    /// A running list of one member at `doing`, its claim held and `cpd-1`
    /// bound to it: a run in flight, with something that could be lost.
    fn in_flight(tag: &str) -> (util::Isolated, Store) {
        let (env, store) = scratch(tag);
        member(&store, "m-1", Status::Doing);
        list(&store, &["m-1"], "claude");
        claim(&store, "m-1", "cpd-1");
        (env, store)
    }

    fn claim(store: &Store, id: &str, seat: &str) {
        store.set_claim(id, serde_json::json!({ "workspace": seat }));
        store.set_binding(seat, serde_json::json!({ "task_id": id }));
    }

    fn git_in(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo(env: &util::Isolated, store: &Store) -> PathBuf {
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        repo
    }

    /// A member at `review` in a project whose `m-1` branch is level with the
    /// trunk — its work has landed and its row says nothing about how.
    fn landed_member(env: &util::Isolated, store: &Store, ahead: bool) -> String {
        let repo = repo(env, store);
        git_in(&repo, &["init", "--quiet", "-b", "master"]);
        git_in(&repo, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
        git_in(&repo, &["checkout", "--quiet", "-b", "m-1"]);
        git_in(&repo, &["commit", "--quiet", "--allow-empty", "-m", "the member's work"]);
        git_in(&repo, &["checkout", "--quiet", "master"]);
        let sha = if ahead {
            // The member's commit is still on its branch, which is what
            // `wsp-142`'s stalled review looked like: settled, not landed, and
            // so no verifier can run.
            String::new()
        } else {
            git_in(&repo, &["merge", "--ff-only", "--quiet", "m-1"]);
            git_in(&repo, &["rev-parse", "master"])
        };
        let mut t = Task::new("m-1", "m-1");
        t.project = Some("p".into());
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        list(store, &["m-1"], "claude");
        sha
    }

    /// Backdate the notice a member carries, so a threshold can be crossed
    /// without a test sleeping for ten minutes.
    ///
    /// **Rewriting the log line's own stamp, not `updated`.** That is what the
    /// repair reads — the line is the record, which is the whole reason it is
    /// durable across a daemon restart — so a test that aged `updated` instead
    /// would pass against a repair keyed on the wrong thing, which is the
    /// defect `wsp-136` item 1 is about.
    fn age_the_notice(store: &Store, id: &str) {
        let mut t = store.find_task(id).unwrap();
        let log = t.section("Log").unwrap_or_default();
        let aged: Vec<String> = log
            .lines()
            .map(|l| {
                if l.contains(NOTICED) {
                    format!("- 2026-01-01T00:00:00Z{}", &l[10..])
                } else {
                    l.to_string()
                }
            })
            .collect();
        let body = format!("## Log\n\n{}\n", aged.join("\n"));
        crate::model::set_section_in(&mut t.body, "Log", &body);
        store.save_task(&t).unwrap();
    }

    fn row_log(store: &Store, id: &str) -> String {
        store.find_task(id).and_then(|t| t.section("Log")).unwrap_or_default()
    }

    fn key(store: &Store, id: &str) -> Option<String> {
        landed(&store.find_task(id)?)
    }

    // ---- a governor seat nobody is in ---------------------------------------

    /// One pass of the reconciler, which is the shape every test here takes.
    fn a_pass(store: &Store, seats: &dyn Seats) {
        tick(store, seats, &mut Pass::new());
    }

    /// What `wsp-148`'s repair filled a seat on, in order.
    fn reseated() -> Vec<String> {
        RESEATED.with(|s| s.borrow_mut().drain(..).collect())
    }

    /// `n` lines sitting in `scope`'s wake spool — somebody has written to it and
    /// nobody has taken it, which is the fact the second trigger reads.
    fn held_for(store: &Store, scope: &str, n: usize) {
        store.update_watch(&crate::wake::key_for(scope), |rec| {
            crate::cmd_watch::Spool::append(
                rec,
                (0..n)
                    .map(|i| {
                        crate::cmd_watch::Spooled::of(
                            0,
                            crate::cmd_watch::Line::Note(
                                crate::cmd_watch::Class::Message,
                                format!("owed {i}"),
                            ),
                        )
                    })
                    .collect(),
            );
        });
    }

    /// A governor seat on `scope`, occupying `room` as `pane`, at a tier.
    fn seated(store: &Store, scope: &str, room: &str, pane: &str, model: &str, effort: &str) {
        store.set_governor(
            scope,
            serde_json::json!({
                "workspace": room,
                "pane": pane,
                "host": util::hostname(),
                "since": "2026-10-04T00:00:00Z",
                "kind": "claude",
                "model": model,
                "effort": effort,
            }),
        );
    }

    /// A run with a live governor: the state every seat repair here starts from,
    /// and the one where the answer is *do nothing*.
    #[test]
    fn a_run_whose_governor_is_working_is_left_alone() {
        let (_env, store) = in_flight("seat-live");
        seated(&store, "run", "w1", "cpd-1", "", "");
        a_pass(&store, &Fake::new(&[("cpd-1", Some(State::Idle))]));

        let fills = reseated();
        assert!(fills.is_empty(), "a governor at a prompt is not a vacancy: {fills:?}");
        assert_eq!(cmd_govern::vacancy(&store.governors(), "run").unseated, 0);
    }

    /// **The two ticks are the whole delay, and a test that skipped them would
    /// pass against a repair that seated a successor into a pane between
    /// agents.** A pane with a shell in it and no named agent reads `Empty`, and
    /// that is the ordinary state of a seat whose agent was just cleared — so the
    /// first pass records and says so, and starts nobody.
    #[test]
    fn a_seat_that_reads_empty_once_is_counted_and_nobody_is_seated() {
        let (_env, store) = in_flight("seat-one-tick");
        seated(&store, "run", "w1", "cpd-1", "", "");
        a_pass(&store, &Fake::new(&[("cpd-1", Some(State::Empty))]));

        let fills = reseated();
        assert!(fills.is_empty(), "one tick is not two: {fills:?}");
        assert_eq!(
            cmd_govern::vacancy(&store.governors(), "run").unseated,
            1,
            "and the count is on the record, where a daemon that execs on install cannot lose it"
        );
        let log = stamped();
        assert!(
            log.iter().any(|l| l.contains("the seat reads empty")),
            "and the pass that noticed says so: {log:?}"
        );
    }

    /// The second tick seats exactly one successor, and **the third does not.**
    /// The claim is written on the record before the process starts, so a tick
    /// that arrives while the spawn is still running finds a scope that is
    /// neither empty nor unseated-and-counted — it is claimed.
    #[test]
    fn the_second_tick_seats_one_successor_and_a_third_ticks_nobody() {
        let (_env, store) = in_flight("seat-two-ticks");
        seated(&store, "run", "w1", "cpd-1", "", "");
        let gone = Fake::new(&[("cpd-1", Some(State::Gone))]);

        let mut fills = Vec::new();
        a_pass(&store, &gone);
        assert_eq!(cmd_govern::vacancy(&store.governors(), "run").unseated, 1, "one so far");
        a_pass(&store, &gone);
        fills.extend(reseated());
        a_pass(&store, &gone);
        a_pass(&store, &gone);
        fills.extend(reseated());
        assert_eq!(fills, vec!["run".to_string()], "two ticks seat one successor and no more: {fills:?}");
        assert!(
            cmd_govern::vacancy(&store.governors(), "run").reseating.is_some(),
            "the claim is what stopped the second, and it is on the record for the process that took it"
        );
    }

    /// **A seat nobody can be asked about is not a seat that is empty.**
    /// `Seats::state` answers `None` on a machine with no backend answering, and
    /// this is the repair that starts agents: reading an absence as a death would
    /// seat a successor for every running list at the moment the terminal server
    /// is restarting, which is the same failure [`Fleet`]'s docs warn about and
    /// it is the one place the warning applies.
    #[test]
    fn a_seat_nobody_can_be_asked_about_is_never_reseated() {
        let (_env, store) = in_flight("seat-unaskable");
        seated(&store, "run", "w1", "cpd-1", "", "");
        a_pass(&store, &Fake::new(&[("cpd-1", None)]));
        a_pass(&store, &Fake::new(&[("cpd-1", None)]));

        let fills = reseated();
        assert!(fills.is_empty(), "an absence is not a death: {fills:?}");
        assert_eq!(cmd_govern::vacancy(&store.governors(), "run").unseated, 0);
    }

    /// The other half of `wsp-148`: a list that starts with no seat on its scope
    /// gets one, and does not wait two ticks for it — there is no pane to misread
    /// and no agent that could still be coming up.
    #[test]
    fn a_running_list_with_no_seat_at_all_is_seated_on_the_first_pass() {
        let (_env, store) = in_flight("seat-none");
        a_pass(&store, &Fake::empty());

        assert_eq!(reseated(), vec!["run".to_string()], "governors are wsp's to allocate");
    }

    /// **And it is the *list's* seat, not a project's.** `governing_post` walks
    /// the chain for a post that exists at all rather than for a filled one, so a
    /// run governed from its project's seat is reseated *there* — filling the list
    /// instead would give a run two governors and leave the project's empty.
    #[test]
    fn a_run_governed_from_its_project_is_reseated_on_the_project() {
        let (_env, store) = in_flight("seat-project");
        member(&store, "m-1", Status::Doing);
        let mut t = store.find_task("m-1").unwrap();
        t.project = Some("p".into());
        store.save_task(&t).unwrap();
        seated(&store, "p", "w1", "cpd-1", "", "");
        a_pass(&store, &Fake::new(&[("cpd-1", Some(State::Gone))]));
        a_pass(&store, &Fake::new(&[("cpd-1", Some(State::Gone))]));

        assert_eq!(reseated(), vec!["p".to_string()], "the project's seat, not a new one on the list");
    }

    /// **A seat on another machine is somebody else's seat.** It reads as no seat
    /// to everything local, and filling those would put a second governor on a run
    /// that already has one — on the machine that cannot see it.
    #[test]
    fn a_seat_held_on_another_host_is_left_to_that_host() {
        let (_env, store) = in_flight("seat-remote");
        store.set_governor(
            "run",
            serde_json::json!({
                "workspace": "w9", "pane": "w9:p1", "host": "somebody-elses-machine",
                "since": "2026-10-04T00:00:00Z", "kind": "claude",
            }),
        );
        a_pass(&store, &Fake::empty());
        a_pass(&store, &Fake::empty());

        let fills = reseated();
        assert!(fills.is_empty(), "another host's governor is not ours to replace");
    }

    /// A seat that comes back stops the count, so a seat that dies again starts
    /// from one tick rather than from whatever it reached before. **Without this
    /// a seat that was once empty is empty enough forever**, and the two-tick
    /// delay `EMPTY_TICKS` exists to buy is not bought at all.
    #[test]
    fn a_seat_that_comes_back_to_life_starts_counting_again_from_one() {
        let (_env, store) = in_flight("seat-recovers");
        seated(&store, "run", "w1", "cpd-1", "", "");
        a_pass(&store, &Fake::new(&[("cpd-1", Some(State::Empty))]));
        assert_eq!(cmd_govern::vacancy(&store.governors(), "run").unseated, 1);

        a_pass(&store, &Fake::new(&[("cpd-1", Some(State::Working))]));
        assert_eq!(
            cmd_govern::vacancy(&store.governors(), "run").unseated,
            0,
            "a seat with somebody in it is the receipt, and the receipt clears the count"
        );

        a_pass(&store, &Fake::new(&[("cpd-1", Some(State::Empty))]));
        let fills = reseated();
        assert!(fills.is_empty(), "and the delay is paid again, not skipped: {fills:?}");
    }

    /// **The other half of `wsp-148`'s trigger: held items, not a running
    /// list.** A finished list and a project that was never a list both still owe
    /// somebody an answer, and the loop over running lists is the only thing that
    /// puts a governor anywhere — so without this the backlog on exactly those
    /// scopes sits held for ever while `wsp watch --status` reads `reseating`.
    /// This scope has no worklist at all, which is the stronger version.
    #[test]
    fn a_scope_owing_an_answer_with_a_vacant_seat_is_reseated_though_no_list_runs() {
        let (_env, store) = in_flight("seat-held");
        // A finished list would do; this one is absent, so nothing in the pass
        // above can reach this scope at all.
        store.set_governor(
            "quiet",
            serde_json::json!({
                "host": util::hostname(), "since": "2026-10-04T00:00:00Z", "kind": "claude",
            }),
        );
        held_for(&store, "quiet", 2);

        let gone = Fake::empty();
        a_pass(&store, &gone);
        assert_eq!(cmd_govern::vacancy(&store.governors(), "quiet").unseated, 1, "counted first");
        a_pass(&store, &gone);

        assert!(reseated().contains(&"quiet".to_string()), "nothing else would ever seat this");
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **And a scope nobody ever made a seat for is left to the person who wrote
    /// to it.** `wake` reads it as `no seat on this scope`, which is true; seating
    /// a governor for every scope anybody has ever sent a message to is a
    /// different feature and one nobody asked for.
    #[test]
    fn a_scope_owing_an_answer_with_no_seat_record_is_left_alone() {
        let (_env, store) = in_flight("seat-held-none");
        held_for(&store, "quiet", 2);
        a_pass(&store, &Fake::empty());
        a_pass(&store, &Fake::empty());

        assert!(
            !reseated().contains(&"quiet".to_string()),
            "a post nobody created is not a vacancy: {:?}",
            reseated()
        );
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **The one this row is about, and the reading it could not express until the
/// rehearsal.** A governor pane that was *closed* is not `Empty` and not `Gone`:
    /// it is a pane no backend has, which `Seats::state` answers `None` for
    /// exactly as it answers `None` for a machine that cannot be reached. Found
    /// by running wsp against a sandbox on 2026-10-05 — the row's whole subject
    /// was invisible to it, and every other reading of a dead seat has a `State`.
    ///
    /// **The two ticks are still two.** Only the reading changed: `absent` is a
    /// fact and can be acted on at once, and the count is not removed, because
    /// this is still a pane a reading had to be asked about twice.
    #[test]
    fn a_governor_pane_that_was_closed_is_reseated_after_two_ticks() {
        let (_env, store) = in_flight("seat-closed");
        seated(&store, "run", "w1", "cpd-1", "", "");
        let closed = Absent(vec!["cpd-1".to_string()]);

        a_pass(&store, &closed);
        assert!(reseated().is_empty(), "one tick is not two: {:?}", reseated());
        assert_eq!(
            cmd_govern::vacancy(&store.governors(), "run").unseated,
            1,
            "and a closed pane is counted like any other emptiness"
        );
        a_pass(&store, &closed);
        assert_eq!(reseated(), vec!["run".to_string()], "which is the case that was invisible");
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **And a machine that cannot be asked is still nothing to act on**, which
    /// is the refusal [`Seats::absent`] exists to keep beside that one. Both
    /// answer `None`; only this one is silent rather than empty, and a repair
    /// that treated silence as absence would seat a successor for every running
    /// list at the moment the terminal server is restarting.
    #[test]
    fn a_seat_nobody_can_reach_and_nobody_has_is_still_left_alone() {
        let (_env, store) = in_flight("seat-vanished");
        seated(&store, "run", "w1", "cpd-1", "", "");
        // A fake that cannot see the seat and cannot say it is not there.
        a_pass(&store, &Fake::new(&[("cpd-1", None)]));
        a_pass(&store, &Fake::new(&[("cpd-1", None)]));

        assert!(reseated().is_empty(), "silence is not absence: {:?}", reseated());
        assert_eq!(cmd_govern::vacancy(&store.governors(), "run").unseated, 0);
    }

    /// **A seat that reads `gone`, in the shape `reconcile` leaves behind.** Found
    /// by running wsp in a sandbox on 2026-10-05: the rehearsal's governor was
    /// vacated rather than removed, so its record carries `host`, `pane` and
    /// `kind` under `last` and nothing at the top — the shape every other reader
    /// here is tested against is a *live* one, and this is the one the row is
    /// about. The host is the machine's own, so the refusal for another machine's
    /// seat is not what stops it.
    #[test]
    fn a_vacated_seat_on_this_machine_is_reseated_and_not_mistaken_for_a_remote_one() {
        let (_env, store) = in_flight("seat-vacated");
        store.set_governor(
            "run",
            serde_json::json!({
                "last": {
                    "pane": "cpd-5", "workspace": "cpd-5", "kind": "claude",
                    "host": util::hostname(), "since": "2026-10-05T08:52:47Z",
                },
                "vacated": "2026-10-05T08:54:12Z",
            }),
        );
        let closed = Absent(vec!["cpd-5".to_string()]);

        a_pass(&store, &closed);
        assert_eq!(
            cmd_govern::vacancy(&store.governors(), "run").unseated,
            1,
            "a vacated seat is read through `last` and counted like any other"
        );
        a_pass(&store, &closed);

        assert_eq!(reseated(), vec!["run".to_string()], "{:?}", reseated());
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **And the same record on another machine is left there**, which is the
    /// refusal above and the reason the host is read rather than assumed. A
    /// vacated record has its host under `last`, so a reader that only asked the
    /// top level would see no host at all — and "no host" is not another
    /// machine, it is a seat this one should fill.
    #[test]
    fn a_vacated_seat_held_elsewhere_is_left_to_that_host() {
        let (_env, store) = in_flight("seat-vacated-remote");
        store.set_governor(
            "run",
            serde_json::json!({
                "last": {
                    "pane": "w9:p1", "workspace": "w9", "kind": "claude",
                    "host": "somebody-elses-machine", "since": "2026-10-05T08:52:47Z",
                },
                "vacated": "2026-10-05T08:54:12Z",
            }),
        );
        let closed = Absent(vec!["w9:p1".to_string()]);
        a_pass(&store, &closed);
        a_pass(&store, &closed);

        assert!(reseated().is_empty(), "{:?}", reseated());
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **One pass counts a seat once, whatever asked for it.** A scope on a running
    /// list and a scope with a backlog are the same seat, and the first version
    /// of `wsp-148` asked twice — once in the loop over running lists and once
    /// over [`crate::wake::scopes_owed`] — so the two ticks this row buys were
    /// spent by a single pass and the log carried both lines on the same second.
    /// Found by running it in a sandbox on 2026-10-05; every test above passes
    /// against it, because each drives only one of the two triggers.
    #[test]
    fn a_pass_says_the_same_thing_twice_but_counts_it_once() {
        let (_env, store) = in_flight("seat-twice");
        seated(&store, "run", "w1", "cpd-1", "", "");
        // The backlog trigger, pointed at the seat the list already governs.
        held_for(&store, "run", 2);
        let gone = Fake::new(&[("cpd-1", Some(State::Gone))]);

        a_pass(&store, &gone);
        let said = stamped();
        assert_eq!(
            said.iter().filter(|l| l.contains("the seat reads gone")).count(),
            1,
            "one pass, one notice: {said:?}"
        );
        assert_eq!(
            cmd_govern::vacancy(&store.governors(), "run").unseated,
            1,
            "and one pass is one tick however many triggers named it"
        );
        assert!(reseated().is_empty(), "so the second tick is still owed: {:?}", reseated());

        a_pass(&store, &gone);
        assert_eq!(reseated(), vec!["run".to_string()], "and that is the second tick");
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **The verb and the pass have to answer "is anybody in this seat?" the same
    /// way, and on 2026-10-05 they did not.** `tooling`'s record reads
    /// `workspace: compound, pane: ""`: a workspace with nobody in it. The
    /// reconciler counted that as a vacancy and launched `govern --reseat`, and
    /// the verb — asking [`crate::cmd_govern::seat_of_scope`], which stops at the
    /// workspace — refused it. The claim was left on a post nobody was ever
    /// going to fill, for twenty minutes, while the seat stayed empty.
    ///
    /// Asserted as the agreement itself rather than as either reader, because
    /// one reader cannot catch a disagreement with the other.
    #[test]
    fn the_repair_and_the_verb_read_the_same_record_as_the_same_vacancy() {
        let (_env, store) = in_flight("seat-agree");
        // A workspace and an empty pane: exactly `tooling`'s shape.
        store.set_governor(
            "run",
            serde_json::json!({
                "workspace": "compound", "pane": "", "host": util::hostname(),
                "since": "2026-10-02T14:58:04Z", "kind": "",
            }),
        );
        let gone = Fake::empty();

        a_pass(&store, &gone);
        assert_eq!(cmd_govern::vacancy(&store.governors(), "run").unseated, 1, "counted as empty");
        a_pass(&store, &gone);
        assert_eq!(
            reseated(),
            vec!["run".to_string()],
            "and the verb must accept what this pass offered it"
        );
        // **The agreement itself, and the launch is a no-op under `cfg(test)`**
        // — so the seat is still vacant here and the record still says so. What
        // is asserted is the guard's answer on that same record: the verb refuses
        // a scope only when this returns `Some`, and it returns `None`.
        assert!(
            cmd_govern::seat_held("run", &store.governors()).is_none(),
            "which is what stops the verb refusing the process this pass just launched: {:?}",
            store.governors()["run"]
        );
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **The other side of the same agreement, and the case that would be
    /// expensive to get wrong the other way**: a record with a real pane *is*
    /// occupied, by both readers, so the verb still refuses and this pass still
    /// leaves it alone.
    #[test]
    fn a_record_naming_a_pane_is_a_seat_and_neither_reader_disagrees() {
        let (_env, store) = in_flight("seat-pane");
        seated(&store, "run", "compound", "cpd-9", "", "");
        assert!(cmd_govern::seat_held("run", &store.governors()).is_some());

        a_pass(&store, &Fake::empty());
        assert!(
            !reseated().contains(&"run".to_string()),
            "a pane with nobody answering it is not vacant: {:?}",
            reseated()
        );
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **A seat a person stood down stays down, and the test is the one the row was
/// closed over.** Ed decided on 2026-10-05 that `tooling` should be closed, the
/// governor ran `wsp govern tooling --clear`, and the reconciler put `cpd-275`
/// back two ticks later — because `--clear` and `reconcile`'s own vacate left the
/// *same record*, so a position somebody had deliberately left empty read exactly
/// like a governor that had died.
///
/// A scope with **held items** and no running list, because that is `tooling`'s
/// shape and it is the trigger the first version leaned on hardest: the backlog
/// alone was enough to bring the seat back.
#[test]
fn a_seat_a_person_stood_down_is_never_reseated_however_much_is_held_for_it() {
    let (_env, store) = in_flight("seat-stood");
    seated(&store, "run", "w1", "cpd-1", "", "");
    held_for(&store, "run", 7);
    // What `--clear` leaves: the occupancy dropped, the way back kept, and now the
    // sentence that says a person meant it.
    cmd_govern::vacate(&store, "run");
    cmd_govern::mark_stood_down(&store, "run");

    let gone = Fake::new(&[("cpd-1", Some(State::Gone))]);
    for _ in 0..(EMPTY_TICKS + 2) {
        a_pass(&store, &gone);
    }

    assert!(reseated().is_empty(), "a decision is not a vacancy to be repaired: {:?}", reseated());
    let rec = &store.governors()["run"];
    assert_eq!(
        cmd_govern::vacancy(&store.governors(), "run").unseated,
        0,
        "and it is not counted towards a threshold it will never cross: {rec}"
    );
    assert!(
        !stamped().iter().any(|l| l.contains("the seat reads")),
        "and cycle.log says nothing, because silence is the correct report for it: {:?}",
        stamped()
    );
    let _ = std::fs::remove_dir_all(&store.root);
}

/// **A stand-down survives `reconcile`, which vacates the same record.** Without
/// this the decision lasts until the next dead pane, which on this machine is
/// not long: `vacate` is what the reaper calls on a slot whose workspace herdr
/// has closed, so any unrelated project losing its governor would have quietly
/// unseated every stood-down scope in the store on the same pass.
#[test]
fn a_seat_stays_stood_down_through_a_reconcile_that_vacates_it_again() {
    let (_env, store) = in_flight("seat-stood-reap");
    seated(&store, "run", "w1", "cpd-1", "", "");
    cmd_govern::mark_stood_down(&store, "run");
    // The reconciler's own empty-the-occupancy, on a record somebody closed.
    cmd_govern::vacate(&store, "run");
    assert!(
        cmd_govern::stood_at(&store.governors(), "run").is_some(),
        "a vacating that did not fill the seat did not lift the decision: {:?}",
        store.governors()["run"]
    );

    // `Absent`, not `Fake::empty()`: a fake that cannot see the seat declines to
    // act whatever the record says, so this test would pass without the fix and
    // test nothing. The point is that the *record* is what stops it.
    held_for(&store, "run", 3);
    let gone = Absent(vec!["cpd-1".to_string()]);
    for _ in 0..(EMPTY_TICKS + 2) {
        a_pass(&store, &gone);
    }
    assert!(reseated().is_empty(), "{:?}", reseated());
    let _ = std::fs::remove_dir_all(&store.root);
}

/// **And filling the seat is what lifts it**, which is the only undo there is and
/// the reason the marker lives on the record rather than in a file of decisions:
/// a seat somebody is *in* has already answered the question, so `take` replacing
/// the record drops the sentence, and the next death is an ordinary vacancy.
#[test]
fn taking_a_stood_down_seat_lifts_the_decision_so_the_next_death_is_ordinary() {
    let (_env, store) = in_flight("seat-stood-retake");
    seated(&store, "run", "w1", "cpd-1", "", "");
    cmd_govern::mark_stood_down(&store, "run");
    assert!(cmd_govern::stood_at(&store.governors(), "run").is_some());

    // A person filling it by hand, which is `wsp govern` in the seat's own pane.
    cmd_govern::take(&store, "run", "w1", "cpd-1");
    assert!(
        cmd_govern::stood_at(&store.governors(), "run").is_none(),
        "a seat somebody is in is not a stood-down seat: {:?}",
        store.governors()["run"]
    );

    cmd_govern::vacate(&store, "run");
    let gone = Fake::new(&[("cpd-1", Some(State::Gone))]);
    a_pass(&store, &gone);
    a_pass(&store, &gone);
    assert_eq!(reseated(), vec!["run".to_string()], "so a dead governor is repaired again");
    let _ = std::fs::remove_dir_all(&store.root);
}

/// **The third disagreement between these two readers, and the one that loops.**
/// `tokenhub-spec-sync` at 12:35 on 2026-10-05: its record names `cpd-272` under
/// `last`, that pane is gone, and the daemon counted the vacancy and launched
/// `govern --reseat` — which asked whether the *record* names a pane, said yes,
/// and refused. Once a minute, for ever, with the count climbing to 54 and the
/// seat never filled.
///
/// A record naming a pane that has since died is the **ordinary** shape of the
/// vacancy this row repairs, so a guard that refuses on it refuses on nearly
/// everything. Asserted through the guard's own function rather than through the
/// verb, which needs a backend.
#[test]
fn a_seat_whose_recorded_pane_has_died_is_a_vacancy_the_guard_agrees_with() {
    let (_env, store) = in_flight("seat-died-pane");
    store.set_governor(
        "run",
        serde_json::json!({
            "last": {
                "pane": "cpd-272", "workspace": "cpd-272", "kind": "claude",
                "host": util::hostname(), "since": "2026-10-05T10:09:55Z",
            },
            "vacated": "2026-10-05T10:28:01Z",
        }),
    );
    // The pane it names has gone, which is what `last` is normally for.
    let gone = Fake::new(&[("cpd-272", Some(State::Gone))]);

    a_pass(&store, &gone);
    a_pass(&store, &gone);
    assert_eq!(reseated(), vec!["run".to_string()], "the vacancy is real");

    // And the guard's question, on the record it is about to be handed.
    assert_eq!(
        reading(&store, &gone, "run"),
        Occupancy::Vacant("gone"),
        "so `govern --reseat` opens a pane rather than refusing this post for ever"
    );
    let _ = std::fs::remove_dir_all(&store.root);
}

/// **The other end, and the reason the guard is not simply deleted**: a seat with
/// somebody in it is still refused, so a person running the verb by hand cannot
/// open a second governor behind a live one.
#[test]
fn a_seat_with_somebody_in_it_is_still_refused_by_the_guard() {
    let (_env, store) = in_flight("seat-busy");
    seated(&store, "run", "w1", "cpd-1", "", "");
    let busy = Fake::new(&[("cpd-1", Some(State::Working))]);

    assert_eq!(reading(&store, &busy, "run"), Occupancy::Occupied);
    a_pass(&store, &busy);
    assert!(reseated().is_empty(), "{:?}", reseated());
    let _ = std::fs::remove_dir_all(&store.root);
}

/// **And a machine nobody can be asked about is neither**, which is the refusal
/// the guard inherits from the reconciler for free: opening a pane on a scope
/// whose occupancy is unknown would be a second governor on a run that may well
/// have one.
#[test]
fn a_seat_nobody_can_be_asked_about_is_not_something_the_guard_will_open() {
    let (_env, store) = in_flight("seat-unaskable-guard");
    seated(&store, "run", "w1", "cpd-1", "", "");
    let silent = Fake::new(&[("cpd-1", None)]);

    assert_eq!(reading(&store, &silent, "run"), Occupancy::Unreadable);
    let _ = std::fs::remove_dir_all(&store.root);
}

/// **A scope that holds three lines and owes a seat none of them.**
///
/// `wsp-178`'s live case: `tokenhub-spec-sync` is a `done` list, all three of its
/// spool entries are `edge: left`, and `wsp-166` withholds those from a governor
/// seat — so it owes nothing, while the spool's depth says it is holding three.
/// `wsp-148`'s second trigger asked about depth and read that as a scope owing an
/// answer, and reseated it every twenty minutes for hours on a compound agent
/// that never comes up (`unseated: 250`).
///
/// **Built with `edge: left` and not with `held_for`.** The helper appends a
/// `Class::Message`, and a message always counts as owed — so a fixture written
/// with it passes against the bug, which is the fourth time on this row that a
/// fixture's own convenience was the thing hiding the defect. This is
/// `tokenhub-spec-sync`'s shape exactly: a level that moved on, whose lines stay
/// in the record and are not typed.
#[test]
fn a_scope_holding_only_withheld_lines_owes_nothing_and_is_never_reseated() {
    let (_env, store) = in_flight("seat-withheld");
    seated(&store, "run", "w1", "cpd-1", "", "");
    // The other half of `tokenhub-spec-sync`: a `done` list, so nothing in this
    // scope can arrive by the running-list trigger either.
    let mut w = store.worklist("run").unwrap();
    w.set_status(crate::model::WorklistStatus::Done);
    store.save_worklist(&w).unwrap();

    store.update_watch(&crate::wake::key_for("run"), |rec| {
        crate::cmd_watch::Spool::append(
            rec,
            (0..3)
                .map(|i| {
                    let mut e = crate::cmd_watch::Emit {
                        edge: crate::cmd_watch::Edge::Left,
                        signal: crate::cmd_watch::Signal::new(
                            crate::cmd_watch::Kind::Review,
                            "m-1",
                            "the level moved on",
                        )
                        .to("run"),
                        held: 0,
                        to: "run".into(),
                    };
                    e.held = i;
                    crate::cmd_watch::Spooled::of(0, crate::cmd_watch::Line::News(e))
                })
                .collect(),
        );
    });

    let held = store
        .watches()
        .get(&crate::wake::key_for("run"))
        .and_then(|v| v.get("spool"))
        .and_then(|s| s.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(held, 3, "the spool really is holding three: the fixture has to be this shape");
    assert!(
        !crate::wake::scopes_owed(&store).iter().any(|(s, _)| s == "run"),
        "and it owes a seat none of them: {:?}",
        crate::wake::scopes_owed(&store)
    );

    let gone = Absent(vec!["cpd-1".to_string()]);
    for _ in 0..(EMPTY_TICKS + 3) {
        a_pass(&store, &gone);
    }

    assert!(
        reseated().is_empty(),
        "a scope nobody is owed anything to is not a vacancy: {:?}",
        reseated()
    );
    let _ = std::fs::remove_dir_all(&store.root);
}

/// **And the other side of the same line, so the fixture cannot be satisfied by a
/// trigger that never fires.** One `edge: up` on a running list is owed, and a
/// scope owing something with an empty seat is exactly what the second trigger is
/// for — asserted here because the test above passes against a trigger that
/// returns nothing at all, which is the same way the withholding fix could have
/// been shipped broken.
#[test]
fn a_scope_owing_one_line_is_still_reseated_when_its_seat_is_empty() {
    let (_env, store) = in_flight("seat-owed");
    seated(&store, "run", "w1", "cpd-1", "", "");
    held_for(&store, "run", 1);
    assert!(
        crate::wake::scopes_owed(&store).iter().any(|(s, _)| s == "run"),
        "a message counts as owed, which is the half the other test cannot see"
    );

    let gone = Fake::new(&[("cpd-1", Some(State::Gone))]);
    a_pass(&store, &gone);
    a_pass(&store, &gone);
    assert_eq!(reseated(), vec!["run".to_string()], "so the trigger is still live");
    let _ = std::fs::remove_dir_all(&store.root);
}

// ---- 1. a member that is gone ----------------------------------------

    /// A member whose agent has exited: the claim is held, the row is at
    /// `doing`, and nothing in the run would ever say so again. **No verb runs
    /// and one tick repairs it** — the member's own seat is told, the governing
    /// seat is told, and both say which.
    #[test]
    fn a_member_whose_agent_exited_is_told_and_the_run_is_told_with_no_verb_and_one_tick() {
        let (_env, store) = in_flight("gone");

        tick(&store, &Fake::new(&[("cpd-1", Some(State::Gone))]), &mut Pass::new());

        let told = member_told();
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].0, "m-1", "the member's own seat, not the governor's");
        assert!(told[0].1.contains("wsp review m-1"), "{} — it is told how to finish", told[0].1);
        let gov = governed();
        assert_eq!(gov.len(), 1, "and the governing seat: {gov:?}");
        assert!(gov[0].contains("m-1") && gov[0].contains("reads gone"), "{}", gov[0]);
        assert!(
            stamped().iter().any(|l| l.contains("nothing working on it")),
            "and `cycle.log` says it, since the log is where the run's history is read"
        );
    }

    /// A tick that finds it still gone does not tell twice, and does not end it
    /// on the first sighting: the record on the member stops the second tell,
    /// and the threshold is what ends it.
    #[test]
    fn a_member_told_once_is_not_told_again_and_is_ended_only_past_the_threshold() {
        let (_env, store) = in_flight("once");
        let seats = Fake::new(&[("cpd-1", Some(State::Gone))]);

        tick(&store, &seats, &mut Pass::new());
        assert_eq!(member_told().len(), 1);
        assert!(ended().is_empty(), "an agent that has just gone is told first, not ended");

        tick(&store, &seats, &mut Pass::new());
        assert!(member_told().is_empty(), "the marker on the member is what stops a second tell");
        assert!(ended().is_empty());

        age_the_notice(&store, "m-1");
        tick(&store, &seats, &mut Pass::new());
        assert_eq!(
            ended(),
            vec!["m-1".to_string()],
            "told once and still gone: the agent is ended and the run starts the member again"
        );
    }

    /// An agent sitting at a prompt with its row at `doing` is not gone — it is
    /// working between turns, and ending it would be the reconciler breaking a
    /// run that is going. This is the distinction the whole repair turns on.
    #[test]
    fn an_agent_at_a_prompt_is_left_alone_until_the_row_has_been_doing_too_long() {
        let (_env, store) = in_flight("prompt");
        let seats = Fake::new(&[("cpd-1", Some(State::Idle))]);

        tick(&store, &seats, &mut Pass::new());
        assert_eq!(member_told().len(), 1, "at a prompt with nothing done, it is told once");
        assert!(ended().is_empty());

        tick(&store, &seats, &mut Pass::new());
        assert!(ended().is_empty(), "and left alone while it may still be thinking");

        age_the_notice(&store, "m-1");
        tick(&store, &seats, &mut Pass::new());
        assert_eq!(ended(), vec!["m-1".to_string()], "past the threshold, it is ended");
    }

    /// An agent mid-turn is nobody's business, and a seat nobody can answer for
    /// is not a dead agent. Both must read as "leave it alone": the first
    /// because the run is working, the second because the reconciler would be
    /// ending every seat on a machine whose backend is not answering — the
    /// repair firing hardest exactly where it can least see.
    #[test]
    fn a_working_agent_and_a_seat_nobody_can_answer_for_are_both_left_alone() {
        for (tag, answer) in [("busy", Some(State::Working)), ("blind", None)] {
            let (_env, store) = in_flight(tag);
            tick(&store, &Fake::new(&[("cpd-1", answer)]), &mut Pass::new());
            assert!(member_told().is_empty(), "`{tag}`: nobody is told");
            assert!(ended().is_empty(), "`{tag}`: nobody is ended");
            assert!(governed().is_empty(), "`{tag}`: and nothing is invented about the run");
        }
    }

    /// A claim whose pane is gone with nothing to rebuild the binding from: said,
    /// and never respawned on, because there is no seat to end and the tree may
    /// be somebody's.
    #[test]
    fn a_claim_with_no_seat_bound_to_it_is_said_and_left_alone() {
        let (_env, store) = scratch("unbound");
        member(&store, "m-1", Status::Doing);
        list(&store, &["m-1"], "claude");
        store.set_claim("m-1", serde_json::json!({ "workspace": "cpd-9" }));

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(stamped().iter().any(|l| l.contains("no seat is bound to it")), "{:?}", stamped());
        assert!(member_told().is_empty() && ended().is_empty());
    }

    /// A group its list runs by hand has a governor reading its own reviews.
    /// Respawning its members would be a second governor nobody asked for, so
    /// the pass steps over the whole group.
    ///
    /// **And this fixture has no governor at all**, which is why the log line the
    /// seat repair leaves is not asserted empty here: seating a governor for a
    /// hand-run list is `wsp-148`'s other half and it is the right answer — a list
    /// nobody is running is not a list being run by eye — so the line below is
    /// about this pass's *group* repairs, and the seat is somebody else's test
    /// (`a_running_list_with_no_seat_at_all_is_seated_on_the_first_pass`).
    #[test]
    fn a_hand_run_group_is_left_entirely_alone() {
        let (_env, store) = scratch("hand");
        member(&store, "m-1", Status::Doing);
        list(&store, &["m-1"], "");
        claim(&store, "m-1", "cpd-1");

        tick(&store, &Fake::new(&[("cpd-1", Some(State::Gone))]), &mut Pass::new());
        assert!(member_told().is_empty(), "a governor is reading this group by eye");
        assert!(ended().is_empty());
        assert!(
            !stamped().iter().any(|l| l.contains("m-1")),
            "and nothing is said about the member itself: {:?}",
            stamped()
        );
    }

    /// The tick takes the run's own steps, so a member no verb ever started is
    /// started. This is `wsp-143`'s symptom closed: a spawn that opened a pane
    /// and never claimed left the row where it was, with nothing behind it to
    /// move it on.
    #[test]
    fn a_tick_starts_a_member_no_verb_ever_started() {
        let (_env, store) = scratch("kick");
        member(&store, "m-1", Status::Todo);
        list(&store, &["m-1"], "claude");

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())]);
    }

        // ---- 2. a start that never claimed ------------------------------------

    /// A start that claimed nothing is taken again once it has had ten minutes,
    /// on the tick rather than on the next event. `wsp-134`'s retake ran only
    /// when some other verb fired, which on a wedged member is never.
    #[test]
    fn a_start_that_never_claimed_is_taken_again_by_a_tick_and_not_before() {
        let (_env, store) = scratch("wedged");
        member(&store, "m-1", Status::Todo);
        list(&store, &["m-1"], "claude");

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert_eq!(spawned().len(), 1, "the first start");

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(spawned().is_empty(), "a start in flight is left alone");

        let mut t = store.find_task("m-1").unwrap();
        t.updated = "2026-01-01T00:00:00Z".into();
        store.save_task(&t).unwrap();
        tick(&store, &Fake::empty(), &mut Pass::new());
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "ten minutes on: taken again");
    }

    // ---- 3. a landing nobody recorded ------------------------------------

    /// tokenhub-022: an agent fast-forwarded the trunk itself, so the member is
    /// at review, its branch is on the trunk, and nothing anywhere said so.
    /// **No verb and one tick**, the landing is recorded on the row naming the
    /// commit, and the run is told.
    #[test]
    fn a_landing_nobody_recorded_is_written_down_with_its_commit_and_told() {
        let (env, store) = scratch("unrecorded");
        let sha = landed_member(&env, &store, false);

        tick(&store, &Fake::empty(), &mut Pass::new());

        assert_eq!(
            key(&store, "m-1"),
            Some(sha.clone()),
            "the row records the commit its work is on — which is what a re-verify is keyed on"
        );
        let gov = governed();
        assert!(gov.iter().any(|g| g.contains(&sha)), "the commit is named to the seat: {gov:?}");
        assert!(stamped().iter().any(|l| l.contains("no land was recorded")), "{:?}", stamped());

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(
            row_log(&store, "m-1").matches(LANDED).count() <= 1,
            "the record is the idempotence key, so a second tick finds it and writes nothing"
        );
    }

    /// A member whose branch is still ahead has no landing to record, and
    /// recording one would invent a commit. This is the guard that keeps the
    /// repair a reading rather than a guess — and the run is told to land it,
    /// which is the useful half of `wsp-142`'s stall.
    #[test]
    fn a_member_whose_branch_is_still_ahead_gets_no_landing_and_is_told_to_land() {
        let (env, store) = scratch("ahead");
        let repo = repo(&env, &store);
        git_in(&repo, &["init", "--quiet", "-b", "master"]);
        git_in(&repo, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
        git_in(&repo, &["checkout", "--quiet", "-b", "m-1"]);
        git_in(&repo, &["commit", "--quiet", "--allow-empty", "-m", "work"]);
        git_in(&repo, &["checkout", "--quiet", "master"]);
        let mut t = Task::new("m-1", "m-1");
        t.project = Some("p".into());
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        list(&store, &["m-1"], "claude");

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(key(&store, "m-1").is_none(), "there is no landing to record");
        assert!(
            governed().iter().any(|g| g.contains("wsp land m-1")),
            "and the run is told to land it instead: {:?}",
            governed()
        );
    }

    // ---- 4. a member `advance` skipped -----------------------------------

    /// `wsp-142`. The member is at review and nothing has been started for it.
    /// Before this row `advance` started nothing and **wrote nothing**, so
    /// `worklist next` read `somewhere it did not record` and the governor went
    /// looking in the wrong record. The repair's whole contract is that it says
    /// so, and this is one tick with no verb producing the sentence.
    #[test]
    fn a_settled_member_nothing_started_for_is_reported_and_the_seat_is_told_once() {
        let (env, store) = scratch("skipped");
        // A branch with the member's commit still on it: settled, and not
        // finished, so the run owes it a `wsp land` and nothing else.
        landed_member(&env, &store, true);

        tick(&store, &Fake::empty(), &mut Pass::new());
        // The line in `cycle.log` comes from `cycle::step`, which already said
        // this before the reconciler existed; what is asserted here is that the
        // *seat* is told, once.
        assert!(
            stamped().iter().any(|l| l.contains("m-1 is at review with 1 commit not on master")),
            "the log half is `cycle`'s, not a second writer here: {:?}",
            stamped()
        );
        let gov = governed();
        assert_eq!(gov.len(), 1, "and told: {gov:?}");
        assert!(gov[0].contains("wsp land m-1"), "{}", gov[0]);

        // Every pass stamps it; the seat hears it once, because the spool is a
        // queue an agent reads with a whole context behind it.
        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(
            stamped().iter().any(|l| l.contains("m-1 is at review with 1 commit not on master")),
            "the log says it on every pass: a member still waiting has not stopped waiting"
        );
        assert!(governed().is_empty(), "and the seat is not handed the same sentence twice");
    }

    /// The barrier's own silence, one level up: every member settled, the run
    /// does not move, and nothing said which member holds it. `cycle.log` names
    /// the ids, because a count is the question a reader has and cannot act on.
    #[test]
    fn a_barrier_that_cannot_open_names_the_members_it_is_waiting_on() {
        let (env, store) = scratch("barrierwait");
        landed_member(&env, &store, false);
        let mut v = Task::new("Verify m-1", "v-1");
        v.parent = Some("m-1".into());
        v.tags = vec![crate::cycle::VERIFY_TAG.into()];
        v.set_status(Status::Doing);
        store.save_task(&v).unwrap();

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(
            stamped().iter().any(|l| l.contains("the barrier still waits on m-1")),
            "{:?}",
            stamped()
        );
    }

    /// **The reconciler's lines go in `cycle.log`, and that is not the same as
    /// "through stdout".** Found by reading `daemon.log` after installing: every
    /// repair was logging into launchd's file, so a governor opening
    /// `cycle.log` after an agent died saw nothing and the reconciler read as
    /// silent — which is the whole failure this row was filed for, in the place
    /// it was least wanted.
    ///
    /// Asserted against the file rather than against the in-memory record,
    /// because the in-memory record is written by both paths and cannot tell
    /// them apart.
    #[test]
    fn a_repair_lands_in_cycle_log_itself_and_not_only_on_stdout() {
        let (_env, store) = scratch("cyclelog");
        member(&store, "m-1", Status::Doing);
        list(&store, &["m-1"], "claude");
        store.set_claim("m-1", serde_json::json!({ "workspace": "cpd-1" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));

        tick(&store, &Fake::new(&[("cpd-1", Some(State::Gone))]), &mut Pass::new());

        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert!(
            log.contains("nothing working on it"),
            "the file a governor reads for a run's history has to carry it: {log:?}"
        );
    }

    /// The reconciler's second loop reaches a **held** list. This is the shape
    /// of the bug installing found: `last_barrier_left_behind` was widened to
    /// end a check behind a held run's position, and the loop that calls it
    /// still filtered to running lists, so the widened half was never asked.
    ///
    /// A predicate nothing calls is not a predicate, and a caller that silently
    /// narrows its callee's scope is worse than one that does not — the second
    /// is a bug in one place and the first reads as done.
    #[test]
    fn a_held_lists_finished_barrier_check_is_ended_by_a_tick() {
        let (_env, store) = scratch("heldcheck");
        member(&store, "m-1", Status::Review);
        member(&store, "m-2", Status::Review);
        let mut w = list(&store, &["m-1", "m-2"], "claude");
        let mut g = w.groups();
        g[0].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        let mut check = Task::new("Barrier: run group 1", "b-1");
        check.tags = vec![crate::cycle::BARRIER_TAG.into()];
        check.set_status(Status::Review);
        store.save_task(&check).unwrap();
        store.set_claim("b-1", serde_json::json!({ "workspace": "w" }));
        // Held at group 2's barrier: nothing more starts, and group 1's check
        // has run and been passed.
        let mut w = store.worklist("run").unwrap();
        w.set_status(WorklistStatus::Held);
        store.save_worklist(&w).unwrap();

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert_eq!(
            crate::cycle::tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>()),
            vec!["b-1".to_string()],
            "a held run's finished barrier check is ended, not left standing"
        );
    }

// ---- the whole thing, end to end ------------------------------------

    /// The row's done-when, as a test: **a member's agent is killed mid-group
    /// and the run reaches its barrier anyway**, with no verb anywhere.
    ///
    /// Not the four repairs tested separately — the claim this row makes is that
    /// together they move a run that would otherwise have stood still. Nothing
    /// between the kill and the barrier is a person: every step below is one
    /// tick, and the only thing a person did was kill the process.
    #[test]
    fn a_run_whose_member_agent_is_killed_mid_group_reaches_its_barrier_without_a_verb() {
        let (env, store) = scratch("endtoend");
        let repo = repo(&env, &store);
        git_in(&repo, &["init", "--quiet", "-b", "master"]);
        git_in(&repo, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
        member(&store, "m-1", Status::Todo);
        member(&store, "m-2", Status::Todo);
        list(&store, &["m-1", "m-2"], "claude");

        // Group 1 starts. Both members claim, both seats are working.
        tick(&store, &Fake::empty(), &mut Pass::new());
        assert_eq!(spawned().len(), 2, "wsp runs the group");
        claim(&store, "m-1", "cpd-1");
        claim(&store, "m-2", "cpd-2");

        // **m-1's agent is killed.** Its seat still holds the claim, its row
        // still says `doing`, and every reading of the run agrees it is going.
        let going = Fake::new(&[("cpd-1", Some(State::Gone)), ("cpd-2", Some(State::Working))]);
        tick(&store, &going, &mut Pass::new());
        assert_eq!(member_told().len(), 1, "the dead member is told first, once");
        assert!(barriers(&store).is_empty(), "and nothing is passed over it");

        // Told once and still gone: the agent is ended and the claim released,
        // and the run takes the member again on the same branch and tree.
        age_the_notice(&store, "m-1");
        tick(&store, &going, &mut Pass::new());
        assert_eq!(
            crate::cycle::tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>()),
            vec!["m-1".to_string()],
            "the agent that exited is ended"
        );
        store.clear_claim("m-1");
        store.clear_binding("cpd-1");
        let _ = spawned();
        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(
            crate::cycle::tests::SPAWNED.with(|s| s.borrow().iter().any(|(id, _)| id == "m-1")),
            "and the member starts again, on the branch and tree named after it"
        );

        // Both finish and land. Nobody runs `wsp land` on either: the landing is
        // recorded by the repair that notices the branch reached the trunk, and
        // the verifier starts on that recording.
        for m in ["m-1", "m-2"] {
            git_in(&repo, &["checkout", "--quiet", "-b", m]);
            git_in(&repo, &["commit", "--quiet", "--allow-empty", "-m", "work"]);
            git_in(&repo, &["checkout", "--quiet", "master"]);
            git_in(&repo, &["merge", "--ff-only", "--quiet", m]);
            set_status(&store, m, Status::Review);
        }
        let _ = spawned();
        tick(&store, &Fake::empty(), &mut Pass::new());
        assert_eq!(verifiers(&store).len(), 2, "each member gets its verifier, on the recorded landing");
        for v in verifiers(&store) {
            set_status(&store, &v, Status::Review);
        }
        let _ = spawned();

        // The barrier opens — but not while a member's seat still reads working,
        // which is the condition wsp-164 exists for.
        tick(&store, &Fake::new(&[("cpd-2", Some(State::Working))]), &mut Pass::new());
        assert!(barriers(&store).is_empty(), "a member still working holds the barrier");

        tick(&store, &Fake::empty(), &mut Pass::new());
        assert_eq!(barriers(&store).len(), 1, "and it opens once every pane is quiet");

        // And the pass ends what the group opened. The claims put back above
        // stand for agents this run started; after it, none of them is standing.
        let mut w = store.worklist("run").unwrap();
        let mut g = w.groups();
        g[0].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        claim(&store, "m-1", "cpd-1");
        claim(&store, "m-2", "cpd-2");
        let _ = crate::cycle::tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>());
        crate::cycle::end_group(&store, &w, 1);
        let standing = crate::cycle::tests::ENDED.with(|e| e.borrow().clone());
        assert!(
            standing.contains(&"m-1".to_string()) && standing.contains(&"m-2".to_string()),
            "the pass ends the group's own agents: {standing:?}"
        );
    }

    fn verifiers(store: &Store) -> Vec<String> {
        store
            .tasks()
            .into_iter()
            .filter(|t| t.tags.iter().any(|g| g == crate::cycle::VERIFY_TAG))
            .map(|t| t.id)
            .collect()
    }

    fn barriers(store: &Store) -> Vec<String> {
        store
            .tasks()
            .into_iter()
            .filter(|t| t.tags.iter().any(|g| g == crate::cycle::BARRIER_TAG))
            .map(|t| t.id)
            .collect()
    }

    fn set_status(store: &Store, id: &str, status: Status) {
        let mut t = store.find_task(id).unwrap();
        t.set_status(status);
        t.touch();
        store.save_task(&t).unwrap();
    }

    // ---- the gate the daemon asks ----------------------------------------

    /// A fresh pass is due at once — a daemon started by hand is normally
    /// started because something is already wrong — and then not again until the
    /// interval has passed. Tested against a supplied clock rather than a
    /// waited one, so it costs no second and cannot flake.
    #[test]
    fn the_reconciler_runs_at_once_and_then_on_its_own_interval() {
        assert!(Pass::new().due(0), "a daemon started by hand is started because something is wrong");
        let (_env, store) = scratch("due");
        let mut p = Pass::new();
        tick(&store, &Fake::empty(), &mut p);
        let now = util::epoch_secs();
        assert!(!p.due(now), "and not again a second later");
        assert!(p.due(now + EVERY), "but after its interval");
    }
}