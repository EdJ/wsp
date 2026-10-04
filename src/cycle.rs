//! The run's steps, taken by wsp: `wsp-134`.
//!
//! A worklist used to advance only when its governor did something — spawned
//! each member, read each finished one, passed the barrier, spawned the next
//! group, rotated. So the governor had to be awake at every step, and a
//! harness has one way of keeping an agent awake: a conversation held open,
//! watchers and wakeups inside it, or a person typing "cpd-144 needs input"
//! into its pane. Ed, 2026-09-30: **wsp runs the steps, and agents are for
//! decisions and action.** They do not coordinate the steps.
//!
//! # Driven by the verbs, not by a daemon
//!
//! The wake path in [`crate::wake`] already existed and had been dead for three
//! days when this row was written: its only launcher was herdr's `[[startup]]`,
//! and the fleet had moved to compound. Ed's answer to "how does the daemon
//! run without herdr" was the better question — *do we need one?* — and the
//! answer is no. Every event a run advances on is a wsp verb run in some
//! agent's own process: a member's `review` and `land`, a verifier's
//! `review` or `block`, the barrier agent's `go` or `hold`. So the verb is
//! the trigger. [`poke`] sits at the seam every status verb already shares
//! (`cmd_task::mutate_saying`), and at `land`, `go` and `hold`.
//!
//! What that gives up is anything with no verb behind it: an agent that dies,
//! or stalls on a prompt, before it runs `wsp review` never triggers the next
//! step. That stays the person's to see, on the panel, as it always was.
//!
//! # The chain
//!
//! For a running list whose current group carries a [`Policy`]:
//!
//! 1. Members not yet started are spawned, up to the group's cap, each on its
//!    own `agent <id>:` line where it has one and the group's otherwise
//!    ([`Group::policy_for`], `wsp-150`).
//! 2. A member that has reached `review` **and landed** gets a read-only
//!    verifier, on the member's policy: a child row tagged [`VERIFY_TAG`]
//!    whose overview is its work order. It notes its verdict on the member and
//!    reviews its own row, or blocks it.
//! 3. When every member is landed and every verifier is at `review`, one agent
//!    checks the barrier — a row tagged [`BARRIER_TAG`] — on the group's own
//!    line, since it reads the group as a whole, and ends it with
//!    `wsp worklist go` or `hold`.
//! 4. A pass rotates the governing seat onto a fresh successor and starts the
//!    next group (step 1 again). A governor is *told* what happened at each
//!    point that asks for a decision: a block, a hold, a pass. It never has to
//!    act for the run to move.
//!
//! # Levels, so a lost trigger costs a delay and nothing else
//!
//! [`advance`] reads where the run is from the store and git alone and does
//! whatever is owed; it does not remember what it did last time. So it is
//! idempotent, a second trigger while the first is running finds the work
//! already done, and `wsp worklist advance` by hand repairs a trigger that was
//! lost. Each thing it starts is keyed on a record written *before* the spawn,
//! inside the store lock — the member moved to `doing`, the verifier or
//! barrier row created — so two advances racing find the record and start
//! nothing twice.
//!
//! # Detached, because the trigger is somebody else's command
//!
//! A member's `wsp review` must return in a second, and a spawn waits for an
//! agent to come up. So [`poke`] starts `wsp worklist advance` in a process
//! group of its own — the reason `cmd_spawn::arrange_ending` gives: an agent's
//! ending signals its group — with its output in `cycle.log` in the state
//! directory, since nobody is reading the pane it started from.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::model::{Group, Policy, Status, Task, Worklist, WorklistStatus};
use crate::place::{Seat, State};
use crate::store::Store;
use crate::util;
use crate::worklist::{self, Reading, Settlement};
use crate::Args;

/// The tag on a verifier's row. Its parent is the member it verifies.
pub(crate) const VERIFY_TAG: &str = "verify";
/// The tag on the row of the agent that checks a barrier.
pub(crate) const BARRIER_TAG: &str = "barrier";

/// Set in the environment of anything [`poke`] starts, and read by nothing but
/// a test or a person who wants a verb to leave the run alone.
const OFF: &str = "WSP_NO_CYCLE";

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

/// After a status verb: start the steps this may have made due.
///
/// Cheap and in-process up to the one decision that matters — is this row
/// part of a run at all? — so the ordinary `wsp review` on a task nobody
/// listed pays for one read of `worklists/` and starts nothing.
pub(crate) fn poke_task(store: &Store, id: &str, verb: &str) {
    let Some(t) = store.find_task(id) else { return };
    let ours = t.tags.iter().any(|g| g == VERIFY_TAG || g == BARRIER_TAG);
    let member = worklist::Running::read(store).list_of(&t.id).is_some();
    if !ours && !member {
        return;
    }
    launch(store, &["worklist", "advance", "--task", &t.id, "--verb", verb]);
}

/// After `go` or `hold` on a list: tell whoever governs it, and take the steps
/// the new position owes. `passed` is the barrier crossed, if one was.
pub(crate) fn poke_list(store: &Store, list: &str, event: &str, passed: Option<usize>) {
    let n = passed.map(|n| n.to_string()).unwrap_or_default();
    let mut argv = vec!["worklist", "advance", list, "--event", event];
    if passed.is_some() {
        argv.extend(["--passed", n.as_str()]);
    }
    launch(store, &argv);
}

fn launch(store: &Store, argv: &[&str]) {
    // A test's `current_exe` is the test harness, and handing it `worklist
    // advance` would run the suite again from inside itself.
    if cfg!(test) || std::env::var_os(OFF).is_some() {
        return;
    }
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    let log = store.state_file("cycle.log");
    let Ok(out) = std::fs::OpenOptions::new().create(true).append(true).open(&log) else { return };
    let Ok(err) = out.try_clone() else { return };
    let mut cmd = Command::new(exe);
    cmd.args(argv)
        // Nobody's pane: a spawn started from here must not read itself as
        // the agent whose verb triggered it.
        .env_remove(crate::place::SEAT_ENV)
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0);
    let _ = cmd.spawn();
}

/// `wsp worklist advance [<slug>] [--task ID --verb V] [--event go|hold --passed N]`
///
/// Take every step the store says is owed, on one list or on every running
/// one. The event flags are what the trigger saw, and they only decide what a
/// governor is told: the steps themselves are read off the store.
pub fn advance(store: &Store, args: &Args) -> i32 {
    let named = args.rest.get(1).cloned();
    stamp(&format!("advance {}", args.rest[1..].join(" ")));

    if let (Some(id), Some(verb)) = (args.get("task"), args.get("verb")) {
        told_about_task(store, &id, &verb);
    }
    if let (Some(list), Some(event)) = (named.as_deref(), args.get("event")) {
        let passed = args.get("passed").and_then(|n| n.parse::<usize>().ok());
        if let Some(w) = store.worklist(list) {
            match event.as_str() {
                "hold" => tell(store, &w, &format!(
                    "The {} run was held at its barrier: `wsp worklist show {}` has the reason. \
                     wsp starts nothing more until somebody runs `wsp worklist go {}`.",
                    w.id, w.id, w.id
                )),
                "go" => {
                    // Whose `go` this was decides what follows it. A group
                    // run by hand was passed by its governor, who ends its
                    // agents and rotates itself under the manual order — doing
                    // either here as well is a double rotation on a live run
                    // (wsp-135's FAIL, on ux-revamp held at barrier 16). The
                    // step is still taken: a hand-run group may be followed
                    // by one wsp runs.
                    if !ran_by_wsp(&w, passed) {
                        let _ = step(store, &w, &Fleet);
                        return 0;
                    }
                    // Rotate first, so what is told next reaches the successor
                    // rather than a seat that is about to be ended. Not on the
                    // last pass: there is nothing left to govern.
                    if let Some(n) = passed {
                        end_behind(store, &w, n);
                        if n < w.groups().len() {
                            rotate(store, &w);
                        }
                    }
                    let _ = step(store, &w, &Fleet);
                    tell(store, &w, &passed_sentence(store, &w, passed));
                    return 0;
                }
                _ => {}
            }
        }
    }

    let lists: Vec<Worklist> = match &named {
        Some(n) => store.worklist(n).into_iter().collect(),
        None => store.worklists(),
    };
    for w in lists.iter().filter(|w| w.status() == WorklistStatus::Running) {
        let _ = step(store, w, &Fleet);
    }
    0
}

/// Whether the group a `go` concerns is one wsp runs: the group just passed,
/// or on a start, the first group the run stands at.
fn ran_by_wsp(w: &Worklist, passed: Option<usize>) -> bool {
    let groups = w.groups();
    let g = match passed {
        Some(n) => groups.get(n - 1),
        None => groups.iter().find(|g| g.verdict.trim().is_empty()),
    };
    g.is_some_and(|g| g.policy().is_some())
}

