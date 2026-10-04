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

use crate::model::{Status, Task, Worklist};
use crate::place::{Seat, State};
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

/// Where a seat's state is read from.
///
/// **A parameter and not built here, which is the whole of what makes this
/// drivable without a terminal.** The same shape as [`crate::attention`]'s
/// `Source` and for the same reason: `wsp verify` runs on a machine where no
/// compound seat exists, and a test that needed one real seat to prove a
/// member's agent had died would not be a test.
pub(crate) trait Seats {
    /// What the seat named is doing, or `None` where nobody can say.
    ///
    /// **`None` is not [`State::Gone`] and is never treated as it.** It is what
    /// a machine with no backend answering returns, and a reconciler that read
    /// it as "gone" would kill every seat on that machine exactly when it can
    /// least see — the repair firing hardest where it knows least.
    fn state(&self, seat: &str) -> Option<State>;
}

/// The real reading, over the backends wsp can spawn onto — with opencode's
/// own database overruling a screen that has stopped repainting. `wsp-160`.
///
/// Herdr first because it watches a pty and can answer about *now*;
/// `Refusal::NoSeat` is its "not mine", so a compound seat falls through to the
/// second. One list, from [`crate::cmd_spawn`], rather than a second copy here.
///
/// **The overrule is here and not in [`place_compound::Compound::detected_state`]**,
/// and the argument is the cost: that function is called from `survey`, from
/// `wsp wip`, and from a panel four times a second, and the overrule shells to
/// `opencode db`. A reader that fires it on every call would run a process per
/// opencode seat per panel frame. The reconciler runs it once a minute, on the
/// seats it is already asking about, and can say in `cycle.log` that it did —
/// which is what `wsp-160` asks for ("log the repair beside it") and what a
/// function returning a [`State`] has nowhere to put.
pub(crate) struct Fleet;

impl Seats for Fleet {
    fn state(&self, seat: &str) -> Option<State> {
        let seat_id = Seat::new(seat);
        let compound = crate::place_compound::Compound::new();
        let raw = crate::cmd_spawn::local_backends().iter().find_map(|b| b.state(&seat_id).ok())?;
        Some(self.overrule_frozen_opencode(&compound, seat, raw))
    }
}

impl Fleet {
    /// A compound opencode seat reading `Working` that opencode's own database
    /// says finished with, more than a minute ago.
    ///
    /// **Only that one state, and only for opencode.** The frozen screen has
    /// been seen to lie in exactly one direction — `working` for ever, on a
    /// busy frame it never repainted — and `idle` is the safe direction to be
    /// wrong in: a seat wrongly called idle gets a sentence typed at it, and a
    /// seat wrongly called working waits for ever.
    ///
    /// **A live turn still reads `working`,** which is the half of the test that
    /// keeps this honest: `opencode_settled` requires a *completed* turn older
    /// than the threshold, so a session mid-stream is untouched whatever the
    /// screen says.
    ///
    /// Said every pass rather than once per seat, and that is deliberate here
    /// where it is not for the others: the repair is the reconciler's *finding*,
    /// and a governor reading `cycle.log` after a frozen seat has been sitting
    /// there for an hour wants to see it said for the whole hour. It costs one
    /// line a minute on a seat that is broken, which is nothing against a run
    /// that has stalled on it.
    fn overrule_frozen_opencode(
        &self,
        compound: &crate::place_compound::Compound,
        seat: &str,
        raw: State,
    ) -> State {
        if raw != State::Working {
            return raw;
        }
        let session = compound.session_of(&Seat::new(seat));
        if session.is_empty() {
            return raw;
        }
        let Some(finished) = crate::agent_commands::opencode_finished_at(&session) else { return raw };
        if !crate::agent_commands::opencode_settled(&session, util::epoch_secs()) {
            return raw;
        }
        stamp(&format!(
            "{seat}: the screen says working and opencode finished this session at {finished} — reading it idle"
        ));
        State::Idle
    }
}