/// What the run looks like after a `go`, in the words a governor is told.
fn passed_sentence(store: &Store, w: &Worklist, passed: Option<usize>) -> String {
    let w = store.worklist(&w.id).unwrap_or_else(|| w.clone());
    let groups = w.groups();
    let head = match passed {
        Some(n) => {
            let verdict = groups.get(n - 1).map(|g| util::truncate(g.verdict.trim(), 400)).unwrap_or_default();
            format!("Barrier {n} of {} passed. The verdict: {verdict}", w.id)
        }
        None => format!("{} started.", w.id),
    };
    let pos = worklist::position(store, &w, Reading::Settled);
    let tail = match pos.at.and_then(|at| groups.get(at - 1).map(|g| (at, g))) {
        None => format!(" Nothing is left in the run: `wsp worklist done {}` closes it.", w.id),
        Some((at, g)) if g.policy().is_some() => {
            format!(" wsp is running group {at}: {}. You are told if one of them needs a decision.", g.members.join(" "))
        }
        Some((at, g)) => format!(
            " Group {at} ({}) has no `agent:` line, so it is run by hand: `wsp worklist next {}`.",
            g.members.join(" "),
            w.id
        ),
    };
    format!("{head}{tail}")
}

/// A block on anything in a run is a decision somebody owes, so the seat
/// governing the run is told. A review is not: it is the next step, and wsp
/// takes it — unless the member's work is not on the trunk, when there is no
/// step wsp can take and nobody else will notice.
fn told_about_task(store: &Store, id: &str, verb: &str) {
    let Some(t) = store.find_task(id) else { return };
    let Some(w) = list_of(store, &t) else { return };
    if verb == "review" {
        return told_unlanded(store, &w, &t);
    }
    if verb != "blocked" {
        return;
    }
    let what = if t.tags.iter().any(|g| g == VERIFY_TAG) {
        "A verifier found a problem"
    } else if t.tags.iter().any(|g| g == BARRIER_TAG) {
        "The barrier check stopped on a question"
    } else {
        "A member is blocked"
    };
    tell(store, &w, &format!(
        "{what} in the {} run: {} ({}) is blocked. `wsp show {}` says on what. \
         The run waits here until it is unblocked.",
        w.id, t.id, util::truncate(&t.title, 60), t.id
    ));
}

/// A member that reached `review` with commits its trunk has not got.
///
/// `review` is an agent's terminal verb — it lands first — so a member that
/// did not land has stopped, and its verifier is keyed on the landing. Only a
/// group wsp runs: by hand, the governor is the one reading reviews.
fn told_unlanded(store: &Store, w: &Worklist, t: &Task) {
    if t.tags.iter().any(|g| g == VERIFY_TAG || g == BARRIER_TAG) {
        return;
    }
    let pos = worklist::position(store, w, Reading::Landed);
    let Some(g) = pos.at.and_then(|at| w.groups().get(at - 1).cloned()) else { return };
    if g.policy().is_none() {
        return;
    }
    let Some(s) = pos.members.iter().find(|s| s.id == t.id) else { return };
    if s.finished() || !s.settlement.settled() {
        return;
    }
    tell(store, w, &format!("In the {} run, {}", w.id, unlanded(&s.id, &s.note())));
}

/// The sentence for a reviewed member that has not landed, in the log and
/// to the seat alike.
fn unlanded(id: &str, note: &str) -> String {
    format!(
        "{id} is at review with {note}, so it has no verifier yet. \
         `wsp land {id}` puts it there, and wsp starts the verifier on the land."
    )
}

/// The running list a row belongs to: itself a member, a verifier under one,
/// or the barrier row named after the list.
fn list_of(store: &Store, t: &Task) -> Option<Worklist> {
    let running = worklist::Running::read(store);
    let id = match t.tags.iter().any(|g| g == VERIFY_TAG) {
        true => t.parent.clone().unwrap_or_default(),
        false => t.id.clone(),
    };
    if let Some(l) = running.list_of(&id) {
        return store.worklist(l);
    }
    let slug = t.title.strip_prefix(BARRIER_TITLE)?.split_whitespace().next()?.to_string();
    store.worklist(&slug)
}

/// Everything the current group owes, taken in order. Returns what it started,
/// for the log.
///
/// **`pub(crate)` and not private since `wsp-147`**, because the daemon's tick
/// takes these steps itself rather than starting a `wsp worklist advance` per
/// list: one code path means the tick and the verb cannot come to disagree
/// about what is owed, and the tick has to reach them without a process per
/// running list every minute.
pub(crate) fn step(store: &Store, w: &Worklist, seats: &dyn Seats) -> Vec<String> {
    let mut started = Vec::new();
    let w = match store.worklist(&w.id) {
        Some(w) => w,
        None => return started,
    };
    if w.status() != WorklistStatus::Running {
        return started;
    }
    let groups = w.groups();
    let pos = worklist::position(store, &w, Reading::Landed);
    let Some(at) = pos.at else { return started };
    let Some(g) = groups.get(at - 1) else { return started };
    let Some(policy) = g.policy() else { return started };

    // 1. Members nobody has started, up to the cap — counted again inside
    // the lock by `take_member`, because the two advances a pass can start
    // (the barrier agent's `go` and its own `review`) both read this list.
    let cap = g.parallelism(None);
    for s in &pos.members {
        let Some(t) = store.find_task(&s.id) else { continue };
        if let Some((why, own)) = take_member(store, &t, &w.id, at, &g.members, cap) {
            stamp(&format!("{} group {at}: starting {} on {} ({why})", w.id, s.id, own.kind));
            if spawn(store, &s.id, &own, Undo::Member) {
                started.push(s.id.clone());
            } else {
                failed(store, &w, &s.id);
            }
        }
    }

    // 2. A verifier for each member that is finished — reviewed and landed.
    for s in pos.members.iter().filter(|s| s.finished()) {
        let Some(member) = store.find_task(&s.id) else { continue };
        if let Some(v) = open_verifier(store, &member, &w.id, at, g) {
            // On the kind the work ran on — `wsp-134` d1 (C) — which in a
            // mixed group is the member's own line and not the group's.
            let on = g.policy_for(&member.id).unwrap_or_else(|| policy.clone());
            if spawn(store, &v, &on, Undo::Row) {
                started.push(v);
            } else {
                failed(store, &w, &v);
            }
        }
    }

    // 2a. And one that is reviewed and *not* landed gets nothing, which the
    // log has to say: tokenhub-003 went to review with its commit still on
    // its branch, this started nothing and wrote nothing, and its governor
    // went looking for the cause in the wrong record (`wsp-142`).
    for s in pos.members.iter().filter(|s| s.settlement.settled() && !s.finished()) {
        stamp(&format!("{} group {at}: {}", w.id, unlanded(&s.id, &s.note())));
    }

    // 3. The barrier, once every member is landed, every verdict is in, and
    // every member has stopped working. `wsp-164`.
    let tasks = store.tasks();
    if pos.at_barrier() && pos.members.iter().all(|s| verified(&tasks, &s.id)) {
        match holding_barrier(store, seats, &w, &pos) {
            None => {
                if let Some(b) = open_barrier(store, &w, at, g) {
                    if spawn(store, &b, &policy, Undo::Row) {
                        started.push(b);
                    } else {
                        failed(store, &w, &b);
                    }
                }
            }
            Some(waiting) => {
                // Said every pass, never silently. `wsp-142`'s lesson is that a
                // no-op which writes nothing is indistinguishable from a stall
                // with no cause, and the cause here is a fact about a pane.
                stamp(&format!("{} group {at}: barrier waits: {}", w.id, waiting.join("; ")));
            }
        }
    }
    if !started.is_empty() {
        stamp(&format!("{} group {at}: started {}", w.id, started.join(" ")));
    }
    started
}

/// How long a member may read `working` with its group otherwise finished
/// before the run stops waiting and says so to the seat. `wsp-164`.
const OVERWORKED_AFTER: i64 = 30 * 60;

/// The members whose pane is still working, and are holding the barrier.
///
/// **The backstop for `wsp-163`'s case where nothing was reopened.** A member
/// can be working on more with its row at `review` — it picked something up
/// itself, or a person typed to it — and every other reading says the group is
/// finished. Opening a barrier over a member that is mid-turn hands the group to
/// a fresh agent while one of its own is still writing to the trunk, which is
/// the failure `wsp-146`'s pass already had to reason about and is worse here
/// because nothing in the store records it happening.
///
/// **Read through [`Seats`], the same reading the reconciler uses** — which is
/// the whole point of it being one thing. A member with no claim, or a claim
/// with no pane bound to it, does not hold the barrier: there is no agent there
/// to be working. `None` from a reader means nobody can say, and that never
/// holds a barrier — the failure of holding for ever is the one that needs no
/// verb to fix.
///
/// **Bounded, and the bound reports rather than proceeds.** A member still
/// working half an hour after everything else is in is not going to be waited
/// on for ever, and it is not passed over either: the seat is told which member
/// and what its screen says, which is the question a governor has and cannot
/// answer from the panel. Proceeding past it would be the run racing a member;
/// the sentence is what makes waiting a decision rather than a hang.
fn holding_barrier(
    store: &Store,
    seats: &dyn Seats,
    w: &Worklist,
    pos: &worklist::Position,
) -> Option<Vec<String>> {
    let mut waiting = Vec::new();
    let mut long = Vec::new();
    for s in &pos.members {
        let Some(seat) = bound_seat(store, &s.id) else { continue };
        match seats.state(&seat) {
            Some(State::Working) => {
                let said = format!("{seat} reads working");
                let over = store
                    .find_task(&s.id)
                    .and_then(|t| {
                        t.section("Log")
                            .and_then(|l| l.lines().filter(|l| l.contains(BUSY_SINCE)).last().map(|x| x.to_string()))
                    })
                    .and_then(|l| l.split_whitespace().nth(1).map(util::epoch_of))
                    .map(|since| since + OVERWORKED_AFTER <= util::epoch_secs())
                    .unwrap_or(false);
                // First sighting records the clock, so "half an hour" is
                // measured from when the run noticed rather than from when the
                // member last wrote to its row.
                if !over {
                    let id = s.id.clone();
                    store.locked(|| {
                        let Some(mut t) = store.find_task(&id) else { return false };
                        if t.section("Log").is_some_and(|l| l.contains(BUSY_SINCE)) {
                            return false;
                        }
                        t.log(&format!("{BUSY_SINCE} {} — the barrier is waiting on it", util::now_iso()));
                        t.touch();
                        store.save_task(&t).is_ok()
                    });
                }
                waiting.push(if over { format!("{seat} has been working past {OVERWORKED_AFTER}s") } else { said });
                if over {
                    long.push(format!("{seat} ({})", s.id));
                }
            }
            _ => {}
        }
    }
    if !long.is_empty() {
        tell(store, w, &format!(
            "The {} run's barrier has been waiting on {} — their panes read working with everything else in. \
             `wsp peek` shows what is on the screen; if they are stuck, `wsp reopen <id> \"what is owed\"` \
             moves the row and tells the pane.",
            w.id,
            long.join(" ")
        ));
    }
    (!waiting.is_empty()).then_some(waiting)
}

/// The marker in a member's `## Log` for "the barrier has been waiting on this
/// pane since".
const BUSY_SINCE: &str = "wsp: its pane has been working since";

/// The pane a member's agent is in, or `None` where there is no agent to hold a
/// barrier.
///
/// **A claim is not enough and a binding is.** A claim outlives its pane by
/// design ([`crate::store`]'s own argument) and a binding is the only record
/// that names a seat — so a member whose pane has gone and whose claim stands
/// reads as "no seat", which is correct: there is nobody working.
fn bound_seat(store: &Store, member: &str) -> Option<String> {
    let seat = store.panes_for_task(member).into_iter().next()?;
    store.claims().contains_key(member).then_some(seat)
}

/// How long a start may take to show up as a claim before wsp reads it as
/// one that never happened — an advance killed between its record and its
/// spawn, or a spawn that died without saying so. `wsp spawn` claims within a
/// minute when it works; ten leaves a slow one alone.
const WEDGED_AFTER: i64 = 10 * 60;

/// Unclaimed, and written long enough ago that a start in flight would have
/// claimed it by now.
fn wedged(t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> bool {
    !claims.contains_key(&t.id) && util::epoch_secs() - util::epoch_of(&t.updated) > WEDGED_AFTER
}

/// The line `take_member` writes, and how it recognises its own record.
const STARTED_BY: &str = "started by wsp:";

/// Move a member to `doing` under the store lock, if it may start now, and
/// say why it may and what it starts on. `None` is "leave it".
///
/// **The policy is read inside the lock too**, from the list as it stands
/// then, and not off the group [`step`] read before it. A member's line is
/// editable until the member starts (`wsp worklist member`, which writes
/// under the same lock and refuses once the member is `doing`), so the start
/// and the edit are ordered by the lock: an edit that lands first is the line
/// the member starts on, and one that comes after finds it started and is
/// refused. Read outside, an edit in between would be accepted and then
/// silently not be what ran.
///
/// It may when it is `todo` and unclaimed, or when it is a start of wsp's
/// own that never took (`doing`, unclaimed, [`STARTED_BY`] its last word, and
/// [`wedged`]). Either way **the cap is counted here, inside the lock**: two
/// advances run at a pass, each with its own reading of the group, and only a
/// count taken under the lock both of them write through can hold the cap.
fn take_member(
    store: &Store,
    t: &Task,
    list: &str,
    at: usize,
    members: &[String],
    cap: Option<usize>,
) -> Option<(&'static str, Policy)> {
    let id = t.id.clone();
    let took = store.locked(|| {
        let t = store.find_task(&id)?;
        let claims = store.claims();
        let ours = t.section("Log").and_then(|l| l.lines().last().map(|x| x.contains(STARTED_BY))).unwrap_or(false);
        let why = match t.status() {
            Status::Todo if !claims.contains_key(&id) => "new",
            Status::Doing if ours && wedged(&t, &claims) => "again: the last start never claimed it",
            _ => return None,
        };
        let going = members
            .iter()
            .filter(|m| **m != id)
            .filter_map(|m| store.find_task(m))
            .filter(|m| !Settlement::of(m).settled())
            .filter(|m| m.status() == Status::Doing || claims.contains_key(&m.id))
            .count();
        if cap.is_some_and(|c| going >= c) {
            return None;
        }
        let policy = store.worklist(list)?.groups().get(at - 1)?.policy_for(&id)?;
        let mut t = t;
        t.set_status(Status::Doing);
        t.log(&format!("{STARTED_BY} {list} group {at}"));
        t.touch();
        store.save_task(&t).ok()?;
        Some((why, policy))
    })?;
    store.git_commit(&format!("wsp: {list} group {at} starts {id}"));
    Some(took)
}

/// The newest verifier row under a member.
fn latest_verifier<'a>(tasks: &'a [Task], member: &str) -> Option<&'a Task> {
    tasks
        .iter()
        .filter(|t| t.parent.as_deref() == Some(member) && t.tags.iter().any(|g| g == VERIFY_TAG))
        .max_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)))
}

/// Whether a member's latest verifier has recorded a verdict.
///
/// The predicate the barrier is gated on, and `wsp-147`'s third repair reads it
/// to say *which* members the barrier is waiting on — a count is the question a
/// reader has and cannot act on, a list of ids is both.
pub(crate) fn verified(tasks: &[Task], member: &str) -> bool {
    latest_verifier(tasks, member).is_some_and(|v| matches!(v.status(), Status::Review | Status::Done))
}

/// Create the verifier row for a member, if one is owed, and hand back its id.
///
/// Owed when there is none, or when the last one blocked — found a problem —
/// and **the member's work has moved since it was read**: that is the member
/// coming back with a fix, and it is verified again by a fresh agent rather
/// than by the one that already made up its mind.
///
/// # Keyed on the landing, not on `updated` (`wsp-136` item 1)
///
/// This compared `member.updated > v.updated`, and **any write moves
/// `updated`** — a governor's `wsp note`, an edit to the overview, a `wsp mv`.
/// So saying one sentence about a member bought a whole new verifier agent: a
/// fresh context, a fresh read of the tree, and a verdict about code nobody had
/// touched. Three lines of note was enough, and nothing said so.
///
/// The key is [`crate::repair::landed`] — the commit the member's own `## Log`
/// records its work as landed on — read off the member when the verifier row
/// is created and off the verifier when the question is asked again. A fix
/// changes it; a note does not.
///
/// **A member with no landing recorded falls back to `updated`**, and that is
/// deliberate rather than a gap: design-only work has no repository and never
/// will, so it has no commit to key on, and a member whose fix is prose has
/// genuinely changed when it is touched. The landing exists wherever there is
/// somewhere to put it.
fn open_verifier(store: &Store, member: &Task, list: &str, at: usize, g: &Group) -> Option<String> {
    let now_landing = crate::repair::landed(member);
    let made = store.locked(|| {
        let tasks = store.tasks();
        let owed = match latest_verifier(&tasks, &member.id) {
            None => true,
            Some(v) if v.status() == Status::Todo && wedged(v, &store.claims()) => {
                return restart(store, v);
            }
            Some(v) => v.status() == Status::Blocked && moved_since(&now_landing, &member.updated, v),
        };
        if !owed {
            return None;
        }
        let id = store.alloc_task_id(member.project.as_deref()).ok()?;
        let mut t = Task::new(&format!("Verify {}: {}", member.id, util::truncate(&member.title, 60)), &id);
        t.project = member.project.clone();
        t.parent = Some(member.id.clone());
        t.tags = vec![VERIFY_TAG.to_string()];
        t.status_raw = Status::Todo.as_str().to_string();
        crate::model::set_section_in(&mut t.body, "Overview", &verifier_order(member, list, at, g, &id));
        // The commit this verifier is being asked to read, on the verifier's
        // own row: it is what the next comparison asks against, and it is what
        // makes the key survive the member being edited.
        if let Some(sha) = &now_landing {
            t.log(&format!("{} {sha}", crate::repair::READING));
        }
        store.save_task(&t).ok()?;
        Some(id)
    })?;
    store.git_commit(&format!("wsp: verify {} for {list} group {at}", member.id));
    Some(made)
}

/// Whether the member's work has moved since this verifier read it.
///
/// **The landing when the member records one, and `updated` when it does
/// not.** Design-only work has no repository and never will, so there is no
/// commit to compare and a member whose fix is prose has genuinely changed when
/// it is touched — which is what the old predicate was right about, and the
/// only case it was right about.
///
/// **Both sides need a landing.** A verifier created before `wsp land` wrote
/// one records no commit, so there is nothing to compare against; the fallback
/// is the old predicate rather than `true`, so the first landing after it does
/// not itself buy a verifier for code that was never re-read.
fn moved_since(now_landing: &Option<String>, member_updated: &str, verifier: &Task) -> bool {
    match (now_landing, crate::repair::landed(verifier)) {
        (Some(now), Some(then)) => now != &then,
        _ => member_updated > verifier.updated.as_str(),
    }
}