/// The half of [`Fleet::overrule_frozen_opencode`] that decides, split out so a
/// test can drive it without an opencode, a database and a frozen TUI.
///
/// **`finished` is the session's last completed turn in seconds, and `None` is
/// "we do not know"** rather than "still working". That distinction is the whole
/// safety of the repair: a reader that treated an unreadable database as a
/// running turn would be right, and one that treated it as a finished one would
/// end agents on a machine where opencode simply could not be asked.
fn overrule(raw: State, finished: Option<i64>, at: i64) -> State {
    match (raw, finished) {
        (State::Working, Some(f)) if f + crate::agent_commands::SETTLED_AFTER <= at => State::Idle,
        _ => raw,
    }
}

/// How long between passes. A minute, and the same interval as
/// [`crate::attention`]'s for a reason worth stating rather than inheriting:
/// every threshold this module compares against is minutes long, so a shorter
/// pass would re-read every seat in every running group several times per
/// threshold and change nothing, and a longer one would make the reconciler
/// slower than the thing it is repairing.
const EVERY: i64 = 60;

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
    for w in store.worklists().into_iter().filter(|w| w.status().is_running()) {
        let _ = crate::cycle::step(store, &w);
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
    for w in store.worklists().into_iter().filter(|w| w.status().is_running()) {
        crate::cycle::end_all(store, crate::cycle::last_barrier_left_behind(store, &w));
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
            stamp(&format!(
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
            // Told once, still not working. End it and let the run start it
            // again — the same branch, the same tree, a fresh agent.
            stamp(&format!(
                "{} group {at}: {} was told to finish or say why and its seat still reads {word} — ending it, and the run starts it again",
                w.id, t.id
            ));
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
        stamp(&format!("{} group {at}: {} reads {word} ({seat}) with nothing working on it — telling it", w.id, id));
        tell_member(&id, &format!(
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
fn tell_member(id: &str, text: &str) {
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
            stamp(&format!("tell {id}: could not run `wsp tell` ({e})"));
            return;
        }
    };
    if let Some(mut to) = child.stdin.take() {
        use std::io::Write;
        let _ = writeln!(to, "{text}");
    }
    match child.wait_with_output() {
        Ok(o) if o.status.success() => stamp(&format!("told {id}: it has the sentence")),
        Ok(o) => stamp(&format!(
            "told {id}: not delivered — {}",
            util::truncate(String::from_utf8_lossy(&o.stderr).trim(), 120)
        )),
        Err(e) => stamp(&format!("told {id}: not delivered — {e}")),
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
        stamp(&format!(
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

/// Every member of the current group that is settled and got nothing started
/// for it, and why.
///
/// `wsp-142`: the member went to `review`, `advance` ran, spawned no verifier
/// and **wrote nothing**, so `worklist next` read `somewhere it did not record`
/// and the governor went looking for the cause in the wrong record. The stall
/// was real; the silence was the defect.
///
/// **Every pass, not once.** A line written only on the first pass is a line
/// easy to miss above a busy log, which is the same failure with better
/// manners. This cannot grow without bound either: the run's own steps resolve
/// what it describes, so a member that lands gets its verifier on the next
/// pass and stops being reported.
///
/// Only where a member is *waiting*. A member at `doing` with a live agent is
/// not a no-op and gets no line, or `cycle.log` would carry a line per member
/// per minute for a run that is working.
/// Only where a member is *waiting*. A member at `doing` with a live agent is
/// not a no-op and gets no line, or `cycle.log` would carry a line per member
/// per minute for a run that is working.
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
        stamp(&format!(
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
            stamp(&format!(
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
/// line is this exact sentence — so a member that lands and then waits on
/// something else is told again, because a governor told "it has not landed"
/// ten minutes ago and now told nothing has learned that something changed.
fn told_once(store: &Store, id: &str, said: &str) -> bool {
    let line = format!("- {} {said}", util::now_iso());
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        if t.section("Log").and_then(|l| l.lines().last().map(|x| x.trim().to_string())).as_deref()
            == Some(line.as_str())
        {
            return false;
        }
        t.log(said);
        t.touch();
        store.save_task(&t).is_ok()
    })
}

/// One dated line in `cycle.log`.
///
/// Through [`crate::cycle::stamp`], so a repair's lines and the run steps' are
/// in one file in one order — and **recorded in memory under `cfg(test)`**,
/// which is the only reason the tests below can assert on what was said. A
/// repair whose whole contract is "says why, in a file" cannot be tested by
/// reading a file the code path deliberately does not write when there is no
/// terminal to write it to.
fn stamp(line: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::STAMPED.with(|s| s.borrow_mut().push(line.to_string()));
    }
    crate::cycle::stamp(line);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cycle::tests::{ENDED, MEMBER_TOLD, ROTATED, SPAWNED, TOLD};
    use crate::model::{Group, WorklistStatus};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    thread_local! {
        pub(super) static STAMPED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

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
    }

    fn stamped() -> Vec<String> {
        STAMPED.with(|s| s.borrow_mut().drain(..).collect())
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
    #[test]
    fn a_hand_run_group_is_left_entirely_alone() {
        let (_env, store) = scratch("hand");
        member(&store, "m-1", Status::Doing);
        list(&store, &["m-1"], "");
        claim(&store, "m-1", "cpd-1");

        tick(&store, &Fake::new(&[("cpd-1", Some(State::Gone))]), &mut Pass::new());
        assert!(member_told().is_empty(), "a governor is reading this group by eye");
        assert!(ended().is_empty());
        assert!(stamped().is_empty(), "{:?}", stamped());
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

    // ---- 5. a screen that has stopped telling the truth (wsp-160) --------

    /// An opencode seat whose TUI stopped painting at the end of a turn reads
    /// `working` for ever, and since opencode fires none of the hooks compound
    /// listens for that screen is its *only* source. So every downstream
    /// reading believed it: `wsp tell` refused, anything held for idle waited
    /// for ever, and the run saw no landing.
    ///
    /// A completed turn the database knows about and the screen does not is
    /// `idle`, and the repair says so in `cycle.log` beside the rest.
    #[test]
    fn a_frozen_opencode_screen_is_overruled_by_the_database() {
        let now = 1_800_000_000;
        let ago = now - crate::agent_commands::SETTLED_AFTER - 1;
        assert_eq!(
            overrule(State::Working, Some(ago), now),
            State::Idle,
            "the screen says working and the session finished a minute ago: a frozen screen"
        );
        // And the frozen screen is the only thing overruled: an agent genuinely
        // mid-turn must read `working`.
        assert_eq!(
            overrule(State::Working, Some(now - 5), now),
            State::Working,
            "a turn that finished five seconds ago is a turn that just ended, not a frozen screen"
        );
        assert_eq!(
            overrule(State::Working, None, now),
            State::Working,
            "a database that could not be read is not a finished session — overrule on a guess is how this would break a live run"
        );
    }

    /// Every other state is left exactly as the screen read it. The repair has
    /// been seen to lie in one direction only, and `idle` is the safe direction
    /// to be wrong in: a seat wrongly called idle has a sentence typed at it,
    /// and a seat wrongly called working waits for ever.
    #[test]
    fn only_a_working_screen_is_ever_overruled() {
        let now = 1_800_000_000;
        let long = now - crate::agent_commands::SETTLED_AFTER - 1;
        for read in [State::Idle, State::Starting, State::Blocked, State::Empty, State::Gone, State::Unknown] {
            assert_eq!(overrule(read, Some(long), now), read, "{read:?} is not this repair's to overrule");
        }
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
        let lines = stamped();
        assert!(lines.iter().any(|l| l.contains("m-1 is at review and nothing was started for it")), "{lines:?}");
        let gov = governed();
        assert_eq!(gov.len(), 1, "and told: {gov:?}");
        assert!(gov[0].contains("wsp land m-1"), "{}", gov[0]);

        // Every pass stamps it; the seat hears it once, because the spool is a
        // queue an agent reads with a whole context behind it.
        tick(&store, &Fake::empty(), &mut Pass::new());
        assert!(
            stamped().iter().any(|l| l.contains("nothing was started for it")),
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
        let _ = ROTATED.with(|r| r.borrow_mut().clear());
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