/// A row wsp made whose agent never arrived, taken again: touched, so a
/// second advance in the next ten minutes leaves it to this one.
fn restart(store: &Store, t: &Task) -> Option<String> {
    let mut t = t.clone();
    t.log("wsp is starting an agent on this again: the last one never claimed it");
    t.touch();
    store.save_task(&t).ok()?;
    Some(t.id)
}

/// How a barrier row's title starts; the list's slug follows it.
const BARRIER_TITLE: &str = "Barrier: ";

fn barrier_title(list: &str, at: usize) -> String {
    format!("{BARRIER_TITLE}{list} group {at}")
}

/// Whether a title is this group's barrier, whatever serial a re-check carries.
///
/// **A prefix test and not `==`, and the serial is why.** A held barrier is
/// checked again by a fresh agent (`open_barrier` below), so a group can hold
/// twice and its rows are then `Barrier: run group 2` and `Barrier: run group 2
/// (recheck 2)`. Three readers used to compare the whole title — `open_barrier`
/// finding the row to restart, `end_behind` finding the previous barrier's
/// check, and [`list_of`] finding the run a barrier row belongs to — and each
/// would quietly stop finding the re-check. Naming the serial once, here, is
/// what keeps one spelling of the question.
fn is_barrier(t: &Task, list: &str, at: usize) -> bool {
    t.tags.iter().any(|g| g == BARRIER_TAG) && t.title.starts_with(&barrier_title(list, at))
}

/// Create the barrier row, if one is owed, and hand back its id.
///
/// # A held barrier is checked again (`wsp-136` item 2)
///
/// The row's title is the idempotence key, and for as long as the barrier has
/// never been held that is exactly right: one row per group, and a second
/// advance finds it. But a `hold` leaves that row **settled** — the barrier
/// agent reviewed its own row and the group carries no verdict — so a resume
/// found the old row, started nothing, and the run stood still with every
/// member verified and nothing said.
///
/// So the key is the row **while it is open**, and a settled row is a barrier
/// that has already had its one check: the resume is a new check, on a new row,
/// with the number of prior checks in the title so a reader can tell which is
/// which and so [`is_barrier`] still finds them all. The resumer's `go` does
/// not pass the barrier itself; a fresh agent reads the group again, which is
/// what the barrier is for.
///
/// A `todo` row nobody claimed is still retaken rather than superseded — that
/// is a start that never arrived, not a check that finished.
fn open_barrier(store: &Store, w: &Worklist, at: usize, g: &Group) -> Option<String> {
    let project = g.members.iter().find_map(|m| store.find_task(m)).and_then(|t| t.project);
    let made = store.locked(|| {
        let tasks = store.tasks();
        let mut prior: Vec<&Task> = tasks.iter().filter(|t| is_barrier(t, &w.id, at)).collect();
        prior.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
        if let Some(b) = prior.last() {
            // `review` or `done`: the agent that ran this check has finished it.
            if !matches!(b.status(), Status::Review | Status::Done) {
                return match b.status() == Status::Todo && wedged(b, &store.claims()) {
                    true => restart(store, b).map(|id| (id, b.title.clone())),
                    false => None,
                };
            }
        }
        let title = match prior.len() {
            0 => barrier_title(&w.id, at),
            n => format!("{} (recheck {n})", barrier_title(&w.id, at)),
        };
        let id = store.alloc_task_id(project.as_deref()).ok()?;
        let mut t = Task::new(&title, &id);
        t.project = project.clone();
        t.tags = vec![BARRIER_TAG.to_string()];
        t.status_raw = Status::Todo.as_str().to_string();
        crate::model::set_section_in(&mut t.body, "Overview", &barrier_order(w, at, g, &id));
        store.save_task(&t).ok()?;
        Some((id, title))
    })?;
    let (id, title) = made;
    store.git_commit(&format!("wsp: {title}"));
    Some(id)
}

/// A verifier's work order, which is its row's overview: the brief every
/// spawned agent reads already carries it, so no kind needs a second channel.
fn verifier_order(member: &Task, list: &str, at: usize, g: &Group, me: &str) -> String {
    let stop = match g.stop.trim() {
        "" => String::new(),
        s => format!("\n\nThe barrier after this group asks: {s}"),
    };
    format!(
        "Verify **{m}** ({title}), a member of group {at} of the `{list}` worklist. wsp spawned \
         you when {m} reached review and landed; nobody else is going to read this work before \
         the barrier.\n\n\
         **You are read-only.** Do not edit, commit, land, rebase, or change {m}'s status. \
         Read what was asked and what was done with `wsp show {m}`, then check it against the \
         code on the trunk rather than against the report: the commits it landed, whether they \
         do what the overview asked, and the tests that cover them where those are cheap to run.\
         {stop}\n\n\
         **Finish in two commands.** Write your verdict as a note on {m}, three lines — what \
         you checked, what held, what did not — with `wsp note {m} --from FILE`. Then:\n\
         - it holds: `wsp review {me} -` with one line saying so;\n\
         - it does not: `wsp block {me} --from FILE`, saying what has to change.\n\n\
         wsp takes the next step from either; there is nobody to tell.",
        m = member.id,
        title = util::truncate(&member.title, 80),
    )
}

/// The barrier agent's work order.
fn barrier_order(w: &Worklist, at: usize, g: &Group, me: &str) -> String {
    let stop = match g.stop.trim() {
        "" => "There is no stop condition written at this barrier.".to_string(),
        s => format!("The stop condition, which your verdict answers:\n\n> {s}"),
    };
    format!(
        "Check the barrier after group {at} of the `{list}` worklist. Every member has landed \
         and each has a verifier's verdict, as a note on the member: {members}.\n\n\
         {stop}\n\n\
         **Do not edit, commit, land or rebase.** The per-member reviews are done; what is \
         left is what only the group as a whole can show. `wsp worklist next {list}` gives \
         what the group landed and which members touched one file. Read the verdicts, check \
         the group together on the trunk — it builds, its suites pass together, no two members \
         undid each other — and whether the next group has what it depends on.\n\n\
         **Then decide, and wsp does the rest.** Write the verdict in under 300 words to a \
         file, then:\n\
         - it passes: `wsp worklist go {list} --from FILE`. wsp starts the next group and \
         rotates the governor;\n\
         - it does not: `wsp worklist hold {list} --from FILE`, saying what has to happen. \
         The governor is told.\n\n\
         Either way, finish with `wsp review {me} -` and one line.",
        list = w.id,
        members = g.members.join(" "),
    )
}

/// What a failed start puts back.
#[derive(Clone, Copy)]
enum Undo {
    /// A member wsp moved to `doing`: back to `todo`, so the next advance —
    /// or a governor's own `wsp spawn` — finds it as it was.
    Member,
    /// A verifier or barrier row: left at `todo`, which the next advance
    /// takes again once it is [`wedged`].
    Row,
}

/// Start an agent on a row, and wait for `wsp spawn` to say how it went.
/// A failure is written on the row and put back per [`Undo`]; the caller
/// tells the seat. Never silent: a run that cannot start its next agent is
/// a run that has stopped.
fn spawn(store: &Store, id: &str, policy: &Policy, undo: Undo) -> bool {
    let failed = start(id, policy);
    let Some(why) = failed else { return true };
    stamp(&format!("FAILED spawn {id}: {why}"));
    let saved = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        t.log(&format!("wsp could not spawn an agent on this: {}", util::truncate(&why, 300)));
        if matches!(undo, Undo::Member) && t.status() == Status::Doing {
            t.set_status(Status::Todo);
        }
        t.touch();
        store.save_task(&t).is_ok()
    });
    if saved {
        store.git_commit(&format!("wsp: spawn failed on {id}"));
    }
    false
}

/// `wsp spawn <id> --agent …`, and its refusal if it refused.
fn start(id: &str, policy: &Policy) -> Option<String> {
    // A test's binary is the harness: record what would have started instead.
    if cfg!(test) {
        #[cfg(test)]
        return tests::SPAWNED.with(|s| {
            s.borrow_mut().push((id.to_string(), policy.kind.clone()));
            tests::FAIL.with(|f| f.borrow().clone())
        });
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Some(e.to_string()),
    };
    let mut argv: Vec<String> = vec!["spawn".into(), id.into(), "--agent".into()];
    argv.extend(policy.spawn_flags());
    stamp(&format!("spawn {}", argv[1..].join(" ")));
    match Command::new(exe).args(&argv).stdin(Stdio::null()).output() {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Some(e.to_string()),
    }
}

/// Tell the seat a start failed, so a stopped run is somebody's to see.
fn failed(store: &Store, w: &Worklist, id: &str) {
    tell(store, w, &format!(
        "wsp could not start an agent on {id} in the {} run, so the run is waiting there. \
         `wsp show {id}` has the reason; `wsp worklist advance {}` tries again.",
        w.id, w.id
    ));
}

/// End the agents a passed group no longer needs: its members, their
/// verifiers, and the check on the barrier *before* it.
///
/// Not this barrier's own check. Its agent is the one running the `go` that
/// got us here, and ending it now would cut the turn in which it reviews its
/// own row; it is ended at the next pass instead, when it has long been idle.
/// The governor used to do all of this by hand, and a run wsp starts has to be
/// a run wsp tidies, or every group leaves its agents standing.
fn end_behind(store: &Store, w: &Worklist, passed: usize) {
    let groups = w.groups();
    let Some(g) = groups.get(passed - 1) else { return };
    let tasks = store.tasks();
    let mut ids: Vec<String> = g.members.clone();
    ids.extend(
        tasks
            .iter()
            .filter(|t| t.tags.iter().any(|x| x == VERIFY_TAG))
            .filter(|t| t.parent.as_ref().is_some_and(|p| g.members.contains(p)))
            .map(|t| t.id.clone()),
    );
    if passed > 1 {
        // **Every row of the previous barrier, not the one with the bare
        // title.** A group that held twice has two barrier rows (`wsp-136`
        // item 2), and a re-check is an agent like any other — leaving the
        // held one standing is exactly the leak this row exists to close.
        ids.extend(tasks.iter().filter(|t| is_barrier(t, &w.id, passed - 1)).map(|t| t.id.clone()));
    }
    end_all(store, ids);
}

/// End every row in `ids` that still holds a claim, through `wsp despawn`.
///
/// **Split out because the last pass needs it and `end_behind` cannot reach
/// it.** The pass at the *end* of a run has no next pass, so `end_behind`'s
/// "the previous barrier, ended at the next pass" has no second call to arrive
/// at — and the last barrier's agent, the one whose `go` finished the run, was
/// left holding a claim for ever (`wsp-136` item 3). Ending it from inside the
/// `go` is not available either: that is the turn in which it reviews its own
/// row. So it is ended on the tick *after* the run has nothing left to govern,
/// which is [`crate::repair`]'s call and the only place where "the last barrier
/// is finished" is knowable without cutting a turn short.
///
/// **The claim is the test, and `wsp despawn` is the whole ending**: agent,
/// claim and tree, keeping the tree when it has uncommitted work in it, which
/// it says in `cycle.log` rather than leaving silent.
pub(crate) fn end_all(store: &Store, ids: Vec<String>) {
    let claims = store.claims();
    for id in ids.iter().filter(|id| claims.contains_key(*id)) {
        despawn(id);
    }
}

/// The barrier checks of a run that has nothing left in it, whose agents are
/// still holding claims.
///
/// Called by the reconciler on every tick, and keyed on the claims rather than
/// on a record, because there is nothing to write: an ended agent leaves no
/// claim, so the second tick finds nothing and a tick while the run is still
/// running finds no finished barrier. `wsp-136` item 3.
pub(crate) fn last_barrier_left_behind(store: &Store, w: &Worklist) -> Vec<String> {
    let pos = worklist::position(store, w, worklist::Reading::Settled);
    if !pos.finished() {
        return Vec::new();
    }
    let tasks = store.tasks();
    let claims = store.claims();
    let standing: Vec<String> = tasks
        .iter()
        .filter(|t| t.tags.iter().any(|g| g == BARRIER_TAG))
        .filter(|t| matches!(t.status(), Status::Review | Status::Done))
        .filter(|t| list_of(store, t).map(|l| l.id == w.id).unwrap_or(false))
        .filter(|t| claims.contains_key(&t.id))
        .map(|t| t.id.clone())
        .collect();
    if !standing.is_empty() {
        stamp(&format!("{}: the run has nothing left in it, and {} finished", w.id, standing.join(" ")));
    }
    standing
}

pub(crate) fn despawn(id: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::ENDED.with(|s| s.borrow_mut().push(id.to_string()));
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let out = Command::new(exe).args(["despawn", id]).stdin(Stdio::null()).output();
    let said = match out {
        Ok(o) if o.status.success() => "ended".to_string(),
        Ok(o) => format!("not ended: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("not ended: {e}"),
    };
    stamp(&format!("despawn {id}: {said}"));
}

/// The seat that answers for a run: the list's own, when somebody holds it,
/// and otherwise the seat routing reaches for its first member.
fn governing_scope(store: &Store, w: &Worklist) -> Option<String> {
    let governors = store.governors();
    if crate::cmd_govern::seat_of_scope(&w.id, &governors).is_some() {
        return Some(w.id.clone());
    }
    let index = crate::resolve::Index::new(store.projects());
    let first = w.groups().iter().flat_map(|g| g.members.clone()).find_map(|m| store.find_task(&m))?;
    let p = first.project?;
    std::iter::once(p.clone())
        .chain(index.ancestors(&p))
        .find(|s| crate::cmd_govern::seat_of_scope(s, &governors).is_some())
}

/// Tell whoever governs this run. With nobody seated, the sentence is raised
/// as a hand on the run's first member instead, where a person's panel draws
/// it — Ed, 2026-09-30: "assuming a governor is in place".
///
/// **The spool, and the daemon, since `wsp-146`.** This used to run
/// `wsp govern --tell`, which types at the seat's pane and **refuses a governor
/// mid-turn** — and a verdict at a barrier is the sentence a governor is
/// mid-turn for exactly when it is wanted. Its fallback is below, and it was
/// firing for the wrong reason: a governor that was mid-turn got its run's
/// verdict raised on a *member*, so the governor was told about its own barrier
/// on a row it does not own.
///
/// Now busy means later, and [`crate::wake::say`] is the only thing this calls:
/// the words go in the seat's spool inside the lock, the same gate the daemon
/// asks before typing, and it is cleared only when a turn comes of it.
pub(crate) fn tell(store: &Store, w: &Worklist, text: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::TOLD.with(|s| s.borrow_mut().push(text.to_string()));
        return;
    }
    stamp(&format!("tell {}: {}", w.id, util::truncate(text, 120)));
    if hand_it_to_the_governor(store, w, text) {
        return;
    }
    // On a member of the group the run stands at, where a person looking at
    // the run is looking — not group 1's, long since finished.
    let w = store.worklist(&w.id).unwrap_or_else(|| w.clone());
    let groups = w.groups();
    let here = groups.iter().find(|g| g.verdict.trim().is_empty()).or_else(|| groups.last());
    let Some(first) = here.and_then(|g| g.members.first()).cloned() else { return };
    let args = Args::synth("flag", &[first.as_str(), text], &[]);
    let _ = crate::cmd_agent::flag(store, &args);
}

/// The half of [`tell`] that decides where a verdict goes, and whether a post
/// took it.
///
/// **Split out so it can be driven.** `tell` records what it said and stops
/// there under `cfg(test)`, because a cycle test asserts on the sentences and
/// not on where they land — which is right for them and useless for this row:
/// `wsp-146` is about *where*, and a test that cannot reach the half that
/// decides would not notice that half being deleted.
///
/// Returns whether a seat on the scope holds it, and `false` is the caller
/// falling back to a hand on the run's first member.
pub(crate) fn hand_it_to_the_governor(store: &Store, w: &Worklist, text: &str) -> bool {
    let Some(scope) = governing_scope(store, w) else { return false };
    match crate::wake::say(store, &scope, text, None) {
        // Said as well as returned, because `cycle.log` is how a governor reads
        // what the run has been doing, and *the seat is mid-turn* is the answer
        // to "why has nobody started my next group" that used to be missing from
        // it — the refusal this row removed was reported as a hand on a member.
        Some(report) => {
            stamp(&format!("told {scope}: {} · {} held", report.why, report.held));
            true
        }
        None => {
            stamp(&format!("told {scope}: no seat on the scope"));
            false
        }
    }
}

/// A pass hands the governing seat to a fresh successor, on wsp's own
/// initiative: `cmd_spawn::rotate_on_behalf` carries the argument.
fn rotate(store: &Store, w: &Worklist) {
    if cfg!(test) {
        #[cfg(test)]
        tests::ROTATED.with(|s| s.borrow_mut().push(w.id.clone()));
        return;
    }
    let Some(scope) = governing_scope(store, w) else { return };
    let code = crate::cmd_spawn::rotate_on_behalf(store, &scope);
    stamp(&format!("rotate {scope}: exit {code}"));
}

/// One dated line in `cycle.log`, which is this process's stdout.
///
/// **`pub(crate)` since `wsp-147`**: the reconciler logs through this rather
/// than opening the file itself, so a repair's lines and the run steps' lines
/// are in one file in one order with one format — and the record the barrier's
/// wait keeps under `cfg(test)` is here for the same reason the reconciler's is:
/// a step whose contract is "says why" cannot be tested by reading a file the
/// code deliberately does not write with no terminal to write it to.
pub(crate) fn stamp(line: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::SAID.with(|s| s.borrow_mut().push(line.to_string()));
    }
    if cfg!(test) {
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{} {line}", util::now_iso());
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// No backend answers for any seat.
    ///
    /// **The right answer for most of these tests, not a dodge.** A store with
    /// no project has no member with a seat, and `state_of_a_seat_nobody_can
    /// answer_for` must not hold a barrier — so a test about the *chain* wants
    /// a reader that says nothing rather than one that happens to say idle. The
    /// tests that are about the gate name the seats they need.
    pub(super) struct Blind;

    impl Seats for Blind {
        fn state(&self, _seat: &str) -> Option<State> {
            None
        }
    }

    /// The seats a test says something about, everything else unknown.
    pub(super) struct Fixed(std::collections::BTreeMap<String, State>);

    impl Fixed {
        pub(super) fn new(pairs: &[(&str, State)]) -> Fixed {
            Fixed(pairs.iter().map(|(s, v)| (s.to_string(), *v)).collect())
        }
    }

    impl Seats for Fixed {
        fn state(&self, seat: &str) -> Option<State> {
            self.0.get(seat).copied()
        }
    }

    thread_local! {
        pub(crate) static SPAWNED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
        pub(crate) static ENDED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        pub(crate) static TOLD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        pub(crate) static ROTATED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Set, and every start in this thread fails with it.
        pub(super) static FAIL: RefCell<Option<String>> = const { RefCell::new(None) };
        /// What `cycle.log` was told this thread, in order.
        pub(crate) static SAID: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// What `wsp-147`'s reconciler told a member's seat, as (task, sentence).
        /// Distinct from `TOLD`, which is the run's *governing* seat: a repair
        /// tells both and they are answers to different questions.
        pub(crate) static MEMBER_TOLD: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    }

    fn spawned() -> Vec<(String, String)> {
        SPAWNED.with(|s| s.borrow_mut().drain(..).collect())
    }

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("cycle-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    fn task(store: &Store, id: &str, status: Status) {
        let mut t = Task::new(id, id);
        t.status_raw = status.as_str().into();
        store.save_task(&t).unwrap();
    }

    fn set(store: &Store, id: &str, status: Status) {
        let mut t = store.find_task(id).unwrap();
        t.set_status(status);
        t.touch();
        store.save_task(&t).unwrap();
    }

    fn list(store: &Store, groups: &[(&[&str], &str)]) -> Worklist {
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(
            &groups
                .iter()
                .map(|(m, a)| Group {
                    members: m.iter().map(|x| x.to_string()).collect(),
                    agent: a.to_string(),
                    ..Group::default()
                })
                .collect::<Vec<_>>(),
        );
        store.save_worklist(&w).unwrap();
        w
    }

    fn tagged(store: &Store, tag: &str) -> Vec<Task> {
        store.tasks().into_iter().filter(|t| t.tags.iter().any(|g| g == tag)).collect()
    }

    /// The whole chain, one step at a time, and each step taken once however
    /// often it is asked: the run advances off the store, not off a memory of
    /// what was done.
    #[test]
    fn a_group_with_a_policy_is_started_verified_and_brought_to_its_barrier_by_wsp() {
        let (_env, store) = scratch("chain");
        task(&store, "m-1", Status::Todo);
        task(&store, "m-2", Status::Todo);
        let w = list(&store, &[(&["m-1", "m-2"], "opencode some/model")]);

        step(&store, &w, &Blind);
        assert_eq!(
            spawned(),
            vec![("m-1".into(), "opencode".into()), ("m-2".into(), "opencode".into())],
            "both members start, on the group's policy"
        );
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing, "the record that stops a second start");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a second advance starts nothing twice");

        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        let v = tagged(&store, VERIFY_TAG);
        assert_eq!(v.len(), 1, "one verifier, for the member that finished");
        assert_eq!(v[0].parent.as_deref(), Some("m-1"));
        assert!(v[0].section("Overview").unwrap().contains("read-only"), "its order is its overview");
        assert_eq!(spawned(), vec![(v[0].id.clone(), "opencode".into())], "on the same policy: the floor holds");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty() && tagged(&store, VERIFY_TAG).len() == 1, "and only once");

        set(&store, "m-2", Status::Review);
        step(&store, &w, &Blind);
        let _ = spawned();
        assert!(tagged(&store, BARRIER_TAG).is_empty(), "no barrier check while a verdict is outstanding");

        for v in tagged(&store, VERIFY_TAG) {
            set(&store, &v.id, Status::Review);
        }
        step(&store, &w, &Blind);
        let b = tagged(&store, BARRIER_TAG);
        assert_eq!(b.len(), 1, "every member verified: one barrier check");
        assert_eq!(b[0].title, "Barrier: run group 1");
        assert!(b[0].section("Overview").unwrap().contains("wsp worklist go run --from FILE"));
        assert_eq!(spawned().len(), 1);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the barrier row is the key; nothing starts twice");
    }

    /// `wsp-150`: one claude member beside an opencode one, in one group and
    /// behind one barrier. Each starts on its own kind, each verifier on the
    /// kind its member ran on, and the barrier check on the group's line.
    #[test]
    fn a_mixed_group_starts_and_verifies_each_member_on_its_own_kind() {
        let (_env, store) = scratch("mixed");
        task(&store, "m-1", Status::Todo);
        task(&store, "m-2", Status::Todo);
        let mut w = list(&store, &[(&["m-1", "m-2"], "opencode some/model-free")]);
        let mut g = w.groups();
        g[0].member_agents.insert("m-2".into(), "claude opus high".into());
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();

        step(&store, &w, &Blind);
        assert_eq!(
            spawned(),
            vec![("m-1".into(), "opencode".into()), ("m-2".into(), "claude".into())],
            "the member with a line of its own starts on it, and the other on the group's"
        );

        set(&store, "m-1", Status::Review);
        set(&store, "m-2", Status::Review);
        step(&store, &w, &Blind);
        let by_parent: Vec<(String, String)> = spawned()
            .into_iter()
            .map(|(id, kind)| (store.find_task(&id).unwrap().parent.unwrap(), kind))
            .collect();
        assert_eq!(
            by_parent,
            vec![("m-1".into(), "opencode".into()), ("m-2".into(), "claude".into())],
            "a verifier runs on the kind its member's work ran on"
        );

        for v in tagged(&store, VERIFY_TAG) {
            set(&store, &v.id, Status::Review);
        }
        step(&store, &w, &Blind);
        assert_eq!(
            spawned(),
            vec![(tagged(&store, BARRIER_TAG)[0].id.clone(), "opencode".into())],
            "the barrier reads the group as a whole, on the group's line"
        );
    }

    /// A verifier that found a problem blocks, the member comes back with a
    /// fix, and it is read again by a fresh agent.
    #[test]
    fn a_member_fixed_after_a_failed_verdict_gets_a_fresh_verifier() {
        let (_env, store) = scratch("again");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        let first = tagged(&store, VERIFY_TAG).remove(0);
        set(&store, &first.id, Status::Blocked);
        step(&store, &w, &Blind);
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 1, "blocked, and nothing new until the member moves");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 2, "the fix is verified again");
        let _ = spawned();
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

    /// `wsp-164`. Every member landed and verified is not the same as every
    /// member *finished*: one can be mid-turn on more work with its row at
    /// `review`, because it picked something up itself or a person typed to it.
    /// Opening a barrier over that hands the group to a fresh agent while one of
    /// its own is still writing to the trunk.
    ///
    /// So the barrier waits, **and says so**: a no-op which writes nothing is
    /// indistinguishable from a stall with no cause, which is `wsp-142`'s
    /// lesson.
    #[test]
    fn a_barrier_waits_for_a_member_whose_pane_reads_working_and_says_why() {
        let (_env, store) = scratch("busybarrier");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Review);
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        step(&store, &w, &Blind);
        for v in tagged(&store, VERIFY_TAG) {
            set(&store, &v.id, Status::Review);
        }
        let _ = spawned();
        for m in ["m-1", "m-2"] {
            store.set_claim(m, serde_json::json!({ "workspace": "w" }));
        }
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        store.set_binding("cpd-2", serde_json::json!({ "task_id": "m-2" }));

        // One member's seat is mid-turn; the other is idle at a prompt.
        let seats = Fixed::new(&[("cpd-1", State::Working), ("cpd-2", State::Idle)]);
        step(&store, &w, &seats);
        assert!(tagged(&store, BARRIER_TAG).is_empty(), "no barrier over a member that is still working");
        assert!(tests::SAID.with(|s| s.borrow().iter().any(|l: &String| l.contains("cpd-1 reads working"))));

        // And it opens on the tick after that seat falls idle.
        let idle = Fixed::new(&[("cpd-1", State::Idle), ("cpd-2", State::Idle)]);
        step(&store, &w, &idle);
        assert_eq!(tagged(&store, BARRIER_TAG).len(), 1, "a tick later, and it opens");
    }

    /// **The dependency `wsp-164` names**, and the reason `wsp-160` had to land
    /// first. A frozen opencode screen reads `working` for ever; without the
    /// database overruling it this gate would hold every barrier in the fleet
    /// for ever, and the fix would have been a way of passing them.
    #[test]
    fn a_frozen_opencode_seat_does_not_hold_the_barrier_because_the_database_says_it_finished() {
        let (_env, store) = scratch("frozenbarrier");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        for v in tagged(&store, VERIFY_TAG) {
            set(&store, &v.id, Status::Review);
        }
        let _ = spawned();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));

        // What `Fleet` reports for a screen frozen on its busy frame: the
        // screen says working, the session finished a minute ago.
        struct Frozen;
        impl Seats for Frozen {
            fn state(&self, _seat: &str) -> Option<State> {
                Some(overrule(State::Working, Some(util::epoch_secs() - crate::agent_commands::SETTLED_AFTER - 1), util::epoch_secs()))
            }
        }
        step(&store, &w, &Frozen);
        assert_eq!(tagged(&store, BARRIER_TAG).len(), 1, "a frozen screen is not a turn in flight");
    }

    /// `wsp-164`'s bound. A member still working half an hour after everything
    /// else is in is not waited on for ever and is not passed over either: the
    /// seat is told which member and what its screen says. Proceeding would race
    /// it, and silence would make the waiting look like a hang.
    #[test]
    fn a_member_working_past_the_bound_is_reported_to_the_seat_rather_than_waited_on_silently() {
        let (_env, store) = scratch("overworked");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        for v in tagged(&store, VERIFY_TAG) {
            set(&store, &v.id, Status::Review);
        }
        let _ = spawned();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        let seats = Fixed::new(&[("cpd-1", State::Working)]);

        step(&store, &w, &seats);
        assert!(tagged(&store, BARRIER_TAG).is_empty(), "not passed over");
        assert!(drained(&TOLD).is_empty(), "and not reported yet: the clock starts now");

        age_the_wait(&store, "m-1");
        step(&store, &w, &seats);
        assert!(
            drained(&TOLD).iter().any(|t| t.contains("cpd-1") && t.contains("wsp reopen")),
            "half an hour on, the seat is told which member and what its screen says"
        );
    }

    /// A member with a claim and no pane bound to it has no agent that could be
    /// working, so it does not hold a barrier — and neither does a member whose
    /// claim has gone. Holding for a seat that is not there would need a verb to
    /// fix, and that is the failure mode this gate exists to avoid.
    #[test]
    fn a_member_with_no_seat_or_no_claim_does_not_hold_the_barrier() {
        for (tag, claim, bound) in [("nopane", true, false), ("noclaim", false, true)] {
            let (_env, store) = scratch(tag);
            task(&store, "m-1", Status::Review);
            let w = list(&store, &[(&["m-1"], "claude")]);
            step(&store, &w, &Blind);
            for v in tagged(&store, VERIFY_TAG) {
                set(&store, &v.id, Status::Review);
            }
            let _ = spawned();
            if claim {
                store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
            }
            if bound {
                store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
            }
            let seats = Fixed::new(&[("cpd-1", State::Working)]);
            step(&store, &w, &seats);
            assert_eq!(tagged(&store, BARRIER_TAG).len(), 1, "`{tag}`: there is nobody there to hold it");
        }
    }

    /// Backdate the note that starts the barrier's wait on a pane.
    fn age_the_wait(store: &Store, id: &str) {
        let mut t = store.find_task(id).unwrap();
        let log = t.section("Log").unwrap_or_default();
        let aged: Vec<String> = log
            .lines()
            .map(|l| {
                if l.contains(BUSY_SINCE) {
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

    /// `wsp-136` item 1. A note on a member is somebody **saying** something
    /// about it, and it used to buy a whole fresh verifier agent: a new
    /// context, a new read of the tree, and a verdict about code nobody had
    /// touched. Three lines of note was enough and nothing said so.
    ///
    /// The fix keys the re-verify on the commit the member's work is on. This
    /// test runs both halves: a note changes nothing, a new landing does.
    #[test]
    fn a_note_on_a_member_does_not_buy_a_fresh_verifier_and_a_new_landing_does() {
        let (_env, store) = scratch("note");
        task(&store, "m-1", Status::Review);
        let mut m = store.find_task("m-1").unwrap();
        m.log(&format!("{} abc1234 on master", crate::repair::LANDED));
        store.save_task(&m).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        let first = tagged(&store, VERIFY_TAG).remove(0);
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 1, "the first verifier, which read abc1234");
        set(&store, &first.id, Status::Blocked);

        step(&store, &w, &Blind);
        let mut m = store.find_task("m-1").unwrap();
        m.log("the verifier is right about the second half");
        m.touch();
        store.save_task(&m).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(
            tagged(&store, VERIFY_TAG).len(),
            1,
            "a note moved `updated` and nothing else: still the same work, still one verifier"
        );

        // The member comes back with a fix, which is a new commit on the trunk.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut m = store.find_task("m-1").unwrap();
        m.log(&format!("{} def5678 on master", crate::repair::LANDED));
        store.save_task(&m).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(
            tagged(&store, VERIFY_TAG).len(),
            2,
            "a new landing is new work, and a fresh agent reads it"
        );
        let _ = spawned();
    }

    /// `wsp-136` item 2. A `hold` leaves the barrier row **settled** — the
    /// agent reviewed its own row — and the title was the idempotence key, so a
    /// resume found the old row, started nothing, and stood there: every member
    /// landed and verified, with nothing said.
    ///
    /// The check is what the resumer's `go` is for, so a resume is a *new*
    /// check on a new row. A row nobody claimed is still retaken rather than
    /// superseded, which is the second half of this test.
    #[test]
    fn a_held_barrier_is_checked_again_on_a_resume_and_an_unclaimed_row_is_still_retaken() {
        let (_env, store) = scratch("recheck");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        // A barrier opens on every member's *verdict*, so settle this one first
        // — the group is otherwise still waiting on the verifier, not on the
        // barrier, and this test is about the barrier.
        set(&store, &tagged(&store, VERIFY_TAG)[0].id, Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(tagged(&store, BARRIER_TAG).len(), 1, "the first check");
        let first = tagged(&store, BARRIER_TAG).remove(0);
        let _ = spawned();
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "and one check is enough while it stands");

        // Held: the agent holds and then reviews its own row.
        set(&store, &first.id, Status::Review);
        step(&store, &w, &Blind);
        let _ = spawned();
        let second: Vec<Task> = tagged(&store, BARRIER_TAG).into_iter().filter(|t| t.id != first.id).collect();
        assert_eq!(second.len(), 1, "a resume is a fresh check, not the old row");
        assert!(
            second[0].title.starts_with(&barrier_title("run", 1)) && second[0].title != first.title,
            "and it says which check it is: {}",
            second[0].title
        );
        assert_eq!(list_of(&store, &second[0]).map(|w| w.id), Some("run".into()), "and still finds its run");

        // And once that one has held too, a third.
        set(&store, &second[0].id, Status::Review);
        step(&store, &w, &Blind);
        let _ = spawned();
        assert_eq!(tagged(&store, BARRIER_TAG).len(), 3, "each hold is its own check");

    }

    /// The other half of that, and the reason the retake arm is still there: a
    /// barrier row whose agent never arrived is **not** a check that finished.
    /// Treating it as one would supersede a row whose agent might be a minute
    /// out, and the run would accumulate barrier checks for a barrier nobody
    /// read.
    #[test]
    fn a_barrier_row_whose_agent_never_arrived_is_retaken_rather_than_superseded() {
        let (_env, store) = scratch("recheckwedge");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        set(&store, &tagged(&store, VERIFY_TAG)[0].id, Status::Review);
        step(&store, &w, &Blind);
        let only = tagged(&store, BARRIER_TAG).remove(0);
        let _ = spawned();
        let mut b = store.find_task(&only.id).unwrap();
        b.updated = "2026-01-01T00:00:00Z".into();
        store.save_task(&b).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(
            tagged(&store, BARRIER_TAG).len(),
            1,
            "the same row, started again — a start that never arrived is not a check that finished"
        );
        assert_eq!(spawned(), vec![(only.id, "claude".into())]);
    }

    /// `wsp-136` item 3. `end_behind` ends the check on the barrier *before* the
    /// one just passed, so the check whose `go` finished the run was left
    /// holding a claim for ever — there being no next pass to reach it from, and
    /// no way to end it inside the `go` without cutting the turn in which it
    /// reviews its own row.
    ///
    /// So the reconciler ends it on a tick, and only once the run has nothing
    /// left to govern: ending a check while the run still has groups to come
    /// would kill an agent that is about to be asked something.
    #[test]
    fn the_last_barriers_agent_is_ended_by_a_tick_once_the_run_has_nothing_left() {
        use crate::repair::Pass;
        let (_env, store) = scratch("lastbarrier");
        task(&store, "m-1", Status::Review);
        let mut w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        set(&store, &tagged(&store, VERIFY_TAG)[0].id, Status::Review);
        step(&store, &w, &Blind);
        let check = tagged(&store, BARRIER_TAG).remove(0);
        let _ = spawned();
        set(&store, &check.id, Status::Review);
        store.set_claim(&check.id, serde_json::json!({ "workspace": "w" }));

        // The run is still standing at group 1, so nothing is ended: the check
        // is not finished work, it is a barrier somebody may still pass.
        let mut g = w.groups();
        g[0].verdict = String::new();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert!(
            tests::ENDED.with(|e| e.borrow().is_empty()),
            "a barrier the run is still standing at is nobody's to end"
        );

        // With the verdict written the run has nothing left to govern.
        let mut g = store.worklist("run").unwrap().groups();
        g[0].verdict = "passed".into();
        let mut w = store.worklist("run").unwrap();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert_eq!(
            tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>()),
            vec![check.id.clone()],
            "and the run's last barrier check is ended, which nothing else could reach"
        );

        // And once, because the claim is the record. `despawn` is stubbed
        // above, so the release `wsp despawn` does is done here — it is the
        // half of the ending the next tick keys on.
        store.clear_claim(&check.id);
        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert!(tests::ENDED.with(|e| e.borrow().is_empty()), "an ended agent leaves no claim to find");
    }

    /// Everything written before `wsp-134`, and a group somebody turned off,
    /// is left to its governor.
    #[test]
    fn a_group_with_no_policy_or_a_manual_one_is_left_alone() {
        for agent in ["", "manual"] {
            let (_env, store) = scratch(&format!("hand{}", agent.len()));
            task(&store, "m-1", Status::Todo);
            let w = list(&store, &[(&["m-1"], agent)]);
            step(&store, &w, &Blind);
            assert!(spawned().is_empty(), "`{agent}` is run by hand");
            assert_eq!(store.find_task("m-1").unwrap().status(), Status::Todo);
        }
    }

    /// The cap is on the work, and a member already going counts against it.
    #[test]
    fn a_capped_group_starts_no_more_than_its_cap() {
        let (_env, store) = scratch("cap");
        for m in ["m-1", "m-2", "m-3"] {
            task(&store, m, Status::Todo);
        }
        let mut w = list(&store, &[(&["m-1", "m-2", "m-3"], "claude")]);
        let mut g = w.groups();
        g[0].cap = Some(2);
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 2);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "two are going, and the cap is two");
    }

    /// A pass ends what the group needed and nothing the run still does: the
    /// check that is passing it is left to finish its own turn.
    #[test]
    fn a_pass_ends_the_groups_agents_and_the_last_barriers_check_but_not_its_own() {
        let (_env, store) = scratch("ends");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        let mut v1 = Task::new("v", "v-1");
        v1.parent = Some("m-2".into());
        v1.tags = vec![VERIFY_TAG.into()];
        store.save_task(&v1).unwrap();
        for (id, title) in [("b-1", barrier_title("run", 1)), ("b-2", barrier_title("run", 2))] {
            let mut b = Task::new(&title, id);
            b.tags = vec![BARRIER_TAG.into()];
            store.save_task(&b).unwrap();
        }
        for id in ["m-1", "m-2", "v-1", "b-1", "b-2"] {
            store.set_claim(id, serde_json::json!({ "workspace": "w" }));
        }
        end_behind(&store, &w, 2);
        let ended = ENDED.with(|s| s.borrow_mut().drain(..).collect::<Vec<_>>());
        assert_eq!(ended, vec!["m-2", "v-1", "b-1"]);
    }

    fn drained<T>(k: &'static std::thread::LocalKey<RefCell<Vec<T>>>) -> Vec<T> {
        k.with(|s| s.borrow_mut().drain(..).collect())
    }

    fn go(store: &Store, passed: usize) {
        let args = Args::synth("worklist", &["advance", "run"], &[("event", "go"), ("passed", &passed.to_string())]);
        assert_eq!(advance(store, &args), 0);
    }

    /// wsp-135's FAIL: ux-revamp is a hand-run list, and its governor's own
    /// `go` must not have wsp end its members and rotate it on top of its own
    /// `--rotate`.
    #[test]
    fn a_go_on_a_hand_run_group_ends_nothing_rotates_nothing_and_tells_nobody() {
        for agent in ["", "manual"] {
            let (_env, store) = scratch(&format!("handgo{}", agent.len()));
            task(&store, "m-1", Status::Review);
            task(&store, "m-2", Status::Todo);
            let mut w = list(&store, &[(&["m-1"], agent), (&["m-2"], agent)]);
            let mut g = w.groups();
            g[0].verdict = "passed by hand".into();
            w.set_groups(&g);
            store.save_worklist(&w).unwrap();
            store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
            go(&store, 1);
            assert!(drained(&ENDED).is_empty(), "`{agent}`: nothing ended");
            assert!(drained(&ROTATED).is_empty(), "`{agent}`: nobody rotated");
            assert!(drained(&TOLD).is_empty(), "`{agent}`: the governor passed it and needs no telling");
            assert!(spawned().is_empty(), "`{agent}`: and the next hand-run group is not started");
        }
    }

    /// The other half: a group wsp runs is tidied, the seat moves on, and the
    /// next group starts — and on the last pass, nobody is rotated onto a run
    /// with nothing left in it.
    #[test]
    fn a_go_on_a_group_wsp_runs_ends_its_agents_rotates_and_starts_the_next() {
        let (_env, store) = scratch("autogo");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Todo);
        let mut w = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        let mut g = w.groups();
        g[0].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        go(&store, 1);
        assert_eq!(drained(&ENDED), vec!["m-1"]);
        assert_eq!(drained(&ROTATED), vec!["run"]);
        assert_eq!(drained(&TOLD).len(), 1);
        assert_eq!(spawned(), vec![("m-2".into(), "claude".into())]);

        let mut g = w.groups();
        g[1].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        go(&store, 2);
        assert!(drained(&ROTATED).is_empty(), "the last pass has nothing left to govern");
        let _ = (drained(&ENDED), drained(&TOLD));
    }

    /// The count is taken under the lock, so an advance holding a stale
    /// reading of the group cannot start past the cap.
    #[test]
    fn a_start_is_refused_under_the_lock_once_the_cap_is_full() {
        let (_env, store) = scratch("lockcap");
        task(&store, "m-1", Status::Doing);
        task(&store, "m-2", Status::Todo);
        list(&store, &[(&["m-1", "m-2"], "claude")]);
        let members = vec!["m-1".to_string(), "m-2".to_string()];
        let m2 = store.find_task("m-2").unwrap();
        let took = |cap| take_member(&store, &m2, "run", 1, &members, cap).map(|(why, _)| why);
        assert_eq!(took(Some(1)), None, "one going, cap one");
        assert_eq!(took(Some(2)), Some("new"));
        assert_eq!(took(Some(2)), None, "taken already");
    }

    /// A start that fails puts the member back and says so; one that never
    /// arrived is taken again once it has had its ten minutes.
    #[test]
    fn a_failed_or_lost_start_is_put_back_told_and_taken_again() {
        let (_env, store) = scratch("wedge");
        task(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        FAIL.with(|f| *f.borrow_mut() = Some("no compound session".into()));
        step(&store, &w, &Blind);
        FAIL.with(|f| *f.borrow_mut() = None);
        let _ = spawned();
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Todo, "put back, so nothing is wedged at doing");
        assert!(t.section("Log").unwrap().contains("no compound session"), "and the reason is on the row");
        assert_eq!(drained(&TOLD).len(), 1, "and the seat is told");

        // Lost: `doing`, unclaimed, wsp's own start its last word, long ago.
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 1);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a start in flight is left alone");
        let mut t = store.find_task("m-1").unwrap();
        t.updated = "2026-01-01T00:00:00Z".into();
        store.save_task(&t).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 1, "a start that never claimed it is taken again");

        // And a verifier row whose agent never came.
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        let _ = spawned();
        let mut v = tagged(&store, VERIFY_TAG).remove(0);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
        v.updated = "2026-01-01T00:00:00Z".into();
        store.save_task(&v).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![(v.id.clone(), "claude".into())], "the same row, started again");
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 1, "and not a second one");
    }

    #[test]
    fn a_verifier_and_a_barrier_row_find_the_run_they_belong_to() {
        let (_env, store) = scratch("belong");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        let v = tagged(&store, VERIFY_TAG).remove(0);
        assert_eq!(list_of(&store, &v).map(|w| w.id), Some("run".into()));
        set(&store, &v.id, Status::Review);
        step(&store, &w, &Blind);
        let b = tagged(&store, BARRIER_TAG).remove(0);
        assert_eq!(list_of(&store, &b).map(|w| w.id), Some("run".into()));
        let _ = spawned();
    }

    /// `wsp-142`: tokenhub-003 went to review with its commit on its branch,
    /// the advance its review started did nothing and said nothing, and the
    /// governor went looking for a cause in the claim. The verifier is keyed
    /// on the landing, so the seat is told the member is waiting on one.
    #[test]
    fn a_member_reviewed_without_landing_gets_no_verifier_and_the_seat_is_told_why() {
        let (env, store) = scratch("unlanded");
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "--quiet", "-b", "master"]);
        git(&["commit", "--quiet", "--allow-empty", "-m", "first"]);
        git(&["checkout", "--quiet", "-b", "m-1"]);
        git(&["commit", "--quiet", "--allow-empty", "-m", "the member's work"]);
        git(&["checkout", "--quiet", "master"]);
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut t = Task::new("m-1", "m-1");
        t.project = Some("p".into());
        t.status_raw = Status::Review.as_str().into();
        store.save_task(&t).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);

        told_about_task(&store, "m-1", "review");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty() && tagged(&store, VERIFY_TAG).is_empty(), "no verifier on unlanded work");
        let told = drained(&TOLD);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("m-1 is at review with 1 commit not on master"), "{}", told[0]);
        assert!(told[0].contains("`wsp land m-1`"), "{}", told[0]);

        git(&["merge", "--ff-only", "--quiet", "m-1"]);
        told_about_task(&store, "m-1", "review");
        step(&store, &w, &Blind);
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 1, "landed: the verifier starts");
        assert!(drained(&TOLD).is_empty(), "and a landed review is the next step, not news");
        let _ = spawned();
    }
}
