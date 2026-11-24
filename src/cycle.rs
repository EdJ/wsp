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
//! days when this row was written: its only launcher was backend's `[[startup]]`,
//! and the fleet had moved to compound. Ed's answer to "how does the daemon
//! run without backend" was the better question — *do we need one?* — and the
//! answer is no. Every event a run advances on is a wsp verb run in some
//! agent's own process: a member's `review` and `land`, a verifier's
//! `verified`, the barrier agent's `go` or `hold`. So the verb is
//! the trigger. [`poke`] sits at the seam every status verb already shares
//! (`cmd_task::mutate_saying`), and at `land`, `go` and `hold`.
//!
//! What that gives up is anything with no verb behind it: an agent that dies,
//! or stalls on a prompt, before it runs `wsp review` never triggers the next
//! step. That stayed the person's to see until `wsp-144` d1 reversed it (Ed,
//! 2026-10-04): the daemon now runs [`step`] for every running list on every
//! tick as well, and [`crate::repair`] takes the states no verb reaches. The
//! verbs are still the fast path; the tick is what makes a lost one a delay.
//!
//! # The chain
//!
//! For a running list whose current group carries a [`Policy`]:
//!
//! 1. Members not yet started are spawned, up to the group's cap, each on its
//!    own `agent <id>:` line where it has one and the group's otherwise
//!    ([`Group::policy_for`], `wsp-150`). Taken last in a step since
//!    `wsp-231`, and under the seat's cap ([`crate::room`]) after every other
//!    running list's verifiers and checks: a group finishes before the next
//!    work fans out.
//! 2. A member that has reached `review` **and landed** gets a read-only
//!    verifier, on the member's policy. Since `wsp-188` it has no row: a
//!    `running` pass is written on the member's `## Verification`, and the
//!    agent finishes with `wsp verified <member> --holds|--blocks` — see
//!    [`crate::verification`].
//! 3. When every member is landed and every newest pass holds, one agent
//!    checks the barrier — a row tagged [`BARRIER_TAG`] — on the group's own
//!    line, since it reads the group as a whole, and ends it with
//!    `wsp worklist go` or `hold`.
//! 4. A pass rotates the governing seat onto a fresh successor and starts the
//!    next group (step 1 again). A governor is *told* what happened at each
//!    point that asks for a decision: a block, a hold, a pass. It never has to
//!    act for the run to move.
//!
//! # Legs: a row that is a graph of agents (`wsp-293`)
//!
//! A member with a `## Legs` section is not started as one worker. The step
//! opens every leg that is ready ([`take_legs`]), in record order, each through
//! the seat's gate and written `running` with no seat under the store lock, so
//! the cap is counted in the same section and a second advance finds the record.
//! Order in a step: the verifiers and the barrier, then the legs of rows already
//! begun, then new members, whose first legs count as one member against `xN`.
//! `wedged` and `sent_back_unseated` leave a row with the section alone, so no
//! whole-row worker is seated beside the legs. A blocking verdict appends one
//! `rework` leg ([`crate::legs::rework`]) and the step starts it like any other.
//!
//! **The choices the doc left open (v1, Ed 2026-10-18: take the conventional
//! one and keep going):**
//!
//! - **A leg's start is retaken like a member's**, a minute after it was
//!   opened with no seat ([`WEDGED_AFTER`]). A leg whose seat *died* mid-run is
//!   not repaired here: the row stalls with a leg `running`, which `wsp wip` and
//!   `wsp leg show` show, and a person or `wsp leg skip` decides.
//! - **A verdict names a leg by its name as a whole word** (`crate::legs::rework`).
//!   Prose that happens to use a leg's name names it, and the cost is one wrong
//!   tier on a rework, which is the default unless that leg had a `why:`.
//! - **A row at the 8-leg cap gets no rework leg**; the row's log says so and the
//!   governor is told by the verdict's own message. It is not reseated as a whole.
//! - **Outside a list**, `leg done` pokes, the poke runs [`start_inside`], and the
//!   repair tick runs [`legs_owed`] for rows that have begun. A row nothing has
//!   begun is never started by a tick: staging it is not starting it.
//!
//! # Pieces: a larger work in batches, inside one row (`wsp-506`)
//!
//! A member with `## Pieces` is stepped the same way ([`run_pieces`]): in the
//! same place in a step as legs, the batch that is owed a close is closed
//! ([`crate::piece_close::close`]) and written `closed:` on the record, then the
//! lowest open batch's pending pieces open in record order under the row's
//! `cap:` and through the gate ([`take_pieces`]). A conflicting or red close
//! files its `merge batch` or `fix batch` piece in the batch after, which opens
//! next. Once every batch has closed the run types the row's review, the pieces
//! table as the account ([`finish_pieces`]). Outside a list `piece done` pokes
//! and [`start_inside`] runs; the repair tick runs [`pieces_owed`] and ends a
//! done piece's seat ([`pieces_finished`]).
//!
//! - **A batch's close is recorded on the row** (`closed: 1 2` under `cap:`), so
//!   a second advance does not merge it again and the next batch opens only
//!   behind it. A close that is held is said in `cycle.log` and tried again.
//! - **The review is marked before it is typed** ([`PIECES_REVIEWING`]), so a
//!   second advance does not type it twice; a review refused is typed again
//!   after [`REVIEW_TAKES`].
//! - **The row's own cap counts its pieces running**, a start in flight among
//!   them; a piece held by it says nothing, since the next piece to end opens it.
//!
//! # Levels, so a lost trigger costs a delay and nothing else
//!
//! [`advance`] reads where the run is from the store and git alone and does
//! whatever is owed; it does not remember what it did last time. So it is
//! idempotent, a second trigger while the first is running finds the work
//! already done, and `wsp worklist advance` by hand repairs a trigger that was
//! lost. Each thing it starts is keyed on a record written *before* the spawn,
//! inside the store lock — the member moved to `doing`, the pass written on
//! it, the barrier row created — so two advances racing find the record and
//! start nothing twice.
//!
//! # Detached, because the trigger is somebody else's command
//!
//! A member's `wsp review` must return in a second, and a spawn waits for an
//! agent to come up. So [`poke`] starts `wsp worklist advance` in a process
//! group of its own — the reason `cmd_spawn::arrange_ending` gives: an agent's
//! ending signals its group — with its output in `cycle.log` in the state
//! directory, since nobody is reading the pane it started from.

use std::process::Stdio;
use wsp_base::calls::Command;

use crate::model::{Group, Policy, Status, Task, Worklist, WorklistStatus, DEFAULT_POLICY, MANUAL};
use crate::place::State;
use crate::store::Store;
use crate::util;
use crate::worklist::{self, Reading, Settlement};
use crate::Args;

/// The run a start belongs to, handed to `wsp spawn` in its environment so the
/// row records its owner. `wsp-158`.
///
/// **A recorded fact and not a guess from titles.** A governor's helper — the
/// agent spawned to install a binary mid-group — is a member of nothing and a
/// verifier of nothing, and the only way wsp ends it at the pass is to have
/// written down that this run opened it. The environment is the seam: the
/// spawn is already a process this module starts, and a row that names its own
/// owner survives a daemon restart, a rename and a machine moving.
pub(crate) const OWNED_LIST: &str = "WSP_RUN_LIST";
/// The group within it, 1-based — see [`OWNED_LIST`].
pub(crate) const OWNED_GROUP: &str = "WSP_RUN_GROUP";

/// The marker a row carries for "this run opened me", written by `wsp spawn`
/// from [`OWNED_LIST`]. `wsp-158`.
///
/// **In the `## Log` and not in the tags**, because tags are what `wsp ls -t`
/// and `wsp project show` are for: a governance fact about which run happened
/// to start a row is not a thing anybody filters work by. The log is a store
/// record that survives everything, which is what a pass needs to find the
/// owner of a helper months later.
pub(crate) const OWNED: &str = "wsp: started by this run";

/// The tag on a Verify row, from before `wsp-188` put verdicts on the member.
/// Nothing files one now; the rows already filed are read until they are
/// migrated, and the readers that still find them use this.
pub(crate) const VERIFY_TAG: &str = crate::verification::VERIFY_TAG;
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
    ///
    /// **A pane that is not there at all reads `None` here too**, because
    /// `Refusal::NoSeat` is a backend saying "not mine" and this port has no way
    /// to tell that from a backend that could not be reached — see
    /// [`Seats::absent`], which is the one question that does tell them apart.
    /// The two were one answer until `wsp-148`, and the row is about a governor
    /// pane that died: found by running it, because every other reading of a
    /// dead seat has a `State` and this one does not.
    fn state(&self, seat: &str) -> Option<State>;

    /// Whether this seat is **positively** not there — every backend has
    /// answered and none of them has it — as against the `None` above, which a
    /// backend that could not be reached also returns.
    ///
    /// **A question of its own and not an inference from `state`, because the
    /// answer is not in `state`.** `Refusal::NoSeat` is how a backend says "this
    /// is not my seat", so a fan-out over every backend that ends in `NoSeat`
    /// everywhere is a fact: the seat is on none of them. One that ended in a
    /// socket error is not — that is the machine that cannot be seen.
    ///
    /// **Defaults to `false`, and the default is the safe answer rather than the
    /// convenient one.** Every implementation of this port that is not the real
    /// one is a test, and a test fake that answers `true` would be seating
    /// governors out of a fixture rather than out of a reading. A caller that
    /// gets `false` where it wanted `true` does nothing, and a caller that gets
    /// it wrong the other way starts agents.
    fn absent(&self, _seat: &str) -> bool {
        false
    }

    /// Let go of `row`'s claim, held by `seat`, which [`Seats::absent`] has
    /// just called gone (`wsp-344`): binding and claim, with `why` on the row.
    /// `true` when it was let go. An act verb on the port because the repair
    /// tick that decides it is below the seats that own the release.
    ///
    /// **Defaults to `false`, releasing nothing**, for [`Seats::absent`]'s
    /// reason: a fake that is not asked to act does not act.
    fn release_gone(&self, _store: &Store, _row: &str, _seat: &str, _why: &str) -> bool {
        false
    }

    /// Whether a turn began at this seat at or after `since`, as the manager's
    /// events folded say ([`crate::fold::turn_began_since`]): the `turn` edge,
    /// or the turn an `order.acked` names. `None` is nobody can say, a seat
    /// nothing was heard of, and a caller holds on `None`. A fake overrides it.
    fn turn_began_since(&self, store: &Store, seat: &str, since: i64) -> Option<bool> {
        crate::fold::turn_began_since(store, seat, since)
    }

    /// Whether the seat's prompt is holding typed text —
    /// [`crate::place::Place::typed`]. `None` is nobody can say, and holds.
    fn typed(&self, _seat: &str) -> Option<bool> {
        None
    }

    /// Press submit at the seat — [`crate::place::Place::nudge`]. Whether a
    /// backend took the press.
    fn press(&self, _seat: &str) -> bool {
        false
    }

    /// Type a sentence at the seat the way `wsp tell` does, through its kind.
    /// `None` is nothing here answers for it.
    fn resend(&self, _seat: &str, _text: &str) -> Option<crate::place::Result<crate::place::Delivery>> {
        None
    }

    /// The processes that are this seat's agent, read now —
    /// [`crate::place::Place::pids`]. `None` is no backend can name one.
    fn pids(&self, _seat: &str) -> Option<Vec<u32>> {
        None
    }

    /// These seats' own shells — [`crate::waiting::Shells::read`]. None by
    /// default, for [`Seats::absent`]'s reason: a fake that answered from the
    /// machine's process table would be holding seats out of whatever this
    /// machine happens to be running.
    fn shells(&self, _seats: &[&str]) -> crate::waiting::Shells {
        crate::waiting::Shells::default()
    }

    /// The last paragraph on the seat's screen — what its turn ended on
    /// (`wsp-581`). `None` where nobody can read it, and the default, for
    /// [`Seats::absent`]'s reason: a fake that read a real screen would be
    /// reading whatever this machine happens to be running.
    fn last_words(&self, seat: &str) -> Option<crate::waiting::Words> {
        self.screen(seat).as_deref().and_then(crate::waiting::last_words)
    }

    /// The seat's screen as text, read now — what a prompt standing on it says
    /// (`wsp-616`). `None` where nobody can read it, and the default, for
    /// [`Seats::absent`]'s reason.
    fn screen(&self, _seat: &str) -> Option<String> {
        None
    }

    /// How the seat's last turn ended, while that `Stop` is still its last
    /// word: when, its last words off the transcript, and whether a call in it
    /// was denied (`wsp-606`). `None` where no hook recorded a stop, and the
    /// default, for [`Seats::absent`]'s reason.
    fn turn_end(&self, _seat: &str) -> Option<crate::waiting::TurnEnd> {
        None
    }

    /// Which of these are still running. A parameter for the same reason
    /// [`Seats::state`] is: a test names pids that were never processes.
    fn running(&self, pids: &[u32]) -> Vec<u32> {
        crate::host::Host::alive(&crate::host::LOCAL, pids).into_iter().collect()
    }

    /// Every seat the backends here hold, with its label — for the sweeps that
    /// look for a pane by what it is called because no record names it. `None`
    /// where no backend answered, and the default, for [`Seats::absent`]'s
    /// reason: a sweep over a census it never took ends nothing.
    fn census(&self) -> Option<Vec<(String, String)>> {
        None
    }

    /// Hand the governing seat of `scope` to a fresh successor on wsp's own
    /// initiative — `cmd_spawn::rotate_on_behalf`. The exit code of the
    /// rotation. **Does nothing by default**, for [`Seats::absent`]'s reason: a
    /// fake that rotated would be starting agents out of a fixture.
    fn rotate(&self, _store: &Store, _scope: &str) -> i32 {
        0
    }

    /// Arrange the seat's ending for once it is not mid-turn —
    /// `cmd_spawn::end_seat_when_idle`. Does nothing by default, and says so as
    /// success: a fake that ended panes would be ending them out of a fixture.
    fn end_when_idle(&self, _store: &Store, _seat: &str) -> Result<(), String> {
        Ok(())
    }

    /// Block until the pane is not mid-turn, or twenty minutes have passed —
    /// `cmd_spawn::wait_for_pane`. Returns at once by default.
    fn wait_idle(&self, _seat: &str) {}

    /// Arrange the ending of an accepted exit's seat at its `Stop` —
    /// `cmd_spawn::end_exit_when_idle` (`wsp-615`). Does nothing by default, for
    /// [`Seats::end_when_idle`]'s reason; the tick ends the seat instead.
    fn end_exit_when_idle(&self, _store: &Store, _seat: &str) -> Result<(), String> {
        Ok(())
    }

    /// Fold what was typed at `scope`'s seat and never confirmed, so a successor
    /// is not handed the payload its predecessor refused —
    /// `wake::fold_what_was_typed` (`wsp-694`). How many were folded. **Does
    /// nothing by default**, for [`Seats::rotate`]'s reason: the spool is the
    /// seats layer's, and a fake has none to fold.
    fn fold_typed(&self, _store: &Store, _scope: &str) -> usize {
        0
    }
}

/// The real reading, over the backends on this machine — with opencode's own
/// database overruling a screen that has stopped repainting. `wsp-160`.
///
/// **A backend that is not running here is not asked** (`wsp-148`, live on
/// 2026-10-06). backend is out of the picture (Ed, 2026-10-06), and while the
/// fleet still asked it, its `Unreachable` with no server was read as "cannot
/// say" — so every dead governor seat was `Unreadable` for as long as backend
/// stayed down, which is always now, and the reconciler refilled none of them:
/// compound, jolt-app and wsp. backend owns its panes, so no server here means no
/// backend pane here to be in, and a seat recorded on one is a seat compound has
/// no record of: nobody is in it, and a reseat fills it on compound. Only a
/// bare id — one qualified `@machine` is that machine's backend's to answer.
///
/// [`crate::backend::available_now`] rather than the cached answer, because the
/// daemon lives for days: a cache latched `false` would go on skipping backend
/// after it came back, and skipping is the direction that seats a successor
/// beside a live pane. One connect, once a minute, per seat asked about.
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


/// The half of [`Fleet::overrule_frozen_opencode`] that decides, split out so a
/// test can drive it without an opencode, a database and a frozen TUI.
///
/// **`finished` is the session's last completed turn in seconds, and `None` is
/// "we do not know"** rather than "still working". That distinction is the whole
/// safety of the repair: a reader that treated an unreadable database as a
/// running turn would be right, and one that treated it as a finished one would
/// end agents on a machine where opencode simply could not be asked. Every other
/// state is returned as it was read, because a screen has been seen to lie in
/// one direction only.
pub(crate) fn overrule(raw: State, finished: Option<i64>, at: i64) -> State {
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
    // A row run as legs or pieces has its next parts to start, in a list or not.
    if !ours && !member && !crate::inner::run_inside(&t) {
        return;
    }
    launch(store, &["worklist", "advance", "--task", &t.id, "--verb", verb]);
}

/// After `go`, `hold`, `park` or `resume` on a list: tell whoever governs it, and take the steps
/// the new position owes. `passed` is the barrier crossed, if one was.
pub(crate) fn poke_list(store: &Store, list: &str, event: &str, passed: Option<usize>) {
    if let Some((w, text)) = store.worklist(list).and_then(|w| heard(&w, event, passed).map(|t| (w, t))) {
        tell_above(store, &w, &text);
    }
    let n = passed.map(|n| n.to_string()).unwrap_or_default();
    let mut argv = vec!["worklist", "advance", list, "--event", event];
    if passed.is_some() {
        argv.extend(["--passed", n.as_str()]);
    }
    launch(store, &argv);
}

/// A member of a held run sent back: the run goes back to its group. `wsp-286`.
///
/// **A held run had one way back, and it was the pass.** `hold` starts nothing
/// more and `go` is what un-holds, so a barrier check that held because a
/// member still owed work left whoever reopened that member two choices: start
/// an agent on it by hand, which a custodian's brief forbids, or `go`, which is
/// read as the pass. On 2026-10-14 at wsp-open-ends' barrier 1, wsp-214 was
/// reopened for its missing install with its agent gone, and the custodian
/// started one by hand.
///
/// **So the reopen is the way back, and it records no verdict.** The list goes
/// to `running` at the group it stood at — holding does not move the position —
/// and `step` does the rest with machinery it already has: the member, at
/// `doing` with nobody on it, is reseated ([`sent_back_unseated`]); once it is
/// back at `review` and its verdict holds, the barrier reads finished again and
/// the check that held is settled, so a fresh one opens.
///
/// **Only a hold the barrier's check made**: the newest check on the group is
/// the one that said `holds`. A hold taken mid-group, or with no check, is a
/// stop somebody decided on for reasons of their own, and a reopen is not a
/// word about those — it stays held, and `go` is still its way back.
///
/// Returns the list and group it took back, for the verb to say.
pub(crate) fn reopened(store: &Store, id: &str, reason: &str) -> Option<(String, usize)> {
    let w = store.worklists().into_iter().find(|w| {
        w.status() == WorklistStatus::Held && w.groups().iter().any(|g| g.members.iter().any(|m| m == id))
    })?;
    let taken = store.locked(|| {
        let mut w = store.worklist(&w.id)?;
        if w.status() != WorklistStatus::Held {
            return None;
        }
        let at = worklist::position(store, &w, Reading::Settled).at?;
        let g = w.groups().into_iter().nth(at - 1)?;
        let held_by_check = g.checks.last().is_some_and(|c| c.state == crate::barrier::State::Holds);
        if !g.members.iter().any(|m| m == id) || !held_by_check {
            return None;
        }
        w.set_status(WorklistStatus::Running);
        crate::log::note_to_list(store, &mut w, &format!("started again · {id} sent back, no verdict recorded: {}", util::truncate(reason, 200))).ok()?;
        Some((w, at))
    });
    let (w, at) = taken?;
    store.git_commit(&format!("wsp: {} runs group {at} again — {id} was sent back", w.id));
    store.log_event("worklist-reopened", serde_json::json!({ "id": w.id, "group": at, "member": id, "why": reason }));
    log_line(store, &format!("{} group {at}: {id} was sent back while the run was held — running the group again", w.id));
    tell(store, &w, &format!(
        "The {} run was held at barrier {at}, and {id} has been sent back: {}. The run is running group {at} \
         again — no verdict was recorded — and wsp seats {id} if nobody is on it and checks the barrier afresh \
         once it is back at review.",
        w.id,
        util::truncate(reason, 200),
    ));
    poke_list(store, &w.id, "reopen", None);
    Some((w.id.clone(), at))
}

fn launch(store: &Store, argv: &[&str]) {
    if let Err(why) = start_detached(store, argv) {
        log_line(store, &format!("could not start `wsp {}`: {why}", argv.join(" ")));
    }
}

/// [`launch`]'s process, with why it did not start rather than nothing.
///
/// **Each way out used to be a bare `return`** (`wsp-151`), and on Linux one of
/// them was every launch: `current_exe` read `wsp (deleted)` after an install,
/// the spawn failed with `ENOENT`, and `cycle.log` said "seating a successor"
/// with nothing after it, a minute at a time, for a day. [`util::self_exe`] (`wsp-152`) fixes
/// that cause; this is so the next one is said.
fn start_detached(store: &Store, argv: &[&str]) -> Result<(), String> {
    // A test's `current_exe` is the test harness, and handing it `worklist
    // advance` would run the suite again from inside itself.
    if cfg!(test) || std::env::var_os(OFF).is_some() {
        return Ok(());
    }
    let exe = util::self_exe().map_err(|e| format!("no path to this binary: {e}"))?;
    let log = store.state_file("cycle.log");
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| format!("cannot share {}: {e}", log.display()))?;
    let mut cmd = Command::new(&exe);
    cmd.args(argv)
        // Nobody's pane: a spawn started from here must not read itself as
        // the agent whose verb triggered it.
        .env_remove(crate::base::seat::SEAT_ENV)
        .env_remove("WSP_BACKEND_PANE_ID")
        .env_remove("WSP_BACKEND_WORKSPACE_ID")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0);
    cmd.spawn().map(|_| ()).map_err(|e| format!("{} did not start: {e}", exe.display()))
}

/// [`launch`], for a verb that is not a run step — [`crate::repair`]'s reseat was
/// the first caller, and it is here rather than duplicated because the four
/// things `launch` does are four things that have to be true of any process the
/// run starts: its stdout is `cycle.log`, it inherits nobody's seat, it holds
/// no terminal, and it outlives the tick that asked for it.
pub(crate) fn launch_out(store: &Store, argv: &[&str]) {
    launch(store, argv)
}

/// [`launch_out`], returning why it did not start, for a caller that holds a
/// claim and records the failure against it (`wsp-151`) rather than leaving a
/// claim that times out with nothing said.
pub(crate) fn try_launch(store: &Store, argv: &[&str]) -> Result<(), String> {
    start_detached(store, argv)
}

/// `wsp worklist advance [<slug>] [--task ID --verb V] [--event go|hold --passed N]`
///
/// Take every step the store says is owed, on one list or on every running
/// one. The event flags are what the trigger saw, and they only decide what a
/// governor is told: the steps themselves are read off the store.
pub fn advance(store: &Store, args: &Args) -> i32 {
    // A wake [`wake_after`] scheduled: slept here, before any lock or step, so
    // the wait holds nothing.
    if let Some(secs) = args.get("after").and_then(|a| a.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(secs.min(WAKE_CAP_SECS)));
    }
    cap_deliveries();
    // Everything an advance does spawns or tells, so none of it carries on
    // past a busy state lock (`wsp-663`, `docs/step-lock.md`).
    crate::store::strictly(|| advance_in(store, args))
}

/// The longest `--after` an `advance` sleeps: well past the longest backoff
/// (two minutes), so a wake that names more is a mistake and not a plan.
const WAKE_CAP_SECS: u64 = 10 * 60;

/// The event a [`wake_after`] advance carries. Any event keeps it off the bare
/// form, which is a person asking again and forgives what gave up.
const RETRY_EVENT: &str = "retry";

/// A start that failed was told "starting it again in `secs`": step `list`
/// once that wait is over, so the promise holds (`wsp-087`).
///
/// **The retry used to come on the daemon's next pass**: m1-fixes-3's wsp-079
/// was told "in 30 s" at 09:36:11 and started again at 09:37:25, the next tick
/// after the wait, 44 s late. The backoff gates ([`crate::starts::held`] and
/// the check entry's wait) free a start once *more* than `secs` have passed,
/// so the wake is one second after the wait, never on its edge. A detached
/// process like every other step [`launch`] starts, and the daemon's tick
/// still steps the list too: whichever comes first starts it, and the step
/// lock keeps it to one.
fn wake_after(store: &Store, list: &str, secs: i64) {
    let after = (secs.max(0) + 1).to_string();
    let argv = ["worklist", "advance", list, "--event", RETRY_EVENT, "--after", after.as_str()];
    #[cfg(test)]
    tests::WAKES.with(|w| w.borrow_mut().push(argv.iter().map(|a| a.to_string()).collect()));
    launch(store, &argv);
}

/// How long an `advance` waits on one `compound-sup deliver` (`wsp-666`).
///
/// **`advance` is a step, not a patient pass.** Its tells (a verdict, a
/// barrier, a start) went through the same 30 s `deliver` wait the daemon's
/// tick has, so a seat that would not take the order held the step for the
/// whole of it: 201 `compound-sup` children in 3 days of `calls.jsonl` under
/// `worklist advance`, 4442 s, p95 30015 ms — the `DELIVER_WAIT` to the
/// millisecond. A delivery that outlasts this is left pending under its id and
/// the daemon's tick asks again, so nothing is lost by not sitting through it.
const ADVANCE_DELIVER_CAP: std::time::Duration = std::time::Duration::from_secs(3);

fn cap_deliveries() {
    crate::place_compound::cap_deliver_wait(ADVANCE_DELIVER_CAP);
}

fn advance_in(store: &Store, args: &Args) -> i32 {
    let named = args.rest.get(1).cloned();
    log_line(store, &format!("advance {}", args.rest[1..].join(" ")));

    // An ending and nothing else: the verb that arranged it poked the run's
    // own step separately, and this process spends most of its life waiting.
    if let (Some(list), Some(id)) = (named.as_deref(), args.get("end-check")) {
        end_check(store, list, &id);
        return 0;
    }
    if let (Some(list), Some(seat)) = (named.as_deref(), args.get("end-check-seat")) {
        end_check_seat(store, list, &seat);
        return 0;
    }
    // The barrier's mechanical run (`wsp-326`): minutes long, so it is a
    // process of its own and not a step of the tick.
    if let (Some(list), Some(at)) = (named.as_deref(), args.get("barrier-run").and_then(|a| a.parse::<usize>().ok())) {
        return crate::barrier_run::run(store, list, at);
    }

    if let (Some(id), Some(verb)) = (args.get("task"), args.get("verb")) {
        told_about_task(store, &id, &verb);
        // A row staged and spawned by hand has no list to step it: the legs it
        // is owed start here, through the gate (`start_inside` leaves a list's
        // member to the list).
        if store.find_task(&id).is_some_and(|t| crate::inner::run_inside(&t)) {
            let _ = start_inside(store, &id, &Fleet);
        }
    }
    if let (Some(list), Some(event)) = (named.as_deref(), args.get("event")) {
        let passed = args.get("passed").and_then(|n| n.parse::<usize>().ok());
        if let Some(w) = store.worklist(list) {
            match event.as_str() {
                "hold" => tell(store, &w, &format!(
                    "The {} run was held at its barrier: `wsp worklist show {}` has the reason. \
                     wsp starts nothing more until somebody runs `wsp worklist go {}` — or, where the \
                     check held on a member that still owes work, `wsp reopen <id> \"what is owed\"`, \
                     which runs its group again and checks the barrier afresh.",
                    w.id, w.id, w.id
                )),
                // `wsp-173`: a person's pause, said as one, and said as not a
                // barrier — the sentence `hold` sent here told native-window's
                // seat its barrier had been checked when nobody had.
                "park" => tell(store, &w, &format!(
                    "The {} run was parked by somebody: `wsp worklist show {}` has the reason. \
                     Nothing starts and no barrier is checked, and no verdict was recorded. \
                     `wsp worklist resume {}` takes it back to where it stood; `go` is refused on it.",
                    w.id, w.id, w.id
                )),
                "resume" => tell(store, &w, &format!(
                    "The {} run was resumed and stands where it was parked: `wsp worklist next {}` \
                     says where. A barrier it stood at is still owed and was not passed.",
                    w.id, w.id
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
                    if let Some(n) = passed {
                        after_pass(store, &Fleet, &w, n);
                    }
                    let _ = step(store, &w, &Fleet);
                    tell(store, &w, &passed_sentence(store, &w, passed));
                    return 0;
                }
                _ => {}
            }
        }
    }

    // The bare form is somebody's: the sentence a failed start tells a seat
    // ends in it. A check whose tries ran out starts again from the top.
    if let (Some(list), None, None) = (named.as_deref(), args.get("task"), args.get("event")) {
        if crate::barrier::try_again(store, list) {
            log_line(store, &format!("{list}: the check that gave up is abandoned, and a fresh one opens"));
        }
        let spent = forgive_spent_starts(store, list);
        if spent > 0 {
            log_line(store, &format!("{list}: {spent} start(s) that gave up are tried from the top"));
        }
    }
    for w in stepped(store, named.as_deref(), args.get("task").as_deref()) {
        let _ = step(store, &w, &Fleet);
    }
    0
}

/// The running lists an `advance` steps: the one named; with no list named but
/// a task, only the list that holds that task — as member, as a member's
/// verifier, or as its barrier row — and none when no list holds it; and with
/// neither, every running list.
///
/// **A task's trigger steps its own list and no other** (`wsp-662`). It fell
/// through to every running list, so each `checked` or `verified` was a full
/// step of all of them — ~500 git and ~200 other spawns, minutes of wall — and
/// ten verifiers landing together ran ten of those at once and took load past
/// 400 on 8 cores on 2026-11-17. The other lists lose nothing: the daemon's
/// tick steps every running list each minute.
fn stepped(store: &Store, named: Option<&str>, task: Option<&str>) -> Vec<Worklist> {
    let lists: Vec<Worklist> = match (named, task) {
        (Some(n), _) => store.worklist(n).into_iter().collect(),
        (None, Some(id)) => store.find_task(id).and_then(|t| list_of(store, &t)).into_iter().collect(),
        (None, None) => store.worklists(),
    };
    lists.into_iter().filter(|w| w.status() == WorklistStatus::Running).collect()
}

/// What a pass wsp runs leaves behind it: the group's agents ended, and then
/// either a fresh governor or, past the last barrier, no run at all.
///
/// **Rotate first**, so what is told next reaches the successor rather than a
/// seat that is about to be ended.
///
/// **Past the last barrier the list is closed, by wsp.** It used to stop at
/// "`wsp worklist done` closes it", and on `wsp-unattended` the governor typed
/// that by hand at 11:16:25Z — a hand verb at the end of an unattended run —
/// and the seat wsp had rotated in for the run stayed open after it
/// (`wsp-208`). The closing is [`Closing::Passed`]: everything `done` ends,
/// the list's own seat included, except the check whose `go` this is.
///
/// The last group passed is the whole test, because the position is read from
/// the last verdict on: anything still open behind it is slipped, which `done`
/// writes down, and not a barrier the run stands at.
fn after_pass(store: &Store, seats: &dyn Seats, w: &Worklist, n: usize) {
    end_behind(store, seats, w, n);
    if n < w.groups().len() {
        rotate(store, seats, w);
        return;
    }
    let Some(now) = store.worklist(&w.id) else { return };
    let _ = crate::cmd_worklist::close(store, &Args::synth("worklist", &["done", &w.id], &[]), now, Closing::Passed);
    // The one cold build of the list's tips (`wsp-342`), after the close is
    // recorded and detached, so closing never waits on it.
    crate::cold::at_close(store, &w.id, &launch_out);
}

/// Whether passing barrier `n` closes the list: it is the last, and wsp ran its
/// group, so the step `go` pokes takes [`after_pass`]'s close. `go` says so
/// rather than offering a `done` the pass has already typed (`wsp-216`).
pub(crate) fn closes_on_pass(w: &Worklist, n: usize) -> bool {
    n == w.groups().len() && ran_by_wsp(w, Some(n))
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
        None if w.status() == WorklistStatus::Done => {
            " Nothing is left in the run, so wsp closed it, with its agents and its own seat.".to_string()
        }
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
    if verb == "verified" {
        return told_verdict(store, &w, &t);
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
    let member = what == "A member is blocked";
    let cut = if member { cut_line(&w.id, &t.id) } else { String::new() };
    // The answer that needs no seat and no second verb (`wsp-635`).
    let answer = if member {
        format!(" To answer it, `wsp decide {} \"…\"` — the run reopens the row on a decision made since the block, seats it if nobody is on it, and no person is asked.", t.id)
    } else {
        String::new()
    };
    tell(store, &w, &format!(
        "{what} in the {} run: {} ({}) is blocked. `wsp show {}` says on what. \
         The run waits here until it is unblocked.{answer}{cut}",
        w.id, t.id, util::truncate(&t.title, 60), t.id
    ));
}

/// A verdict on a member, which is the governor's to hear only when it blocks:
/// the member has been sent back, and whether that was right is the decision.
/// Told here and nowhere else, so once — the verb triggers this one advance,
/// and the tick that follows reads the block without saying it again.
fn told_verdict(store: &Store, w: &Worklist, t: &Task) {
    let tasks = store.tasks_shared();
    let Some(p) = crate::verification::blocked(&tasks, &t.id) else { return };
    // A send-back at the cap has told the governor already, once, with its
    // choices (`verification::tell_if_capped`): not a second time here.
    if store.find_task(&t.id).is_some_and(|now| crate::verification::stall_told(&now)) {
        return;
    }
    tell(store, w, &format!(
        "A verifier found a problem in the {} run: {} ({}) was sent back to doing{}. \
         `wsp show {}` has the verdict under ## Verification. {} \
         If the finding is wrong, `wsp verified {} --holds --from FILE --caption \"…\"` records why, in your name.",
        w.id,
        t.id,
        util::truncate(&t.title, 60),
        p.read.as_deref().map(|r| format!(" — it read {r}")).unwrap_or_default(),
        t.id,
        match (crate::legs::staged(t), crate::legs::rework_open(&crate::legs::legs(t))) {
            (_, true) => "It runs as legs: wsp appended a `rework` leg after the last, whose order is the verdict, and starts it.".to_string(),
            (true, false) => format!(
                "It runs as legs and is at the {}-leg cap, so NO rework leg was added and nothing will start: it is yours to decide. \
                 The verdict: {}. Split the row into new rows, or record that the finding is wrong with `wsp verified --holds`.",
                crate::legs::MAX_LEGS,
                util::truncate(&p.text, 400)
            ),
            (false, _) => "The run waits for it to land again.".to_string(),
        },
        t.id
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
    if s.owes_a_verifier() || !s.settlement.settled() {
        return;
    }
    tell(store, w, &format!("In the {} run, {}", w.id, unlanded(&s.id, &s.settlement, &s.note())));
}

/// The sentence for a reviewed member that has not landed, in the log and
/// to the seat alike.
///
/// **The status it has, not `review` always** (`wsp-338`): compound-react-040
/// was at `done`, and this said "at review" on the line above the repair's "at
/// done" every minute — one row, two states.
fn unlanded(id: &str, at: &Settlement, note: &str) -> String {
    format!(
        "{id} is at {} with {note}, so it has no verifier yet. {}",
        at.word(),
        crate::cmd_checkout::Flow::Verified.owes(id)
    )
}

/// The list a barrier row is named after, read off its title.
///
/// Not a test that the list is running — the row's own title is all this reads.
/// [`worklist::Running::list_for`] is the routing question and adds that.
pub(crate) fn barrier_list(t: &Task) -> Option<&str> {
    t.tags.iter().any(|g| g == BARRIER_TAG).then_some(())?;
    t.title.strip_prefix(BARRIER_TITLE)?.split_whitespace().next()
}

/// The list a row belongs to: itself a member, a verifier under one, or the
/// barrier row named after the list. Running or not, for the barrier.
pub(crate) fn list_of(store: &Store, t: &Task) -> Option<Worklist> {
    let running = worklist::Running::read(store);
    let id = match t.tags.iter().any(|g| g == VERIFY_TAG) {
        true => t.parent.clone().unwrap_or_default(),
        false => t.id.clone(),
    };
    if let Some(l) = running.list_of(&id) {
        return store.worklist(l);
    }
    store.worklist(barrier_list(t)?)
}

/// Everything the current group owes, taken in order. Returns what it started,
/// for the log.
///
/// **`pub(crate)` and not private since `wsp-147`**, because the daemon's tick
/// takes these steps itself rather than starting a `wsp worklist advance` per
/// list: one code path means the tick and the verb cannot come to disagree
/// about what is owed, and the tick has to reach them without a process per
/// running list every minute.
///
/// **One at a time on a list** (`wsp-663`): the step holds its list's lock
/// for as long as it runs, so two triggers cannot both read a member as owed
/// and both start it. A trigger that finds the list already being stepped
/// leaves a mark and returns at once; the holder takes one more step for it
/// before letting go ([`crate::stepping`]). Other lists are not held up. And
/// every state lock inside the step is taken for real ([`crate::store::strictly`]):
/// a step is the thing that spawns and tells, so it never carries on without it.
pub(crate) fn step(store: &Store, w: &Worklist, seats: &dyn Seats) -> Vec<String> {
    crate::store::strictly(|| {
        match crate::stepping::one_at_a_time(&store.state, &w.id, true, |_| {
            #[cfg(test)]
            during_step(store, w);
            step_in(store, w, seats, Starts::All)
        }) {
            crate::stepping::Stepped::Ran { steps, out } => {
                if steps > 1 {
                    log_line(store, &format!("{}: stepped {} more time(s), as asked while the step ran", w.id, steps - 1));
                }
                out
            }
            crate::stepping::Stepped::Marked => {
                log_line(store, &format!("{}: a step is under way; it is marked to step again", w.id));
                Vec::new()
            }
            crate::stepping::Stepped::Busy => Vec::new(),
        }
    })
}

/// The test seam: what a test has arrive while this thread's step runs.
#[cfg(test)]
fn during_step(store: &Store, w: &Worklist) {
    tests::STEPS.with(|n| n.set(n.get() + 1));
    if let Some(f) = tests::DURING_STEP.with(|d| d.take()) {
        f(store, w);
    }
}

/// Which of a step's starts to take. `wsp-231`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Starts {
    All,
    /// Verifiers and the barrier only: what finishes a group, taken on every
    /// other running list before a member on this one may have a place under
    /// the seat's cap. Nothing else a step does — the untaken orders, the
    /// unlanded notes, a group sent back — is about who goes first, so none of
    /// it is done twice.
    Finishing,
}

/// Whether this spoke advances `w` (wsp-467): [`crate::hub_lease::drive`],
/// with who drives it said in `cycle.log` once each time it changes.
fn driven_here(store: &Store, w: &Worklist) -> bool {
    let drives = crate::hub_lease::drive(store, w);
    let (user, line) = match &drives {
        crate::hub_lease::Drives::Here if w.workspace.is_private() => return true,
        crate::hub_lease::Drives::Here => match crate::address::logged_in_user() {
            Some(me) => (me.clone(), format!("{}: driven here, by {me}'s wsp", w.id)),
            None => return true,
        },
        crate::hub_lease::Drives::Elsewhere(u) => (u.clone(), format!("{}: driven by {u} — this spoke does not advance it", w.id)),
        crate::hub_lease::Drives::NotYet(why) => {
            log_line(store, &format!("{}: not advanced — the drive lease was not taken: {why}", w.id));
            return false;
        }
    };
    let changed = crate::hub_lease::Book::edit(store, |b| b.driven_by.insert(w.id.clone(), user.clone()) != Some(user));
    if changed {
        log_line(store, &line);
    }
    drives == crate::hub_lease::Drives::Here
}

fn step_in(store: &Store, w: &Worklist, seats: &dyn Seats, starts: Starts) -> Vec<String> {
    let mut started = Vec::new();
    let w = match store.worklist(&w.id) {
        Some(w) => w,
        None => return started,
    };
    if w.status() != WorklistStatus::Running {
        return started;
    }
    // A shared list moves only on the spoke holding its drive lease (wsp-467):
    // any other spoke's run leaves it as it is — nothing started, nothing
    // written to the list.
    // Not the driver's spoke: it still opens the verifier of a member it holds
    // the lease of (wsp-486), the tree being here, and does nothing else.
    let held_only = !driven_here(store, &w);
    if held_only && !w.workspace.is_private() && store.claims().values().all(|c| crate::hub_lease::token_of(c).is_none()) {
        return started;
    }
    if held_only && w.workspace.is_private() {
        return started;
    }
    let groups = w.groups();
    let pos = worklist::position(store, &w, Reading::Landed);
    let Some(at) = pos.at else { return started };
    let Some(g) = groups.get(at - 1) else { return started };
    // A `manual` group stands still, with one exception (`wsp-629`): once every
    // member has landed and its stop is one an agent can check, its barrier is
    // opened on the list's usual line, so the list does not sit "waiting on
    // Ed" with nothing left for Ed to do. Only the barrier: nothing of the
    // members is started or verified here.
    let (policy, barrier_only) = match g.policy() {
        Some(p) => (p, false),
        None => match manual_barrier_policy(&groups, at, g, &pos) {
            Some(p) => (p, true),
            None => return started,
        },
    };
    let all = starts == Starts::All && !held_only;
    // A prerequisite a member names with `after` and no list holds joins the
    // group first (`wsp-495`), and the pass reads the group as it now stands.
    if all && !barrier_only && pull_prerequisites(store, &w, at) {
        return step_in(store, &w, seats, starts);
    }
    // The seat's cap (`wsp-231`), asked by each of the three starts below
    // inside the lock its record is written under. With no cap it reads
    // nothing and admits everything.
    let gate = crate::room::Gate::new(store, seats);
    if all {
        retry_after_hold(store, &w.id);
        // Before any start below, so a member the answer reopens with no seat
        // is seated in this very pass (`wsp-635`).
        reopen_the_answered(store, &w, at, &pos);
    }

    // 1a. Every seat the group is waiting on a first turn from, whose order
    // went in and started nothing (`wsp-252`). Before the verifiers and the
    // barrier, so a seat replaced here is opened again below.
    if all && !barrier_only {
        crate::untaken::look(store, seats, &w, at, g, &pos);
    }

    // 2. A verifier for each member that is finished — reviewed and landed — or
    // ready: reviewed at a commit that the verifier lands on a hold.
    // No row: the pass is written on the member, and the agent is started on
    // the member with `--verify` (`wsp-188`).
    //
    // **Before the members** since `wsp-231`: under the seat's cap a verifier
    // goes ahead of a new member, so a group can finish before the next work
    // fans out. With no cap the order changes nothing — each start is keyed on
    // its own record.
    let gone = |seat: &str| seats.absent(seat) || seats.state(seat) == Some(State::Gone);
    let absent = |seat: &str| seats.absent(seat);
    for s in pos.members.iter().filter(|s| !barrier_only && s.owes_a_verifier()) {
        // A member another user's spoke holds is verified there (wsp-467): its
        // verdict arrives with the row, and the barrier waits for it.
        if crate::hub_lease::running_elsewhere(store, &s.id).is_some() {
            continue;
        }
        // Another spoke's list: only a member this spoke holds (wsp-486).
        if held_only && !store.claims().get(&s.id).is_some_and(|c| crate::hub_lease::token_of(c).is_some()) {
            continue;
        }
        let Some(member) = store.find_task(&s.id) else { continue };
        // On the kind the work ran on — `wsp-134` d1 (C) — which in a
        // mixed group is the member's own line and not the group's.
        let on = g.policy_for(&member.id).unwrap_or_else(|| policy.clone());
        let label = crate::verification::seat_label(&member.id);
        // A hold that owes its land (`wsp-605`) owes no verifier: the land is
        // retried once the tree is clean, and until then nothing here starts.
        if crate::verification::land_owed(&member).is_some() {
            retry_owed_land(store, &w.id, at, &member);
            continue;
        }
        if !prepare_ready(store, &w.id, at, &member) {
            continue;
        }
        let member = store.find_task(&member.id).unwrap_or(member);
        // Before any agent reads it: the mechanical checks, which need none.
        if crate::check::gate(store, &member) == crate::check::Gate::Wait {
            continue;
        }
        if let Some(opened) = open_verifier(store, &member, &on, &gone, &absent, &|| gate.admits(&label)) {
            // A member split into sub-tasks no run started (`wsp-311`): its
            // governor hears it once for each landing, as the pass opens.
            if !opened.again {
                let tasks = store.tasks_shared();
                let unrun = crate::split::unrun(&tasks, &member.id, &worklist::Running::read(store));
                if !unrun.is_empty() {
                    tell(store, &w, &crate::split::told(&w.id, &member, &unrun));
                }
            }
            if let Some(seat) = &opened.lost {
                log_line(store, &format!(
                    "{} group {at}: {}'s verifier in {seat} is gone with no verdict — starting another",
                    w.id, member.id
                ));
            }
            if let Some(seat) = &opened.superseded {
                match opened.lost.as_deref() == Some(seat.as_str()) {
                    true => log_line(store, &format!("{} group {at}: {}'s verifier seat {seat} still stands with its agent gone — ending it", w.id, member.id)),
                    false => log_line(store, &format!("{} group {at}: {} landed again — ending the pass reading in {seat}", w.id, member.id)),
                }
                end_passes(store, seats, &member.id, |p| p.state == crate::verification::State::Superseded);
            }
            if spawn(store, &member.id, &on, Undo::Verifier, (&w.id, &at.to_string())) {
                started.push(label);
            } else {
                failed(store, &w, &member.id);
            }
        }
    }

    if held_only {
        return started;
    }

    // 2a. And one that is reviewed and *not* landed gets nothing, which the
    // log has to say: tokenhub-003 went to review with its commit still on
    // its branch, this started nothing and wrote nothing, and its governor
    // went looking for the cause in the wrong record (`wsp-142`).
    //
    // **Once per reason** (`wsp-338`), by a marker on the member: every pass
    // used to write it, so a member whose seat had been released was ordered
    // to commit and review again once a minute until a person stepped in. A
    // changed reason — fewer commits, a new status, a new wait — is said again.
    if all && !barrier_only {
        for s in pos.members.iter().filter(|s| s.settlement.settled() && !s.owes_a_verifier()) {
            let line = unlanded(&s.id, &s.settlement, &s.note());
            let kind = format!("{} {} is at ", crate::repair::LOGGED_WHY, s.id);
            if crate::repair::said_once_per_reason(store, &s.id, &kind, &format!("{} {line}", crate::repair::LOGGED_WHY)) {
                log_line(store, &format!("{} group {at}: {line}", w.id));
            }
        }
    }

    // 3. The barrier, once every member is landed, every verdict is in, and
    // every member has stopped working. `wsp-164`.
    let tasks = store.tasks_shared();
    if pos.at_barrier() && (barrier_only || pos.members.iter().all(|s| verified(&tasks, &s.id) || crate::hub_lease::verdict_held(store, &s.id))) {
        match holding_barrier(store, seats, &w, &pos) {
            // A Barrier row filed before `wsp-230` and still open is that
            // barrier's check, whose agent may be mid-read: nothing is owed.
            // One at `todo` that nobody ever claimed is a start that never
            // arrived, and a check entry takes its place.
            None if tasks.iter().any(|t| {
                is_barrier(t, &w.id, at)
                    && match t.status() {
                        Status::Review | Status::Done => false,
                        Status::Todo => !wedged(t, &store.claims()),
                        _ => true,
                    }
            }) => {}
            None => {
                let on = Some(policy.label());
                let label = crate::barrier::seat_label(&w.id, at);
                let admit = || gate.admits(&label);
                // A start that was cut off before it could say it failed, on
                // its last try: nothing else will ever see it.
                if let Some(tries) = crate::barrier::give_up(store, &w.id, at) {
                    check_gave_up(store, &w, at, tries, "no start reported back");
                }
                // Only read when an entry would be written, so a barrier that
                // is being waited on costs no git calls each tick.
                let mech = if crate::barrier::would_open(g, &gone) && crate::barrier_run::owed(store, &w, at) {
                    crate::barrier::Mechanical::Yes
                } else {
                    crate::barrier::Mechanical::No
                };
                // A run that holds the tree lock is alive, whatever a liveness
                // probe says of its pid: it is not started again, for the
                // copy would only wait on the lock behind it.
                let held = || crate::barrier_run::lock_held(store, &w, at);
                let opening = crate::barrier::open_unless_held(store, &w, at, trunk_tip(store, &w.id), on, mech, &gone, &admit, &held);
                if matches!(opening, crate::barrier::Opening::Held) && all {
                    log_line(store, &format!("{} group {at}: its barrier run holds the tree lock — waiting for its result", w.id));
                }
                if let crate::barrier::Opening::Opened(o) = opening {
                    if let Some(pid) = o.died {
                        log_line(store, &format!(
                            "{} group {at}: its barrier run ended without a result (pid {pid}), starting it again (try {} of {})",
                            w.id, o.tries, crate::barrier::TRIES
                        ));
                    } else if o.retaken {
                        log_line(store, &format!(
                            "{} group {at}: starting its check again — the last start never arrived (try {} of {})",
                            w.id, o.tries, crate::barrier::TRIES
                        ));
                    }
                    // wsp's own run first, when there is one to make. A retaken
                    // entry whose run had finished has only its start to retry.
                    let ran_already = matches!(o.suite, Some(crate::barrier::Suite::Green | crate::barrier::Suite::Red));
                    if mech == crate::barrier::Mechanical::Yes && !ran_already {
                        launch(store, &["worklist", "advance", &w.id, "--barrier-run", &at.to_string()]);
                        started.push(label);
                    } else {
                        match start_check(store, &w.id, at, &policy) {
                            None => started.push(label),
                            Some(why) => {
                                log_line(store, &format!("FAILED spawn the check on {label}: {why}"));
                                check_start_failed(store, &w, at, &why);
                            }
                        }
                    }
                }
            }
            // Said every pass, never silently. `wsp-142`'s lesson is that a
            // no-op which writes nothing is indistinguishable from a stall
            // with no cause, and the cause here is a fact about a pane.
            Some(waiting) if all => log_line(store, &format!("{} group {at}: barrier waits: {}", w.id, waiting.join("; "))),
            Some(_) => {}
        }
    }

    // 2b. The legs of a row already begun: every one that is ready, through the
    // gate, in record order. After the verifiers and the barrier and before any
    // new member, so a leg is unfinished work on a row the run has begun and
    // finishing goes ahead of fanning out (`docs/legs.md`, *The run*).
    // The pieces of a row already begun go in the same place: its batches close,
    // and the lowest open one's pieces open (`docs/pieces.md`, *The run*).
    for s in pos.members.iter().filter(|s| !s.finished()) {
        if store.find_task(&s.id).is_some_and(|t| crate::pieces::staged(&t)) {
            started.extend(run_pieces(store, Some((&w, at)), &s.id, Rows::Started, None, &gate));
        } else {
            started.extend(run_legs(store, Some((&w, at)), &s.id, Rows::Started, None, &gate));
        }
    }

    // 1. Members nobody has started, up to the group's cap — counted inside
    // the lock by `take_member`, because the two advances a pass can start
    // (the barrier agent's `go` and its own `review`) both read this list.
    //
    // A member that rests on a member of its own group (`after:`, `wsp-256`)
    // is not started until that one has landed. It is left at `todo` and
    // unclaimed, so `take_member` never counts it against the cap, and the
    // land that frees it pokes this step. Said every pass, as the barrier's
    // waits are, because a member nobody started is otherwise a silence.
    //
    // **Last**, and under the seat's cap after every other running list's
    // verifiers and checks as well (`wsp-231`): a group anywhere on the
    // machine finishes before the next work fans out.
    if all && !barrier_only {
        end_second_seats(store, seats, &w, at, &g.members);
        let holds = crate::after::waiting(store, g);
        let unstarted = pos.members.iter().any(|s| {
            !holds.contains_key(&s.id) && crate::cmd_allocate::allocated_elsewhere(store, &s.id).is_none() && store.find_task(&s.id).is_some_and(|t| t.status() == Status::Todo)
        });
        if gate.cap.is_some() && unstarted {
            // Each under its own list's lock, and a list already being stepped
            // is left to its holder, which is taking these very starts: no
            // wait, so two lists finishing each other cannot hold each other
            // up. A mark that arrives while this holds the other list is
            // answered with that list's whole step.
            for other in store.worklists().iter().filter(|o| o.id != w.id && o.status() == WorklistStatus::Running) {
                let _ = crate::stepping::one_at_a_time(&store.state, &other.id, false, |n| {
                    step_in(store, other, seats, if n == 0 { Starts::Finishing } else { Starts::All })
                });
            }
        }
        let own_cap = g.parallelism(None);
        let (cap, raised) = raised_cap(&g.kind, own_cap, gate.cap);
        let going_before = members_going(store, &g.members, "", &store.claims());
        let mut began = 0usize;
        // What every seat needs, before any is opened: a want that is the same
        // for every row parks the list on this line, with nothing tried.
        let blocked = if unstarted { list_wide_blocker(store, &g.members) } else { None };
        if let Some(why) = &blocked {
            park_list_for(store, &w.id, why);
        }
        for s in pos.members.iter().filter(|_| blocked.is_none()) {
            if let Some(on) = holds.get(&s.id) {
                log_line(store, &format!("{} group {at}: {} waits on {} — after: it has not landed", w.id, s.id, on.join(" ")));
                continue;
            }
            // A member allocated to another user runs on their wsp (wsp-416):
            // not started here, not a failure and not stuck, only said.
            if let Some(user) = crate::cmd_allocate::allocated_elsewhere(store, &s.id) {
                log_line(store, &format!("{} group {at}: {} runs elsewhere — allocated to {user}", w.id, s.id));
                continue;
            }
            let Some(t) = store.find_task(&s.id) else { continue };
            // A member another user's spoke holds runs there (wsp-467): not
            // started, reseated or verified here; its landing arrives with the
            // row. One back at `todo` is asked of the hub again below.
            if let Some(user) = crate::hub_lease::running_elsewhere(store, &s.id).filter(|_| t.status() != Status::Todo) {
                log_line(store, &format!("{} group {at}: {} is running on {user}'s wsp", w.id, s.id));
                continue;
            }
            // A staged member starts with its first legs, or its first batch's
            // pieces, which count as one member against the group's `xN`
            // however many it runs.
            if crate::pieces::staged(&t) {
                started.extend(run_pieces(store, Some((&w, at)), &s.id, Rows::New, Some((&g.members, cap)), &gate));
                continue;
            }
            if crate::legs::staged(&t) {
                started.extend(run_legs(store, Some((&w, at)), &s.id, Rows::New, Some((&g.members, cap)), &gate));
                continue;
            }
            // The driver leases a shared member before it starts it (wsp-467);
            // the spawn's claim takes the same lease again.
            let startable = matches!(t.status(), Status::Todo | Status::Doing) && !store.claims().contains_key(&s.id);
            let leased = match startable {
                true => crate::hub_lease::lease_member(store, &s.id),
                false => crate::hub_lease::Member::Free,
            };
            let leased = match leased {
                crate::hub_lease::Member::Free => None,
                crate::hub_lease::Member::Leased(h) => Some(h),
                crate::hub_lease::Member::Elsewhere(user) => {
                    log_line(store, &format!("{} group {at}: {} is running on {user}'s wsp", w.id, s.id));
                    continue;
                }
                crate::hub_lease::Member::NotYet(why) => {
                    log_line(store, &format!("{} group {at}: {} not started: {why}", w.id, s.id));
                    continue;
                }
            };
            let Some((why, own)) = take_member(store, &t, &w.id, at, &g.members, cap, &gate) else {
                if let Some(h) = &leased {
                    crate::hub_lease::unused(store, &s.id, h);
                }
                continue;
            };
            log_line(store, &format!("{} group {at}: starting {} on {} ({why})", w.id, s.id, own.kind));
            // A failed member start is told, retried and flagged by `spawn`.
            let took = spawn(store, &s.id, &own, Undo::Member, (&w.id, &at.to_string()));
            if let (false, Some(h)) = (took, &leased) {
                crate::hub_lease::unused(store, &s.id, h);
            }
            // The spawn has returned and the row's `doing` is on disk: a
            // later pick reads that, and the mark has done its work.
            crate::starts::ended(store, &s.id);
            if took {
                started.push(s.id.clone());
            }
            began += 1;
            // A refusal every row would meet parked the list: no more tries.
            if !took && store.worklist(&w.id).is_some_and(|l| l.status() == WorklistStatus::Parked) {
                break;
            }
        }
        // Said when it made a difference, and not on every pass.
        if let (true, Some(own)) = (raised, own_cap) {
            if going_before + began > own {
                log_line(store, &format!(
                    "{} group {at}: x{own} raised to the seat's free places — a components group's rows touch disjoint files, \
                     so {} are going where x{own} would have allowed {own}",
                    w.id,
                    going_before + began
                ));
            }
        }
    }

    say_capped(store, &w.id, at, &gate, all);
    if all {
        // After the starts, so a silent row the step just reseated is not named
        // first: the naming is a line said of the row, and the reseat reads the
        // row's silence (wsp-524).
        name_silent(store, &w);
        escalate_the_blocked(store, &w, at, &pos);
    }

    // 3a. And the one `wsp-164`'s gate cannot reach: **a barrier that already
    // opened**, before the member was sent back. `wsp-163` found the gap — a
    // governor `wsp tell`ed its verifier to install and drive delivery live, the
    // row stayed at `review` because a tell does not touch status, and the
    // barrier opened over work still outstanding. `wsp reopen` makes the send
    // back one act, and this is the other half of it: a check that is already
    // reading a group must be told that the group changed under it.
    //
    // **Told, not stopped.** The barrier agent is not killed and its row is not
    // moved — it is mid-check and the run owns that turn. `wsp worklist go`
    // refuses while a member holds, so the race cannot be lost; what this
    // prevents is the check *finishing* on a group it was not given.
    if all {
        sent_back_holding(store, &w, at, g);
    }
    if !started.is_empty() {
        log_line(store, &format!("{} group {at}: started {}", w.id, started.join(" ")));
    }
    started
}

/// The line a check is opened on for a `manual` group whose barrier is ready,
/// or `None` when the group is left to its person. `wsp-629`.
///
/// All of: the group's line is `manual` itself (an empty one is a list from
/// before `agent:` and stays by hand); every member has landed; the stop is
/// written and names no person ([`names_a_person`]). The line is the list's
/// usual one ([`usual_line`]).
fn manual_barrier_policy(groups: &[Group], at: usize, g: &Group, pos: &worklist::Position) -> Option<Policy> {
    if g.agent.trim() != MANUAL || !pos.at_barrier() || pos.members.is_empty() {
        return None;
    }
    let stop = g.stop.trim();
    if stop.is_empty() || names_a_person(stop) {
        return None;
    }
    Policy::parse(&usual_line(groups, at))
}

/// Whether a stop says it is for a person to read — `by Ed`, `Ed's results`,
/// `the person` — which an agent cannot check and the run leaves to them.
pub(crate) fn names_a_person(stop: &str) -> bool {
    let lower = stop.to_lowercase();
    lower.split(|c: char| !c.is_alphabetic()).any(|w| matches!(w, "ed" | "person"))
}

/// The agent line a list usually runs on: the nearest group at or before
/// `at` (1-based) that has one, then any group that has, then
/// [`DEFAULT_POLICY`]. For a group left `manual` that a hand is to switch on.
pub(crate) fn usual_line(groups: &[Group], at: usize) -> String {
    let has = |g: &&Group| g.policy().is_some();
    let before = groups.iter().take(at).rev().find(has);
    let any = groups.iter().find(has);
    before.or(any).map(|g| g.agent.trim().to_string()).unwrap_or_else(|| DEFAULT_POLICY.to_string())
}

/// What the seat's cap held back on a list this pass, said once in `cycle.log`
/// and not on every tick (`wsp-231`). The record of what has been said is
/// `capped.json`, one entry per list: a step that takes every start replaces
/// its list's entry with what it held now, so a start that went ahead and was
/// held again later is said again; a [`Starts::Finishing`] pass only adds.
fn say_capped(store: &Store, list: &str, at: usize, gate: &crate::room::Gate, all: bool) {
    let held = gate.take_held();
    let Some(cap) = gate.cap else {
        if all && store.capped().contains_key(list) {
            store.set_capped(list, None);
        }
        return;
    };
    let before = store.capped().get(list).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut now = match all {
        true => serde_json::Map::new(),
        false => before.clone(),
    };
    for (what, room) in held {
        if now.contains_key(&what) {
            continue;
        }
        match before.get(&what) {
            Some(since) => {
                now.insert(what, since.clone());
            }
            None => {
                log_line(store, &format!(
                    "{list} group {at}: {what} waits — the seat is at its cap of {cap}{}",
                    gate.cap_said(&what, &room)
                ));
                now.insert(what, serde_json::Value::String(util::now_iso()));
            }
        }
    }
    if now != before {
        store.set_capped(list, (!now.is_empty()).then_some(serde_json::Value::Object(now)));
    }
}

/// The open barrier check of a group with a member that is no longer settled,
/// told what changed. `wsp-163`.
///
/// **Only a check an agent actually holds** — `doing` or `blocked`, not `todo`
/// and not settled. A check still sitting at `todo` was never given the group,
/// so it starts with the group as it stands and there is nothing to correct; a
/// `todo` row that has been wedged long enough gets taken again a moment later
/// and reads the group then. And only a member at `doing` or worse: one at
/// `review` or `done` is settled and holds nothing.
fn sent_back_holding(store: &Store, w: &Worklist, at: usize, g: &Group) {
    let tasks = store.tasks_shared();
    // A row from before `wsp-230`, or the seat of the group's open check: the
    // name `followup` logs it under, and what `wsp tell` reaches it by.
    let row = tasks
        .iter()
        .filter(|t| is_barrier(t, &w.id, at))
        .filter(|t| matches!(t.status(), Status::Doing | Status::Blocked))
        .max_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)))
        .map(|t| t.id.clone());
    let fresh = store.worklist(&w.id).and_then(|f| f.groups().into_iter().nth(at - 1));
    let seated = fresh.as_ref().and_then(crate::barrier::running).and_then(|c| c.pane.clone());
    let Some(check) = row.or(seated) else {
        return;
    };
    // Rows this check attached itself (`wsp-210`, `followup --blocking`) are
    // not sent back: the check knows they are outstanding, because it put them
    // there, and telling it otherwise would read as a group moving under it.
    let attached: Vec<String> = crate::cmd_worklist::followups(w)
        .into_iter()
        .filter(|(row, _)| *row == check)
        .flat_map(|(_, ids)| ids)
        .collect();
    let holding: Vec<Task> = tasks
        .iter()
        .filter(|t| g.members.contains(&t.id))
        .filter(|t| !attached.contains(&t.id))
        .filter(|t| !matches!(t.status(), Status::Review | Status::Done))
        .cloned()
        .collect();
    if holding.is_empty() {
        return;
    }
    let names: Vec<String> = holding
        .iter()
        .map(|t| {
            let why = t
                .section("Log")
                .and_then(|l| l.lines().filter(|l| l.contains(SENT_BACK)).last().map(|x| x.to_string()))
                .map(|l| l.split(SENT_BACK).nth(1).unwrap_or_default().trim().to_string())
                .unwrap_or_default();
            if why.is_empty() {
                t.id.clone()
            } else {
                format!("{} ({})", t.id, util::truncate(&why, 80))
            }
        })
        .collect();
    log_line(store, &format!(
        "{} group {at}: {} was opened before {} was sent back — the check is told",
        w.id,
        check,
        names.join(", ")
    ));
    tell_barrier(store, w, at, &check, &names);
}

/// The marker a reopened row carries, so the check can be told what changed
/// rather than only that something did. `wsp-163`.
pub(crate) const SENT_BACK: &str = "sent back:";

/// Say to a barrier check's own pane that its group moved under it.
///
/// **`wsp tell` run as a process, for [`crate::cmd_task::reopen`]'s reason**:
/// its refusals are the answer worth having, and a second implementation here
/// would be a second answer that could disagree with the verb.
fn tell_barrier(store: &Store, w: &Worklist, at: usize, check: &str, members: &[String]) {
    let text = format!(
        "The {} run sent {} back after your barrier check {} opened, so the group is not the one you \
         were given. Read `wsp worklist next {}` again before deciding, and if it already holds, \
         `wsp worklist hold {} \"what has to happen\"`.",
        w.id,
        members.join(" and "),
        check,
        w.id,
        w.id
    );
    let key = format!("barrier:{}:{at}", w.id);
    tell_seat(store, check, &text, "the group moved under it", &key);
}

/// The seat a tell to `to` reaches: the pane of the row `to` names (its
/// claim's, where the row has more than one), or `to` itself, a seat.
fn seat_of(store: &Store, to: &str) -> String {
    let panes = store.panes_for_task(to);
    let holder = store
        .claims()
        .get(to)
        .and_then(|claim| crate::address::claim_holder(claim, &panes, &store.agents_held()));
    holder.or_else(|| panes.first().cloned()).unwrap_or_else(|| to.to_string())
}

/// `wsp tell <seat> -` with `text`, said in `cycle.log` as `told <seat>: <what>`.
///
/// **Under the orders machine, an order** (wsp-125): [`crate::refusals::Door::send`]
/// under `key` and the seat, so the tick that tells and any tick that tells the
/// same thing again hold one order (the caller's key is per list, group and
/// what it is about). Nothing waits for the answer; the order's state is the
/// record of whether the seat heard.
fn tell_seat(store: &Store, check: &str, text: &str, what: &str, key: &str) {
    if let Some(door) = crate::refusals::door(store) {
        let seat = seat_of(store, check);
        match door.send(&seat, text, &format!("{key}:{seat}")) {
            Ok(sent) if sent.fresh => log_line(store, &format!("told {check}: {what} (order {})", sent.id.0)),
            Ok(_) => {}
            Err(e) => log_line(store, &format!("told {check}: not delivered — {e}")),
        }
        return;
    }
    if cfg!(test) {
        #[cfg(test)]
        crate::cycle::tests::MEMBER_TOLD.with(|t| t.borrow_mut().push((check.to_string(), text.to_string())));
        return;
    }
    let Ok(exe) = wsp_base::util::self_exe() else { return };
    let mut child = match wsp_base::calls::Command::new(exe)
        .args(["tell", check, "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log_line(store, &format!("tell {check}: could not run `wsp tell` ({e})"));
            return;
        }
    };
    if let Some(mut to) = child.stdin.take() {
        use std::io::Write;
        let _ = writeln!(to, "{text}");
    }
    match child.wait_with_output() {
        Ok(o) if o.status.success() => log_line(store, &format!("told {check}: {what}")),
        Ok(o) => log_line(store, &format!(
            "told {check}: not delivered — {}",
            util::truncate(String::from_utf8_lossy(&o.stderr).trim(), 120)
        )),
        Err(e) => log_line(store, &format!("told {check}: not delivered — {e}")),
    }
}

/// End every seat but the claim's that stands on a member of this group, and
/// tell the claim's seat (`wsp-607`). Two seats in one member's tree write the
/// same files: react-parity-now's compounding-428 had cpd-4814 and cpd-4815
/// both editing it, a test failing on a symbol declared twice, and both
/// stopping to ask a person which one keeps the tree. The claim answers that —
/// its seat keeps the tree, and the other is ended with the tree left to it.
/// A row with two seats and no claim, or none whose agent is the claim's, is
/// left alone: nobody can say which one is the member.
fn end_second_seats(store: &Store, seats: &dyn Seats, w: &Worklist, at: usize, members: &[String]) {
    let claims = store.claims();
    for id in members {
        let Some(claim) = claims.get(id) else { continue };
        let panes = store.panes_for_task(id);
        if panes.len() < 2 {
            continue;
        }
        let Some(holder) = crate::address::claim_holder(claim, &panes, &store.agents_held()) else { continue };
        for p in panes.iter().filter(|p| **p != holder) {
            if !seats.state(p).is_some_and(|s| !matches!(s, State::Gone | State::Empty)) {
                continue;
            }
            match despawn_seat_keeping(p, true) {
                Ok(()) => {
                    log_line(store, &format!(
                        "{} group {at}: {id} had a second seat, {p}, beside {holder} which holds its claim — ended {p}, its tree left to {holder}",
                        w.id
                    ));
                    if let Some(mut row) = store.task(id) {
                        let _ = crate::log::note(store, &mut row, &format!("a second seat {p} stood on this beside {holder}, which holds the claim; wsp ended {p}"));
                    }
                    tell_seat(
                        store,
                        &holder,
                        &format!(
                            "A second seat, {p}, was started on {id} and stood in your tree beside you. You hold the claim, \
                             so wsp ended {p} and the tree is yours: carry on. Check `git status` and `git diff` before your next \
                             commit — anything there you did not write is {p}'s, and is yours to keep or drop."
                        ),
                        &format!("{p} was ended and the tree of {id} is its"),
                        &format!("second-seat:{}:{at}:{id}:{p}", w.id),
                    );
                }
                Err(why) => log_line(store, &format!(
                    "{} group {at}: {id} has a second seat, {p}, beside {holder} which holds its claim, and it could not be ended: {}",
                    w.id,
                    util::truncate(&why, 200)
                )),
            }
        }
    }
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
    let asks = crate::waiting::Asks::read(store);
    // A member whose `## Checks` runs its own check (`wsp check <itself>`) can
    // only fail the barrier, after a full suite: the barrier is not started on
    // it. Refused at review and at the hold since; this is the row that got by.
    for s in &pos.members {
        let Some(t) = store.find_task(&s.id) else { continue };
        let calls = crate::check::calls_itself_in(&t.id, &crate::check::task_steps(&t));
        if !calls.is_empty() {
            waiting.push(format!(
                "{}'s check line calls its own check ({}) — `wsp check {} --set --why \"…\" -` to fix it, then `wsp reopen {} \"what is owed\"`",
                s.id, calls.join("; "), s.id, s.id
            ));
        }
    }
    for s in &pos.members {
        let Some(seat) = bound_seat(store, &s.id) else { continue };
        let state = seats.state(&seat);
        // **Waiting is neither idle nor gone** (`wsp-172`), so it holds the
        // barrier exactly as working does — and with no bound, unlike working:
        // a member on a prompt or behind its own open question has not finished
        // anything, it has stopped in the middle, and the half-hour report below
        // is for a turn that might be stuck, not for a question somebody owes.
        // Asked only of a seat that answered: `None` still never holds.
        // **A shell of its own does not hold the barrier** (`wsp-428`): every
        // member here is verified, so what its pane still runs is nothing the
        // group is owed. Group 5 of react-pass-1 waited 51 minutes on a dev
        // server. A prompt or an open question of its own still holds.
        let shells = Default::default();
        // **A question from a member whose row has landed does not hold it**
        // (`wsp-608`): the work is on the trunk, and what the member still
        // asks is owed by its governor without the group standing behind it.
        // React-parity-now's group 9 stood overnight on a member's "ready at
        // 340f83ed2f" filed as a question. A prompt on its screen still holds.
        let wait = state.and_then(|st| crate::waiting::reading(Some(st), &asks, &shells, &seat, &s.id));
        let landed = store.find_task(&s.id).is_some_and(|t| t.section("Log").is_some_and(|l| l.contains(crate::log::LANDED)));
        let wait = wait.filter(|w| !(landed && matches!(w.why, crate::waiting::Why::Answer(_) | crate::waiting::Why::Question(_))));
        if let Some(wait) = wait {
            waiting.push(format!("{seat} ({}) is {}", s.id, wait.sentence()));
            continue;
        }
        match state {
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
/// spawn, or a spawn that died without saying so. `wsp spawn` claims within
/// seconds when it works, so a minute is the longest a start in flight is left
/// alone (wsp-469: compounding-132's start never claimed and waited ~10 min).
const WEDGED_AFTER: i64 = 60;

/// Unclaimed, and written long enough ago that a start in flight would have
/// claimed it by now.
///
/// **Never a row run as legs or pieces** (`docs/legs.md`, *The run*;
/// [`crate::inner::run_inside`]): it holds no claim by design, so "unclaimed for a minute" is its ordinary state, and starting a
/// whole-row worker on it would put one beside the legs. Excluded by having the
/// section and not by a leg running, since between two legs none does.
fn wedged(t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> bool {
    !crate::inner::run_inside(t) && !claims.contains_key(&t.id) && util::epoch_secs() - util::epoch_of(&t.updated) > WEDGED_AFTER
}

/// How long a row at `doing` may have no seat and no line in `cycle.log` before
/// it is named (wsp-427): compounding-121's next seat's first line was 39
/// minutes after its spawn, with no log line for 33 of them.
pub(crate) const SILENT_AFTER: i64 = 5 * 60;

/// The marker a silent row's naming carries in `cycle.log`.
const SILENT_SAID: &str = "has no seat and no log line for";

/// The rows among `ids` at `doing` with no claim, no seat and nothing said of
/// them — on the row or in `cycle.log` — for [`SILENT_AFTER`], with how long. A
/// row run as legs holds no claim by design and is never one.
pub(crate) fn silent(store: &Store, ids: &[String]) -> Vec<(String, i64)> {
    let now = util::epoch_secs();
    let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
    let claims = store.claims();
    let mut out = Vec::new();
    for id in ids {
        let Some(t) = store.find_task(id) else { continue };
        if t.status() != Status::Doing || crate::inner::run_inside(&t) || claims.contains_key(id) || !store.panes_for_task(id).is_empty() {
            continue;
        }
        // Not silent: waiting, by its own choice, on a row the run is running.
        if awaiting_prerequisite(store, &t, &claims).is_some_and(|on| !on.is_empty()) {
            continue;
        }
        let said = log
            .lines()
            .rev()
            .find(|l| l.split_whitespace().any(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-') == id.as_str()))
            .and_then(|l| l.split_whitespace().next())
            .map(util::epoch_of)
            .unwrap_or(0);
        let quiet = now - said.max(util::epoch_of(&t.updated));
        if quiet > SILENT_AFTER {
            out.push((id.clone(), quiet));
        }
    }
    out
}

/// Name each silent row of the list in `cycle.log`, once per [`SILENT_AFTER`]
/// (the line it writes is the last said of the row, which is what the next
/// reading waits on).
fn name_silent(store: &Store, w: &Worklist) {
    let ids: Vec<String> = w.groups().iter().flat_map(|g| g.members.iter().cloned()).collect();
    let quiet_now = silent(store, &ids);
    // A row that has a seat again, or has said something, starts its count over.
    for id in ids.iter().filter(|i| !quiet_now.iter().any(|(q, _)| q == *i)) {
        let label = format!("{SILENT_LABEL}{id}");
        if store.starts().contains_key(&label) {
            crate::starts::forgive(store, &label);
        }
    }
    for (id, quiet) in quiet_now {
        // **Counted, as a check's tries are** (`wsp-600`): the line used to
        // repeat every five minutes with nothing done and nobody told, and
        // retro-2's wsp-565 stood there 45 minutes. The step reseats a silent
        // row in the same pass, so a notice is one that did not take; the third
        // raises the row to its governor and the person's panel.
        let m = crate::starts::missed_under(store, &format!("{SILENT_LABEL}{id}"), None);
        // Whether the group's own `xN` is what holds it: the step says nothing
        // when the cap is full, so a row waiting for a place reads the same as
        // one whose start was lost. Say which (`wsp-600`).
        let held_at = w
            .groups()
            .iter()
            .find(|g| g.members.contains(&id))
            .and_then(|g| g.parallelism(None).map(|c| (c, members_going(store, &g.members, &id, &store.claims()))))
            .filter(|(c, going)| going >= c);
        let cause = match held_at {
            Some((c, going)) => format!("it waits for a place in its group's x{c} ({going} of {c} in flight)"),
            None => "its start may not have arrived".to_string(),
        };
        log_line(store, &format!(
            "{}: {id} {SILENT_SAID} {} — {cause} (notice {} of {})",
            w.id,
            util::duration_human(quiet),
            m.tries,
            crate::barrier::TRIES
        ));
        if m.last {
            let remedy = format!("`wsp spawn {id} --agent` seats one once the cause is cleared, or `wsp worklist advance {}` tries the run's start again", w.id);
            let last = match held_at {
                Some((c, _)) => format!("no seat and no log line; the group's x{c} is full of members in flight"),
                None => "no seat and no log line".to_string(),
            };
            start_gave_up(
                store,
                Some(w),
                &id,
                &format!("{id} has had no seat for {}, and the run's starts did not seat it; it", util::duration_human(quiet)),
                m.tries,
                &last,
                &remedy,
            );
        }
    }
}

/// What a silent row's notices are counted under in `starts.json`.
const SILENT_LABEL: &str = "silent:";

/// How long a member may stand blocked, or on a question only a person can
/// answer, before the run escalates it (`wsp-471`). compounding-125 held its
/// whole group five hours and nothing said so.
pub(crate) const BLOCKED_ESCALATES_AFTER: i64 = 20 * 60;

/// What the run writes on a member it has escalated, once per block.
const ESCALATED: &str = "wsp: blocked and unanswered for";

/// When the member's newest `blocked:` line was written, if it is blocked.
pub(crate) fn blocked_since(t: &Task) -> Option<i64> {
    if t.status() != Status::Blocked {
        return None;
    }
    t.section("Log")?.lines().rev().find_map(|l| {
        let (stamp, rest) = l.trim().strip_prefix("- ")?.split_once(' ')?;
        rest.starts_with("blocked:").then(|| util::epoch_of(stamp)).filter(|e| *e > 0)
    })
}

/// What the run writes after [`SENT_BACK`] when it reopens a blocked member
/// because the row carries a decision made since the block (`wsp-635`).
pub(crate) const ANSWERED_BY: &str = "answered by decision";

/// The decision that answers a blocked member's question: the newest on its
/// row recorded at or after its newest `blocked:` line.
///
/// **A decision is an answer** (`wsp-635`). On 2026-11-14 compounding-547 asked
/// a question, blocked, and its seat ended; the list's custodian answered with
/// `wsp decide`, which records the answer and leaves the row `blocked` with
/// nobody on it. The cycle logged `compounding-552 waits on compounding-547`
/// every 78 s for seven hours, because nothing read the decision as a reason to
/// move the row. `None` for a row that is not blocked, or has no decision newer
/// than its block. An entry with no instant of its own (a bare date, or none)
/// cannot be placed against the block, so it answers nothing.
pub(crate) fn answer_since_block(t: &Task) -> Option<crate::model::Decision> {
    let since = blocked_since(t)?;
    crate::model::decisions_of(&t.body)
        .into_iter()
        .rev()
        .find(|d| !d.id.is_empty() && d.at.len() == 20 && util::epoch_of(&d.at) >= since)
}

/// A blocked member of the running group whose row has a decision made since
/// its block is sent back by the run, as `wsp reopen` would: `doing`, the
/// answer on its log as a `sent back:` line (which the step reads as "reseat
/// this", so a member with no seat is seated in the same pass), the decision
/// told to a seat that is still in it, and the list's own log and `cycle.log`
/// saying why. No person is needed for it (`wsp-635`).
fn reopen_the_answered(store: &Store, w: &Worklist, at: usize, pos: &worklist::Position) {
    for s in &pos.members {
        let took = store.locked(|| {
            let mut t = store.find_task(&s.id)?;
            let d = answer_since_block(&t)?;
            let question = t.blocked_question().unwrap_or_else(|| "its question".to_string());
            let answer = util::fold(&d.text);
            t.set_status(Status::Doing);
            t.log(&format!(
                "{SENT_BACK} {ANSWERED_BY} {} to its block ({}): {}",
                d.id,
                util::truncate(&util::fold(&question), 80),
                util::truncate(&answer, 400)
            ));
            store.save_task(&t).ok()?;
            if let Some(mut wl) = store.worklist(&w.id) {
                wl.log(&format!(
                    "wsp: group {at} reopens {} — decision {} answers its block ({}), so it is sent back with no person asked",
                    t.id,
                    d.id,
                    util::truncate(&util::fold(&question), 80)
                ));
                let _ = store.save_worklist(&wl);
            }
            Some((t, d))
        });
        let Some((t, d)) = took else { continue };
        store.log_event("task-reopened", serde_json::json!({ "id": t.id, "project": t.project, "status": "doing", "by": "decision", "decision": d.id }));
        store.git_commit(&format!("wsp: reopen {} — decision {} answers its block", t.id, d.id));
        log_line(store, &format!(
            "{} group {at}: {} was blocked and decision {} answers it — reopened, no person asked",
            w.id, t.id, d.id
        ));
        crate::cmd_task::deliver_reason(
            store,
            &t.id,
            &format!("{ANSWERED_BY} {}: {} (`wsp show {}` has it whole)", d.id, util::truncate(&util::fold(&d.text), 400), t.id),
        );
    }
}

/// The move for a member blocked with no answer in sight while the rest of its
/// group has landed (`wsp-517`, wsp-513's last part): cut it, say why, tell the
/// person. Waiting is for a block the person is about to answer.
fn cut_line(list: &str, id: &str) -> String {
    format!(
        " If no answer is coming and the rest of the group has landed, cut it: \
         `wsp worklist cut {list} {id} --why \"…\"` takes it and what rests on it out into a later group \
         and the run goes on; say why in the reason and tell the person."
    )
}

/// A member of the running group that has stood blocked for
/// [`BLOCKED_ESCALATES_AFTER`]: a line on the row (so `wsp wip` and `wsp show`
/// say it, and it is said once per block), a flag to the person, and the
/// list's governor told what would unblock it.
fn escalate_the_blocked(store: &Store, w: &Worklist, at: usize, pos: &worklist::Position) {
    let now = util::epoch_secs();
    for s in &pos.members {
        let Some(mut t) = store.find_task(&s.id) else { continue };
        let Some(since) = blocked_since(&t) else { continue };
        let quiet = now - since;
        if quiet < BLOCKED_ESCALATES_AFTER {
            continue;
        }
        let log = t.section("Log").unwrap_or_default().to_string();
        let lines: Vec<&str> = log.lines().collect();
        let blocked_at = lines.iter().rposition(|l| l.contains(" blocked:")).unwrap_or(0);
        if lines[blocked_at..].iter().any(|l| l.contains(ESCALATED)) {
            continue;
        }
        let question = t.blocked_question().unwrap_or_else(|| "a decision is owed".to_string());
        let mins = quiet / 60;
        t.log(&format!("{ESCALATED} {mins} min — the person is flagged and the governor told"));
        let _ = store.save_task(&t);
        log_line(store, &format!("{} group {at}: {} blocked for {mins} min — flagged", w.id, t.id));
        let text = format!(
            "{}: blocked for {mins} min, and the {} run waits on it\n\n\
             It asked: {question}\n\nAnswering it is what lets group {at} go on: `wsp decide {} \"…\"` (the run reopens the row on a decision made since the block, and seats it), tell the seat (`wsp tell {}`) or `wsp reopen {} \"what is owed\"`.\n\n\
             If no answer is coming, `wsp worklist cut {} {} --why \"…\"` takes it out and the group goes on without it.",
            t.id, w.id, t.id, t.id, t.id, w.id, t.id
        );
        let m = crate::message::Message::new(crate::message::Party::wsp(), crate::message::Kind::Note, &text)
            .about(crate::message::About::Task(t.id.clone()));
        let _ = crate::message::raise(store, &m);
        tell(store, w, &format!(
            "{} ({}) has been blocked for {mins} min and the {} run waits on it; the person is flagged. \
             It is waiting on: {question} — `wsp decide {} \"…\"` (the run reopens the row), an answer to that (`wsp tell {}`) or `wsp reopen {} \"what is owed\"` is what unblocks it.{}",
            t.id, util::truncate(&t.title, 60), w.id, t.id, t.id, t.id, cut_line(&w.id, &t.id)
        ));
    }
}

/// The cap a group's members are counted against (`wsp-471`).
///
/// A group's `xN` is a belief that its rows sit near each other. A
/// `components` group's rows are, by the kind's definition, units on disjoint
/// files, so under a seat's cap the group's own `xN` only stretches the wall
/// while places are free: the group's number is dropped and the seat's cap —
/// which [`crate::room::Gate::admits`] holds under the lock — is the limit, so
/// the group runs as many as there are free places. With no seat cap there are
/// no free places to speak of, and `xN` stands. Returns the cap and whether it
/// was raised.
fn raised_cap(kind: &str, own: Option<usize>, seat: Option<usize>) -> (Option<usize>, bool) {
    let components = crate::scope::GroupKind::parse(kind) == Some(crate::scope::GroupKind::Components);
    match (components, own, seat) {
        (true, Some(_), Some(_)) => (None, true),
        _ => (own, false),
    }
}

/// The line `take_member` writes, and how it recognises its own record.
pub(crate) const STARTED_BY: &str = "started by wsp:";

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
    gate: &crate::room::Gate,
) -> Option<(&'static str, Policy)> {
    let id = t.id.clone();
    let took = store.locked(|| {
        let t = store.find_task(&id)?;
        if crate::inner::run_inside(&t) {
            return None;
        }
        let claims = store.claims();
        // A member that released its seat to wait on a prerequisite the run
        // pulled into the group stays unstarted until that row has landed, and
        // is then seated again on its tree (`wsp-495`). Neither the silence nor
        // the cap below may start it first.
        let prerequisite = (t.status() == Status::Doing).then(|| awaiting_prerequisite(store, &t, &claims)).flatten();
        if prerequisite.as_ref().is_some_and(|on| !on.is_empty()) {
            return None;
        }
        // What the run reseats a `doing` member for when no start of its own
        // is under way (`wsp-524`): nothing said of it and nobody on it for
        // [`SILENT_AFTER`], or a governor ended its seat to have it reseated.
        let reseating = (t.status() == Status::Doing).then(|| {
            if silent(store, std::slice::from_ref(&id)).first().is_some() {
                Some("again: no seat")
            } else if despawned_unseated(store, &t, &claims) {
                Some("again: a governor ended its seat")
            } else {
                None
            }
        });
        let reseating = reseating.flatten();
        // After a start that did not take: its wait is not over, or its tries
        // are spent and a person is to say (`wsp-335`). A spent start is the
        // one thing a reseat takes again, once the seat has room.
        //
        // **A sent-back line newer than every start is a fresh ask, and the
        // tries a spent start used belong to an earlier one** (`wsp-600`): a
        // start since the reopen would have written [`STARTED_BY`] over it, so
        // a spent count beside it is stale. This is a hardening and **not the
        // cause of retro-2's wsp-565 standing seatless**: the record shows no
        // spent start there (the 07:35 `advance` forgave none). That was the
        // group's `x2` counting seatless sent-back members against each other
        // in [`members_going`], which now leaves them out.
        let sent_back = t.status() == Status::Doing && sent_back_unseated(store, &t, &claims);
        let held = crate::starts::held(store, &id);
        let spent = held && crate::starts::spent(store, &id) && (reseating.is_some() || sent_back);
        if held && !spent {
            return None;
        }
        let ours = t.section("Log").and_then(|l| l.lines().last().map(|x| x.contains(STARTED_BY))).unwrap_or(false);
        let mut reseat = None;
        let mut gated = false;
        let why = match t.status() {
            Status::Todo if !claims.contains_key(&id) => "new",
            Status::Doing if prerequisite.is_some() && !spent => "again: its prerequisite landed",
            Status::Doing if ours && wedged(&t, &claims) && !spent => {
                // A start the room holds is queued and is not a miss (wsp-549):
                // the group's cap and the seat's are asked *before* a try is
                // counted, so a row waiting for a slot loses none, however
                // many passes it waits. Only a start that was launched and
                // made no claim is counted.
                if cap.is_some_and(|c| members_going(store, members, &id, &claims) >= c) || !gate.admits_again(&id) {
                    return None;
                }
                gated = true;
                if !retry_lost_start(store, &id, &id, &id, &format!("`wsp worklist advance {list}`")) {
                    return None;
                }
                "again: the last start never claimed it"
            }
            Status::Doing if sent_back => {
                if spent {
                    crate::starts::forgive(store, &id);
                }
                "again: it was sent back and nobody is on it"
            }
            Status::Doing if reseating.is_some() => {
                let (done, stopped) = reseats_lately(&t);
                if done >= RESEATS {
                    if !stopped {
                        stop_reseating(store, &t, list, done);
                    }
                    return None;
                }
                reseat = reseating.map(|r| (r, done + 1));
                reseating.unwrap_or_default()
            }
            _ => return None,
        };
        let going = members_going(store, members, &id, &claims);
        if cap.is_some_and(|c| going >= c) {
            return None;
        }
        // The seat's cap, after the group's: a group under its own `xN` may
        // still find the machine full, and what is said is the machine.
        if !gated && !gate.admits(&id) {
            return None;
        }
        let policy = store.worklist(list)?.groups().get(at - 1)?.policy_for(&id)?;
        // The start's own mark, which only one pick can take (`wsp-607`): a
        // second pick of this row inside the spawn window — this tick's next
        // pass, or an `advance` beside the daemon's tick, with the store lock
        // given up under load — finds it and starts nothing. Read the row again
        // once it is held: a pick that read it before the first start's
        // `doing` reached the disk, and took the mark after that start let it
        // go, sees the row moved and starts nothing either.
        // A test's stall between the read and the write, as load made one.
        #[cfg(test)]
        if let Some(d) = tests::STALL.with(|s| s.get()) {
            std::thread::sleep(d);
        }
        if !crate::starts::begin(store, &id) {
            log_line(store, &format!("{list} group {at}: {id} is already being started — not starting it twice"));
            return None;
        }
        let moved = store.find_task(&id).is_none_or(|now| now.status() != t.status() || now.updated != t.updated);
        if moved {
            crate::starts::ended(store, &id);
            return None;
        }
        let mut t = t;
        if prerequisite.is_some() {
            t.log(&format!("{PREREQUISITE_LANDED}: {}", prerequisites_named(&t).join(" ")));
            log_line(store, &format!("{list}: {id} started again: its prerequisite landed"));
        }
        if let Some((reason, n)) = reseat {
            if spent {
                crate::starts::forgive(store, &id);
            }
            t.log(&format!("{RESEATED} (reseat {n} of {RESEATS}): {reason}"));
            log_line(store, &format!("{list}: {id} started again: no seat — {reason}, reseat {n} of {RESEATS}"));
        }
        t.set_status(Status::Doing);
        t.log(&format!("{STARTED_BY} {list} group {at}"));
        t.touch();
        if store.save_task(&t).is_err() {
            crate::starts::ended(store, &id);
            return None;
        }
        Some((why, policy))
    })?;
    store.git_commit(&format!("wsp: {list} group {at} starts {id}"));
    Some(took)
}

// ---- a member's prerequisite (wsp-495) ---------------------------------------------

/// The line the run writes on a member when it adds a row the member names with
/// `after` to the member's running group: `wsp: prerequisite <row> added to
/// <list> group <n> …`. It is also what [`awaiting_prerequisite`] reads.
pub(crate) const PREREQUISITE: &str = "wsp: prerequisite";

/// What the run writes when the rows a released member waited on have landed
/// and it starts the member again.
const PREREQUISITE_LANDED: &str = "wsp: prerequisite landed, starting again";

/// What the run writes on a member whose `after` names a row it will not pull
/// in, once per row.
const NOT_PULLED: &str = "wsp: not adding";

/// The rows the run has pulled in on this member's account, in the order it
/// wrote them.
fn prerequisites_named(t: &Task) -> Vec<String> {
    t.section("Log")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (_, rest) = l.trim().strip_prefix("- ")?.split_once(' ')?;
            let rest = rest.strip_prefix(PREREQUISITE)?;
            rest.split_whitespace().next().map(str::to_string)
        })
        .collect()
}

/// A member that released its seat to wait on a prerequisite the run pulled in:
/// `None` for any other row. Otherwise the rows still owed, and **empty once
/// every one has landed, which is when the member is due its seat again**.
///
/// The member is at `doing` with no claim and no pane, and the run's
/// [`PREREQUISITE`] line is newer than the last start or claim on its row — read
/// as [`sent_back_unseated`] reads its own mark, so a member that took its seat
/// again is no longer one that waits.
fn awaiting_prerequisite(store: &Store, t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> Option<Vec<String>> {
    if t.status() != Status::Doing || !unseated_since(store, t, claims, PREREQUISITE) {
        return None;
    }
    let mut repos = None;
    let mut on = prerequisites_named(t);
    on.retain(|o| !crate::after::landed(store, &mut repos, o));
    Some(on)
}

/// Add to the running group `at` of `w` each row that one of its members names
/// with `after` and that **no list holds**, so a prerequisite a member finds
/// mid-run is scheduled without a person. Returns whether it added any.
///
/// Safe where an arbitrary late member is not: the row exists only to unblock a
/// member, and the barrier already waits on that member. The row goes in just
/// ahead of the member, [`crate::after::waiting`] holds the member until it has
/// landed, and the group's `xN` and the seat's cap govern its start like any
/// member's. What is **not** added, and says so on the member's row: a row some
/// other list holds. A row that is done owes nothing; a row named by a task that
/// is not a member of the group is nobody's prerequisite here.
fn pull_prerequisites(store: &Store, w: &Worklist, at: usize) -> bool {
    let wanted = |store: &Store, w: &Worklist, groups: &[Group], held: &[(String, String)]| -> Vec<(String, String)> {
        let Some(g) = groups.get(at - 1) else { return Vec::new() };
        let mut out: Vec<(String, String)> = Vec::new();
        for m in &g.members {
            let Some(t) = store.find_task(m) else { continue };
            if Settlement::of(&t).settled() || t.status() == Status::Done {
                continue;
            }
            for o in &t.after {
                if g.members.contains(o) || out.iter().any(|(_, x)| x == o) {
                    continue;
                }
                let Some(row) = store.find_task(o) else { continue };
                if row.status() == Status::Done {
                    continue;
                }
                match held.iter().find(|(r, _)| r == o) {
                    None => out.push((m.clone(), o.clone())),
                    Some((_, list)) if *list != w.id => {
                        let said = format!("{NOT_PULLED} {o} to {} group {at}: {list} holds it", w.id);
                        let mut t = t.clone();
                        if !t.section("Log").unwrap_or_default().lines().any(|l| l.contains(&said)) {
                            t.log(&said);
                            let _ = store.save_task(&t);
                        }
                    }
                    Some(_) => {}
                }
            }
        }
        out
    };
    let held_by = |store: &Store| -> Vec<(String, String)> {
        store
            .worklists()
            .iter()
            .flat_map(|l| l.groups().into_iter().flat_map(|g| g.members).map(|m| (m, l.id.clone())).collect::<Vec<_>>())
            .collect()
    };
    // Read once without the lock, so a pass with nothing to add costs no write.
    if wanted(store, w, &w.groups(), &held_by(store)).is_empty() {
        return false;
    }
    let pulled = store.locked(|| {
        let mut w = store.worklist(&w.id)?;
        if w.status() != WorklistStatus::Running || worklist::position(store, &w, Reading::Settled).at != Some(at) {
            return None;
        }
        let mut groups = w.groups();
        let rows = wanted(store, &w, &groups, &held_by(store));
        if rows.is_empty() {
            return None;
        }
        for (m, o) in &rows {
            let members = &mut groups[at - 1].members;
            let place = members.iter().position(|x| x == m).unwrap_or(members.len());
            members.insert(place, o.clone());
        }
        let said = rows.iter().map(|(m, o)| format!("{o} for {m}")).collect::<Vec<_>>().join(", ");
        w.log(&format!("wsp: group {at} takes in {said} — a member names it with after: and no list holds it"));
        w.set_groups(&groups);
        store.save_worklist(&w).ok()?;
        for (m, o) in &rows {
            if let Some(mut t) = store.find_task(m) {
                t.log(&format!(
                    "{PREREQUISITE} {o} added to {} group {at}: the run starts it first, and this row again once it has landed",
                    w.id
                ));
                t.touch();
                let _ = store.save_task(&t);
            }
            if let Some(mut r) = store.find_task(o) {
                r.log(&format!("added to {} group {at} by the run: {m} names it with after: and no list held it", w.id));
                r.touch();
                let _ = store.save_task(&r);
            }
        }
        Some(rows)
    });
    let Some(rows) = pulled else { return false };
    for (m, o) in &rows {
        log_line(store, &format!("{} group {at}: {o} added — {m} names it with after: and no list holds it", w.id));
    }
    store.git_commit(&format!("wsp: {} group {at} takes in {}", w.id, rows.iter().map(|(_, o)| o.as_str()).collect::<Vec<_>>().join(" ")));
    true
}

/// Sent back since anybody last started on it, and nobody on it now: a member
/// `wsp reopen` put at `doing` after its agent had gone. `wsp-286`.
///
/// **The sent-back line is the record, as `started by wsp:` is for a start**,
/// and it has to be the newest of the two: the start `take_member` writes over
/// it is what keeps a second advance from starting it again. Not the last line
/// outright, unlike [`STARTED_BY`] — a governor's note on the row after the
/// reopen is ordinary, and must not strand it.
///
/// **No claim and no binding**, because a member sent back with its agent still
/// in its seat is the ordinary case and that agent was just told what is owed:
/// starting a second would be two agents in one tree. A claim whose seat went is
/// [`crate::repair`]'s, which tells it, ends it and puts the row back at `todo`.
///
/// **A row run as legs is sent back to a leg**, the `rework` the verdict
/// appended, and the step opens that; it is never reseated as a whole. Nor is
/// a row with pieces ([`crate::inner::run_inside`]).
fn sent_back_unseated(store: &Store, t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> bool {
    unseated_since(store, t, claims, SENT_BACK)
}

/// A governor ended the member's seat and nobody has started on it since
/// (`wsp-524`): [`RESEAT_ASKED`] is what `wsp despawn` writes for it, and it is
/// read exactly as [`sent_back_unseated`] reads the sent-back line.
fn despawned_unseated(store: &Store, t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> bool {
    unseated_since(store, t, claims, RESEAT_ASKED)
}

fn unseated_since(store: &Store, t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>, mark: &str) -> bool {
    if crate::inner::run_inside(t) || claims.contains_key(&t.id) || !store.panes_for_task(&t.id).is_empty() {
        return false;
    }
    let log = t.section("Log").unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    let Some(back) = lines.iter().rposition(|l| l.contains(mark)) else { return false };
    !lines[back + 1..].iter().any(|l| l.contains(STARTED_BY) || l.contains("claimed by pane "))
}

/// What `wsp despawn` writes on a member's row when a governor ends its seat:
/// the step reads it as "reseat this", so freeing the seat so the run reseats
/// does what it says (`wsp-524`).
pub(crate) const RESEAT_ASKED: &str = "despawned by a governor: the run reseats it";

/// The line a reseat writes on the row, and what the cap counts.
const RESEATED: &str = "wsp: no seat, starting again";

/// The line the run writes when it stops reseating, so the person is raised once.
const RESEAT_STOPPED: &str = "wsp: reseated twice with no first turn, not again";

/// How many times in [`RESEAT_WINDOW`] the run reseats one member before it
/// stops and raises the person.
const RESEATS: usize = 2;
const RESEAT_WINDOW: i64 = 60 * 60;

/// The reseats written on the row within [`RESEAT_WINDOW`], and whether the run
/// has already stopped and said so.
fn reseats_lately(t: &Task) -> (usize, bool) {
    let now = util::epoch_secs();
    let mut done = 0;
    let mut stopped = false;
    for l in t.section("Log").unwrap_or_default().lines() {
        let Some((stamp, rest)) = l.trim().strip_prefix("- ").and_then(|x| x.split_once(' ')) else { continue };
        if now - util::epoch_of(stamp) > RESEAT_WINDOW {
            continue;
        }
        done += rest.starts_with(RESEATED) as usize;
        stopped |= rest.starts_with(RESEAT_STOPPED);
    }
    (done, stopped)
}

/// A member reseated [`RESEATS`] times that is silent again: the third is not
/// started, the row says so, and the person is raised once (`wsp-524`). Called
/// under the store lock.
fn stop_reseating(store: &Store, t: &Task, list: &str, done: usize) {
    use crate::message::{self, About, Kind, Message, Party};
    let mut m = t.clone();
    m.log(&format!("{RESEAT_STOPPED} ({done} reseats)"));
    m.touch();
    let _ = store.save_task(&m);
    let text = format!(
        "{} has had no seat after {done} reseats in the {list} run, and the run has stopped starting one\n\n\
         Start it by hand (`wsp spawn {} --agent`) once the cause is cleared.",
        t.id, t.id
    );
    let _ = message::raise(store, &Message::new(Party::wsp(), Kind::Note, &text).about(About::Task(t.id.clone())));
    log_line(store, &format!("{list}: {} flagged the person — no seat after {done} reseats, not started again", t.id));
}

// ---- legs -----------------------------------------------------------------------

/// Set in the environment of a leg the run starts outside any list, where there is
/// no [`OWNED_LIST`] to say it is the run and not a person asking. `wsp spawn`
/// reads either as "not by hand", so it does not say the seat is at its cap and
/// start anyway: the gate has already decided.
pub(crate) const RUN_STEP: &str = "WSP_RUN_STEP";

/// Which rows [`take_legs`] opens legs on: a row the run has already begun
/// (`doing`), or one it has not (`todo`, a new member, which also counts against
/// its group's `xN`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rows {
    Started,
    New,
}

/// A leg opened by [`take_legs`]: its name, and whether it is a start taken
/// again because the last one never seated it.
struct Opening {
    name: String,
    again: bool,
}

/// How many of `members`, other than `id`, are in flight: not settled, and at
/// `doing` or holding a claim. What a group's `xN` is counted against.
fn members_going(store: &Store, members: &[String], id: &str, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> usize {
    members
        .iter()
        .filter(|m| *m != id)
        .filter_map(|m| store.find_task(m))
        .filter(|m| !Settlement::of(m).settled())
        .filter(|m| m.status() == Status::Doing || claims.contains_key(&m.id))
        // A member that released its seat to wait on a prerequisite is not in
        // flight: counting it would hold the prerequisite out of an `x1` group,
        // which is the member waiting on the row it is holding the place of.
        .filter(|m| !awaiting_prerequisite(store, m, claims).is_some_and(|on| !on.is_empty()))
        // **Nor is a member that is itself waiting for a seat** (`wsp-600`).
        // retro-2 group 1 is `x2`; wsp-565, wsp-572 and compounding-284 were
        // all sent back at `doing` with no seat. Each counted the other two as
        // in flight, `2 >= 2`, and `take_member` returned without a line: the
        // three starved each other for 2.5 hours, until wsp-565's hand-made
        // seat reached review and freed a count. A row with no claim and no
        // pane that is waiting to be reseated runs nothing; it takes its place
        // when it is started, and the count taken under the lock then holds the
        // cap. A start the run has made and that has not claimed yet is not
        // among these (its last word is [`STARTED_BY`]), so it still counts.
        .filter(|m| !waits_for_a_seat(store, m, claims))
        .count()
}

/// A member at `doing` with no claim and no seat that the run is to seat: sent
/// back and not started since, ended by a governor and not started since, or
/// silent for [`SILENT_AFTER`]. It holds no place of the group's `xN`.
fn waits_for_a_seat(store: &Store, m: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> bool {
    m.status() == Status::Doing
        && !claims.contains_key(&m.id)
        && (sent_back_unseated(store, m, claims) || despawned_unseated(store, m, claims) || !silent(store, std::slice::from_ref(&m.id)).is_empty())
}

/// Open every leg of `row` the run may start now, each through the seat's gate,
/// and say which. `docs/legs.md`, *The run*.
///
/// **Written under the store lock, with the count taken in the same section**
/// ([`crate::room`], `wsp-231`): each leg is `running` with no seat when the
/// next is asked for, and a leg with no seat counts as an agent starting. So two
/// advances racing find the record and start nothing twice, and a leg that does
/// not fit stays `pending` and is said once in `cycle.log`.
///
/// A leg that is ready, or that was opened with no seat so long ago that the
/// start never arrived ([`WEDGED_AFTER`]), in record order. A row at `doing`
/// for [`Rows::Started`] and at `todo` and unclaimed for [`Rows::New`]; any
/// other status, a blocked row among them, starts nothing.
fn take_legs(
    store: &Store,
    row: &str,
    rows: Rows,
    within: Option<(&[String], Option<usize>)>,
    gate: &crate::room::Gate,
) -> Vec<Opening> {
    let opened = store.locked(|| {
        let mut opened = Vec::new();
        let Some(t) = store.find_task(row) else { return opened };
        if !crate::legs::staged(&t) {
            return opened;
        }
        let claims = store.claims();
        match (t.status(), rows) {
            // A row somebody holds whole opens no leg (`wsp-311`): a seat that
            // stages the rest of its own row adds the legs and then releases,
            // and a leg opened before the release would stand in its tree.
            (Status::Doing, Rows::Started) if !claims.contains_key(row) => {}
            (Status::Todo, Rows::New) if !claims.contains_key(row) => {
                if let Some((members, Some(cap))) = within {
                    if members_going(store, members, row, &claims) >= cap {
                        return opened;
                    }
                }
            }
            _ => return opened,
        }
        let all = crate::legs::legs(&t);
        let lost = |at: &str| util::epoch_secs() - util::epoch_of(at) > WEDGED_AFTER;
        for (name, again) in crate::legs::startable(&all, &lost) {
            let label = crate::legs::label(row, &name);
            // After a start that did not take: its wait is not over, or its
            // tries are spent (`wsp-335`). A start written down and lost is
            // given back too, or it would stand `running` with no seat.
            if crate::starts::held(store, &label) {
                if again {
                    crate::leg_seat::unopen(store, row, &name);
                }
                continue;
            }
            if again && !retry_lost_start(store, row, &label, &format!("leg {name} of {row}"), &format!("`wsp leg reset {row} {name} \"why\"`")) {
                crate::leg_seat::unopen(store, row, &name);
                continue;
            }
            if !gate.admits(&label) {
                continue;
            }
            if again {
                crate::leg_seat::unopen(store, row, &name);
            }
            if crate::leg_seat::open(store, row, &name).is_ok() {
                opened.push(Opening { name, again });
            }
        }
        opened
    });
    if !opened.is_empty() {
        let names: Vec<&str> = opened.iter().map(|o| o.name.as_str()).collect();
        store.git_commit(&format!("wsp: {row} opens leg {}", names.join(", ")));
    }
    opened
}

/// [`take_legs`], and a seat started on each leg it opened. `at` is the list and
/// group these starts belong to, when there is one. Returns the labels started.
fn run_legs(
    store: &Store,
    at: Option<(&Worklist, usize)>,
    row: &str,
    rows: Rows,
    within: Option<(&[String], Option<usize>)>,
    gate: &crate::room::Gate,
) -> Vec<String> {
    let mut started = Vec::new();
    for o in take_legs(store, row, rows, within, gate) {
        let label = crate::legs::label(row, &o.name);
        let by = at.map(|(w, n)| format!("{} group {n}: ", w.id)).unwrap_or_default();
        let again = if o.again { " again: the last start never seated it" } else { "" };
        log_line(store, &format!("{by}starting leg {} of {row}{again}", o.name));
        let owned = at.map(|(w, n)| (w.id.as_str(), n.to_string()));
        match start_leg(store, row, &o.name, owned.as_ref().map(|(l, n)| (*l, n.as_str()))) {
            None => started.push(label),
            Some(why) => {
                log_line(store, &format!("FAILED spawn leg {} of {row}: {why}", o.name));
                leg_start_failed(store, at.map(|(w, _)| w), row, &o.name, &why);
            }
        }
    }
    started
}

/// `wsp spawn <row> --agent --leg <name>`, and its refusal if it refused. The
/// tier is the entry's, which the spawn reads for itself.
fn start_leg(store: &Store, row: &str, leg: &str, owned: Option<(&str, &str)>) -> Option<String> {
    if cfg!(test) {
        #[cfg(test)]
        return tests::SPAWNED.with(|s| {
            let kind = store
                .find_task(row)
                .and_then(|t| crate::legs::legs(&t).into_iter().find(|l| l.name == leg))
                .map(|l| l.policy.kind)
                .unwrap_or_default();
            s.borrow_mut().push((crate::legs::label(row, leg), kind));
            tests::FAIL.with(|f| f.borrow().clone())
        });
    }
    let exe = match wsp_base::util::self_exe() {
        Ok(e) => e,
        Err(e) => return Some(e.to_string()),
    };
    let argv = ["spawn", row, "--agent", "--leg", leg];
    log_line(store, &format!("spawn {}", argv[1..].join(" ")));
    let mut cmd = Command::new(exe);
    cmd.args(argv).stdin(Stdio::null());
    match owned {
        Some((list, group)) => cmd.env(OWNED_LIST, list).env(OWNED_GROUP, group),
        None => cmd.env(RUN_STEP, "1"),
    };
    match cmd.output() {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(why_not(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => Some(e.to_string()),
    }
}

/// The ready legs or pieces of a row nobody's list runs, started through the gate:
/// what `wsp leg done` and `wsp piece done` launch, detached, when they end a part
/// of a row staged and spawned by hand (`docs/legs.md`, *The run*, "Outside a
/// list"; `docs/pieces.md`, *The run*). For pieces it closes what is owed a close
/// and types the review first, as a list's step does. A launch is
/// the run and not a person asking, so it waits for a place like any other start.
/// A row in a list that has not ended is that list's, and is left to it: a
/// running list's step starts it, and a held or parked one starts nothing until
/// it is resumed (`wsp-356`), which is said once here.
pub(crate) fn start_inside(store: &Store, row: &str, seats: &dyn Seats) -> Vec<String> {
    let pieces = store.find_task(row).is_some_and(|t| crate::pieces::staged(&t));
    let parts = if pieces { "pieces" } else { "legs" };
    if let Some(w) = worklist::open_list_of(&store.worklists(), row) {
        if !w.status().is_running() {
            log_line(store, &format!(
                "{row}'s {parts} wait — its list {} is {}; they start when it is resumed",
                w.id,
                w.status().as_str()
            ));
        }
        return Vec::new();
    }
    let gate = crate::room::Gate::new(store, seats);
    let started = if pieces { start_pieces(store, row, &gate) } else { run_legs(store, None, row, Rows::Started, None, &gate) };
    // Said once while it lasts, as a list's step does through `capped.json`
    // (`say_capped`), keyed on the row since there is no list: the repair tick
    // asks every minute and a held leg is the same sentence each time.
    let before = store.capped().get(row).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut now = serde_json::Map::new();
    for (what, room) in gate.take_held() {
        match before.get(&what) {
            Some(since) => {
                now.insert(what, since.clone());
            }
            None => {
                let cap = gate.cap.unwrap_or_default();
                log_line(store, &format!("{what} waits — the seat is at its cap of {cap}{}", gate.cap_said(&what, &room)));
                now.insert(what, serde_json::Value::String(util::now_iso()));
            }
        }
    }
    if now != before {
        store.set_capped(row, (!now.is_empty()).then_some(serde_json::Value::Object(now)));
    }
    started
}

/// A row run as legs that is in no list that has not ended, has started, and has legs
/// ready that nothing is starting: the repair tick's catch for a launch that was
/// lost, and for a leg that was held at the cap when its last neighbour ended.
/// Rows nothing has begun are left alone: staging one is not starting it.
pub(crate) fn legs_owed(store: &Store, seats: &dyn Seats) -> Vec<String> {
    let lists = store.worklists();
    let mut out = Vec::new();
    for t in store.tasks_shared().iter().filter(|t| t.status() == Status::Doing && crate::legs::staged(t)) {
        let all = crate::legs::legs(t);
        let begun = all.iter().any(|l| matches!(l.state, crate::legs::State::Done | crate::legs::State::Running));
        if !begun || worklist::open_list_of(&lists, &t.id).is_some() {
            continue;
        }
        out.extend(start_inside(store, &t.id, seats));
    }
    out
}

/// Whether a tick ends this seat now (`wsp-092`): one not tried yet, or one
/// whose ending failed while the seat is still there. Until 2026-11-18 a failure
/// was never tried again, and `cpd-1025`, whose end had lost a race with its own
/// last hook, kept wsp-076's claim and its queued rework leg for nine minutes,
/// until a person ended it by hand and it went at once.
///
/// **Not a seat positively gone** ([`Seats::absent`]): its pane closed and what
/// failed was a pid that outlived it, which another despawn does not change.
/// A backend that cannot be reached is not "gone", so that seat is tried again.
pub(crate) fn end_due(ending: &crate::verification::Ending, seats: &dyn Seats, seat: &str) -> bool {
    use crate::verification::Ending;
    match ending {
        Ending::Standing => true,
        Ending::Failed(_) => !seats.absent(seat),
        Ending::Ended => false,
    }
}

/// Whether an ending is worth writing on its entry: not a failure after a
/// failure, which is the same answer and would be a commit every tick.
fn end_changed(was: &crate::verification::Ending, now: &crate::verification::Ending) -> bool {
    use crate::verification::Ending;
    !matches!((was, now), (Ending::Failed(_), Ending::Failed(_)))
}

/// What a log line of an ending adds when it was a retry's.
fn again(was: &crate::verification::Ending) -> &'static str {
    match was {
        crate::verification::Ending::Failed(_) => ", on a later try",
        _ => "",
    }
}

/// How a failed ending's log line ends: what happens next.
const TRIED_AGAIN: &str = "tried again each tick while the seat is there, and not said again";

/// How a failed ending's tell ends.
fn told_again(seat: &str) -> String {
    format!(
        "It is tried again each tick, quietly, while the seat is there — \
         `wsp despawn --pane {seat}` if it will not go, and by pid if a process outlived it."
    )
}

/// Every leg that is done and still seated, ended — once each, and never in a
/// turn. The tick's half of ending a leg, as [`passes_finished`] is a verifier's:
/// `wsp leg done` is the agent's own last act, and ending the seat inside that
/// turn cuts it short. The outcome is written on the entry before anything can
/// ask again, `ended` or `end failed: …`, and a failure is tried again on the
/// next tick while the seat is still there ([`end_due`]), told once.
///
/// **From `/`** ([`despawn_seat`]), so the tree a sibling stands in is not kept
/// because the ending process is standing in it. Returns the seats it ended.
pub(crate) fn legs_finished(store: &Store, seats: &dyn Seats) -> Vec<String> {
    use crate::verification::Ending;
    let mut ended = Vec::new();
    for t in store.tasks_shared().iter().filter(|t| crate::legs::staged(t)) {
        for l in crate::legs::legs(t) {
            let Some(seat) = l.pane.clone() else { continue };
            if l.state != crate::legs::State::Done || !end_due(&l.ending, seats, &seat) {
                continue;
            }
            if seats.state(&seat).is_some_and(|s| s.turn_in_flight()) {
                continue;
            }
            let pids = match crate::ending::verifier_pids(seats, &seat) {
                Ok(pids) => pids,
                Err(why) => {
                    crate::ending::say_leg_left(store, &t.id, &l.name, &seat, &why);
                    continue;
                }
            };
            let outcome = despawn_seat(&seat).and_then(|()| crate::ending::confirm_gone(seats, &pids));
            let ending = match &outcome {
                Ok(()) => Ending::Ended,
                Err(why) => Ending::Failed(why.clone()),
            };
            let wrote = end_changed(&l.ending, &ending) && store.locked(|| {
                let Some(mut row) = store.find_task(&t.id) else { return false };
                let mut all = crate::legs::legs(&row);
                let Some(q) = all.iter_mut().find(|q| q.name == l.name && q.pane.as_deref() == Some(seat.as_str())) else { return false };
                q.ending = ending.clone();
                crate::legs::write(&mut row, &all);
                store.save_task(&row).is_ok()
            });
            if wrote {
                store.git_commit(&format!("wsp: {}'s leg {} in {seat} {}", t.id, l.name, if outcome.is_ok() { "ended" } else { "would not end" }));
            }
            match outcome {
                Ok(()) => {
                    log_line(store, &format!("ended {seat}, which ran leg {} of {}{}", l.name, t.id, again(&l.ending)));
                    // A claim on the row in this seat's name goes with it
                    // (`wsp-334`). A leg claims nothing, but a manager's fold
                    // of the seat's start wrote one before it learned not to,
                    // and a row someone holds opens no leg. Left in place, it
                    // keeps the next leg waiting on a seat that is gone.
                    let stale = store.locked(|| {
                        let named = store.claims().get(&t.id).and_then(|c| c.get("workspace_id")?.as_str().map(|w| w == seat)).unwrap_or(false);
                        named && store.clear_claim(&t.id)
                    });
                    if stale {
                        log_line(store, &format!("{}: the claim in {seat}'s name went with it — a leg claims nothing", t.id));
                    }
                    ended.push(seat);
                }
                Err(_) if l.ending != Ending::Standing => {}
                Err(why) => {
                    log_line(store, &format!("could not end {seat}, which ran leg {} of {}: {why} — {TRIED_AGAIN}", l.name, t.id));
                    if let Some(w) = list_of(store, t) {
                        tell(store, &w, &format!(
                            "wsp could not end leg {} of {} in {seat}: {}. {}",
                            l.name, t.id, util::truncate(&why, 200), told_again(&seat)
                        ));
                    }
                }
            }
        }
    }
    ended
}

// ---- pieces -----------------------------------------------------------------------

/// A piece opened by [`take_pieces`]: its number, and whether it is a start taken
/// again because the last one never seated it.
struct PieceOpening {
    n: usize,
    again: bool,
}

/// A piece opened with no seat so long ago that the start never arrived.
fn piece_lost(p: &crate::pieces::Piece) -> bool {
    p.state == crate::pieces::State::Running
        && p.pane.is_none()
        && p.started.as_deref().is_none_or(|at| util::epoch_secs() - util::epoch_of(at) > WEDGED_AFTER)
}

/// Open the pieces of `row` the run may start now: `docs/pieces.md`, *The run*.
/// [`take_legs`]' rules, with the batch for the graph:
///
/// - **The lowest open batch only**, and only once every batch below it is
///   closed ([`crate::pieces::close_owed`]), its pending pieces in record order.
/// - **Under the row's `cap:`**, counting its pieces already running (a seat, or
///   a start still in flight), and then **through the gate**.
/// - **Each opening written under the store lock** with the count, so two
///   advances racing find the record and start nothing twice.
/// - **None while a seat holds the row's claim**: a seat that plans its own row
///   adds the pieces and then releases.
fn take_pieces(
    store: &Store,
    row: &str,
    rows: Rows,
    within: Option<(&[String], Option<usize>)>,
    gate: &crate::room::Gate,
) -> Vec<PieceOpening> {
    let opened = store.locked(|| {
        let mut opened = Vec::new();
        let Some(t) = store.find_task(row) else { return opened };
        if !crate::pieces::staged(&t) {
            return opened;
        }
        let claims = store.claims();
        match (t.status(), rows) {
            (Status::Doing, Rows::Started) if !claims.contains_key(row) => {}
            // A planned row starts with its first batch, and no seat.
            (Status::Todo, Rows::New) if !claims.contains_key(row) => {
                if let Some((members, Some(cap))) = within {
                    if members_going(store, members, row, &claims) >= cap {
                        return opened;
                    }
                }
            }
            _ => return opened,
        }
        let rec = crate::pieces::record(&t);
        if crate::pieces::close_owed(&rec).is_some() {
            return opened;
        }
        let Some(batch) = crate::piece_seat::lowest_open(&rec) else { return opened };
        let ours: Vec<&crate::pieces::Piece> = rec.pieces.iter().filter(|p| p.batch == batch).collect();
        let mut running = ours.iter().filter(|p| p.state == crate::pieces::State::Running && !piece_lost(p)).count();
        let cap = rec.cap.max(1);
        for p in ours {
            let again = match p.state {
                crate::pieces::State::Pending => false,
                crate::pieces::State::Running if piece_lost(p) => true,
                _ => continue,
            };
            if running >= cap {
                // Held by the row's own cap, which says nothing: the next piece
                // to end opens the next one.
                if again {
                    crate::piece_seat::unopen(store, row, p.n);
                }
                continue;
            }
            let label = p.id(row);
            if crate::starts::held(store, &label) {
                if again {
                    crate::piece_seat::unopen(store, row, p.n);
                }
                continue;
            }
            if again && !retry_lost_start(store, row, &label, &format!("piece {} of {row}", p.n), &piece_remedy(row, p.n)) {
                crate::piece_seat::unopen(store, row, p.n);
                continue;
            }
            if !gate.admits(&label) {
                continue;
            }
            if again {
                crate::piece_seat::unopen(store, row, p.n);
            }
            if crate::piece_seat::open(store, row, p.n).is_ok() {
                opened.push(PieceOpening { n: p.n, again });
                running += 1;
            }
        }
        opened
    });
    if !opened.is_empty() {
        let ns: Vec<String> = opened.iter().map(|o| o.n.to_string()).collect();
        store.git_commit(&format!("wsp: {row} opens piece {}", ns.join(", ")));
    }
    opened
}

/// What a person types to start a piece whose tries ran out.
fn piece_remedy(row: &str, n: usize) -> String {
    format!("`wsp spawn {row} --agent --piece {n}` starts it by hand once the cause is cleared")
}

/// The run's whole step on a row with pieces: close the batch that is owed a
/// close, type the review once every batch is closed, then open what is ready and
/// start a seat on each. `at` is the list and group these belong to, when there is
/// one. Returns the labels started.
fn run_pieces(
    store: &Store,
    at: Option<(&Worklist, usize)>,
    row: &str,
    rows: Rows,
    within: Option<(&[String], Option<usize>)>,
    gate: &crate::room::Gate,
) -> Vec<String> {
    if rows == Rows::Started {
        close_batches(store, at, row);
    }
    let mut started = Vec::new();
    for o in take_pieces(store, row, rows, within, gate) {
        let label = crate::pieces::Piece::pending(o.n, "", 0).id(row);
        let by = at.map(|(w, n)| format!("{} group {n}: ", w.id)).unwrap_or_default();
        let again = if o.again { " again: the last start never seated it" } else { "" };
        log_line(store, &format!("{by}starting piece {} of {row}{again}", o.n));
        let owned = at.map(|(w, n)| (w.id.as_str(), n.to_string()));
        match start_piece(store, row, o.n, owned.as_ref().map(|(l, n)| (*l, n.as_str()))) {
            None => started.push(label),
            Some(why) => {
                log_line(store, &format!("FAILED spawn piece {} of {row}: {why}", o.n));
                piece_start_failed(store, at.map(|(w, _)| w), row, o.n, &why);
            }
        }
    }
    started
}

/// `wsp spawn <row> --agent --piece <n>`, and its refusal if it refused. The tier
/// is the entry's, which the spawn reads for itself.
fn start_piece(store: &Store, row: &str, n: usize, owned: Option<(&str, &str)>) -> Option<String> {
    if cfg!(test) {
        #[cfg(test)]
        return tests::SPAWNED.with(|s| {
            let kind = store
                .find_task(row)
                .and_then(|t| crate::pieces::record(&t).pieces.into_iter().find(|p| p.n == n))
                .map(|p| p.policy.kind)
                .unwrap_or_default();
            s.borrow_mut().push((crate::pieces::Piece::pending(n, "", 0).id(row), kind));
            tests::FAIL.with(|f| f.borrow().clone())
        });
    }
    let exe = match wsp_base::util::self_exe() {
        Ok(e) => e,
        Err(e) => return Some(e.to_string()),
    };
    let n = n.to_string();
    let argv = ["spawn", row, "--agent", "--piece", n.as_str()];
    log_line(store, &format!("spawn {}", argv[1..].join(" ")));
    let mut cmd = Command::new(exe);
    cmd.args(argv).stdin(Stdio::null());
    match owned {
        Some((list, group)) => cmd.env(OWNED_LIST, list).env(OWNED_GROUP, group),
        None => cmd.env(RUN_STEP, "1"),
    };
    match cmd.output() {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(why_not(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => Some(e.to_string()),
    }
}

/// A piece's start failed, or its seat never came up: [`leg_start_failed`]'s rule
/// for a piece. The seat is ended, the entry goes back to pending with no seat on
/// it ([`crate::piece_seat::reset`]), and it is started again on the backoff, keyed
/// by the piece's id in `starts.json` — or the person is flagged, on the last try.
pub(crate) fn piece_start_failed(store: &Store, w: Option<&Worklist>, row: &str, n: usize, why: &str) {
    let label = crate::pieces::Piece::pending(n, "", 0).id(row);
    let seat = store
        .find_task(row)
        .and_then(|t| crate::pieces::record(&t).pieces.into_iter().find(|p| p.n == n))
        .and_then(|p| p.pane);
    let ended = match &seat {
        Some(seat) => match despawn_seat(seat) {
            Ok(()) => format!("{seat} was ended"),
            Err(e) => format!("{seat} could not be ended ({e}): `wsp despawn --pane {seat}`"),
        },
        None => "it had no seat to end".to_string(),
    };
    crate::piece_seat::reset(store, row, n);
    let m = crate::starts::missed(store, &label);
    let why = util::truncate(why, 300);
    let wait = m.again_in.map_or_else(|| "it is not tried again".to_string(), |s| format!("it starts again in {}", crate::barrier::span(s)));
    store.locked(|| {
        if let Some(mut t) = store.find_task(row) {
            let _ = crate::log::note(store, &mut t, &format!("wsp could not spawn piece {n} (try {} of {}): {why}", m.tries, crate::barrier::TRIES));
        }
    });
    log_line(store, &format!("{label}: its start did not take on try {} of {} — {ended}; {wait}", m.tries, crate::barrier::TRIES));
    if m.last {
        start_gave_up(store, w, row, &format!("piece {n} of {row}"), m.tries, &why, &piece_remedy(row, n));
    } else if let Some(w) = w {
        tell(store, w, &format!(
            "piece {n} of {row} did not start in the {} run (try {} of {}): {why}. \
             wsp ended the seat it left and {wait}.",
            w.id, m.tries, crate::barrier::TRIES
        ));
    }
}

/// [`crate::piece_close::close`], which a test answers for itself.
fn close_batch(store: &Store, row: &str, batch: usize) -> crate::piece_close::Closed {
    #[cfg(test)]
    {
        let _ = store;
        tests::CLOSED.with(|c| c.borrow_mut().push(format!("{row} batch {batch}")));
        let answer = tests::CLOSE_AS.with(|f| f.get());
        return match answer {
            Some(f) => f(store, row, batch),
            None => crate::piece_close::Closed::Clean { merged: Vec::new(), ran: 0, reused: 0, removed: Vec::new(), kept: Vec::new() },
        };
    }
    #[cfg(not(test))]
    crate::piece_close::close(store, row, batch)
}

/// What a close came to, as the line the row's log keeps; `None` for a close that
/// did not happen (the batch is open, or the close was held), which is tried again.
fn closed_line(batch: usize, c: &crate::piece_close::Closed) -> Option<String> {
    use crate::piece_close::Closed;
    let list = |ns: &[usize]| ns.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
    Some(match c {
        Closed::Open(_) | Closed::Held(_) => return None,
        Closed::Clean { merged, ran, reused, kept, .. } => format!(
            "batch {batch} closed clean{} · {ran} check(s) run, {reused} reused{}",
            if merged.is_empty() { String::new() } else { format!(" · merged piece {}", list(merged)) },
            if kept.is_empty() { String::new() } else { format!(" · kept {}", kept.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(" ")) }
        ),
        Closed::Conflict { added: 0, .. } | Closed::Red { added: 0, .. } => return None,
        Closed::Conflict { files, added, .. } => format!(
            "batch {batch} did not merge ({}) — piece {added}, merge batch {batch}, is filed in batch {} and opens next",
            util::truncate(&files.join(" "), 200),
            batch + 1
        ),
        Closed::Red { step, piece, added } => format!(
            "batch {batch} is red on its merged tip at {step} (piece {piece}'s line) — piece {added}, fix batch {batch}, is filed in batch {} and opens next",
            batch + 1
        ),
    })
}

/// Close every batch of `row` that is owed a close, lowest first, each written
/// `closed:` on the record with its line on the log, under the lock; then type the
/// row's review when that was the last. A close that is held or did not happen is
/// said and tried again at the next step. Nothing while a seat holds the row.
fn close_batches(store: &Store, at: Option<(&Worklist, usize)>, row: &str) {
    let by = at.map(|(w, n)| format!("{} group {n}: ", w.id)).unwrap_or_default();
    // Bounded by the batches there are: each pass closes one or stops.
    for _ in 0..=crate::pieces::MAX_PIECES {
        let Some(t) = store.find_task(row) else { return };
        if t.status() != Status::Doing || !crate::pieces::staged(&t) || store.claims().contains_key(row) {
            return;
        }
        let Some(batch) = crate::pieces::close_owed(&crate::pieces::record(&t)) else { break };
        let closed = close_batch(store, row, batch);
        let Some(line) = closed_line(batch, &closed) else {
            let why = match &closed {
                crate::piece_close::Closed::Open(w) | crate::piece_close::Closed::Held(w) => w.clone(),
                _ => "its merge or fix piece could not be filed".to_string(),
            };
            log_line(store, &format!("{by}{row}: batch {batch} waits to close — {why}"));
            return;
        };
        let wrote = store.locked(|| {
            let Some(mut t) = store.find_task(row) else { return false };
            let mut rec = crate::pieces::record(&t);
            if rec.closed.contains(&batch) {
                return false;
            }
            rec.closed.push(batch);
            crate::pieces::write(&mut t, &rec);
            t.log(&line);
            store.save_task(&t).is_ok()
        });
        if !wrote {
            return;
        }
        store.git_commit(&format!("wsp: {row} batch {batch} closed"));
        log_line(store, &format!("{by}{row}: {line}"));
    }
    finish_pieces(store, at, row);
}

/// The mark the run's review of a row with pieces leaves on its log before the
/// verb runs, so a second advance does not type it again.
const PIECES_REVIEWING: &str = "wsp: every batch of its pieces is closed — the run types the review";

/// How long a review the run began is left to finish before it is typed again.
const REVIEW_TAKES: i64 = 5 * 60;

/// Every batch closed clean and nothing left to run: the run types the row's
/// review, with the pieces table and captions as the account (`docs/pieces.md`,
/// *The run*). Once: the mark is written under the lock first, and a second
/// advance finds it. A row with no caption is given its title's.
fn finish_pieces(store: &Store, at: Option<(&Worklist, usize)>, row: &str) {
    let account = store.locked(|| {
        let mut t = store.find_task(row)?;
        if t.status() != Status::Doing || store.claims().contains_key(row) {
            return None;
        }
        let rec = crate::pieces::record(&t);
        if !crate::pieces::finished(&rec) {
            return None;
        }
        let log = t.section("Log").unwrap_or_default();
        let marked = log.lines().rev().find(|l| l.contains(PIECES_REVIEWING) || l.contains(SENT_BACK) || l.contains("→ doing"));
        if let Some(l) = marked.filter(|l| l.contains(PIECES_REVIEWING)) {
            let stamp = l.trim_start_matches("- ").split_whitespace().next().unwrap_or_default();
            if util::epoch_secs() - util::epoch_of(stamp) < REVIEW_TAKES {
                return None;
            }
        }
        if t.caption.as_deref().is_none_or(|c| c.trim().is_empty()) {
            t.caption = Some(util::truncate(&crate::caption::title(&t.title), crate::caption::LIMIT));
        }
        t.log(PIECES_REVIEWING);
        store.save_task(&t).ok()?;
        let done = rec.pieces.iter().filter(|p| p.state == crate::pieces::State::Done).count();
        Some(format!(
            "Every batch of {row}'s pieces closed clean: {done} piece(s) done in {} batch(es). The run typed this review; the pieces are the account.\n\n{}",
            rec.closed.len(),
            crate::pieces::table(&rec)
        ))
    });
    let Some(account) = account else { return };
    store.git_commit(&format!("wsp: {row}'s pieces are all closed"));
    let by = at.map(|(w, n)| format!("{} group {n}: ", w.id)).unwrap_or_default();
    let code = crate::cmd_task::review(store, &Args::synth("review", &[row, &account], &[]));
    if code == 0 {
        log_line(store, &format!("{by}{row}: every batch closed — the run typed its review"));
    } else {
        log_line(store, &format!("{by}{row}: every batch closed, and the review the run typed was refused (exit {code}); tried again in {}", crate::barrier::span(REVIEW_TAKES)));
    }
}

/// The ready pieces of a row nobody's list runs, closed and started through the
/// gate: what `wsp piece done` launches, detached, outside a list. [`start_inside`]'s
/// half for pieces.
fn start_pieces(store: &Store, row: &str, gate: &crate::room::Gate) -> Vec<String> {
    run_pieces(store, None, row, Rows::Started, None, gate)
}

/// A row with pieces in no list that has not ended, that has begun, and has a
/// close or a piece owed that nothing is starting: the repair tick's catch for a
/// launch that was lost and a piece held at the cap. [`legs_owed`]'s rule.
pub(crate) fn pieces_owed(store: &Store, seats: &dyn Seats) -> Vec<String> {
    let lists = store.worklists();
    let mut out = Vec::new();
    for t in store.tasks_shared().iter().filter(|t| t.status() == Status::Doing && crate::pieces::staged(t)) {
        let rec = crate::pieces::record(t);
        let begun = !rec.closed.is_empty() || rec.pieces.iter().any(|p| matches!(p.state, crate::pieces::State::Done | crate::pieces::State::Running));
        if !begun || worklist::open_list_of(&lists, &t.id).is_some() {
            continue;
        }
        out.extend(start_inside(store, &t.id, seats));
    }
    out
}

/// Every piece that is done and still seated, ended — once each, never in a turn,
/// from `/`: [`legs_finished`]' rule for a piece, the outcome on its entry.
pub(crate) fn pieces_finished(store: &Store, seats: &dyn Seats) -> Vec<String> {
    use crate::verification::Ending;
    let mut ended = Vec::new();
    for t in store.tasks_shared().iter().filter(|t| crate::pieces::staged(t)) {
        for p in crate::pieces::record(t).pieces {
            let Some(seat) = p.pane.clone() else { continue };
            if p.state != crate::pieces::State::Done || !end_due(&p.ending, seats, &seat) {
                continue;
            }
            if seats.state(&seat).is_some_and(|s| s.turn_in_flight()) {
                continue;
            }
            let pids = match crate::ending::verifier_pids(seats, &seat) {
                Ok(pids) => pids,
                Err(why) => {
                    crate::ending::say_part_left(store, &t.id, &format!("piece {}", p.n), &seat, &why);
                    continue;
                }
            };
            let outcome = despawn_seat(&seat).and_then(|()| crate::ending::confirm_gone(seats, &pids));
            let ending = match &outcome {
                Ok(()) => Ending::Ended,
                Err(why) => Ending::Failed(why.clone()),
            };
            let wrote = end_changed(&p.ending, &ending) && store.locked(|| {
                let Some(mut row) = store.find_task(&t.id) else { return false };
                let mut rec = crate::pieces::record(&row);
                let Some(q) = rec.pieces.iter_mut().find(|q| q.n == p.n && q.pane.as_deref() == Some(seat.as_str())) else { return false };
                q.ending = ending.clone();
                crate::pieces::write(&mut row, &rec);
                store.save_task(&row).is_ok()
            });
            if wrote {
                store.git_commit(&format!("wsp: {}'s piece {} in {seat} {}", t.id, p.n, if outcome.is_ok() { "ended" } else { "would not end" }));
            }
            match outcome {
                Ok(()) => {
                    log_line(store, &format!("ended {seat}, which ran piece {} of {}{}", p.n, t.id, again(&p.ending)));
                    ended.push(seat);
                }
                Err(_) if p.ending != Ending::Standing => {}
                Err(why) => {
                    log_line(store, &format!("could not end {seat}, which ran piece {} of {}: {why} — {TRIED_AGAIN}", p.n, t.id));
                    if let Some(w) = list_of(store, t) {
                        tell(store, &w, &format!(
                            "wsp could not end piece {} of {} in {seat}: {}. {}",
                            p.n, t.id, util::truncate(&why, 200), told_again(&seat)
                        ));
                    }
                }
            }
        }
    }
    ended
}

/// The member's newest pass, when it stopped on a problem rather than holding.
///
/// **A block is a verdict somebody has to answer, and the barrier does not open
/// on it** — [`verified`] is `false` for it on purpose. Since `wsp-188` a block
/// also sends the member back, so the run normally waits on the member and not
/// on this; it is still named, because a member whose block was answered by
/// hand — reviewed again with nothing new landed — would otherwise read as one
/// that was never verified at all, which is what hid two of them for hours
/// before `wsp-180`.
pub(crate) fn blocked_verifier(tasks: &[Task], member: &str) -> Option<crate::verification::Pass> {
    crate::verification::blocked(tasks, member)
}

/// Whether a member's newest pass holds.
///
/// The predicate the barrier is gated on, and `wsp-147`'s third repair reads it
/// to say *which* members the barrier is waiting on — a count is the question a
/// reader has and cannot act on, a list of ids is both. Read off the member's
/// own `## Verification` since `wsp-188`, and off a Verify row under it until
/// that row is migrated.
pub(crate) fn verified(tasks: &[Task], member: &str) -> bool {
    crate::verification::verified(tasks, member)
}

/// Open a pass on a member, if one is owed. `Some` is a pass written and
/// waiting for its agent, and names the seat of any pass it superseded.
///
/// Owed when there is none; when the newest **blocked** and the member's work
/// has moved since it was read ([`moved_since`]) — that is the member coming
/// back with a fix, verified again by a fresh agent rather than by the one that
/// already made up its mind — or when it has gone back to review since it was
/// sent back, at the same commit or not ([`reviewed_since_sent_back`]); and
/// when the newest **held** and the member has
/// landed again since — new code on the trunk that nobody has read.
///
/// # A new landing supersedes a pass still reading (`wsp-188`, wsp-176 item 2)
///
/// A pass that is still `running` when the member lands something new is
/// reading a commit that is no longer the member's work. It is marked
/// `superseded`, its seat is handed back to be ended, and a fresh pass opens on
/// the new landing — so there is never more than one verifier on a member, and
/// a late verdict from the old one is refused rather than read as the newest.
///
/// A `running` pass whose agent never arrived — no seat after
/// [`WEDGED_AFTER`] — is taken again in place: its clock restarts so a second
/// advance leaves it to this one. One whose seat was ended without a verdict
/// (a run that closed over it, then resumed) is superseded like any other.
///
/// **And so is one whose seat is `gone` without wsp having ended it**
/// (`wsp-252`, ux-kit group 2). A park closed cpd-615 and cpd-608 under two
/// passes and wrote nothing on them, so both read `running` on panes that no
/// longer existed, this returned `None` for both, and the run, its resume and a
/// `spawn --verify` by hand all waited on two agents that would never answer.
/// The barrier check has had the same reading since `wsp-230`
/// ([`crate::barrier::open`]); this is the verifier's half of it.
///
/// # Keyed on the landing, not on `updated` (`wsp-136` item 1)
///
/// Any write moves `updated` — a governor's `wsp note`, an edit to the
/// overview, a `wsp mv` — and keying on it bought a whole fresh verifier for one
/// sentence about a member. The key is [`crate::repair::landed`], the commit the
/// member's own `## Log` records its work as landed on, written on the pass as
/// `read` when it opens. **A member with no landing falls back to `updated`**,
/// deliberately: design-only work has no repository, and a member whose fix is
/// prose has genuinely changed when it is touched. That is also why a pass's
/// own bookkeeping never touches the member — see [`crate::verification`].
/// Before a pass opens on a ready member (`wsp-341`): its branch rebased onto
/// the trunk in its own tree, and the new head written as a fresh ready line so
/// the checks and the verifier read the rebased commit. `false` when the member
/// is not to have a pass this tick — a verifier is already standing in the tree,
/// or the rebase did not apply and the member was sent back (once: it is at
/// `doing` after, and owes nothing). No agent is spent.
fn prepare_ready(store: &Store, list: &str, at: usize, member: &Task) -> bool {
    use crate::cmd_checkout::Prepared;
    // A by-merge row is not rebased on the local trunk (`wsp-350`): its PR
    // merges into the remote's, and the branch stays as it was pushed.
    if crate::repair::ready(member).is_none() || crate::lands::by_merge(store, &member.id) {
        return true;
    }
    let tasks = store.tasks_shared();
    if crate::verification::all(&tasks, member).last().is_some_and(|p| p.state == crate::verification::State::Running) {
        return true;
    }
    // One check per review tip (`wsp-392`): a rebase while the check runs moves
    // the tip and starts a second, overlapping check. It waits for the first.
    let before = crate::repair::ready(member);
    if before.as_deref().is_some_and(|sha| crate::check::running_at(store, member, sha)) {
        return true;
    }
    match crate::cmd_checkout::rebase_ready(store, &member.id) {
        Prepared::Unchanged => true,
        Prepared::Rebased { head, branch } => {
            store.locked(|| {
                if let Some(mut m) = store.find_task(&member.id) {
                    m.log(&format!("{} {head} on {branch}", crate::repair::READY));
                    if let Some(from) = &before {
                        crate::check::carry_pass(&mut m, from, &head);
                    }
                    let _ = store.save_task(&m);
                }
            });
            log_line(store, &format!("{list} group {at}: {} rebased onto the trunk, ready at {}", member.id, &head[..head.len().min(12)]));
            store.git_commit(&format!("wsp: {} rebased onto the trunk before its verifier", member.id));
            true
        }
        Prepared::Refused(text) => {
            let read = crate::repair::ready(member);
            store.locked(|| {
                if let Some(mut m) = store.find_task(&member.id) {
                    crate::verification::send_back(&mut m, util::now_iso(), "wsp's rebase", read.as_deref(), &text, None);
                    let _ = store.save_task(&m);
                }
            });
            crate::verification::deliver_unless_reworked(store, &member.id, &text);
            log_line(store, &format!("{list} group {at}: {} sent back — {}", member.id, crate::util::fold(&text)));
            store.git_commit(&format!("wsp: {} sent back — the rebase onto the trunk does not apply", member.id));
            false
        }
    }
}

/// The land a hold owes (`wsp-605`), tried again when the member's tree has
/// turned clean. A dirty tree waits silently: the member and the governor were
/// told when the land was refused, and `wsp review` tries it again on demand.
pub(crate) fn retry_owed_land(store: &Store, list: &str, at: usize, member: &Task) {
    let Some(tree) = crate::cmd_checkout::member_tree(store, &member.id) else { return };
    if crate::cmd_checkout::uncommitted(&tree) > 0 {
        return;
    }
    let said = match crate::verification::retry_owed(store, &member.id, "the tree is clean") {
        Some(crate::verification::Owed::Landed(Some(sha))) => format!("the hold's owed land landed at {}", &sha[..sha.len().min(12)]),
        Some(crate::verification::Owed::Landed(None)) => "the hold's owed land had nothing left to land".into(),
        Some(crate::verification::Owed::Still(why)) => format!("the hold's land is still owed: {}", util::fold(&why)),
        Some(crate::verification::Owed::ByMerge(why)) => format!("the hold's land is not wsp's: {}", util::fold(&why)),
        Some(crate::verification::Owed::Blocked(why)) => format!("the hold's owed land was refused, sent back: {}", util::fold(&why)),
        None => return,
    };
    log_line(store, &format!("{list} group {at}: {} {said}", member.id));
}

/// `gone` is a seat whose agent went without wsp; `absent` is one whose pane has
/// closed too. Only the second has nothing left to end (`wsp-085`): on
/// m1-fixes-3 wsp-080's verifier cpd-419 read `Gone` after one turn, its pass was
/// written `ended` though its pane stood on in the member's tree, and so no
/// ending ever came for it — not the next verifier's, not the list's close,
/// which found it "standing in" wsp-080's tree. A seat still standing is
/// superseded instead, and ended at once like any other.
pub(crate) fn open_verifier(
    store: &Store,
    member: &Task,
    on: &Policy,
    gone: &dyn Fn(&str) -> bool,
    absent: &dyn Fn(&str) -> bool,
    admit: &dyn Fn() -> bool,
) -> Option<Opened> {
    use crate::verification::{self as v, Ending, State};
    let now_landing = crate::repair::commit_of(member);
    let landed_again = |p: &v::Pass| match (&now_landing, p.own()) {
        (Some(now), Some(then)) => !v::same_commit(now, then) && !p.landed.as_deref().is_some_and(|l| v::same_commit(now, l)),
        _ => false,
    };
    let made = store.locked(|| {
        let tasks = store.tasks_shared();
        let mut m = store.find_task(&member.id)?;
        let mut own = v::passes(&m);
        let mut superseded = None;
        let mut lost = None;
        match v::all(&tasks, &m).last() {
            None => {}
            // A Verify row still open is the old shape's own to finish.
            Some(p) if p.state == State::Running && p.was.is_some() => return None,
            Some(p) if p.state == State::Running => {
                let i = own.len().checked_sub(1)?;
                let ended = own[i].ending != Ending::Standing;
                if own[i].pane.is_none() && util::epoch_secs() - util::epoch_of(&own[i].at) > WEDGED_AFTER {
                    if !admit() {
                        return None;
                    }
                    own[i].at = util::now_iso();
                    v::write(&mut m, &own);
                    store.save_task(&m).ok()?;
                    return Some(Opened { superseded: None, again: true, lost: None });
                }
                let dead = !ended && own[i].pane.as_deref().is_some_and(gone);
                if !ended && !dead && !landed_again(p) {
                    return None;
                }
                own[i].state = State::Superseded;
                // A dead agent in a pane that still stands is ended like a
                // superseded one; only a closed pane has nothing left to end.
                let closed = dead && own[i].pane.as_deref().is_some_and(absent);
                if dead {
                    lost = own[i].pane.clone();
                }
                if closed {
                    own[i].ending = Ending::Ended;
                }
                superseded = own[i].pane.clone().filter(|_| !ended && !closed);
            }
            Some(p) if p.state == State::Holds => {
                if !landed_again(p) {
                    return None;
                }
            }
            Some(p) if p.state == State::Blocks => {
                // **Never at a commit it has already seen blocked (`wsp-529`).**
                // A second verifier on the same commit spends a turn to say it
                // has not changed (wsp-526). A seat that disagrees says so with
                // `wsp review --anyway`, and the governor is told the block is
                // unanswered ([`answered_block`]).
                let at_blocked = matches!((&now_landing, p.own()), (Some(n), Some(r)) if v::same_commit(n, r));
                if at_blocked || (!moved_since(&now_landing, &member.updated, p) && !reviewed_since_sent_back(&m)) {
                    return None;
                }
            }
            // **A pass that could not verify is no verdict on the work**
            // (`wsp-614`, `cause: env`): the member stayed at review, so the
            // run owes it a new verifier now, at the same commit or not.
            Some(p) if p.state == State::CouldNotVerify => {}
            // The newest was superseded and nothing opened after it — a start
            // that failed — so this is the fresh pass it was waiting for.
            Some(_) => {}
        }
        // Owed, and only now asked whether the seat has room (`wsp-231`): a
        // pass the cap holds back is not written, so the next step finds it
        // owed again rather than in flight.
        if !admit() {
            return None;
        }
        own.push(v::Pass::opened(now_landing.clone(), Some(on.label())));
        v::write(&mut m, &own);
        store.save_task(&m).ok()?;
        Some(Opened { superseded, again: false, lost })
    })?;
    if !made.again {
        read_pr_head(store, &member.id);
    }
    let why = match (&made.superseded, made.again) {
        (_, true) => "again: the last start never arrived".to_string(),
        (_, _) if made.lost.is_some() => format!("the pass in {} lost its seat", made.lost.as_deref().unwrap_or_default()),
        (Some(seat), _) => format!("a new landing supersedes the pass in {seat}"),
        (None, _) => "a pass opens".to_string(),
    };
    store.git_commit(&format!("wsp: verify {} — {why}", member.id));
    Some(made)
}

/// A by-merge row's new pass reads the PR's head (`wsp-350`), not the ready
/// commit. Asked outside the lock, since it calls `gh` and fetches.
fn read_pr_head(store: &Store, id: &str) {
    if !crate::lands::by_merge(store, id) {
        return;
    }
    let Some(member) = store.find_task(id) else { return };
    let Some(ready) = crate::repair::ready(&member) else { return };
    let Some(head) = crate::by_merge::pr_head(store, &member, &ready) else { return };
    store.locked(|| {
        let mut m = store.find_task(id)?;
        let mut own = crate::verification::passes(&m);
        let pass = own.last_mut().filter(|p| p.state == crate::verification::State::Running)?;
        if !crate::verification::same_commit(&head.sha, &ready) {
            pass.ready = pass.read.take();
            pass.read = Some(head.sha.chars().take(12).collect());
        }
        pass.from = head.from.clone();
        crate::verification::write(&mut m, &own);
        store.save_task(&m).ok()
    });
}

/// What [`open_verifier`] did: a fresh pass, or a wedged one taken again, and
/// the seat of a pass it superseded, which is ended at once.
pub(crate) struct Opened {
    superseded: Option<String>,
    again: bool,
    /// The seat of a running pass that went without wsp ending it.
    lost: Option<String>,
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
fn moved_since(now_landing: &Option<String>, member_updated: &str, pass: &crate::verification::Pass) -> bool {
    match (now_landing, pass.own()) {
        // The commit a hold landed is the work it read, rebased.
        (Some(now), Some(then)) => {
            !crate::verification::same_commit(now, then) && !pass.landed.as_deref().is_some_and(|l| crate::verification::same_commit(now, l))
        }
        // Against the verdict and not the opening: the block itself moves the
        // member to `doing`, and that is not the member moving. `decided` is
        // the instant the verdict was recorded (`wsp-193`), and the block's
        // own write carries the same one.
        _ => member_updated > pass.decided.as_deref().unwrap_or(pass.at.as_str()),
    }
}

/// Whether the member went back to review after it was last sent back — a
/// `review:` entry in its `## Log` below the newest [`SENT_BACK`] one.
///
/// **What a block owes need not be a commit (`wsp-212`).** wsp-209 and wsp-217
/// were each blocked for an install and a few stuck rows, did them, and
/// returned to review at the commit the verifier had read. [`moved_since`]
/// asks only whether the work moved, so no verifier came, and the barrier
/// waited for ever on a verdict about a state of the row that no longer held.
/// The verdict is about the row, so a return to review is a return to be
/// verified.
///
/// **Order in the log, not instants.** `## Log` is dated by day, and the
/// order is the one thing it records exactly. And it is one verifier per
/// return: the pass this opens is the newest until its verdict, and a block
/// writes a fresh `sent back:` below the `review:` it answered.
///
/// A member put at review by hand, with no account, has not *returned* — it
/// has been moved — and stays what the barrier names and the seat is told.
fn reviewed_since_sent_back(t: &Task) -> bool {
    let log = t.section("Log").unwrap_or_default();
    let heads: Vec<&str> = log
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|l| match l.split_once(' ') {
            Some((stamp, said)) if util::is_stamp(stamp) => said,
            _ => l,
        })
        .collect();
    let Some(back) = heads.iter().rposition(|h| h.starts_with(SENT_BACK)) else { return false };
    heads[back + 1..].iter().any(|h| h.starts_with("review:") || *h == "→ review")
}

/// Whether a member's block has been answered: it has landed something new
/// since the verifier read, or gone back to review since it was sent back
/// (`wsp-334`). A verifier is then owed on the new state, and the run must not
/// tell its governor that nothing has answered the verdict.
///
/// **The commits only, never `updated`**: a governor's note moves the member
/// and answers nothing.
pub(crate) fn answered_block(t: &Task) -> bool {
    let landed_again = match (crate::repair::commit_of(t), crate::verification::passes(t).last().and_then(|p| p.own().map(str::to_string))) {
        (Some(now), Some(then)) => !crate::verification::same_commit(&now, &then),
        _ => false,
    };
    landed_again || (reviewed_since_sent_back(t) && blocked_at_tip(t).is_none())
}

/// The newest pass on the member when it is a block read at the commit the
/// member is at now: nothing has changed since (`wsp-529`). A return to review
/// there owes no verifier — the run has seen this commit blocked — and the
/// block stays unanswered, which is what the governor is told.
///
/// `None` when either side has no commit (design-only work), which falls back
/// to the older reading of a return to review.
pub(crate) fn blocked_at_tip(t: &Task) -> Option<crate::verification::Pass> {
    let now = crate::repair::commit_of(t)?;
    let pass = crate::verification::passes(t).pop()?;
    let read = pass.own()?;
    (pass.state == crate::verification::State::Blocks && crate::verification::same_commit(&now, read)).then_some(pass)
}

/// A verdict's first lines: what a seat is shown of it where the whole is not
/// wanted, the blocking sentence and what follows it.
pub(crate) fn verdict_head(text: &str) -> String {
    let head: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).take(6).collect();
    util::truncate(&head.join("\n"), 700)
}

/// What a seat started on a sent-back row is told in its first message (`wsp-529`):
/// the reason, the verdict's head, and that a review of an unchanged blocked
/// commit is refused. Read off the row, so it needs no tell that can fail
/// (`wsp-526`: every tell of the reason failed while the seat sat idle).
///
/// The newest `sent back:` with no `review:` below it, and not "the newest log
/// line", because the claim that seats the agent writes its own line first.
/// `None` for a row that was not sent back, or has gone back to review since.
/// One ASCII line, as every work order is.
pub(crate) fn sent_back_order(t: &Task) -> Option<String> {
    if t.status() != Status::Doing {
        return None;
    }
    let log = t.section("Log").unwrap_or_default();
    let heads: Vec<&str> = log
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|l| match l.split_once(' ') {
            Some((stamp, said)) if util::is_stamp(stamp) => said,
            _ => l,
        })
        .collect();
    let back = heads.iter().rposition(|h| h.starts_with(SENT_BACK))?;
    if heads[back + 1..].iter().any(|h| h.starts_with("review:") || *h == "→ review") {
        return None;
    }
    let reason = heads[back].trim_start_matches(SENT_BACK).trim();
    if reason.starts_with(ANSWERED_BY) {
        // Not a verdict: the member asked, and was answered (`wsp-635`).
        return Some(ascii_line(&format!(
            " {id} was blocked on a question and the run reopened it, so it is yours to carry on: {reason}. \
             The decision is under ## Decisions (`wsp show {id}`). Act on it, commit, and `wsp review {id}` when done.",
            id = t.id,
        )));
    }
    let verdict = crate::verification::passes(t)
        .pop()
        .filter(|p| p.state == crate::verification::State::Blocks && !p.text.trim().is_empty())
        .map(|p| format!(" The verdict begins: {}", util::fold(&verdict_head(&p.text))))
        .unwrap_or_default();
    let text = format!(
        " {id} was sent back to doing, so it is yours to fix: {reason}.{verdict} The whole verdict is under ## Verification (`wsp show {id}`). \
         Fix what it found and commit; `wsp review {id}` is refused at a commit a verifier already blocked, unchanged.",
        id = t.id,
    );
    Some(ascii_line(&text))
}

fn ascii_line(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2014}' | '\u{2013}' => "-".to_string(),
            '\u{2018}' | '\u{2019}' => "'".to_string(),
            '\u{201c}' | '\u{201d}' => "\"".to_string(),
            '\u{2192}' => "->".to_string(),
            c if c.is_ascii() && c != '\n' => c.to_string(),
            _ => " ".to_string(),
        })
        .collect()
}

/// How a barrier row's title starts; the list's slug follows it.
pub(crate) const BARRIER_TITLE: &str = "Barrier: ";

pub(crate) fn barrier_title(list: &str, at: usize) -> String {
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
pub(crate) fn is_barrier(t: &Task, list: &str, at: usize) -> bool {
    if !t.tags.iter().any(|g| g == BARRIER_TAG) {
        return false;
    }
    let title = barrier_title(list, at);
    // **The exact title, or the exact title and a serial.** A bare
    // `starts_with` answers true for "Barrier: run group 11" when asked about
    // group 1, because "group 1" is a prefix of "group 11" — found by wsp-167,
    // and unreachable today only because the run walks its groups in order. It
    // would bite the first eleven-group list, in `open_barrier` (which would
    // find group 11's check as group 1's), in `end_group` (which would end
    // group 11's check when group 1 passes) and in `sent_back_holding`.
    //
    // The serial is the only thing a re-check's title carries, and it is
    // written by this module — so the whole set of spellings a barrier row can
    // have is the one word here.
    t.title == title || t.title.starts_with(&format!("{title} (recheck"))
}

/// A verifier's work order: what `wsp spawn --verify` hands the agent, and
/// what `wsp brief` shows in its seat. Composed from the member and the run it
/// is in when it is asked, since a verifier has no row to keep it on.
pub(crate) fn verifier_order(store: &Store, member: &Task) -> String {
    // A React row's verifier is given its packet (`compounding-104`); any other
    // row's order is what it was. Whole and multi-line, as `wsp brief` shows it.
    // The checks changed since they were written are in the packet, or, with
    // none, in the order (wsp-681).
    match crate::verifier_packet::build(store, member) {
        Some(packet) => format!("{}\n\n{packet}", verifier_base(store, member)),
        None => with_checks_changed(verifier_base(store, member), member),
    }
}

/// The order with the row's changed checks after it, when there are any.
fn with_checks_changed(order: String, member: &Task) -> String {
    match crate::verifier_packet::checks_changed(member) {
        c if c.is_empty() => order,
        c => format!("{order}\n\n{}", c.trim_end()),
    }
}

/// The order as one line, for the two places that type it into a seat
/// (`wsp spawn --verify` and an untaken start), and a kind taking it in argv
/// can carry nothing longer. **A packet cannot be folded**: its diff and fences
/// are lines, and one line loses which are removed and which added. So a React
/// row's packet is written to a file under the store, whole, and the line names
/// it and says to read it first.
pub(crate) fn verifier_order_line(store: &Store, member: &Task) -> String {
    match crate::verifier_packet::file(store, member) {
        Some(path) => format!("{} {}", crate::util::fold(&verifier_base(store, member)), crate::verifier_packet::pointer(&path)),
        None => crate::util::fold(&with_checks_changed(verifier_base(store, member), member)),
    }
}

/// The reasons a member gave for running the full suite in its seat
/// (`wsp-538`), from its log, so that an unneeded run is visible to the one
/// reading the work and can be challenged. Empty when there were none.
pub(crate) fn full_suite_line(member: &Task) -> String {
    let log = member.section("Log").unwrap_or_default();
    let why: Vec<String> = log
        .lines()
        .filter_map(|l| l.split_once(crate::throttle::FULL_SUITE_LOGGED).map(|(_, w)| w.trim().to_string()))
        .collect();
    if why.is_empty() {
        return String::new();
    }
    let said = why.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join("; ");
    format!(
        "\n\nThe member ran the full suite in its seat ({}), saying: {said}. The barrier runs the full suite once per group: if this change did not need the whole suite, say so in your verdict.",
        match why.len() {
            1 => "once".to_string(),
            n => format!("{n} times"),
        }
    )
}

/// The order without the packet.
fn verifier_base(store: &Store, member: &Task) -> String {
    let running = worklist::Running::read(store);
    let list = running.list_of(&member.id).and_then(|l| store.worklist(l));
    let placed = list.as_ref().and_then(|w| {
        let groups = w.groups();
        let at = groups.iter().position(|g| g.members.contains(&member.id))?;
        Some((w.id.clone(), at + 1, groups[at].stop.trim().to_string()))
    });
    let (whose, stop) = match &placed {
        Some((list, at, stop)) => (
            format!(", a member of group {at} of the `{list}` worklist"),
            match stop.as_str() {
                "" => String::new(),
                s => format!("\n\nThe barrier after this group asks: {s}"),
            },
        ),
        None => (String::new(), String::new()),
    };
    let pass = crate::verification::passes(member)
        .into_iter()
        .rev()
        .find(|p| p.state == crate::verification::State::Running);
    let read = pass.as_ref().and_then(|p| p.read.as_ref()).map(|r| format!(" at {r}")).unwrap_or_default();
    let ready = crate::repair::ready(member).is_some();
    let by_merge = ready && crate::lands::by_merge(store, &member.id);
    // The pass reads a commit the member's tree does not stand at.
    let elsewhere = pass.as_ref().is_some_and(|p| p.ready.is_some());
    let (started, stand, readonly, holds) = match ready {
        true if by_merge => by_merge_order(store, member, pass.as_ref()),
        true => (
            format!("wsp started you when {m} reached review, ready{read} and rebased on the trunk; it is not on the trunk yet and nobody else is going to read this work before it is", m = member.id),
            format!("**You stand in {m}'s own tree**, at the commit it is ready at, with its build output: verify there, with `cargo test <module>::` or the project's equivalent incrementally, and read the trunk with `git show master:<path>` where you need it. ", m = member.id),
            format!("**You are read-only.** Do not edit, commit, rebase, land, or change {m}'s status; never type `wsp land`. ", m = member.id),
            format!("it holds: `wsp verified {m} --holds --from FILE --caption \"…\"`, which lands {m} itself: it rebases the branch on the trunk if the trunk moved, reruns only the recorded checks the rebase can affect, and puts the commits on the trunk. A land that cannot happen (a conflict, a red rerun) is recorded as a block with the reason, and nothing reaches the trunk", m = member.id),
        ),
        false => (
            format!("wsp started you when {m} reached review and landed{read}; nobody else is going to read this work before the barrier", m = member.id),
            String::new(),
            format!("**You are read-only.** Do not edit, commit, land, rebase, or change {m}'s status. ", m = member.id),
            format!("it holds: `wsp verified {m} --holds --from FILE --caption \"…\"`", m = member.id),
        ),
    };
    format!(
        "Verify **{m}** ({title}){whose}. {started}.\n\n\
         {readonly}{stand}\
         Read what was asked and what was done with `wsp show {m}`, then check it against the \
         code rather than against the report: the commits, whether they \
         do what the overview asked, and the tests that cover them — `cargo test <module>::` in \
         debug for what it touched, never `wsp verify` or a release build.{stop}{legs}{pieces}{whole}{ran_full}{split}\n\n\
         **Finish with one command.** Write your verdict to a file — what you checked, what held, \
         what did not, and when it does not hold, what has to change — then:\n\
         - {holds};\n\
         - it does not: `wsp verified {m} --blocks --from FILE --caption \"…\"`, with a line \
         `cause: work | check | tree | env — <one sentence>` in the file (a block without one is refused). {m} goes back to doing with \
         your verdict as what is owed, and its governor is told. `cause: env` is for when you could not verify \
         (your tree lost, the load, a tool missing): it is no block of the work, {m} stays at review, and the run starts another verifier.\n\n\
         {caption}\n\n\
         {checked}{reuse}\n\n\
         There is no row of your own: the verdict is recorded on {m} under `## Verification`, \
         and wsp ends this seat once it is in.",
        m = member.id,
        title = util::truncate(&member.title, 80),
        caption = CAPTION_ORDER,
        checked = CHECKED_ORDER,
        legs = legs_line(member),
        pieces = pieces_block(store, member),
        whole = crate::size_check::whole_line(member),
        ran_full = full_suite_line(member),
        // The greens are the ready commit's, which says nothing of another.
        reuse = crate::check::greens_held(store, member).filter(|_| !elsewhere).map(|(sha, held)| reuse_text(&sha, &held)).unwrap_or_default(),
        split = crate::split::for_verifier(member, &crate::split::unrun(&store.tasks(), &member.id, &running)),
    )
}

/// The four parts of a by-merge row's order that differ (`wsp-350`): how the
/// verifier was started, where it stands, what it may not do and what a hold
/// does.
fn by_merge_order(store: &Store, member: &Task, pass: Option<&crate::verification::Pass>) -> (String, String, String, String) {
    let m = &member.id;
    let ready = crate::repair::ready(member).unwrap_or_default();
    let read = pass.and_then(|p| p.read.clone()).unwrap_or_else(|| ready.clone());
    let from = match pass.and_then(|p| p.from.as_deref()) {
        Some(url) if url.starts_with("http") => format!("the head of its PR, {url}"),
        Some(branch) => format!("the head of {branch} (no open PR was found)"),
        None => "its ready commit (no PR and no remote branch was found)".to_string(),
    };
    let base = crate::by_merge::base_ref(store, member).unwrap_or_else(|| "the remote's trunk".into());
    let stand = match pass.is_some_and(|p| p.ready.is_some()) {
        true => format!(
            "**You stand in {m}'s own tree, whose HEAD is {ready} and not the commit you read**: read files with `git show {read}:<path>`, \
             and run tests on a copy (`git archive {read} | tar -x -C <a new dir under $WSP_SCRATCH>`). "
        ),
        false => format!("**You stand in {m}'s own tree**, at the commit you read, with its build output: verify there, with `cargo test <module>::` or the project's equivalent incrementally. "),
    };
    (
        format!(
            "wsp started you when {m} reached review; it is {waiting}, and nobody else is going to read this work before it does. \
             **Read commit {read}**, {from}. Compare it against `{base}`, not the local trunk: `git diff {base}...{read}`",
            waiting = crate::lands::WAITING,
        ),
        format!("{stand}Read the trunk with `git show {base}:<path>` where you need it. "),
        format!("**You are read-only.** Do not edit, commit, rebase, push, merge, or change {m}'s status. "),
        format!("it holds: `wsp verified {m} --holds --from FILE --caption \"…\"`, which records the verdict; {m} stays {}", crate::lands::WAITING),
    )
}

/// What a verifier is told when the member's own check is green at the commit it
/// reads (`wsp-425`): the lines of the green, and that they are not to be run
/// again. Starts with the blank line that sets it off; a carried green says where
/// it passed and that the inputs are the same.
pub(crate) fn reuse_text(sha: &str, held: &[(String, crate::greens::Green)]) -> String {
    let at = &sha[..12.min(sha.len())];
    let mut out = format!("\n\n**The row's own check is already green at {at}: do not run it again.** It is in the ledger:\n");
    for (command, g) in held {
        let command = command.lines().next().unwrap_or_default();
        let where_ = match g.carried {
            true => format!("carried from {} (same inputs, rebased since)", &g.sha[..12.min(g.sha.len())]),
            false => "at this commit".to_string(),
        };
        out.push_str(&format!("- `{command}` · {}s · {} · {where_} · by {}, {}\n", g.took, g.env.word(), g.who, g.when));
    }
    out.push_str(
        "Run only what the packet or the row's done-when names that these commands do not cover \
         (a test they do not run, a build of another target); a verdict does not block on what they covered.",
    );
    out
}

/// The line a verifier's order gains on a row that ran in legs: the table, and
/// that a verdict which blocks names the leg whose work fails, since wsp sends
/// the row back to a `rework` leg on that leg's tier. Empty for any other row.
fn legs_line(member: &Task) -> String {
    let all = crate::legs::legs(member);
    if all.is_empty() {
        return String::new();
    }
    format!(
        "\n\n**This row ran in legs**, each a fresh agent handed the one before's baton:\n\n{}\n\n\
         If it does not hold, **name the leg whose work fails** in your verdict, by its name from the table. \
         wsp appends one `rework` leg after the last, with your verdict as its order, and runs it on that \
         leg's tier. A verdict that names no leg sends it back on the default.",
        crate::legs::table(&all)
    )
}

/// The block a verifier's order gains on a row that ran as pieces: per piece its
/// id, title, done-when and the result of its check lines on the row's tip, and
/// that a blocking verdict names the pieces (`<row>-<n>`) it is about, since wsp
/// adds one rework piece for each. Empty for a row with no pieces.
fn pieces_block(store: &Store, member: &Task) -> String {
    let rec = crate::pieces::record(member);
    if rec.pieces.is_empty() {
        return String::new();
    }
    let results = crate::check::pieces_results(store, member);
    let mut out = String::from("\n\n**This row ran as pieces**, parts of one work, each its own agent and judged here, by you, once:\n");
    for p in &rec.pieces {
        let id = p.id(&member.id);
        match p.state {
            crate::pieces::State::Dropped => out.push_str(&format!("\n- `{id}` · {} · dropped: {}", util::fold(&p.title), util::fold(p.why.as_deref().unwrap_or_default()))),
            crate::pieces::State::Promoted => out.push_str(&format!("\n- `{id}` · {} · promoted to {}", util::fold(&p.title), p.promoted.as_deref().unwrap_or_default())),
            _ => {
                out.push_str(&format!("\n- `{id}` · {} · {}", util::fold(&p.title), p.state.as_str()));
                out.push_str(&format!("\n  done-when: {}", util::fold(&p.done_when)));
                match results.iter().find(|(n, _)| *n == p.n) {
                    Some((_, said)) => out.push_str(&format!("\n  checks on the row's tip: {}", said.join("; "))),
                    None if p.checks.is_empty() => out.push_str("\n  checks: none"),
                    None => out.push_str("\n  checks: not run"),
                }
            }
        }
    }
    out.push_str(
        "\n\n**Judge each piece's done-when item by item**, as you judge the row's. If it does not hold, \
         **name the pieces whose work fails** in your verdict, by id from the list. wsp appends one `rework` \
         piece for each in a new batch, on that piece's tier, with your verdict as its order. A verdict that \
         names no piece gets one rework piece over the row.",
    );
    out
}

/// How to write a verdict's caption, in both work orders that end in one
/// (`wsp-241`). The examples are tails from gpui-migration, compound's
/// `docs/ux-design.md` §17: each reads whole after `holds` or `passed`.
const CAPTION_ORDER: &str = "**The caption is required**, and it is what a person reads at a glance: the \
     reason in under 60 characters, written to follow the verdict word wsp puts in front of it \
     (`holds`, `blocks`, `passed`, `held`). Plain words, no commit hashes, no ids. Like: \
     `the wiring between the two has no test`, `the wiring is tested now`, \
     `all six hold; Ed settled his two items`, `waiting on Ed's read of the design`.";

/// What a verifier is asked to write about what it checked (`wsp-325`): the one
/// part of what a verdict records that a transcript cannot say.
const CHECKED_ORDER: &str = "**Say which done-when items you checked.** In the verdict file, one line for each item of \
     the row's done-when you looked at, ending in `held` or `not held`: `checked: <the item, in a few words> — held`. \
     wsp takes these lines out of your prose and records them beside what it reads off your session itself — \
     the commands you ran, what you drove, what you read — so do not list those. A blocking verdict's `not held` \
     items are sent back as what is owed. A line that does not end in `held` or `not held` is refused.";

/// A check's work order, composed from the list as it stands when it is asked,
/// since a check has no row to keep it on: what `wsp spawn --check` hands the
/// agent and what `wsp brief` shows in its seat.
pub(crate) fn check_order(store: &Store, list: &str, at: usize) -> Option<String> {
    let w = store.worklist(list)?;
    let g = w.groups().into_iter().nth(at - 1)?;
    let packet = packet_text(store, &w, at, &g);
    Some(barrier_order(&w, at, &g, &packet))
}

/// What the check is handed so it need not gather it (`wsp-326`): each member
/// and its verdict, the stop's mechanical items, and — when wsp ran the
/// barrier's checks — what they said and whose fault a failure reads as; then
/// the check's exit template (`wsp-617`).
fn packet_text(store: &Store, w: &Worklist, at: usize, g: &Group) -> String {
    let tasks = store.tasks_shared();
    let mut out = format!("**The packet.** The members and their verifiers' verdicts:\n\n{}", crate::barrier_run::head_text(&tasks, g));
    if !g.stop_checks.is_empty() {
        out.push_str("\nThe stop condition's mechanical items, which wsp runs with the suite:\n");
        for i in &g.stop_checks {
            out.push_str(&format!("- {i}\n"));
        }
    }
    let read = crate::barrier::running(g).and_then(|c| c.read.clone());
    match crate::barrier_run::packet_for(store, &w.id, at, read.as_deref()) {
        Some(results) => out.push_str(&format!(
            "\n**wsp has already run the barrier's checks at the trunk. Do not build or run them again** unless you doubt a \
             result; they are:\n\n{results}"
        )),
        None => out.push_str("\nwsp ran no checks for this barrier; build and test the group yourself.\n"),
    }
    // It ends with the check's exit, submitted for the list (`wsp-617`).
    out.push_str(&format!("\n{}", crate::closing::exit_template(crate::exit::Kind::Check, &w.id, &[])));
    out
}

/// The barrier agent's work order.
///
/// The third answer, `wsp worklist followup` (`wsp-210`), is offered only while
/// the group still has its one round. A group that has spent it is told so, so
/// the check knows that whatever more it finds goes to the governor.
fn barrier_order(w: &Worklist, at: usize, g: &Group, packet: &str) -> String {
    let stop = match g.stop.trim() {
        "" => "There is no stop condition written at this barrier.".to_string(),
        s => format!("The stop condition, which your verdict answers:\n\n> {s}"),
    };
    let tail = match crate::cmd_worklist::spent_by(w, &g.members) {
        None => format!(
            "\n\n**Small work found at this barrier can be attached, not just reported.** File each row with \
             `wsp add \"…\" --from <member>` (a follow-up: it records the member it was found on and never \
             holds it open), with a **Done when:** line in its overview, then run one of \
             these in place of `go` or `hold`:\n\
             - it passes, with follow-ups: `wsp worklist followup {list} <id>… --next --from FILE --caption \"…\"`. The rows \
             become the next group, ahead of anything planned, on this group's agent line;\n\
             - it holds, pending follow-ups: `wsp worklist followup {list} <id>… --blocking --from FILE --caption \"…\"`. The \
             rows join this group, and a fresh check reads it again once they land.\n\
             At most four rows, and one round: a follow-up group cannot add its own. Anything larger, or \
             anything that needs a design call, is `wsp ask {list} -`, not a follow-up.",
            list = w.id,
        ),
        Some(prior) => format!(
            "\n\nThis group has had its one round of follow-ups ({prior}). If you find more, \
             `wsp worklist followup` hands them to the governor as a decision; you still finish with `go` or `hold`."
        ),
    };
    format!(
        "Check the barrier after group {at} of the `{list}` worklist. Every member has landed \
         and each has a verifier's verdict, as a note on the member: {members}.\n\n\
         {stop}\n\n\
         {packet}\n\
         **Do not edit, commit, land or rebase.** The per-member reviews are done; what is \
         left is what only the group as a whole can show. `wsp worklist next {list}` gives \
         what the group landed and which members touched one file. Read the verdicts, check \
         the group together on the trunk — it builds, its suites pass together, no two members \
         undid each other — and whether the next group has what it depends on.\n\n\
         **Then decide, and wsp does the rest.** Write the verdict in under 300 words to a \
         file, then:\n\
         - it passes: `wsp worklist go {list} --from FILE --caption \"…\"`. wsp starts the next group and \
         rotates the governor;\n\
         - it does not: `wsp worklist hold {list} --from FILE --caption \"…\"`, saying what has to happen. \
         The governor is told. A hold that owes work attaches it with `wsp worklist followup {list} <id>… --blocking`, \
         and never leaves it in prose: a row named only in the reason stands on no list, and nothing starts it.{tail}\n\n\
         {caption}\n\n\
         A question for the governor before you decide is `wsp ask {list} -`; its answer comes back here.\n\n\
         There is no row of your own: the verb you finish with records this check on group {at}, \
         and wsp ends this seat once your turn is over.",
        list = w.id,
        members = g.members.join(" "),
        caption = CAPTION_ORDER,
        packet = packet,
    )
}

/// What a failed start puts back.
#[derive(Clone, Copy)]
enum Undo {
    /// A member wsp moved to `doing`: back to `todo`, so the next advance —
    /// or a governor's own `wsp spawn` — finds it as it was.
    Member,
    /// A pass on a member: left `running` with no seat, which the next
    /// advance takes again once it is as old as [`WEDGED_AFTER`]. The member
    /// is not moved — it is at `review`, and its work is finished.
    Verifier,
}

/// Why a `wsp spawn` failed, from what it wrote: its notes about a dependency
/// clone are left out (wsp-427). A clone that could not be done is a note and the
/// seat starts cold, so a line saying so is never the reason, and it was the
/// first thing the 300 characters kept.
fn why_not(stderr: &str) -> String {
    let kept: Vec<&str> = stderr.lines().filter(|l| !(l.contains(" not cloned") || l.contains(" cloned from the parent tree "))).collect();
    kept.join("\n").trim().to_string()
}

/// Start an agent on a row, and wait for `wsp spawn` to say how it went.
/// A failure is written on the row and put back per [`Undo`]; the caller
/// tells the seat. Never silent: a run that cannot start its next agent is
/// a run that has stopped.
fn spawn(store: &Store, id: &str, policy: &Policy, undo: Undo, owned: (&str, &str)) -> bool {
    let verify = matches!(undo, Undo::Verifier);
    let failed = start(store, id, policy, owned, verify);
    let Some(why) = failed else { return true };
    log_line(store, &format!("FAILED spawn {}{id}: {why}", if verify { "a verifier on " } else { "" }));
    let saved = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        match undo {
            // On the member's log and nothing else: its work is finished and
            // its `updated` is what a landing-less re-verify is keyed on.
            Undo::Verifier => {
                t.log(&format!("wsp could not start a verifier on this: {}", util::truncate(&why, 300)));
            }
            _ => {
                t.log(&format!("wsp could not spawn an agent on this: {}", util::truncate(&why, 300)));
                if matches!(undo, Undo::Member) && t.status() == Status::Doing {
                    t.set_status(Status::Todo);
                }
                t.touch();
            }
        }
        store.save_task(&t).is_ok()
    });
    if saved {
        store.git_commit(&format!("wsp: spawn failed on {id}"));
    }
    if matches!(undo, Undo::Member) {
        if is_list_wide(&why) {
            list_wide_refusal(store, owned.0, id, &why);
        } else {
            member_start_failed(store, owned.0, id, &why);
        }
    }
    false
}

/// What a refusal says when its cause is the same for every row of the list,
/// not the row's own: the pack builder is not built.
const LIST_WIDE: &[&str] = &["the pack builder is not built", "WSP_PACK_BIN="];

/// Whether a spawn was refused for a cause no other row of the list escapes.
fn is_list_wide(why: &str) -> bool {
    LIST_WIDE.iter().any(|m| why.contains(m))
}

/// What every seat of a group needs and does not have yet, in one line, or
/// `None`. Checked before any seat is opened: a start that fails this fails for
/// every row, five times over, with a seat's minute each (wsp-017).
fn list_wide_blocker(store: &Store, rows: &[String]) -> Option<String> {
    #[cfg(test)]
    {
        let _ = (store, rows);
        return tests::PACK_MISSING.with(|p| p.borrow().clone());
    }
    #[cfg(not(test))]
    {
        let needs = rows.iter().any(|id| store.find_task(id).is_some_and(|t| t.status() == Status::Todo && crate::packet_gen::eligible(&t).is_ok()));
        if !needs {
            return None;
        }
        crate::packet_gen::pack_bin().err()
    }
}

/// Park the list for a cause shared by every row, after one try. The row is
/// already back at `todo`; its seat is ended and the governor is told.
fn list_wide_refusal(store: &Store, list: &str, id: &str, why: &str) {
    despawn(store, id);
    let why = util::truncate(why, 300);
    park_list_for(store, list, &why);
}

/// Park `list` with the cause, log it and tell its seat.
fn park_list_for(store: &Store, list: &str, why: &str) {
    let Some(mut w) = store.worklist(list) else { return };
    let said = format!("every seat of this run needs what is missing, so nothing is started: {why}");
    if crate::cmd_worklist::park_for_cause(store, &mut w, &said) {
        log_line(store, &format!("{list}: parked — {why}"));
        tell(store, &w, &format!("wsp parked the {list} run without trying each row: {why}. Fix it, then `wsp worklist resume {list}`."));
    }
}

/// `wsp spawn <id> --agent …`, and its refusal if it refused.
fn start(store: &Store, id: &str, policy: &Policy, owned: (&str, &str), verify: bool) -> Option<String> {
    // A test's binary is the harness: record what would have started instead.
    if cfg!(test) {
        #[cfg(test)]
        return tests::SPAWNED.with(|s| {
            let id = if verify { crate::verification::seat_label(id) } else { id.to_string() };
            s.borrow_mut().push((id, policy.kind.clone()));
            tests::FAIL.with(|f| f.borrow().clone())
        });
    }
    let exe = match wsp_base::util::self_exe() {
        Ok(e) => e,
        Err(e) => return Some(e.to_string()),
    };
    let mut argv: Vec<String> = vec!["spawn".into(), id.into(), "--agent".into()];
    if verify {
        argv.push("--verify".into());
    }
    argv.extend(policy.spawn_flags());
    log_line(store, &format!("spawn {}", argv[1..].join(" ")));
    let child = Command::new(exe)
        .args(&argv)
        .stdin(Stdio::null())
        // The run this start belongs to, so the row records its owner and a
        // later pass can end everything the group opened. `wsp-158`.
        .env(crate::cycle::OWNED_LIST, owned.0)
        .env(crate::cycle::OWNED_GROUP, owned.1)
        .output();
    match child {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(why_not(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => Some(e.to_string()),
    }
}

/// Start the check on group `at`'s barrier: `wsp spawn <list> --agent --check
/// <at>`, which names its seat on the entry [`crate::barrier::open`] wrote.
/// `Some` is why it did not start; the entry is left open with no seat, and
/// the next step takes it again once it is wedged.
pub(crate) fn start_check(store: &Store, list: &str, at: usize, policy: &Policy) -> Option<String> {
    if cfg!(test) {
        #[cfg(test)]
        return tests::SPAWNED.with(|s| {
            s.borrow_mut().push((crate::barrier::seat_label(list, at), policy.kind.clone()));
            tests::FAIL.with(|f| f.borrow().clone())
        });
    }
    let exe = match wsp_base::util::self_exe() {
        Ok(e) => e,
        Err(e) => return Some(e.to_string()),
    };
    let mut argv: Vec<String> = vec!["spawn".into(), list.into(), "--agent".into(), "--check".into(), at.to_string()];
    argv.extend(policy.spawn_flags());
    log_line(store, &format!("spawn {}", argv[1..].join(" ")));
    let child = Command::new(exe).args(&argv).stdin(Stdio::null()).output();
    match child {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(why_not(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => Some(e.to_string()),
    }
}

/// The commit on the trunk of the list's repository now: what a check opened
/// now is reading. `None` with no repository to ask, which is every test.
fn trunk_tip(store: &Store, list: &str) -> Option<String> {
    if cfg!(test) {
        return None;
    }
    let index = crate::resolve::Index::new(store.projects());
    let root = crate::resolve::list_root(store, &index, list)?;
    crate::cmd_checkout::trunk_tip(&root)
}

/// Tell the seat a start failed, so a stopped run is somebody's to see.
pub(crate) fn failed(store: &Store, w: &Worklist, id: &str) {
    tell(store, w, &format!(
        "wsp could not start an agent on {id} in the {} run, so the run is waiting there. \
         `wsp show {id}` has the reason; `wsp worklist advance {}` tries again.",
        w.id, w.id
    ));
}

/// A barrier check's start failed: end the pane it left, take it off the entry
/// so the run does not read it as a check in flight, and say when it is tried
/// again — or, on the last try, flag the person (`wsp-328`).
///
/// **Not [`failed`], which tells a seat to run `wsp worklist advance`.** That
/// was the whole of the recovery before this row: the pane `wsp spawn` named
/// stayed on the entry, idle, and the run waited on it until a person ended it
/// and spawned the check again by hand.
pub(crate) fn check_start_failed(store: &Store, w: &Worklist, at: usize, why: &str) {
    let Some(u) = crate::barrier::unseat(store, &w.id, at) else { return };
    let ended = match &u.seat {
        Some(seat) => match despawn_seat(seat) {
            Ok(()) => format!("{seat} was ended"),
            Err(e) => format!("{seat} could not be ended ({e}): `wsp despawn --pane {seat}`"),
        },
        None => "it had no seat to end".to_string(),
    };
    let why = util::truncate(why, 300);
    if u.spent {
        log_line(store, &format!("{} group {at}: its check did not start on try {} of {} — giving up; {ended}", w.id, u.tries, crate::barrier::TRIES));
        check_gave_up(store, w, at, u.tries, &why);
        return;
    }
    let secs = crate::barrier::wait_after(u.tries).unwrap_or_default();
    let wait = crate::barrier::span(secs);
    log_line(store, &format!("{} group {at}: its check did not start on try {} of {} — {ended}; starting it again in {wait}", w.id, u.tries, crate::barrier::TRIES));
    wake_after(store, &w.id, secs);
    tell(store, w, &format!(
        "The {} run's barrier check on group {at} did not start (try {} of {}): {why}. \
         wsp ended the pane it left and starts the check again in {wait}.",
        w.id, u.tries, crate::barrier::TRIES
    ));
}

/// The check's tries ran out: the person is flagged, on the list's group, and
/// the governor told. Said once, because [`crate::barrier::unseat`] and
/// [`crate::barrier::give_up`] each answer it once.
pub(crate) fn check_gave_up(store: &Store, w: &Worklist, at: usize, tries: u32, last: &str) {
    use crate::message::{self, About, Kind, Message, Party};
    let (evidence, passable) = barrier_evidence(store, w, at);
    let verdict = if passable {
        format!(
            "Everything the stop reads mechanically is met: {evidence}. \
             `wsp worklist go {list}` passes it in one step.",
            list = w.id
        )
    } else {
        format!("What the check would have read: {evidence}. That does not meet the stop, so `wsp worklist go {list}` is not yet the answer.", list = w.id)
    };
    let text = format!(
        "{list} group {at}: the barrier check never started, after {tries} tries\n\n\
         Last error: {last}\n\n\
         {verdict}\n\n\
         `wsp worklist advance {list}` starts the check again once the cause is cleared, \
         and `wsp worklist go {list}` passes the barrier without one.",
        list = w.id,
    );
    let m = Message::new(Party::wsp(), Kind::Note, &text).about(About::Group { scope: w.id.clone(), group: at });
    let _ = message::raise(store, &m);
    log_line(store, &format!("{} group {at}: flagged the person — the check never started in {tries} tries: {last}; {evidence}", w.id));
    tell(store, w, &format!(
        "The {} run's barrier check on group {at} did not start in {tries} tries, and the person is flagged. {verdict} \
         `wsp worklist advance {}` starts it again once the cause is cleared.",
        w.id, w.id
    ));
}

/// What a barrier check that never started would have read, and whether that
/// already meets the stop's mechanical parts (`wsp-600`): the barrier run's
/// result, and each member's landing and verdict. retro-2's group 4 waited
/// for a person who found it all green by hand.
pub(crate) fn barrier_evidence(store: &Store, w: &Worklist, at: usize) -> (String, bool) {
    let run = crate::barrier::suite_of(store, &w.id, at);
    let tasks = store.tasks_shared();
    let pos = worklist::position(store, w, Reading::Landed);
    let mut held = Vec::new();
    let mut all = true;
    for m in &pos.members {
        let verified = crate::verification::verified(&tasks, &m.id);
        let landed = m.finished();
        if !(landed && verified) {
            all = false;
            held.push(format!("{} ({})", m.id, if !landed { "not landed" } else { "verifier not holding" }));
        }
    }
    let run_said = match run {
        Some(crate::barrier::Suite::Green) => "the barrier run is green",
        Some(crate::barrier::Suite::Red) => "the barrier run is red",
        Some(crate::barrier::Suite::Running) => "the barrier run is still going",
        None => "no barrier run is recorded",
    };
    let members = if held.is_empty() {
        format!("all {} members landed with their verifiers holding", pos.members.len())
    } else {
        format!("held: {}", held.join(", "))
    };
    (format!("{run_said}; {members}"), all && run == Some(crate::barrier::Suite::Green))
}

/// The members of `list`, and the legs of those run as legs, whose starts ran
/// out of tries: forgiven, so the step that follows starts them again.
fn forgive_spent_starts(store: &Store, list: &str) -> usize {
    crate::starts::forgive_spent(store, &start_labels(store, list))
}

/// The starts that gave up under a throttle or memory hold are tried again once
/// the hold lets go, without a hand `advance` (`wsp-600`). Says which, once.
fn retry_after_hold(store: &Store, list: &str) {
    let spent = crate::starts::forgive_after_hold(store, &start_labels(store, list), !cfg!(test) && crate::throttle::holding_back(store).is_some());
    if !spent.is_empty() {
        log_line(store, &format!("{list}: the hold has let go — {} tried again, its start having given up under it", spent.join(", ")));
    }
}

/// The labels the run keeps start counts under for `list`: each member, its
/// legs and its pieces.
fn start_labels(store: &Store, list: &str) -> Vec<String> {
    let Some(w) = store.worklist(list) else { return Vec::new() };
    let mut labels = Vec::new();
    for g in w.groups() {
        for id in &g.members {
            labels.push(id.clone());
            if let Some(t) = store.find_task(id) {
                labels.extend(crate::legs::legs(&t).iter().map(|l| crate::legs::label(id, &l.name)));
                labels.extend(crate::pieces::record(&t).pieces.iter().map(|p| p.id(id)));
            }
        }
    }
    labels
}

/// A member's start failed, in the step: its agent and claim are ended so the
/// row is as it was before the start, the miss is counted, and the governor is
/// told when it is tried again or that the person is flagged (`wsp-335`).
///
/// **Ended here and not left for [`crate::repair`]**: that reading is of a row at
/// `doing` with a claim, and [`spawn`] has just put this one back at `todo` —
/// a claim held by a pane whose agent never came up, which nothing else reads.
fn member_start_failed(store: &Store, list: &str, id: &str, why: &str) {
    despawn(store, id);
    let m = crate::starts::missed(store, id);
    let why = util::truncate(why, 300);
    let w = store.worklist(list);
    if m.last {
        log_line(store, &format!("{list}: {id} did not start on try {} of {} — giving up", m.tries, crate::barrier::TRIES));
        start_gave_up(store, w.as_ref(), id, id, m.tries, &why, &format!("`wsp worklist advance {list}`"));
        return;
    }
    let secs = m.again_in.unwrap_or_default();
    let wait = crate::barrier::span(secs);
    log_line(store, &format!("{list}: {id} did not start on try {} of {} — starting it again in {wait}", m.tries, crate::barrier::TRIES));
    wake_after(store, list, secs);
    if let Some(w) = &w {
        tell(store, w, &format!(
            "wsp could not start an agent on {id} in the {list} run (try {} of {}): {why}. \
             wsp ended what the start left and starts it again in {wait}.",
            m.tries, crate::barrier::TRIES
        ));
    }
}

/// A leg's start failed, or its seat never came up: the seat is ended, the entry
/// goes back to pending with no seat on it, and the leg is started again on the
/// backoff — or the person is flagged, on the last try (`wsp-335`). `w` is the
/// list that runs the row, when one does.
///
/// **The entry is reset with its seat named, which [`crate::legs::unopen`] will
/// not do.** `wsp spawn` names the pane on the entry before the agent starts, so
/// a start that then fails readiness left `running · pane cpd-2559` with the
/// pane gone: `unopen` took that for a leg somebody is running, `spawn --leg`
/// refused it as not pending, and the seat cap counted it until a person edited
/// the store (`compound-956`).
pub(crate) fn leg_start_failed(store: &Store, w: Option<&Worklist>, row: &str, name: &str, why: &str) {
    let label = crate::legs::label(row, name);
    let seat = store
        .find_task(row)
        .and_then(|t| crate::legs::legs(&t).into_iter().find(|l| l.name == name))
        .and_then(|l| l.pane);
    let ended = match &seat {
        Some(seat) => match despawn_seat(seat) {
            Ok(()) => format!("{seat} was ended"),
            Err(e) => format!("{seat} could not be ended ({e}): `wsp despawn --pane {seat}`"),
        },
        None => "it had no seat to end".to_string(),
    };
    crate::leg_seat::reset(store, row, name);
    let m = crate::starts::missed(store, &label);
    let why = util::truncate(why, 300);
    let wait = m.again_in.map_or_else(|| "it is not tried again".to_string(), |s| format!("it starts again in {}", crate::barrier::span(s)));
    store.locked(|| {
        if let Some(mut t) = store.find_task(row) {
            let _ = crate::log::note(store, &mut t, &format!("wsp could not spawn leg {name} (try {} of {}): {why}", m.tries, crate::barrier::TRIES));
        }
    });
    log_line(store, &format!("{label}: its start did not take on try {} of {} — {ended}; {wait}", m.tries, crate::barrier::TRIES));
    if m.last {
        let remedy = format!("`wsp leg reset {row} {name} \"why\"` starts it again once the cause is cleared");
        start_gave_up(store, w, row, &format!("leg {name} of {row}"), m.tries, &why, &remedy);
    } else if let Some(w) = w {
        tell(store, w, &format!(
            "leg {name} of {row} did not start in the {} run (try {} of {}): {why}. \
             wsp ended the seat it left and {wait}.",
            w.id, m.tries, crate::barrier::TRIES
        ));
    }
}

/// A start written down that never arrived — an advance killed between its
/// record and its spawn — is a miss of the same kind as one that failed. Counts
/// it, and says whether the start may go ahead: not when this was the last try,
/// when the person is flagged instead.
fn retry_lost_start(store: &Store, row: &str, label: &str, what: &str, remedy: &str) -> bool {
    let m = crate::starts::missed(store, label);
    if !m.last {
        log_line(store, &format!("{label}: the start made no claim within {WEDGED_AFTER} s — starting again, try {} of {}", m.tries + 1, crate::barrier::TRIES));
        return true;
    }
    log_line(store, &format!("{label}: the last start never arrived, on try {} of {} — giving up", m.tries, crate::barrier::TRIES));
    start_gave_up(store, None, row, what, m.tries, "the start never arrived", remedy);
    false
}

/// A member's or a leg's tries ran out: the person is flagged on the row, and
/// the governor told. Said once, because [`crate::starts::missed`] answers it once.
fn start_gave_up(store: &Store, w: Option<&Worklist>, row: &str, what: &str, tries: u32, last: &str, remedy: &str) {
    use crate::message::{self, About, Kind, Message, Party};
    let text = format!(
        "{what} never started, after {tries} tries\n\nLast error: {last}\n\n\
         The run has stopped trying and the row is waiting. {remedy}."
    );
    let m = Message::new(Party::wsp(), Kind::Note, &text).about(About::Task(row.to_string()));
    let _ = message::raise(store, &m);
    log_line(store, &format!("{row}: flagged the person — {what} never started in {tries} tries: {last}"));
    if let Some(w) = w {
        tell(store, w, &format!("{what} did not start in {tries} tries and the person is flagged. {remedy}."));
    }
}

/// End the agents a passed group no longer needs: its members, their
/// verifiers, and the check on the barrier *before* it.
///
/// Not this barrier's own check. Its agent is the one running the `go` that
/// got us here, and ending it now would cut the turn in which it reviews its
/// own row; that `go` arranged its ending for when its turn is over
/// ([`end_checks_when_idle`], `wsp-230`). Until then it was ended here, at the
/// next pass — a whole group late, and the last one never. Here it is only the
/// backstop for an arranged ending that did not happen.
/// The governor used to do all of this by hand, and a run wsp starts has to be
/// a run wsp tidies, or every group leaves its agents standing.
fn end_behind(store: &Store, seats: &dyn Seats, w: &Worklist, passed: usize) {
    end_group(store, seats, w, passed);
}

/// Every row a passed group opened, ended. `wsp-158`.
///
/// **Four kinds, and the fourth is the point.** A group's members and their
/// verifiers were named by the group text, so they were always findable. The
/// barrier check is named by a title. But a governor will spawn a helper — the
/// agent that installs a binary, or drives a delivery live — and that row is a
/// member of nothing and a verifier of nothing, so nothing could find it and
/// every group left one more seat standing. Measured on `wsp-process` with
/// group 1 passed and group 2 running: wsp-153 (the barrier check, idle and
/// holding its claim), wsp-154 (a helper, idle at review) and wsp-157 (a
/// verifier that had recorded its verdict and had nothing left to do).
///
/// So ownership is a fact written when the spawn is issued rather than a guess
/// made when the pass runs — [`OWNED`], on the row's log. A helper that was
/// never claimed as a run row is a helper this cannot end, and the honest
/// thing for that is a report rather than a guess from a title.
///
/// **The barrier check behind the pass, not the one in front of it.** The check
/// in front is running the `go` that got us here, and ending it now would cut
/// the turn in which it reviews its own row. That `go` arranged its ending
/// once the turn is over ([`end_checks_when_idle`]).
///
/// Every row of the previous barrier, not the one with the bare title: a group
/// that held twice has two rows (`wsp-136` item 2), and a re-check is an agent
/// like any other.
pub(crate) fn end_group(store: &Store, seats: &dyn Seats, w: &Worklist, passed: usize) {
    let groups = w.groups();
    let Some(g) = groups.get(passed - 1) else { return };
    let tasks = store.tasks_shared();
    let mut ids: Vec<String> = g.members.clone();
    ids.extend(
        tasks
            .iter()
            .filter(|t| t.tags.iter().any(|x| x == VERIFY_TAG))
            .filter(|t| t.parent.as_ref().is_some_and(|p| g.members.contains(p)))
            .map(|t| t.id.clone()),
    );
    if passed > 1 {
        ids.extend(tasks.iter().filter(|t| is_barrier(t, &w.id, passed - 1)).map(|t| t.id.clone()));
    }
    // **Owned, but not the check in front.** `wsp spawn` stamps every row a
    // run starts, the barrier check included, so ownership alone named the
    // agent whose `go` this is — and the `advance` that `go` pokes ended it in
    // the second its verdict landed, before its `wsp review`. Every
    // agent-checked barrier from `wsp-158` on was left at `doing` (`wsp-209`).
    ids.extend(owned_by(tasks.iter(), &w.id, passed).into_iter().filter(|id| !front_check(&tasks, &w.id, passed, id)));
    ids.sort();
    ids.dedup();
    // The verifiers first: each stands in its member's tree, and a member
    // ended while its verifier is still in there keeps a tree nobody needs.
    for m in &g.members {
        end_passes(store, seats, m, |_| true);
    }
    end_all(store, ids);
}

/// Whether `id` is a check on the barrier after group `at` — the agent a
/// closing at that barrier is being run by, or may be.
fn front_check(tasks: &[Task], list: &str, at: usize, id: &str) -> bool {
    tasks.iter().any(|t| t.id == id && is_barrier(t, list, at))
}

/// The rows the run opened that the group text does not name: a governor's
/// helper, and anything else spawned while the group was going.
///
/// **Matched on the marker and the two values beside it**, so a helper of group
/// 3 is not ended when group 2 passes. A marker alone would do that, and a run
/// whose groups pass out of order — which a `hold` and a resume can produce —
/// would end work that is still in front of it.
fn owned_by<'a>(tasks: impl Iterator<Item = &'a Task>, list: &str, group: usize) -> Vec<String> {
    let want = format!("{OWNED} {list} group {group}");
    tasks
        .filter(|t| t.section("Log").is_some_and(|l| l.lines().any(|x| x.contains(&want))))
        .map(|t| t.id.clone())
        .collect()
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
/// which is [`crate::repair`]'s call. Since `wsp-230` the `go` arranges that
/// ending itself ([`end_checks_when_idle`]), and the tick is the backstop.
///
/// **The claim is the test, and `wsp despawn` is the whole ending**: agent,
/// claim and tree, keeping the tree when it has uncommitted work in it, which
/// it says in `cycle.log` rather than leaving silent.
pub(crate) fn end_all(store: &Store, ids: Vec<String>) {
    let claims = store.claims();
    for id in ids.iter().filter(|id| claims.contains_key(*id)) {
        despawn(store, id);
    }
}

/// The checks on group `at`'s barrier, each ended once its turn is over —
/// arranged by the verb that settled the barrier. `wsp-230`.
///
/// **From the verb, because nothing after it could do this on time.** The
/// check runs `go` or `followup` from inside its own pane, so the pass that
/// verb pokes cannot end it there and then — that cuts the turn in which it
/// reviews its row — and so `end_behind` left it for the *next* pass. On
/// `gpui-migration` that was every barrier agent seated, idle and holding its
/// tree for a whole group, and the last one left to a repair tick that may not
/// come. `govern --rotate` solved
/// the same problem for a governor: a process outside the pane waits for its
/// turn to end, then ends it. This is that, with `despawn`'s whole ending.
///
/// **Every claimed check on the barrier, not the caller's alone**, so a
/// governor passing a barrier by hand ends the check it overruled, and a
/// `followup --blocking` re-check's pass ends the re-check. The idle wait is
/// what makes the issuer safe to include: until its command exits and its
/// turn ends, nothing is touched.
///
/// The ending runs in a launched `worklist advance --end-check`, so what it
/// did, and a tree kept for uncommitted work, is said in `cycle.log`.
///
/// **Seats, and rows filed before `wsp-230`**: a check is a seat on its group's
/// entry now, and a row still open from before is ended the way it always was.
pub(crate) fn end_checks_when_idle(store: &Store, list: &str, at: usize) -> Vec<String> {
    let claims = store.claims();
    let ids: Vec<String> = store
        .tasks()
        .into_iter()
        .filter(|t| claims.contains_key(&t.id) && is_barrier(t, list, at))
        .map(|t| t.id)
        .collect();
    for id in &ids {
        #[cfg(test)]
        tests::CHECKS_ENDING.with(|s| s.borrow_mut().push(id.clone()));
        launch(store, &["worklist", "advance", list, "--end-check", id]);
    }
    let seats = store
        .worklist(list)
        .and_then(|w| w.groups().into_iter().nth(at - 1))
        .map(|g| crate::barrier::standing_on(&g))
        .unwrap_or_default();
    // A check that submitted its verdict is its exit's to end (`wsp-615`): one
    // ending per seat, and that one keeps the tree and marks the entry.
    let owned = crate::ending::exit_owns(store);
    let seats: Vec<String> = seats.into_iter().filter(|s| !owned.contains(s)).collect();
    for seat in &seats {
        #[cfg(test)]
        tests::CHECKS_ENDING.with(|s| s.borrow_mut().push(seat.clone()));
        launch(store, &["worklist", "advance", list, "--end-check-seat", seat]);
    }
    ids.into_iter().chain(seats).collect()
}

/// `worklist advance <list> --end-check <id>`: wait for the check's turn to
/// end, then end it. [`end_checks_when_idle`] launches it.
fn end_check(store: &Store, list: &str, id: &str) {
    let pane = store.panes_for_task(id).into_iter().next();
    if let Some(pane) = pane.as_deref().filter(|_| !cfg!(test)) {
        Seats::wait_idle(&Fleet, pane);
    }
    if !store.claims().contains_key(id) {
        log_line(store, &format!("{list}: {id} settled its barrier and has already gone — nothing to end"));
        return;
    }
    log_line(store, &format!("{list}: {id} settled its barrier and its turn is over — ending it"));
    despawn(store, id);
}

/// `worklist advance <list> --end-check-seat <seat>`: [`end_check`] for a check
/// with no row, which holds no claim — its standing entry is the test.
fn end_check_seat(store: &Store, list: &str, seat: &str) {
    if !cfg!(test) {
        Seats::wait_idle(&Fleet, seat);
    }
    if crate::barrier::seat_of(store, seat).is_none() {
        log_line(store, &format!("{list}: the check in {seat} settled its barrier and has already gone — nothing to end"));
        return;
    }
    if crate::ending::exit_owns(store).contains(seat) {
        log_line(store, &format!("{list}: the check in {seat} submitted its verdict — its exit's ending ends it"));
        return;
    }
    log_line(store, &format!("{list}: the check in {seat} settled its barrier and its turn is over — ending it"));
    let ended = despawn_seat(seat);
    if let Err(why) = &ended {
        log_line(store, &format!("{list}: the check in {seat} was not ended: {}", util::truncate(why, 200)));
    }
    crate::barrier::mark_ended(store, seat, ended.is_ok());
    // And its tree, as the same step: `despawn` takes it, and this says what
    // became of it — or of one an earlier ending left behind (`wsp-289`).
    crate::check_tree::tidy(store, &[seat.to_string()]);
    crate::leg_tree::tidy(store, &[seat.to_string()]);
}

/// The lists that have a barrier check holding a claim right now.
///
/// **Read off the claims, and each claimed row alone**, because the question the
/// tick asks of a done list ("is a check still standing?") is answered by the
/// handful of rows that hold a claim, and asking it of the list means
/// [`worklist::position`] plus every task in the store. A done list only has
/// that to find for the one tick after it closed: [`Closing::Passed`] leaves
/// the last check standing and nothing else does, so the other lists, which
/// grow in number for ever, are skipped on the answer here. `wsp-218`.
pub(crate) fn lists_with_a_barrier_claim(store: &Store) -> std::collections::BTreeSet<String> {
    store
        .claims()
        .keys()
        .filter_map(|id| store.task(id))
        .filter_map(|t| barrier_list(&t).map(str::to_string))
        .collect()
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
    // **A barrier behind the position, or a run with nothing left to govern.**
    // These are different questions with one answer, and the original form
    // answered only the second: a list held at group 3's barrier has two
    // finished checks behind it and `pos.finished()` is false, so neither was
    // ended — which is what installing and looking found.
    //
    // A check behind the position has run and been passed. The check in front
    // of a stopped list is ended here too once it stands at `review`, because
    // nothing else will: a `hold` is that check's own turn as often as not, and
    // ending it there cut the turn before its `wsp review` (`wsp-209`). A check
    // still at `doing` is still deciding, and is left alone by the status read.
    //
    // A parked list is the same case as a held one: stopped short of the end,
    // with checks behind it that have been passed and one in front that has
    // not. `wsp-173`.
    //
    // A done list is over whatever its position says: [`Closing::Passed`]
    // leaves the last check standing for this to end, and a `done` typed early
    // has nothing in front of it that anybody will ask about again.
    //
    // A running list has one such check too (`wsp-210`): a check that answered
    // `followup --blocking` and reviewed its row. Its barrier did not pass, so
    // the position still stands at its group, and the recheck opens on a fresh
    // row once the follow-ups land. Nothing else ends it until a pass two
    // groups on. **Only that check**, named by the log: any other check at
    // review in front of a running run may still be the one somebody passes
    // the barrier from.
    let stopped = matches!(w.status(), WorklistStatus::Held | WorklistStatus::Parked);
    let over = pos.finished() || w.status() == WorklistStatus::Done;
    let running = w.status() == WorklistStatus::Running && !over;
    let held_for: Vec<String> = crate::cmd_worklist::followups(w).into_iter().map(|(row, _)| row).collect();
    if !over && !stopped && !running {
        return Vec::new();
    }
    let behind = pos.at.unwrap_or(usize::MAX);
    let settled = stopped && !over;
    let tasks = store.tasks_shared();
    let claims = store.claims();
    let standing: Vec<String> = tasks
        .iter()
        .filter(|t| t.tags.iter().any(|g| g == BARRIER_TAG))
        .filter(|t| matches!(t.status(), Status::Review | Status::Done))
        .filter(|t| list_of(store, t).map(|l| l.id == w.id).unwrap_or(false))
        .filter(|t| claims.contains_key(&t.id))
        // Up to the position, or anywhere at all once the run is over.
        .filter(|t| !settled || group_of(t, &w.id).is_some_and(|at| at <= behind))
        .filter(|t| !running || (group_of(t, &w.id) == Some(behind) && held_for.contains(&t.id)))
        .map(|t| t.id.clone())
        .collect();
    if !standing.is_empty() {
        log_line(store, &format!(
            "{}: {} and nothing left for them to decide — ending {}",
            w.id,
            if settled || running { "has finished barrier checks still standing" } else { "has nothing left in it" },
            standing.join(" ")
        ));
    }
    standing
}

/// Which group a barrier row belongs to, read off its own title.
///
/// **The title and not a search over the groups**, for the reason
/// [`is_barrier`] exists: a group number is a position in the text, so it is
/// parsed once here and nowhere else.
fn group_of(t: &Task, list: &str) -> Option<usize> {
    let after = t.title.strip_prefix(&format!("{BARRIER_TITLE}{list} group "))?;
    after.split(['(', ' ']).next()?.parse().ok()
}

/// Which closing [`end_what_the_run_opened`] is ending a run's agents at.
///
/// **They differ over the check in front.** A `hold` is the barrier check's own
/// verdict as often as anybody's, and ending that agent inside the verb cuts the
/// turn in which it reviews its own row — `wsp-158` did, and every held check
/// was left at `doing` (`wsp-209`). A hold stops nothing in flight anyway, so
/// the check is left to finish and [`last_barrier_left_behind`] ends it once it
/// stands at `review`. A `done` ends the run outright, typed by whoever is
/// over it, and a check still reading is one more thing it was opened for and
/// will never be asked.
///
/// **`Passed` is `Done` taken by wsp past the last barrier** ([`after_pass`]),
/// and differs from it only over that check: the `go` that closed the run is
/// its turn, so it is left to review its row and [`last_barrier_left_behind`]
/// ends it a tick later, as it does a held list's.
///
/// **`Done` and `Passed` stand the list's own seat down; `Held` does not**
/// ([`stand_down_own_seat`]). A held list is waiting on a decision, and its
/// seat is who makes it.
#[derive(Clone, Copy)]
pub(crate) enum Closing {
    Held,
    Done,
    Passed,
}

/// Everything a run opened, ended at a closing that is not a pass: `hold` and
/// `done`. `wsp-158`.
///
/// **Why these two and not only a pass.** A barrier that closes is a barrier
/// that closes — `wsp-158`'s own heading is "a closed barrier leaves nothing
/// open", and a `done` and a `hold` are how a run ends when the plan was wrong
/// or the work is over early. Ending only at a `go` leaves the commonest ending
/// tidied by hand, which is the state this row was written about.
///
/// **`at` is the group the run stands at, and `None` means every group.** A
/// `done` on a list with a group still open is the case the row calls out, so
/// every group is closed over rather than only the last one passed — and the
/// verdict is `wsp despawn`'s own, which keeps a tree with uncommitted work and
/// says so.
///
/// **Said, not done quietly.** The seat is told, because ending an agent on work
/// somebody may have meant to return to is not a thing to do without a record,
/// and `cycle.log` is where the record of a run is read.
///
/// **A `hold` spares the check in front; a `done` does not** ([`Closing`]).
pub(crate) fn end_what_the_run_opened(store: &Store, seats: &dyn Seats, list: &str, at: Option<usize>, how: Closing) {
    let Some(w) = store.worklist(list) else { return };
    // First, so the sentences below reach whoever answers for the list now and
    // not a seat being ended: a hand on a member, for a list wsp runs and whose
    // own seat is stood down (`wsp-202`); its project's seat, for one run by hand.
    if !matches!(how, Closing::Held) {
        stand_down_own_seat(store, list);
    }
    let tasks = store.tasks_shared();
    let mut ids: Vec<String> = Vec::new();
    match at {
        // Every barrier of the list, and every verifier under a member of it:
        // nothing the run opened is still doing what it was opened for.
        //
        // **Barriers of *this* list, by title, as the `Some` arm reads them.**
        // This used to take every row tagged `barrier` in the store, so `wsp
        // worklist done wsp-unattended` tried to end wsp-process's group 7
        // check mid-run (`wsp-207`) — saved only because no seat was bound to
        // its claim. The tag says what a row is; only its title and its
        // `OWNED` record say whose.
        None => {
            let last = w.groups().len();
            for at in 1..=last {
                if matches!(how, Closing::Passed) && at == last {
                    continue;
                }
                ids.extend(tasks.iter().filter(|t| is_barrier(t, list, at)).map(|t| t.id.clone()));
            }
            ids.extend(
                tasks
                    .iter()
                    .filter(|t| t.tags.iter().any(|g| g == VERIFY_TAG))
                    .filter(|t| {
                        t.parent.as_deref().and_then(|p| store.find_task(p)).is_some_and(|m| {
                            w.groups().iter().any(|g| g.members.contains(&m.id))
                        })
                    })
                    .map(|t| t.id.clone()),
            );
            for (i, _) in w.groups().iter().enumerate() {
                let spare = |id: &String| matches!(how, Closing::Passed) && front_check(&tasks, list, i + 1, id);
                ids.extend(owned_by(tasks.iter(), list, i + 1).into_iter().filter(|id| !spare(id)));
            }
        }
        Some(at) => {
            let groups = w.groups();
            let Some(g) = groups.get(at - 1) else { return };
            ids.extend(g.members.clone());
            ids.extend(
                tasks
                    .iter()
                    .filter(|t| t.tags.iter().any(|x| x == VERIFY_TAG))
                    .filter(|t| t.parent.as_ref().is_some_and(|p| g.members.contains(p)))
                    .map(|t| t.id.clone()),
            );
            let spare = |id: &String| matches!(how, Closing::Held) && front_check(&tasks, list, at, id);
            if matches!(how, Closing::Done | Closing::Passed) {
                ids.extend(tasks.iter().filter(|t| is_barrier(t, list, at)).map(|t| t.id.clone()));
            }
            ids.extend(owned_by(tasks.iter(), list, at).into_iter().filter(|id| !spare(id)));
        }
    }
    ids.sort();
    ids.dedup();
    // The passes on those members, whose seats are not claims and so are not
    // in `standing` below — ended here, each once, verdict or not.
    let members: Vec<String> = match at {
        None => w.groups().iter().flat_map(|g| g.members.clone()).collect(),
        Some(at) => w.groups().get(at - 1).map(|g| g.members.clone()).unwrap_or_default(),
    };
    let mut passes_ended: Vec<String> = Vec::new();
    for m in &members {
        passes_ended.extend(end_passes(store, seats, m, |_| true));
    }
    let claims = store.claims();
    let standing: Vec<String> = ids.iter().filter(|id| claims.contains_key(*id)).cloned().collect();
    if !passes_ended.is_empty() {
        log_line(store, &format!("{list} is closing — ended the verifier seat(s) {}", passes_ended.join(" ")));
    }
    if standing.is_empty() {
        return;
    }
    log_line(store, &format!(
        "{list} is closing over {} that the run opened and left standing — ending {}",
        standing.len(),
        standing.join(" ")
    ));
    if let Some(w) = store.worklist(list) {
        tell(store, &w, &format!(
            "The {list} run is closing, and these agents it opened had nothing left to do: {}. \
             `wsp wip` shows anything that stayed standing.",
            standing.join(" ")
        ));
    }
    end_all(store, standing);
}

/// A closed list's own governor, stood down and ended once it is not mid-turn.
/// `wsp-208`.
///
/// **Ended, and not offered.** worklist-037 offers a person the stand-down of
/// a seat with nothing standing under it, which fits a project's seat: it
/// outlives every run. A list's own seat exists for the run, and a list that is
/// done leaves it nothing to answer for. On `wsp-unattended` that seat, cpd-390,
/// said "Nothing else needs this seat, so I'm stopping" and stayed open, named
/// in `governors.json`, until a person ended it.
///
/// **The list's scope only.** A project's seat in the chain answers for more
/// than this run and is left alone. Whoever seated the list's own seat, it is
/// ended: every pass before the last rotates it on the same terms
/// ([`rotate_on_behalf`](crate::cmd_spawn::rotate_on_behalf)), so after one
/// group it is wsp's whatever it was before.
///
/// **Stood down, not only vacated**, so the reconciler reads a decision and
/// not a governor that died, and does not fill it (`STOOD_DOWN`'s docs); and
/// vacated before the ending, because `wsp despawn` refuses a pane that still
/// holds a seat.
///
/// **When idle**, because the turn that closed the list is often this seat's
/// own — `wsp worklist done`, typed by the governor, is how `wsp-unattended`
/// ended.
///
/// **A post with nobody in it is stood down too** (`wsp-216`). A reseat that
/// failed leaves a record with its markers and no workspace, and this used to
/// return at the seat lookup: `wsp-acceptance-fixes` closed with its post not
/// stood down, and `wsp govern` went on saying a successor was being seated on
/// a done list. One held from another machine is left alone, as the reconciler
/// leaves it.
pub(crate) fn stand_down_own_seat(store: &Store, list: &str) {
    let governors = store.governors();
    let Some(seat) = crate::governors::seat_of_scope(list, &governors) else {
        let empty = governors
            .get(list)
            .is_some_and(|rec| rec.get("workspace").and_then(serde_json::Value::as_str).unwrap_or_default().is_empty());
        if empty && crate::governors::stood_at(&governors, list).is_none() {
            crate::cmd_govern::mark_stood_down(store, list);
            log_line(store, &format!("{list} is closing — stood its own seat down; nobody was sitting in it"));
        }
        return;
    };
    // Read before the vacate: the room a compound seat is found by is on the
    // record the vacate empties.
    let sitting = occupied(store, &seat);
    crate::cmd_govern::vacate(store, list);
    crate::cmd_govern::mark_stood_down(store, list);
    let Some(seat) = sitting else {
        log_line(store, &format!("{list} is closing — stood its own seat down; nobody was sitting in it"));
        return;
    };
    match end_seat_when_idle(store, &seat) {
        Ok(()) => log_line(store, &format!("{list} is closing — stood its own seat {seat} down; it is ended once it is not mid-turn")),
        Err(why) => log_line(store, &format!(
            "{list} is closing — stood its own seat {seat} down, but could not arrange its ending: {why}. \
             `wsp despawn --pane {seat} --keep-tree` ends it"
        )),
    }
}

/// The pane a seat's agent is in now, through the port, as
/// [`rotate_on_behalf`](crate::cmd_spawn::rotate_on_behalf) finds the one it
/// replaces; under test, the recorded one.
pub(crate) fn occupied(store: &Store, seat: &crate::governors::Seat) -> Option<String> {
    if cfg!(test) {
        return (!seat.pane.is_empty()).then(|| seat.pane.clone());
    }
    let backends = crate::place::local_backends();
    crate::cmd_govern::occupant(store, &backends, seat).map(|(_, found)| found.seat.as_str().to_string())
}

/// [`crate::cmd_spawn::end_seat_when_idle`], recorded instead under test.
pub(crate) fn end_seat_when_idle(store: &Store, seat: &str) -> Result<(), String> {
    if cfg!(test) {
        #[cfg(test)]
        tests::ENDED.with(|s| s.borrow_mut().push(seat.to_string()));
        return Ok(());
    }
    Seats::end_when_idle(&Fleet, store, seat)
}

/// Every agent this run opened that has nothing left to do, and holds a claim.
///
/// **Three kinds, and the first is the commonest leak.** A verifier ends its
/// turn by reviewing or blocking its own row, which is the last thing it is
/// asked to do — and it was then left sitting there holding a claim for ever.
/// Measured on `wsp-process`: wsp-157 idle at review in cpd-246, hours after
/// its verdict was on the member. A re-verify is a fresh agent anyway
/// (`wsp-136` item 1), so nothing is lost by ending it and a whole context is
/// saved.
///
/// **A blocker is ended too.** A verifier that blocked has said what has to
/// change; its job is finished either way, and the member's agent is who does
/// the changing.
///
/// **Ended on a tick rather than on the verdict**, and that is the same ordering
/// [`last_barrier_left_behind`] argues: the verb that records the verdict is the
/// agent's own last turn, and ending it inside that turn cuts it short. A tick
/// later it has long been idle and the claim is the record that it has not been
/// ended yet.
///
/// **Running *and* held lists, which is what the running-list reading missed.**
/// Found by installing and looking: `wsp-process` was held at its barrier with
/// two agents from it still standing — a barrier check and a verifier that had
/// recorded its verdict. A hold stops nothing already in flight and says so,
/// and it stops nothing that has *finished* either; `Running::read` answers for
/// running lists alone, so a held run's finished agents were nobody's.
///
/// A verifier's parent chain says which member, and the member says which list;
/// a draft's is not this run's, because a plan is not a run and its agents are
/// a governor's to finish.
pub(crate) fn verdicts_recorded(store: &Store) -> Vec<String> {
    let tasks = store.tasks_shared();
    let claims = store.claims();
    let lists = store.worklists();
    let open = |id: &str| {
        lists.iter().any(|w| {
            matches!(w.status(), WorklistStatus::Running | WorklistStatus::Held | WorklistStatus::Parked)
                && w.groups().iter().any(|g| g.members.contains(&id.to_string()))
        })
    };
    tasks
        .iter()
        .filter(|t| t.tags.iter().any(|g| g == VERIFY_TAG))
        .filter(|t| matches!(t.status(), Status::Review | Status::Done | Status::Blocked))
        .filter(|t| claims.contains_key(&t.id))
        .filter(|t| t.parent.as_deref().and_then(|p| store.find_task(p)).is_some_and(|m| open(&m.id)))
        .map(|t| t.id.clone())
        .collect()
}

/// Every pass that has its verdict, or was superseded, and whose seat wsp has
/// not ended — on any member, whatever its list is doing. The tick's half of
/// [`end_passes`].
///
/// **Ended on a tick rather than on the verdict**, for [`verdicts_recorded`]'s
/// reason: `wsp verified` is the agent's own last turn, and ending it inside
/// that turn cuts it short. A tick later it has long been idle, and the entry
/// is the record that it has not been ended yet.
pub(crate) fn passes_finished(store: &Store, seats: &dyn Seats) -> Vec<String> {
    use crate::verification::{passes, State};
    let mut out = Vec::new();
    // A seat whose exit was accepted is its exit's to end (`wsp-615`): one
    // ending per seat, and that one writes the pass as this would.
    let owned = crate::ending::exit_owns(store);
    let due = |p: &crate::verification::Pass| p.state != State::Running && !p.pane.as_ref().is_some_and(|s| owned.contains(s));
    for t in store.tasks_shared().iter().filter(|t| t.body.contains("## Verification")) {
        let done = passes(t).iter().any(|p| p.pane.as_deref().is_some_and(|seat| end_due(&p.ending, seats, seat)) && due(p));
        if done {
            out.extend(end_passes(store, seats, &t.id, due));
        }
    }
    out
}

/// End the seats of a member's passes that `which` picks, **once each**.
///
/// The outcome is written on the pass before anything else can ask again:
/// `ended`, or `end failed: …`. A failure is said in `cycle.log` and told to
/// the run's governor **once** — wsp-176 item 1, a verifier whose despawn
/// failed was tried again every tick, each attempt a line and none of them a
/// different answer. It is still tried again, quietly, on the next tick while
/// the seat is there ([`end_due`], `wsp-092`): a seat whose end failed on a
/// passing race kept its claim, and a rework leg queued behind it, until a
/// person ended it by hand.
///
/// **Ended means gone, as for the member** (`wsp-193`): the seat's process
/// tree is read before the despawn, and the pass is `ended` only once none of
/// it is running. A pid still running after it is `end failed`, told on the
/// same once-only terms; a pane that is gone is not tried again for it. A
/// seat with no pid to confirm by is not ended at all —
/// see [`crate::ending::verifier_pids`] — and stays standing to be asked again.
///
/// Returns the seats it ended.
pub(crate) fn end_passes(store: &Store, seats: &dyn Seats, member: &str, which: impl Fn(&crate::verification::Pass) -> bool) -> Vec<String> {
    end_passes_keeping(store, seats, member, false, which)
}

/// [`end_passes`], with `--keep-tree` when `keep`: the ending of a verifier's
/// accepted exit ([`crate::ending::end_exit`], `wsp-615`), which keeps the tree
/// the verifier read in.
pub(crate) fn end_passes_keeping(
    store: &Store,
    seats: &dyn Seats,
    member: &str,
    keep: bool,
    which: impl Fn(&crate::verification::Pass) -> bool,
) -> Vec<String> {
    use crate::verification::{passes, write, Ending};
    let Some(t) = store.find_task(member) else { return Vec::new() };
    let due: Vec<_> = passes(&t)
        .into_iter()
        .filter(|p| p.pane.as_deref().is_some_and(|seat| end_due(&p.ending, seats, seat)) && which(p))
        .collect();
    let mut ended = Vec::new();
    for p in due {
        let Some(seat) = p.pane.clone() else { continue };
        let pids = match crate::ending::verifier_pids(seats, &seat) {
            Ok(pids) => pids,
            Err(why) => {
                let w = store.find_task(member).and_then(|t| list_of(store, &t));
                crate::ending::say_verifier_left(store, w.as_ref(), member, &seat, &p.at, &why);
                continue;
            }
        };
        // Read before the despawn: it clears the binding the session is found by.
        // No transcript is read (wsp-028): the spend is the pass's own and the
        // folded `seat.spent`'s, and what it did the `seat.did` digests'.
        let spent = crate::verification::Spent::ending(p.spent.as_ref(), None, None);
        let spent = crate::verification::Spent::with_session(spent, None);
        let did = crate::cmd_agent::did_of_seat(store, &seat, &p.at);
        let outcome = despawn_seat_keeping(&seat, keep).and_then(|()| crate::ending::confirm_gone(seats, &pids));
        let ending = match &outcome {
            Ok(()) => Ending::Ended,
            Err(why) => Ending::Failed(why.clone()),
        };
        let wrote = end_changed(&p.ending, &ending) && store.locked(|| {
            let Some(mut t) = store.find_task(member) else { return false };
            let mut ps = passes(&t);
            let Some(q) = ps.iter_mut().find(|q| q.at == p.at && q.pane == p.pane) else { return false };
            q.ending = ending.clone();
            q.spent = spent.clone();
            q.did = did.clone();
            write(&mut t, &ps);
            store.save_task(&t).is_ok()
        });
        if wrote {
            store.git_commit(&format!("wsp: {member}'s verifier in {seat} {}", if outcome.is_ok() { "ended" } else { "would not end" }));
        }
        match outcome {
            Ok(()) => {
                log_line(store, &format!("ended {seat}, which verified {member} ({}){}", p.state.as_str(), again(&p.ending)));
                ended.push(seat);
            }
            Err(_) if p.ending != Ending::Standing => {}
            Err(why) => {
                log_line(store, &format!("could not end {seat}, which verified {member}: {why} — {TRIED_AGAIN}"));
                let list = store.find_task(member).and_then(|t| list_of(store, &t));
                if let Some(w) = list {
                    tell(store, &w, &format!(
                        "wsp could not end the verifier of {member} in {seat}: {}. {}",
                        util::truncate(&why, 200),
                        told_again(&seat)
                    ));
                }
            }
        }
    }
    ended
}

/// Every barrier check that has settled and is still seated, ended — once
/// each, and never in a turn (`wsp-230`). The backstop behind the ending its
/// own verb arranged, and the only ending of a check that held: a hold is
/// where its turn ends as often as not, and the tick after is when it is idle.
pub(crate) fn checks_finished(store: &Store, seats: &dyn Seats) -> Vec<String> {
    let mut out = Vec::new();
    // A check whose exit was accepted is its exit's to end (`wsp-615`).
    let owned = crate::ending::exit_owns(store);
    for w in store.worklists() {
        // A list closed by `done` while a check was still reading has nothing
        // left for that check to decide, settled or not.
        let groups = w.groups();
        for (i, g) in groups.iter().enumerate() {
            // **Or a barrier already passed** (`wsp-385`): a second check seated
            // while the first settled it has nothing to decide either.
            let over = crate::barrier::moot(&w, &groups, i);
            for c in g.checks.iter().filter(|c| !c.open() || over) {
                let Some(seat) = c.standing() else { continue };
                if owned.contains(seat) || seats.state(seat).is_some_and(|s| s.turn_in_flight()) {
                    continue;
                }
                let ended = despawn_seat(seat);
                if let Err(why) = &ended {
                    log_line(store, &format!(
                        "{} barrier {}: the check in {seat} was not ended: {}",
                        w.id,
                        i + 1,
                        util::truncate(why, 200)
                    ));
                }
                crate::barrier::mark_ended(store, seat, ended.is_ok());
                if c.open() && ended.is_ok() {
                    // Said on the entry too, so nothing reads it as still deciding.
                    crate::barrier::settle(store, &w.id, i + 1, crate::barrier::State::Abandoned);
                }
                out.push(seat.to_string());
            }
        }
    }
    out
}

/// Seats whose ending failed here, so a pane that will not close is tried once
/// a daemon's lifetime and not once a minute.
static ORPHAN_NOT_ENDED: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// **A check pane no entry names, ended once its group's barrier has settled.**
/// `checks_finished` can only end the seat an entry records, and a start that
/// was retried records only the retry's: on 2026-11-02 `stability · barrier 4`
/// stood idle in the first try's pane after the second try's check passed the
/// barrier (`wsp-340`).
///
/// Found by label ([`crate::barrier::seat_label`]) among the backends' panes.
/// Not while the group has a check still open — the pane may be the one
/// reading it — and not a pane mid-turn.
pub(crate) fn checks_orphaned(store: &Store, seats: &dyn Seats) -> Vec<String> {
    let Some(census) = seats.census() else { return Vec::new() };
    let lists = store.worklists();
    let named: std::collections::BTreeSet<String> = lists
        .iter()
        .flat_map(|w| w.groups())
        .flat_map(|g| g.checks)
        .filter(|c| c.standing().is_some() || c.ending == crate::barrier::Ending::Failed)
        .filter_map(|c| c.pane)
        .collect();
    let mut out = Vec::new();
    for (seat, label) in census {
        let Some((list, at)) = crate::barrier::parse_label(&label) else { continue };
        if named.contains(&seat) {
            continue;
        }
        let Some(w) = lists.iter().find(|w| w.id == list) else { continue };
        let Some(g) = w.groups().into_iter().nth(at - 1) else { continue };
        let settled = w.status() == WorklistStatus::Done || (g.checks.iter().any(|c| !c.open()) && !g.checks.iter().any(|c| c.open()));
        if !settled || seats.state(&seat) != Some(State::Idle) {
            continue;
        }
        if ORPHAN_NOT_ENDED.lock().is_ok_and(|failed| failed.contains(&seat)) {
            continue;
        }
        log_line(store, &format!("{list}: the check in {seat} outlived its barrier {at} — ending it"));
        match despawn_seat(&seat) {
            Ok(()) => out.push(seat),
            Err(why) => {
                log_line(store, &format!("{list}: the check in {seat} was not ended: {} — not tried again", util::truncate(&why, 200)));
                if let Ok(mut failed) = ORPHAN_NOT_ENDED.lock() {
                    failed.insert(seat);
                }
            }
        }
    }
    out
}

/// `wsp despawn --pane <seat>`, as a process — a verifier holds no claim, so
/// its seat is the only handle there is.
///
/// **From `/`**, not from wherever this process stands: the ending a check's
/// `go` launches stands in the check's own tree, and `despawn` kept every one
/// of them for "you are standing in it" (`wsp-289`).
pub(crate) fn despawn_seat(seat: &str) -> Result<(), String> {
    despawn_seat_keeping(seat, false)
}

/// The argv of `wsp despawn`, with `--keep-tree` when the tree is to stay.
fn despawn_argv(target: &[&str], keep: bool) -> Vec<String> {
    let mut argv: Vec<String> = vec!["despawn".into()];
    argv.extend(target.iter().map(|s| s.to_string()));
    if keep {
        argv.push("--keep-tree".into());
    }
    argv
}

/// [`despawn_seat`], leaving the tree when `keep`: a member handed to its
/// verifier, which reads in it (`wsp-430`).
pub(crate) fn despawn_seat_keeping(seat: &str, keep: bool) -> Result<(), String> {
    if cfg!(test) {
        #[cfg(test)]
        tests::ON_END.with(|h| h.borrow().as_ref().map(|f| f(seat)));
        #[cfg(test)]
        return tests::ENDED.with(|s| {
            s.borrow_mut().push(seat.to_string());
            if keep {
                tests::KEPT.with(|k| k.borrow_mut().push(seat.to_string()));
            }
            tests::END_FAIL.with(|f| f.borrow().clone()).map_or(Ok(()), Err)
        });
    }
    let exe = wsp_base::util::self_exe().map_err(|e| e.to_string())?;
    let out = Command::new(exe)
        .args(despawn_argv(&["--pane", seat], keep))
        .current_dir("/")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// [`despawn_seat_keeping`] with the tree kept and `--why`, for a caller that
/// may stand in the primary seat, whose despawn gives a why (`wsp-344`'s
/// `wsp done` on a verified row whose seat sits idle). Its gate is despawn's.
pub(crate) fn despawn_seat_because(seat: &str, why: &str) -> Result<(), String> {
    if cfg!(test) {
        return despawn_seat_keeping(seat, true);
    }
    let exe = wsp_base::util::self_exe().map_err(|e| e.to_string())?;
    let mut argv = despawn_argv(&["--pane", seat], true);
    argv.extend(["--why".to_string(), why.to_string()]);
    let out = Command::new(exe).args(argv).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// `wsp despawn <id>`, as a process, for the same reason every other spawn is:
/// the ending is a verb's whole behaviour and reimplementing it here would be a
/// second answer that could disagree with it.
pub(crate) fn despawn(store: &Store, id: &str) {
    let _ = despawned(store, id);
}

/// [`despawn`], answering whether `wsp despawn` succeeded — `Err` carries what
/// it said. For the caller that has to record an ending exactly once and a
/// failure exactly once, which a fire-and-forget cannot tell apart.
pub(crate) fn despawned(store: &Store, id: &str) -> Result<(), String> {
    despawned_keeping(store, id, false)
}

/// [`despawned`], with `--keep-tree` when `keep`.
pub(crate) fn despawned_keeping(store: &Store, id: &str, keep: bool) -> Result<(), String> {
    say_uncommitted(store, id);
    if cfg!(test) {
        #[cfg(test)]
        if let Some(why) = tests::DESPAWN_FAILS.with(|f| f.borrow().clone()) {
            return Err(why);
        }
        #[cfg(test)]
        tests::ENDED.with(|s| s.borrow_mut().push(id.to_string()));
        #[cfg(test)]
        if keep {
            tests::KEPT.with(|s| s.borrow_mut().push(id.to_string()));
        }
        return Ok(());
    }
    let Ok(exe) = wsp_base::util::self_exe() else { return Err("could not find wsp itself".into()) };
    let out = Command::new(exe).args(despawn_argv(&[id], keep)).stdin(Stdio::null()).output();
    let (said, result) = match out {
        Ok(o) if o.status.success() => ("ended".to_string(), Ok(())),
        Ok(o) => {
            let why = String::from_utf8_lossy(&o.stderr).trim().to_string();
            (format!("not ended: {why}"), Err(why))
        }
        Err(e) => (format!("not ended: {e}"), Err(e.to_string())),
    };
    // **`wsp despawn` says whether it kept a tree, and so do we.** Ending is the
    // whole of `despawn` — agent, claim, tree — and the tree is kept whenever it
    // has uncommitted work or somebody is standing in it. That is the right
    // behaviour and it is invisible from here unless it is carried: a run that
    // closed its group leaving five trees behind is a state a governor has to be
    // able to find, and `cycle.log` is where the run's history is read.
    let kept = said.contains("kept") || said.contains("uncommitted");
    log_line(store, &format!("despawn {id}: {said}"));
    kept.then(|| log_line(store, &format!("despawn {id}: its tree was kept — `wsp wip` shows what is standing in it")));
    result
}

/// Before an agent is ended, whatever ended it: the uncommitted work its tree
/// holds, said in `cycle.log` and on the row. `wsp-171`'s second half.
///
/// **Before, and not only after**, because the line after is `despawn`'s own
/// "its tree was kept", which says the tree survived and not that eight hours of
/// somebody's work is in it — the shape `cpd-254` left on 2026-10-05, found by
/// a person reading the tree. **On the row as well**, because the row is what
/// the restart is briefed from; [`crate::cmd_spawn`] says it again in the
/// work order itself, read off the tree at the moment the next agent starts.
fn say_uncommitted(store: &Store, id: &str) {
    let Some(cwd) = store.claims().get(id).and_then(|c| c.get("cwd")).and_then(|c| c.as_str()).map(util::expand) else {
        return;
    };
    // Only the agent's own tree, named after its task. A verifier, or a member
    // started with `--no-tree`, stands in the trunk, and what is uncommitted
    // there is whoever else is standing in it — not this agent's to leave.
    let dir = std::path::Path::new(&cwd);
    if dir.file_name().and_then(|n| n.to_str()) != Some(id) {
        return;
    }
    let n = crate::cmd_checkout::uncommitted(dir);
    if n == 0 {
        return;
    }
    let tree = util::contract(std::path::Path::new(&cwd));
    log_line(store, &format!(
        "ending {id} with {n} uncommitted path(s) in {tree} — the tree is kept, and whoever starts it next inherits them"
    ));
    let id = id.to_string();
    store.locked(|| {
        let Some(mut t) = store.find_task(&id) else { return false };
        t.log(&format!(
            "wsp: ended with {n} uncommitted path(s) in {tree} — the last agent's work, not committed; read `git status` there before changing anything"
        ));
        t.touch();
        store.save_task(&t).is_ok()
    });
}

/// The seat that answers for a run: the list's own, when somebody holds it,
/// and otherwise — for a list run by hand — the seat routing reaches for its
/// first member. A list wsp runs has only its own; see [`the_chain`].
/// Whether a governor's seat is filled for this run — [`governing_scope`]
/// answering (`wsp-606`).
pub(crate) fn governor_seated(store: &Store, w: &Worklist) -> bool {
    governing_scope(store, w).is_some()
}

fn governing_scope(store: &Store, w: &Worklist) -> Option<String> {
    let governors = store.governors();
    the_chain(store, w).into_iter().find(|s| crate::governors::seat_of_scope(s, &governors).is_some())
}

/// The scope whose seat is *meant* to answer for this run — the first in the
/// same chain with a post at all, whether it is filled, vacated, or has never
/// been filled.
///
/// **A different question from [`governing_scope`], and the reconciler needs
/// this one.** "Who answers for this run" and "whose seat is supposed to" come
/// apart in exactly the case `wsp-148` is about: a governor whose pane died is
/// *vacated* by `reconcile` before the reconciler's next pass, so the filled
/// answer is `None` and filling it would put a seat on the list — changing the
/// shape of a run that was governed from its project's seat, and giving a
/// project two governors for one question. Since `wsp-202` that shape is only a
/// list run by hand's: a list wsp runs is its own post, and nothing above it governs
/// the run — its home project's seat is above it only for what it cannot
/// answer (`wsp-263`, [`crate::governors::seat_above`]).
///
/// So the chain is walked once and read two ways, and the scope this names is
/// the one whose record the repair will look at. It is [`governing_scope`] with
/// the fill test replaced by *is there a post here at all*, which is what
/// `Slot`'s own docs argue a vacant record is.
pub(crate) fn governing_post(store: &Store, w: &Worklist) -> Option<String> {
    let governors = store.governors();
    the_chain(store, w)
        .into_iter()
        .find(|s| governors.contains_key(s) || crate::cmd_govern::last_seat(&governors, s).is_some())
}

/// `list, then the project of its first member that has one, then that project's
/// ancestors` — the walk [`crate::governors::seat_for`] documents and this
/// file has now asked for twice.
///
/// One walk rather than two, because the two readers above have to agree about
/// the order and a chain spelled twice is a chain that will be spelled
/// differently one day. A list that stands nowhere and has no member with a
/// project is one scope long, which is the ordinary case for a list nobody has
/// filled.
///
/// **A list wsp runs is one scope long, whatever its members stand in** —
/// `wsp-202`, decided by the wsp seat on 2026-10-06. Walking on to the project
/// had the run governed by a seat that answers for the whole project: the
/// reconciler saw a post and seated nothing, a pass rotated the *project's*
/// governor, and a dead project seat was the one refilled for the run. wsp-149's
/// acceptance run needed one hand `spawn --govern` to get a governor of its
/// own. So the post is the list's, the reconciler fills it as a post nobody has
/// ever filled, and the project seat is never rotated, refilled or ended for a
/// run. A list run by hand keeps the walk, because its governor is whoever is
/// running it and that is often the project's.
fn the_chain(store: &Store, w: &Worklist) -> Vec<String> {
    let mut chain = vec![w.id.clone()];
    if w.runs_itself() {
        return chain;
    }
    let index = crate::resolve::Index::new(store.projects());
    if let Some(first) = w.groups().iter().flat_map(|g| g.members.clone()).find_map(|m| store.find_task(&m)) {
        if let Some(project) = first.project {
            chain.extend(std::iter::once(project.clone()).chain(index.ancestors(&project)));
        }
    }
    chain
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
    log_line(store, &format!("tell {}: {}", w.id, util::truncate(text, 120)));
    tell_now(store, w, text);
}

/// [`tell`], for a sentence whose fact the caller has already written to
/// `cycle.log` on its own line: the delivery without a second line saying it
/// (`wsp-338`). A repair's stamp followed by `tell <list>: <the same fact>`
/// was two lines a minute for one member in the file a governor reads.
pub(crate) fn tell_logged(store: &Store, w: &Worklist, text: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::TOLD.with(|s| s.borrow_mut().push(text.to_string()));
        return;
    }
    tell_now(store, w, text);
}

/// [`tell`]'s delivery, which [`hand_on_what_done_lists_held`] takes too:
/// returns whether anybody, or the record on a member, took it.
///
/// **A list wsp runs with nobody coming to its post says so** (`wsp-216`).
/// The notice on a member reaches the nearest filled seat above, which is not
/// the seat the sentence was written for — so it is told whose it was, and
/// why it has it.
fn tell_now(store: &Store, w: &Worklist, text: &str) -> bool {
    if hand_it_to_the_governor(store, w, text) {
        return true;
    }
    let w = store.worklist(&w.id).unwrap_or_else(|| w.clone());
    let Some(first) = noticed_on(&w) else { return false };
    let text = match w.runs_itself().then(|| nobody_coming(store, &w.id)).flatten() {
        Some(why) => format!("Meant for the {} list's own seat, and nobody is coming to it: {why}.\n\n{text}", w.id),
        None => text.to_string(),
    };
    // wsp's own notice, for the seat that answers for the member — not a
    // `wsp flag` with no pane behind it, which recorded the run as a person
    // and put every barrier pass on the person's panel (`wsp-272`).
    crate::cmd_agent::notice(store, &first, &text).is_some()
}

/// wsp's notice on a row on no list, for the seat that answers for it (its
/// project's custodian, else the person): [`crate::cmd_agent::notice`], here so
/// the engine modules that tell a row's stall reach it through the run and not
/// up into the seats (`wsp-690`). Returns whether it was raised.
pub(crate) fn notice_on_row(store: &Store, member: &str, text: &str) -> bool {
    crate::cmd_agent::notice(store, member, text).is_some()
}

/// The member [`tell_now`] puts a run's notice on: the first of the group the
/// run stands at, where a person looking at the run is looking — not group 1's,
/// long since finished.
fn noticed_on(w: &Worklist) -> Option<String> {
    let groups = w.groups();
    let here = groups.iter().find(|g| g.verdict.trim().is_empty()).or_else(|| groups.last());
    here.and_then(|g| g.members.first()).cloned()
}

/// Whether a notice [`tell_now`] puts on this list's member reaches a seat, as
/// against the person's panel.
///
/// **The walk `cmd_govern::answering_seat` makes, from the layer below it**
/// ([`crate::governors::seat_for`]: the member's running list, then its
/// project). Its third step, the governor whose work order started the row,
/// never applies here: a run member's log names the run as its starter, which
/// ends that walk.
fn notice_reaches_a_seat(store: &Store, w: &Worklist) -> bool {
    let Some(task) = noticed_on(w).and_then(|m| store.task(&m)) else { return false };
    let index = crate::resolve::Index::new(store.projects());
    let lists = worklist::Running::read(store);
    let governors = store.governors();
    crate::governors::seat_for(&governors, &index, lists.list_for(&task), task.project.as_deref(), None).is_some()
}

/// Whether a sentence asks anybody for anything, by the rule a turn's last
/// words are read by ([`crate::waiting::last_words`]), asked of each paragraph.
fn asks_anything(text: &str) -> bool {
    text.split("\n\n").any(|p| crate::waiting::last_words(p).is_some_and(|w| w.asks))
}

/// Why nobody is coming to a list's own post, or `None` while somebody is in
/// it or on the way.
///
/// **The governor's rule for where a self-run list's sentences go**
/// (`wsp-216`, cpd-413's call). While the post is seated or a reseat is in
/// flight, they wait on the list's scope — `wsp-202`, and
/// [`crate::governors::seat_for`] for a member's ask. Once nobody is coming —
/// the list is done, its post was stood down, or its reseat failed and backed
/// off — they go to the nearest filled seat above, saying whose they were.
/// A late answer from the project's seat beats none; that seat is still never
/// rotated, refilled or ended for the run. A held or parked list is coming: a
/// `go` or a `resume` puts it back where the reconciler seats it.
pub(crate) fn nobody_coming(store: &Store, list: &str) -> Option<String> {
    if store.worklist(list).is_some_and(|w| w.status() == WorklistStatus::Done) {
        return Some("the list is done".into());
    }
    let governors = store.governors();
    if let Some(at) = crate::governors::stood_at(&governors, list) {
        return Some(format!("its seat was stood down at {}", util::local_hm(at)));
    }
    if crate::governors::vacancy(&governors, list).backed_off() {
        let why = crate::cmd_govern::reseat_why(&governors, list).map(|w| format!(" — {}", util::truncate(&w, 120)));
        return Some(format!("its reseat failed{}", why.unwrap_or_default()));
    }
    None
}

/// What a done list's own post was still holding, handed on (`wsp-216`).
///
/// **A sentence held on a post nobody is coming to is a sentence for nobody.**
/// `wsp-acceptance-fixes` closed with two held on its scope — a member left at
/// `doing`, and the barrier-3 verdict — and its spool read `the list is done,
/// so nobody is coming` for a week. Each is said again through [`tell_now`],
/// which for a done list is the notice on a member, reaching the nearest filled
/// seat above it or the person, and taken off the spool once something has it.
/// Messages only: the news beside them is the attention pass's, about members
/// long settled, and is withheld from a seat anyway (`wsp-166`).
///
/// Read off the spools rather than the lists, so a tick costs one read of
/// `watches.json` and a list only where a spool holds a message.
pub(crate) fn hand_on_what_done_lists_held(store: &Store) {
    for key in store.watches().keys() {
        let Some(scope) = key.strip_prefix("wake:") else { continue };
        let held = crate::wake::messages_held(store, scope);
        if held.is_empty() {
            continue;
        }
        let Some(w) = store.worklist(scope) else { continue };
        if !w.runs_itself() || w.status() != WorklistStatus::Done {
            continue;
        }
        // **Needs-you means act** (`wsp-151`, the `wsp-095` family). With no
        // seat above, the notice lands on the person's panel, and cr-linux
        // closed with 26 of 26 held sentences put there — started, parked,
        // resumed, verified, none of them asking anything. The news of a run
        // that has finished is nobody's to act on: a sentence that asks nothing
        // is let go rather than raised. A seat above still hears all of it,
        // because a governor is kept current where a person is asked.
        let to_a_seat = notice_reaches_a_seat(store, &w);
        let mut handed = Vec::new();
        let mut let_go = Vec::new();
        for (seq, at, text) in &held {
            if !to_a_seat && !asks_anything(text) {
                let_go.push(*seq);
                continue;
            }
            // The date as well as the clock: what a done list was left holding
            // is days old as often as not, and `15:42` alone reads as today.
            let since = format!("{} {}", util::local_ymd(&util::iso_at(*at)), util::local_hm(*at));
            if tell_now(store, &w, &format!("(Held on its scope since {since}.) {text}")) {
                handed.push(*seq);
            }
        }
        crate::wake::settle_held(store, scope, &[handed.as_slice(), let_go.as_slice()].concat());
        // Said when something moved, not every tick something could not: a
        // done list with no member to put a notice on would otherwise write
        // this line a minute for ever. What stays is still on `--status`.
        if handed.is_empty() && let_go.is_empty() {
            continue;
        }
        let dropped = match let_go.len() {
            0 => String::new(),
            n => format!(", and let go {n} that asked nothing, with no seat above to keep current"),
        };
        log_line(store, &format!(
            "{scope} is done and its own seat with it — handed on {} of the {} sentence(s) held for it{dropped}",
            handed.len(),
            held.len()
        ));
    }
}

/// The line the governor above a list hears for one of its events, or `None`
/// for an event it is not told — `drop` is composing, not running.
///
/// One line and a pointer, never the verdict in full: the list's own governor
/// is the one that acts on a barrier, and the project's is being kept current,
/// so a sentence it has to read twice is a context read spent on somebody
/// else's decision.
fn heard(w: &Worklist, event: &str, passed: Option<usize>) -> Option<String> {
    let id = &w.id;
    let of = w.groups().len();
    Some(match (event, passed) {
        ("go", Some(n)) => {
            let verdict = w.groups().get(n - 1).map(|g| util::truncate(g.verdict.trim(), 160)).unwrap_or_default();
            format!("The {id} list passed barrier {n} of {of}: {verdict} · wsp worklist show {id}")
        }
        ("go", None) => format!("The {id} list is running ({of} groups) · wsp worklist show {id}"),
        ("hold", _) => format!("The {id} list was held at its barrier · wsp worklist show {id} says why"),
        ("followup", _) => {
            format!("The {id} list's barrier holds pending follow-ups, which wsp runs first · wsp worklist show {id}")
        }
        ("park", _) => format!("The {id} list was parked: nothing starts until it is resumed · wsp worklist show {id}"),
        ("resume", _) => format!("The {id} list was resumed where it was parked · wsp worklist show {id}"),
        ("reopen", _) => format!(
            "The {id} list was held at its barrier and a member was sent back: it runs that group again, with no verdict recorded · wsp worklist show {id}"
        ),
        _ => return None,
    })
}

/// One line to the governor of the project a list stands in: **the project's
/// governor hears its lists** (`wsp-263`). A barrier passed, held or followed
/// up, a list started, parked, resumed or done — so the custodian knows what
/// its lists are doing without polling them, now that a list's seat answers
/// to it ([`crate::governors::seat_above`]).
///
/// Spooled, like everything that reaches a seat, so a busy custodian hears it
/// on its next idle rather than having it typed over a turn. Recorded rather
/// than said under `cfg(test)`, as [`tell`] is; [`hand_it_up`] is the half that
/// decides, and is driven on its own.
pub(crate) fn tell_above(store: &Store, w: &Worklist, text: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::HEARD.with(|s| s.borrow_mut().push(text.to_string()));
        return;
    }
    if let Some(scope) = hand_it_up(store, w, text) {
        log_line(store, &format!("told {scope}, above {}: {}", w.id, util::truncate(text, 120)));
    }
}

/// The scope [`tell_above`] said `text` to, or `None` where nobody above the
/// list is seated — the person reads a list on the panel, and a line to
/// everyone about every barrier is the noise `wsp-272` took out.
///
/// **Not the seat that already heard it.** A list run by hand from its
/// project's seat is governed by that seat, and [`tell`] has given it the whole
/// sentence; and a custodian that typed the verb itself does not need its own
/// words back.
pub(crate) fn hand_it_up(store: &Store, w: &Worklist, text: &str) -> Option<String> {
    let above = crate::worklist::seat_above_in(store, &w.id)?;
    if governing_scope(store, w).as_deref() == Some(above.scope.as_str()) {
        return None;
    }
    if crate::cmd_message::caller(store) == above.scope {
        return None;
    }
    crate::wake::say(store, &above.scope, text, None);
    Some(above.scope)
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
///
/// **The `false` is [`governing_scope`]'s answer and not `say`'s.** `say` spools
/// whatever it is given and reports, so it no longer says "there was nowhere to
/// put this" — and it should not, because a vacancy is later rather than nowhere.
/// Whether a run has a governor to hand a barrier to is a different question, and
/// it is the older one: a scope a person stood down has none, and a hand on a
/// member is exactly right there.
pub(crate) fn hand_it_to_the_governor(store: &Store, w: &Worklist, text: &str) -> bool {
    // **A list wsp runs has a post the reconciler is about to fill**, so with
    // nobody in it yet the verdict waits there — the first tick after `go`
    // seats it, and a hand on a member would route to the project's seat this
    // list no longer answers to. **Only while somebody is coming to it**
    // (`wsp-216`): stood down, done, or a reseat failed and backed off is
    // nowhere, and the caller's hand on a member reaches the seat above.
    let own = || (w.runs_itself() && nobody_coming(store, &w.id).is_none()).then(|| w.id.clone());
    let Some(scope) = governing_scope(store, w).or_else(own) else { return false };
    let report = crate::wake::say(store, &scope, text, None);
    // Said as well as returned, because `cycle.log` is how a governor reads what
    // the run has been doing, and *the seat is mid-turn* is the answer to "why
    // has nobody started my next group" that used to be missing from it. The
    // reason rides in `report.why` now, so a scope with no seat still says so
    // here — it used to be a second, hand-written copy of that sentence.
    log_line(store, &format!("told {scope}: {} · {} held", report.why, report.held));
    true
}

/// A pass hands the governing seat to a fresh successor, on wsp's own
/// initiative: [`Seats::rotate`] carries the argument.
fn rotate(store: &Store, seats: &dyn Seats, w: &Worklist) {
    if cfg!(test) {
        #[cfg(test)]
        tests::ROTATED.with(|s| s.borrow_mut().push(w.id.clone()));
        return;
    }
    let Some(scope) = governing_scope(store, w) else { return };
    let code = seats.rotate(store, &scope);
    log_line(store, &format!("rotate {scope}: exit {code}"));
}

pub(crate) use crate::store::log_line;

#[cfg(test)]
pub(crate) mod tests {
    /// `wsp-538`: the reason a member gave for a full suite is in its verifier's
    /// order, so an unneeded run can be challenged.
    #[test]
    fn a_verifiers_order_shows_the_reason_for_a_full_suite_run_in_the_members_seat() {
        let mut m = Task::new("a member", "m-1");
        assert_eq!(full_suite_line(&m), "");
        m.log(&format!("{} the fixture every test reads", crate::throttle::FULL_SUITE_LOGGED));
        let line = full_suite_line(&m);
        assert!(line.contains("\"the fixture every test reads\"") && line.contains("once") && line.contains("say so in your verdict"), "{line}");
    }

    use super::*;
    use crate::store::a_reader;
    pub(crate) use crate::store::SAID;
    use std::cell::RefCell;

    /// No backend answers for any seat.
    ///
    /// **The right answer for most of these tests, not a dodge.** A store with
    /// no project has no member with a seat, and `state_of_a_seat_nobody_can
    /// answer_for` must not hold a barrier — so a test about the *chain* wants
    /// a reader that says nothing rather than one that happens to say idle. The
    /// tests that are about the gate name the seats they need.
    pub(crate) struct Blind;

    impl Seats for Blind {
        fn state(&self, _seat: &str) -> Option<State> {
            None
        }
    }

    /// Every seat names a process, and every process has exited by the time
    /// anyone looks — what a verifier's ending is confirmed by (`wsp-193`).
    pub(crate) struct Exits;

    impl Seats for Exits {
        fn state(&self, _seat: &str) -> Option<State> {
            None
        }
        fn pids(&self, _seat: &str) -> Option<Vec<u32>> {
            Some(vec![1])
        }
        fn running(&self, _pids: &[u32]) -> Vec<u32> {
            Vec::new()
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
        /// What the governor above a list was told (`wsp-263`) — distinct from
        /// `TOLD`, the list's own seat, as `MEMBER_TOLD` is.
        pub(crate) static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Set, and a member's pick on this thread stalls this long between its
        /// read of the row and its start, as load stalled compounding-428's.
        pub(crate) static STALL: std::cell::Cell<Option<std::time::Duration>> = const { std::cell::Cell::new(None) };
        /// Set, and every start in this thread fails with it.
        pub(super) static FAIL: RefCell<Option<String>> = const { RefCell::new(None) };
        /// Set, and the pack builder is missing for every start in this thread.
        pub(super) static PACK_MISSING: RefCell<Option<String>> = const { RefCell::new(None) };
        /// Set, and every verifier seat this thread ends fails with it.
        pub(super) static END_FAIL: RefCell<Option<String>> = const { RefCell::new(None) };
        /// Set, and it runs for each seat an end on this thread ends, standing
        /// in for what a real `wsp despawn` does in its own process — as its
        /// release writing on the row's log (`wsp-509`).
        pub(crate) static ON_END: RefCell<Option<Box<dyn Fn(&str)>>> = const { RefCell::new(None) };
        /// The ends that asked for the tree to be kept (`--keep-tree`).
        pub(crate) static KEPT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// What `wsp-147`'s reconciler told a member's seat, as (task, sentence).
        /// Distinct from `TOLD`, which is the run's *governing* seat: a repair
        /// tells both and they are answers to different questions.
        pub(crate) static MEMBER_TOLD: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
        /// The scopes `wsp-148`'s repair filled a seat on this thread. A record
        /// of what was *asked for*, like `ROTATED` beside it, because the verb it
        /// launches is one process per fill and a test cannot run it.
        pub(crate) static RESEATED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Set, and every despawn in this thread is refused with it.
        pub(crate) static DESPAWN_FAILS: RefCell<Option<String>> = const { RefCell::new(None) };
        /// Set, and a member's seat takes no tell on this thread (`deliver` is false).
        pub(crate) static TELL_REFUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// Set, and a member's seat reads unvouched to `wsp tell` on this thread: a
        /// tell without `--anyway` is refused (`deliver_as` is `No`), one with it lands
        /// (`wsp-658`: idle to the run, `cannot say whether it is at a prompt` to the verb).
        pub(crate) static TELL_UNVOUCHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// Set, and a member's seat answers every tell "delivery still pending" on this
        /// thread (`deliver_as` is `Pending`), ahead of [`TELL_REFUSED`].
        pub(crate) static TELL_PENDING_BACKEND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// The barrier checks whose ending a verb arranged on this thread
        /// (`wsp-230`) — a launched process each, which a test cannot run.
        pub(crate) static CHECKS_ENDING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// The batches the run asked to close on this thread, as `<row> batch <n>`.
        pub(crate) static CLOSED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Set, and a batch's close on this thread answers with it; else clean.
        pub(crate) static CLOSE_AS: std::cell::Cell<Option<fn(&Store, &str, usize) -> crate::piece_close::Closed>> = const { std::cell::Cell::new(None) };
        /// How many steps ran on this thread under their list's lock (`wsp-663`).
        pub(crate) static STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        /// Set, and the next step on this thread runs it once it holds its list:
        /// the advances that arrive while a step runs.
        pub(crate) static DURING_STEP: std::cell::Cell<Option<fn(&Store, &Worklist)>> = const { std::cell::Cell::new(None) };
        /// The wakes a failed start scheduled on this thread (`wsp-087`), as
        /// the argv [`wake_after`] launches: a process a test cannot run.
        pub(crate) static WAKES: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
    }

    fn spawned() -> Vec<(String, String)> {
        SPAWNED.with(|s| s.borrow_mut().drain(..).collect())
    }

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        crate::testkit::isolated_store(&format!("cycle-{tag}"))
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

    /// `wsp-206`'s verifier: a pass reads the list, the running group's stop
    /// is amended, and then the pass opens the barrier. The amendment saw no
    /// check and said the barrier would be handed it, so the check's order has
    /// to carry the stop as stored and not the one the pass read — which it
    /// does since `wsp-230` by being composed from the list when it is asked.
    #[test]
    fn a_stop_amended_while_a_pass_was_reading_is_the_one_the_barrier_is_handed() {
        let (_env, store) = scratch("amend-mid-pass");
        task(&store, "m-1", Status::Review);
        let stale = list(&store, &[(&["m-1"], "claude")]);
        let mut w = store.worklist("run").unwrap();
        let mut groups = w.groups();
        groups[0].stop = "sonnet stays at 2/10".into();
        w.set_groups(&groups);
        store.save_worklist(&w).unwrap();

        assert!(crate::barrier::open(&store, &stale, 1, None, None, &|_| false).is_some(), "a check opens");
        let order = check_order(&store, "run", 1).expect("an order");
        assert!(order.contains("sonnet stays at 2/10"), "the barrier was handed a stop already corrected: {order}");
    }

    /// `wsp-345`: a barrier check files what it finds as follow-ups of the
    /// member (`--from`), which never hold the member open, and is no longer
    /// told to file them as its sub-tasks.
    #[test]
    fn a_barrier_check_files_its_findings_with_from_and_never_as_sub_tasks() {
        let mut w = Worklist::new("run", "run");
        w.set_groups(&[Group { members: vec!["m-1".into()], agent: "claude".into(), ..Group::default() }]);
        let g = w.groups().remove(0);
        let order = barrier_order(&w, 1, &g, "");
        assert!(order.contains("`wsp add \"…\" --from <member>`"), "{order}");
        assert!(!order.contains("--parent"), "{order}");
    }

    /// `wsp-326`: the check is handed the members, their verdicts, the stop's
    /// mechanical items and — when wsp ran the barrier — what it found, so it
    /// spends its turns on what only it can say; and with no run, it is told to
    /// build and test itself.
    #[test]
    fn the_checks_order_carries_the_packet_and_says_whether_wsp_already_ran_the_suite() {
        let (_env, store) = scratch("packet-order");
        task(&store, "m-1", Status::Review);
        let stale = list(&store, &[(&["m-1"], "claude")]);
        let mut w = store.worklist("run").unwrap();
        let mut groups = w.groups();
        groups[0].stop_checks = vec!["gate (1m): sh gate.sh".into()];
        w.set_groups(&groups);
        store.save_worklist(&w).unwrap();
        assert!(crate::barrier::open(&store, &stale, 1, None, None, &|_| false).is_some());

        let bare = check_order(&store, "run", 1).expect("an order");
        assert!(bare.contains("The packet") && bare.contains("m-1"), "{bare}");
        assert!(bare.contains("gate (1m): sh gate.sh"), "the stop's mechanical items are shown: {bare}");
        assert!(bare.contains("ran no checks for this barrier"), "{bare}");
        // The packet ends with the check's exit template, submitted for the list (`wsp-617`).
        let w = store.worklist("run").unwrap();
        let packet = packet_text(&store, &w, 1, &w.groups()[0]);
        assert!(packet.ends_with(&crate::closing::exit_template(crate::exit::Kind::Check, "run", &[])), "{packet}");
        assert!(bare.contains("verdict: go | hold | followup") && bare.contains("`wsp submit run --dry-run` once"), "{bare}");

        let file = crate::barrier_run::packet_file(&store, "run", 1);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "read: abc123\n  ✗ full · cargo test\n`full` failed — attributed to m-1.\n").unwrap();
        let ran = check_order(&store, "run", 1).expect("an order");
        assert!(ran.contains("Do not build or run them again") && ran.contains("attributed to m-1"), "{ran}");
        assert!(!ran.contains("ran no checks"), "{ran}");
    }

    /// The checks on group `at` of the `run` list.
    fn checks(store: &Store, at: usize) -> Vec<crate::barrier::Check> {
        store.worklist("run").unwrap().groups()[at - 1].checks.clone()
    }

    /// The open check on group `at` seated, as `wsp spawn --check` does.
    fn check_seated(store: &Store, at: usize, seat: &str) {
        assert!(crate::barrier::name_seat(store, "run", at, seat), "no open check on group {at} to seat");
    }

    fn tagged(store: &Store, tag: &str) -> Vec<Task> {
        store.tasks().into_iter().filter(|t| t.tags.iter().any(|g| g == tag)).collect()
    }

    /// The members with a pass still reading: what a Verify row in `todo` used
    /// to be (`wsp-188`).
    fn reading(store: &Store) -> Vec<String> {
        store
            .tasks()
            .iter()
            .filter(|t| crate::verification::passes(t).last().is_some_and(|p| p.state == crate::verification::State::Running))
            .map(|t| t.id.clone())
            .collect()
    }

    fn passes_on(store: &Store, id: &str) -> Vec<crate::verification::Pass> {
        crate::verification::passes(&store.find_task(id).unwrap())
    }

    /// Every pass still reading gets its verdict, the way `wsp verified` writes
    /// it from the verifier's own seat.
    fn verdict_all(store: &Store, state: crate::verification::State) {
        for m in reading(store) {
            let pane = passes_on(store, &m).last().and_then(|p| p.pane.clone());
            crate::verification::record(store, &m, state, "checked it on the trunk", pane.as_deref(), None).unwrap();
        }
    }

    fn hold_all(store: &Store) {
        verdict_all(store, crate::verification::State::Holds)
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
        assert_eq!(reading(&store), vec!["m-1".to_string()], "one pass, on the member that finished");
        assert!(tagged(&store, VERIFY_TAG).is_empty(), "and no row for it: the pass is on the member (wsp-188)");
        assert_eq!(passes_on(&store, "m-1")[0].on.as_deref(), Some("opencode some/model"), "what it runs on, recorded");
        let order = verifier_order(&store, &store.find_task("m-1").unwrap());
        assert!(order.contains("read-only") && order.contains("wsp verified m-1 --holds --from FILE"), "{order}");
        // `wsp-241`: and how to write the caption the verb now requires.
        assert!(order.contains("--holds --from FILE --caption") && order.contains("under 60 characters") && order.contains("no commit hashes"), "{order}");
        // `wsp-325`: and the one thing about the pass only the verifier can say.
        assert!(order.contains("checked: <the item") && order.contains("held` or `not held"), "{order}");
        assert_eq!(spawned(), vec![("m-1 · verifying".into(), "opencode".into())], "on the same policy: the floor holds");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty() && passes_on(&store, "m-1").len() == 1, "and only once");

        set(&store, "m-2", Status::Review);
        step(&store, &w, &Blind);
        let _ = spawned();
        assert!(checks(&store, 1).is_empty(), "no barrier check while a verdict is outstanding");

        hold_all(&store);
        step(&store, &w, &Blind);
        let c = checks(&store, 1);
        assert_eq!(c.len(), 1, "every member verified: one barrier check");
        assert!(c[0].open() && c[0].on.as_deref() == Some("opencode some/model"), "open, and on what: {c:?}");
        assert!(tagged(&store, BARRIER_TAG).is_empty(), "and no row for it: the check is on the group (wsp-230)");
        let order = check_order(&store, "run", 1).unwrap();
        assert!(order.contains("wsp worklist go run --from FILE --caption") && order.contains("hold run --from FILE --caption"), "{order}");
        assert!(order.contains("`the wiring is tested now`"), "with the examples: {order}");
        assert_eq!(spawned(), vec![("run · barrier 1".into(), "opencode".into())], "started on the list, as its check");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the open entry is the key; nothing starts twice");
    }

    /// `wsp-210`, `followup --blocking`: the check finds the group's own
    /// done-when unmet, files a row and attaches it. The row joins the group
    /// being run, wsp starts and verifies it like any member, the check that
    /// added it is not told its group moved under it, and once the row lands a
    /// fresh check reads the group again — told it has had its round.
    #[test]
    fn a_blocking_follow_up_joins_the_running_group_is_run_and_verified_and_the_barrier_is_checked_again() {
        let (env, store) = scratch("followup-blocking");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        check_seated(&store, 1, "cpd-chk");
        let check = "cpd-chk".to_string();
        let order = check_order(&store, "run", 1).unwrap_or_default();
        assert!(
            order.contains("wsp worklist followup run <id>… --next") && order.contains("--blocking"),
            "the check is told it can attach what it finds: {order}"
        );
        assert!(
            order.contains("A hold that owes work attaches it with `wsp worklist followup run <id>… --blocking`, and never leaves it in prose"),
            "the check is told a hold never leaves its owed work in prose: {order}"
        );

        let from = env.home().join("verdict");
        std::fs::write(&from, "m-1 does not yet meet the done-when; m-2 closes it").unwrap();
        let from = from.display().to_string();
        let args = Args::synth("worklist", &["followup", "run", "m-2"], &[("blocking", "true"), ("from", &from)]);
        assert_eq!(crate::cmd_worklist::followup_by(&store, &args, &[check.clone()]), 0);
        let w = store.worklist("run").unwrap();
        assert_eq!(w.groups()[0].members, vec!["m-1", "m-2"], "the row joins the group being run");
        assert_eq!(w.status(), WorklistStatus::Running, "held pending follow-ups, and still running them");
        assert!(w.groups()[0].verdict.is_empty(), "the barrier has not passed");
        assert_eq!(checks(&store, 1)[0].state, crate::barrier::State::Blocking, "and the check says why, on the group");
        assert!(
            TOLD.with(|t| t.borrow().iter().any(|s| s.contains("holds, pending follow-ups: m-2"))),
            "the governor is told the barrier held and why"
        );

        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-2".into(), "claude".into())], "wsp starts it on the group's line");
        assert!(MEMBER_TOLD.with(|t| t.borrow().is_empty()), "the check is not told its own follow-up moved the group");

        // The check has said all it will, and a tick ends it: the barrier it
        // answered stays where it is, and the recheck is a fresh check.
        assert_eq!(checks_finished(&store, &Blind), vec![check.clone()], "a check that held pending follow-ups has finished");
        assert_eq!(checks(&store, 1)[0].ending, crate::barrier::Ending::Ended);

        set(&store, "m-2", Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(reading(&store), vec!["m-2".to_string()], "the follow-up is verified like any member");
        assert_eq!(checks(&store, 1).len(), 1, "and the barrier waits for its verdict");
        hold_all(&store);
        step(&store, &w, &Blind);
        let c = checks(&store, 1);
        assert_eq!(c.len(), 2, "the barrier is checked again, by a fresh check");
        assert!(c[1].open() && c[0].state == crate::barrier::State::Blocking, "beside the first, not over it: {c:?}");
        let order = check_order(&store, "run", 1).unwrap_or_default();
        assert!(order.contains("one round of follow-ups (") && order.contains(&check), "and told it has had its round: {order}");
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
            .map(|(id, kind)| (id.strip_suffix(" · verifying").unwrap().to_string(), kind))
            .collect();
        assert_eq!(
            by_parent,
            vec![("m-1".into(), "opencode".into()), ("m-2".into(), "claude".into())],
            "a verifier runs on the kind its member's work ran on"
        );

        hold_all(&store);
        step(&store, &w, &Blind);
        assert_eq!(
            spawned(),
            vec![("run · barrier 1".to_string(), "opencode".into())],
            "the barrier reads the group as a whole, on the group's line"
        );
    }

    /// A verifier that found a problem blocks, the member comes back with a
    /// fix, and it is read again by a fresh agent.
    /// **`wsp-212`: what a block owed was not a commit.** wsp-217 was blocked
    /// for one `wsp install`, did it, and went back to review at the commit
    /// its verifier had read. **`wsp-529` reverses the run's half of that:** a
    /// second verifier at a commit already seen blocked spent a turn saying it
    /// had not changed (wsp-526), so the run opens none, and the block stays
    /// unanswered, which is what the governor is told. A new commit is verified.
    #[test]
    fn the_run_opens_no_verifier_at_a_commit_it_has_seen_blocked_and_one_at_a_new_commit() {
        let (_env, store) = scratch("samecommit");
        task(&store, "m-1", Status::Review);
        let mut t = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut t, &format!("{} 323faba83e4ce0ea6ae7253674a1e919cee27f78", crate::repair::LANDED)).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        verdict_all(&store, crate::verification::State::Blocks);
        assert_eq!(passes_on(&store, "m-1")[0].read.as_deref(), Some("323faba83e4c"), "read at the landing");

        // Put back by hand, with no account: moved, not returned. Past a
        // second, so the clock would say it moved if the clock were asked.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 1, "a hand move at the same commit is not a return to be verified");

        let mut t = store.find_task("m-1").unwrap();
        t.set_status(Status::Doing);
        t.log("review: installed it; no code change");
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        step(&store, &w, &Blind);
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 1, "back at review at the commit it was blocked at: no verifier");
        assert!(blocked_at_tip(&store.find_task("m-1").unwrap()).is_some());
        assert!(!answered_block(&store.find_task("m-1").unwrap()), "and the block is not answered: the governor hears of it");

        let mut t = store.find_task("m-1").unwrap();
        t.log(&format!("{} c9a5c581639b54db01dbb88ff344c8892baca5e7", crate::repair::LANDED));
        t.log("review: a real fix");
        store.save_task(&t).unwrap();
        assert!(blocked_at_tip(&store.find_task("m-1").unwrap()).is_none());
        assert!(answered_block(&store.find_task("m-1").unwrap()));
        step(&store, &w, &Blind);
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.len(), 2, "a new commit is verified");
        assert_eq!(ps[1].read.as_deref(), Some("c9a5c581639b"));
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 2, "one verifier per return to review, not one per tick");
        let _ = spawned();
    }

    /// `wsp-529`: a seat started on a sent-back row finds the reason and the
    /// verdict's head in its order; a row not sent back, or back at review,
    /// finds nothing added. Checked through the builder, not a pane.
    #[test]
    fn the_order_of_a_seat_started_after_a_send_back_carries_the_reason_and_the_verdict_head() {
        let (_env, store) = scratch("sentbackorder");
        task(&store, "m-1", Status::Review);
        let mut t = store.find_task("m-1").unwrap();
        assert!(sent_back_order(&t).is_none(), "not sent back");
        crate::log::note(&store, &mut t, &format!("{} 323faba83e4ce0ea6ae7253674a1e919cee27f78", crate::repair::LANDED)).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        verdict_all(&store, crate::verification::State::Blocks);
        let mut t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Doing);
        // The claim that seats the new agent writes its own line after the send-back.
        t.log("claimed by pane cpd-9");
        store.save_task(&t).unwrap();
        let said = sent_back_order(&store.find_task("m-1").unwrap()).expect("a sent-back row gives its seat the reason");
        assert!(said.contains("sent back") && said.contains("## Verification") && said.contains("wsp review m-1"), "{said}");
        assert!(said.is_ascii() && !said.contains('\n'), "{said:?}");
        let head = passes_on(&store, "m-1")[0].text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string();
        assert!(head.is_empty() || said.contains(&head), "the verdict's head is in it: {said}");

        let mut t = store.find_task("m-1").unwrap();
        t.log("review: fixed");
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        assert!(sent_back_order(&store.find_task("m-1").unwrap()).is_none(), "back at review: nothing to say");
        let _ = spawned();
    }

    /// **`wsp-334`: block -> re-land -> review -> re-verify.** wsp-331 was
    /// blocked at one commit, landed a fix at another and went back to review;
    /// the run told its governor that nothing had answered the block. The block
    /// is answered by the landing and the return, and a fresh verifier opens on
    /// the new commit.
    #[test]
    fn a_member_that_lands_a_fix_after_a_block_and_returns_to_review_is_verified_on_the_new_commit() {
        let (_env, store) = scratch("relanded");
        task(&store, "m-1", Status::Review);
        let mut t = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut t, &format!("{} af84a9320f7e3bf18fa5f19a6eeaefd867dacc2c", crate::repair::LANDED)).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        verdict_all(&store, crate::verification::State::Blocks);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing);
        assert!(crate::verification::blocked(&store.tasks_shared(), "m-1").is_some());
        assert!(!answered_block(&store.find_task("m-1").unwrap()), "nothing has answered it yet");

        let mut t = store.find_task("m-1").unwrap();
        t.log(&format!("{} c9a5c581639b54db01dbb88ff344c8892baca5e7", crate::repair::LANDED));
        t.log("review: answered the finding");
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        assert!(answered_block(&store.find_task("m-1").unwrap()), "a landing and a return to review answer it");

        let _ = spawned();
        step(&store, &w, &Blind);
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.len(), 2, "a fresh verifier opens on the re-landing");
        assert_eq!(ps[1].read.as_deref(), Some("c9a5c581639b"), "on the new commit");
        assert_eq!(ids(spawned()), ["m-1 · verifying".to_string()]);
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 2, "and once");
        let _ = spawned();
    }

    #[test]
    fn a_member_fixed_after_a_failed_verdict_gets_a_fresh_verifier() {
        let (_env, store) = scratch("again");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        verdict_all(&store, crate::verification::State::Blocks);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing, "a block sends the member back");
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 1, "blocked, and nothing new until the member moves");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.len(), 2, "the fix is verified again");
        assert_eq!(ps[0].state, crate::verification::State::Blocks, "and the block stays on the record beneath it");
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
        hold_all(&store);
        let _ = spawned();
        for m in ["m-1", "m-2"] {
            store.set_claim(m, serde_json::json!({ "workspace": "w" }));
        }
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        store.set_binding("cpd-2", serde_json::json!({ "task_id": "m-2" }));

        // One member's seat is mid-turn; the other is idle at a prompt.
        let seats = Fixed::new(&[("cpd-1", State::Working), ("cpd-2", State::Idle)]);
        step(&store, &w, &seats);
        assert!(checks(&store, 1).is_empty(), "no barrier over a member that is still working");
        assert!(tests::SAID.with(|s| s.borrow().iter().any(|l: &String| l.contains("cpd-1 reads working"))));

        // And it opens on the tick after that seat falls idle.
        let idle = Fixed::new(&[("cpd-1", State::Idle), ("cpd-2", State::Idle)]);
        step(&store, &w, &idle);
        assert_eq!(checks(&store, 1).len(), 1, "a tick later, and it opens");
    }

    /// `wsp-172`. A barrier opens only when every member is idle or gone, and a
    /// member that is **waiting** is neither: on a prompt, or behind its own
    /// open `wsp ask`, it stopped in the middle of something. Held with no
    /// ceiling, said every pass, and the seat is not told — the half-hour report
    /// is for a turn that may be stuck, and nothing here is stuck.
    #[test]
    fn a_barrier_holds_on_a_member_waiting_on_a_prompt_or_its_own_question() {
        let (_env, store) = scratch("waitbarrier");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        let _ = spawned();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));

        step(&store, &w, &Fixed::new(&[("cpd-1", State::Blocked)]));
        assert!(checks(&store, 1).is_empty(), "no barrier over a member on a prompt");
        assert!(tests::SAID.with(|s| s.borrow().iter().any(|l: &String| l.contains("prompt only a person can answer"))));

        let q = crate::message::Message::question(
            crate::message::Party::pane("cpd-1", ""),
            crate::message::Kind::Note,
            "which file?",
            crate::message::Waiting::new("cpd-1", "m-1"),
        )
        .about(crate::message::About::Task("m-1".into()));
        store.save_message(&q).unwrap();
        tests::TOLD.with(|t| t.borrow_mut().clear());
        step(&store, &w, &Fixed::new(&[("cpd-1", State::Idle)]));
        assert!(checks(&store, 1).is_empty(), "nor over one idle behind its own open question");
        let told = tests::TOLD.with(|t| t.borrow().clone());
        assert!(told.iter().all(|t| !t.contains("m-1")), "and nobody is told to hurry it: {told:?}");

        let mut answered = store.message(&q.id).unwrap();
        answered.state_raw = crate::message::State::Answered.as_str().into();
        store.save_message(&answered).unwrap();
        step(&store, &w, &Fixed::new(&[("cpd-1", State::Idle)]));
        assert_eq!(checks(&store, 1).len(), 1, "answered and idle: it opens");
    }

    /// compounding-617: a held member whose `## Checks` runs `wsp check` on its
    /// own row — compounding-520's, wsp-638's — can only fail the barrier, after
    /// a full suite. The barrier is not started on it, and the wait line names the
    /// line; another member's line that merely mentions a different row is no
    /// cause. Fixing the line lets the barrier open.
    #[test]
    fn a_held_member_whose_check_line_runs_its_own_check_does_not_start_the_barrier() {
        let (_env, store) = scratch("selfcheck");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        let _ = spawned();
        let write = |line: &str| {
            let mut t = store.find_task("m-1").unwrap();
            crate::model::set_section_in(&mut t.body, "Checks", line);
            store.save_task(&t).unwrap();
        };
        write("- timeline tests (2s): wsp check m-1");
        tests::SAID.with(|s| s.borrow_mut().clear());
        step(&store, &w, &Blind);
        assert!(checks(&store, 1).is_empty(), "no barrier over a check line that calls itself");
        let said = tests::SAID.with(|s| s.borrow().clone());
        let line = said.iter().find(|l| l.contains("barrier waits")).expect("the barrier says why");
        assert!(line.contains("m-1's check line calls its own check") && line.contains("timeline tests"), "{line}");

        write("- timeline tests (2s): cargo test timeline::");
        step(&store, &w, &Blind);
        assert_eq!(checks(&store, 1).len(), 1, "with the line fixed it opens");
    }

    /// `wsp-608`: react-parity-now's group 9 stood overnight on cpd-4845's
    /// "ready at 340f83ed2f, and the verifier lands it", filed as a question
    /// after its row had landed. A question from a member whose row is on the
    /// trunk does not hold the barrier; the same question before the land does,
    /// and its wait line names the seat the question is routed to.
    #[test]
    fn a_question_from_a_member_whose_row_has_landed_no_longer_holds_its_barrier() {
        let (_env, store) = scratch("landedq");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        let _ = spawned();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        let mut q = crate::message::Message::question(
            crate::message::Party::pane("cpd-1", ""),
            crate::message::Kind::Note,
            "m-1: ready at 340f83ed2f, and the verifier lands it",
            crate::message::Waiting::new("cpd-1", "m-1"),
        )
        .about(crate::message::About::Task("m-1".into()));
        q.for_person = true;
        store.save_message(&q).unwrap();

        tests::SAID.with(|s| s.borrow_mut().clear());
        step(&store, &w, &Fixed::new(&[("cpd-1", State::Idle)]));
        assert!(checks(&store, 1).is_empty(), "before the land, its question holds");
        let said = tests::SAID.with(|s| s.borrow().clone());
        let line = said.iter().find(|l| l.contains("barrier waits")).expect("the barrier says why");
        // No governor is seated over m-1, so only the person can close it, and
        // the line says so rather than naming a seat that would be refused.
        assert!(line.contains(&format!("waiting on a person to answer its question {}", q.id)), "{line}");
        assert_eq!(crate::waiting::route(&store, &q), crate::waiting::On::Person);

        let mut t = store.find_task("m-1").unwrap();
        t.log(&format!("{} 340f83ed2f", crate::log::LANDED));
        store.save_task(&t).unwrap();
        step(&store, &w, &Fixed::new(&[("cpd-1", State::Idle)]));
        assert_eq!(checks(&store, 1).len(), 1, "landed: the question is its governor's, and the group goes on");
        assert!(store.message(&q.id).unwrap().wants_answering(), "and the question itself is still open for whoever owes it");

        // A prompt on a landed member's screen still holds: that is a keypress
        // somebody owes now, not a report.
    }

    /// `wsp-428`: group 5 of react-pass-1 waited 51 minutes on a verified
    /// member's dev server. A shell of its own does not hold the barrier.
    #[test]
    fn a_barrier_does_not_wait_on_a_verified_members_shell_of_its_own() {
        struct Shelled;
        impl Seats for Shelled {
            fn state(&self, _seat: &str) -> Option<State> {
                Some(State::Idle)
            }
            fn shells(&self, _seats: &[&str]) -> crate::waiting::Shells {
                crate::waiting::Shells::one("cpd-1", 1)
            }
        }
        let (_env, store) = scratch("shellbarrier");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        let _ = spawned();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        step(&store, &w, &Shelled);
        assert_eq!(checks(&store, 1).len(), 1, "the barrier opens over it");
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
        hold_all(&store);
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
        assert_eq!(checks(&store, 1).len(), 1, "a frozen screen is not a turn in flight");
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
        hold_all(&store);
        let _ = spawned();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        let seats = Fixed::new(&[("cpd-1", State::Working)]);

        step(&store, &w, &seats);
        assert!(checks(&store, 1).is_empty(), "not passed over");
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
            hold_all(&store);
            let _ = spawned();
            if claim {
                store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
            }
            if bound {
                store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
            }
            let seats = Fixed::new(&[("cpd-1", State::Working)]);
            step(&store, &w, &seats);
            assert_eq!(checks(&store, 1).len(), 1, "`{tag}`: there is nobody there to hold it");
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

    /// `wsp-163`, item 2. **A reopen before the barrier opens** stops the barrier:
    /// `Standing::finished` reads `settled()`, so a member back at `doing` holds
    /// the group and `at_barrier` is false — the predicate was already right,
    /// which is why the governor could have prevented the whole thing with
    /// `wsp reopen`.
    #[test]
    fn a_member_reopened_before_the_barrier_opens_stops_it_being_opened() {
        let (_env, store) = scratch("reopenbefore");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        let _ = spawned();

        // Sent back after its verdict was in, before the barrier opened.
        let mut t = store.find_task("m-1").unwrap();
        t.set_status(Status::Doing);
        crate::log::note(&store, &mut t, "sent back: install and drive delivery live").unwrap();

        step(&store, &w, &Blind);
        assert!(checks(&store, 1).is_empty(), "no barrier over a member holding work");
    }

    /// `wsp-163`, item 2, second half. **A reopen after the barrier opened**
    /// cannot un-open it — the check is mid-turn and the run owns that — so the
    /// check is *told* what changed, in `cycle.log` and on its own pane. `wsp
    /// worklist go` refuses while a member holds, so the race cannot be lost;
    /// what this prevents is the check finishing on a group it was not given.
    #[test]
    fn a_member_reopened_after_the_barrier_opened_tells_the_check_which_member_and_why() {
        let (_env, store) = scratch("reopenafter");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        // Its agent is seated, which is what the entry records: the check is
        // mid-read, and a group that changes now is a group it was not given.
        check_seated(&store, 1, "cpd-chk");

        let mut t = store.find_task("m-1").unwrap();
        t.set_status(Status::Doing);
        crate::log::note(&store, &mut t, "sent back: install and drive delivery live").unwrap();

        step(&store, &w, &Blind);
        assert!(
            drained(&SAID).iter().any(|l| l.contains("cpd-chk") && l.contains("sent back")),
            "the log names the check and says the group changed under it"
        );
        let told = MEMBER_TOLD.with(|t| t.borrow_mut().drain(..).collect::<Vec<_>>());
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].0, "cpd-chk", "the check's own seat, not the governor's");
        assert!(told[0].1.contains("m-1"), "{}", told[0].1);
        assert!(
            told[0].1.contains("install and drive delivery live"),
            "and what is owed, so it can read the row or not: {}",
            told[0].1
        );

    }

    /// The other side of that, and the reason the check is only told when it is
    /// open: a check still sitting at `todo` was never given the group, so it
    /// starts with the group as it stands and there is nothing to correct.
    #[test]
    fn a_check_that_has_not_started_is_not_told_about_a_reopen() {
        let (_env, store) = scratch("reopentodo");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        let _ = spawned();
        // Written by hand rather than by a step, because the reopen stops the
        // barrier from opening — which is the point being tested on the other
        // side of it.
        let mut pending = Task::new(&barrier_title("run", 1), "b-1");
        pending.tags = vec![BARRIER_TAG.into()];
        pending.set_status(Status::Todo);
        store.save_task(&pending).unwrap();
        let mut t = store.find_task("m-1").unwrap();
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        step(&store, &w, &Blind);
        assert!(
            !MEMBER_TOLD.with(|t| t.borrow().iter().any(|(id, _)| *id == pending.id)),
            "a check that has not started reads the group as it stands"
        );
    }

    /// `wsp-230`, the rows installed over. A Barrier row still open when this
    /// binary arrived is that barrier's check, mid-read, and no entry opens
    /// beside it; one at `todo` that nobody ever claimed is a start that never
    /// arrived, and an entry takes its place.
    #[test]
    fn a_barrier_row_from_before_is_the_check_while_open_and_a_wedged_one_is_replaced() {
        for (tag, status, updated, opens) in [
            ("legacy-doing", Status::Doing, "2026-01-01T00:00:00Z", false),
            ("legacy-fresh", Status::Todo, "", false),
            ("legacy-wedged", Status::Todo, "2026-01-01T00:00:00Z", true),
        ] {
            let (_env, store) = scratch(tag);
            task(&store, "m-1", Status::Review);
            let w = list(&store, &[(&["m-1"], "claude")]);
            step(&store, &w, &Blind);
            hold_all(&store);
            let mut b = Task::new(&barrier_title("run", 1), "b-1");
            b.tags = vec![BARRIER_TAG.into()];
            b.set_status(status);
            if !updated.is_empty() {
                b.updated = updated.into();
            }
            store.save_task(&b).unwrap();
            let _ = spawned();
            step(&store, &w, &Blind);
            assert_eq!(checks(&store, 1).len(), usize::from(opens), "`{tag}`");
            assert_eq!(spawned().len(), usize::from(opens), "`{tag}`");
        }
    }

    /// A barrier row of one group is not a barrier row of another, whatever the
    /// ordinals look like. Found by `wsp-167`: `starts_with` answered true for
    /// "Barrier: run group 11" when asked about group 1, and three callers
    /// would have believed it.
    #[test]
    fn a_barrier_of_one_group_is_never_a_barrier_of_another() {
        for (list, at, title, want) in [
            ("run", 1, "Barrier: run group 1", true),
            ("run", 1, "Barrier: run group 1 (recheck 2)", true),
            ("run", 1, "Barrier: run group 11", false),
            ("run", 11, "Barrier: run group 11", true),
            ("run", 11, "Barrier: run group 1", false),
            ("run", 1, "Barrier: other group 1", false),
            ("run", 2, "Barrier: run group 2 (recheck 3)", true),
            ("run", 1, "Barrier: run group 1 reviewed", false),
        ] {
            let mut t = Task::new(title, "b-1");
            t.tags = vec![BARRIER_TAG.into()];
            assert_eq!(is_barrier(&t, list, at), want, "{title:?} at group {at} of {list}");
        }
        // And an untagged row with the right title is not a barrier: the tag is
        // what a reader finds it by, and a task someone happened to name that.
        let mut t = Task::new("Barrier: run group 1", "b-1");
        assert!(!is_barrier(&t, "run", 1), "the title alone is not a barrier");
    }

    /// Both of the gaps installing and looking found, in one shape: **a held
    /// run's finished agents are nobody's.** `wsp-process` was held at its
    /// barrier with a barrier check and a verdict-recorded verifier still
    /// standing — `Running::read` answers for running lists alone, so a hold
    /// put both out of reach of everything that ends agents.
    ///
    /// A hold stops nothing already in flight and says so; it stops nothing that
    /// has *finished* either, which is the same fact about a different agent.
    #[test]
    fn a_held_runs_finished_agents_are_ended_and_its_standing_barrier_is_not() {
        let (_env, store) = scratch("heldagents");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Review);
        let mut w = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        let mut g = w.groups();
        g[0].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        let mut check1 = Task::new(&barrier_title("run", 1), "b-1");
        check1.tags = vec![BARRIER_TAG.into()];
        check1.set_status(Status::Review);
        store.save_task(&check1).unwrap();
        let mut check2 = Task::new(&barrier_title("run", 2), "b-2");
        check2.tags = vec![BARRIER_TAG.into()];
        check2.set_status(Status::Doing);
        store.save_task(&check2).unwrap();
        let mut v = Task::new("Verify m-2", "v-1");
        v.parent = Some("m-2".into());
        v.tags = vec![VERIFY_TAG.into()];
        v.set_status(Status::Review);
        store.save_task(&v).unwrap();
        store.set_claim("b-1", serde_json::json!({ "workspace": "w" }));
        store.set_claim("b-2", serde_json::json!({ "workspace": "w" }));
        store.set_claim("v-1", serde_json::json!({ "workspace": "w" }));
        // Held at group 2's barrier, with group 2's check mid-read.
        let mut w = store.worklist("run").unwrap();
        w.set_status(WorklistStatus::Held);
        store.save_worklist(&w).unwrap();

        let checks = last_barrier_left_behind(&store, &w);
        assert_eq!(checks, vec!["b-1".to_string()], "the check behind the position, and not the one still reading");
        let verdicts = verdicts_recorded(&store);
        assert_eq!(verdicts, vec!["v-1".to_string()], "and the verifier that has nothing left to do");

        // `wsp-209`: the check in front ran the `hold` and then reviewed its own
        // row. Nothing else ends it — the hold spared it — so the tick does.
        set(&store, "b-2", Status::Review);
        let checks = last_barrier_left_behind(&store, &w);
        assert_eq!(checks, vec!["b-1".to_string(), "b-2".to_string()], "a check at review has finished deciding");
    }

    /// `wsp-158` item 1, and the commonest of the four leaks. A verifier's turn
    /// ends with its own verdict, and it was then left sitting there for the
    /// rest of the night. Measured on `wsp-process`: wsp-157 idle at review in
    /// cpd-246, hours after recording it.
    ///
    /// **Ended on a tick rather than on the verdict**, because the verb recording
    /// the verdict *is* the agent's last turn and ending it inside that turn cuts
    /// it short. **And ended once** (`wsp-188`, wsp-176 item 1): the pass records
    /// that it was, and the next tick finds nothing to do.
    #[test]
    fn a_verifier_whose_verdict_is_recorded_is_ended_by_a_tick_and_only_once() {
        use crate::repair::Pass;
        use crate::verification::{Ending, State as V};

        let (_env, store) = scratch("verdicted");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        let _ = spawned();
        assert!(crate::verification::name_seat(&store, "m-1", "cpd-9"), "what `spawn --verify` writes");

        crate::repair::tick(&store, &Exits, &mut Pass::new());
        assert!(drained(&ENDED).is_empty(), "a verifier still reading is not ended by a tick");

        crate::verification::record(&store, "m-1", V::Holds, "it holds", Some("cpd-9"), None).unwrap();
        crate::repair::tick(&store, &Exits, &mut Pass::new());
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()], "a verdict is the last thing a verifier is asked for");
        assert_eq!(passes_on(&store, "m-1")[0].ending, Ending::Ended, "and the pass says so");

        crate::repair::tick(&store, &Exits, &mut Pass::new());
        assert!(drained(&ENDED).is_empty(), "ended once: the next tick finds nothing standing");
    }

    /// The other verdict. A verifier that blocked has said what has to change;
    /// its job is finished either way, and it is the member's agent who does the
    /// changing. **And a seat that will not end is said once and tried again
    /// quietly** (`wsp-092`): the failure is on the pass and with the governor,
    /// and the next tick ends it once the end goes through, so no claim is kept
    /// on a seat whose end lost a passing race.
    #[test]
    fn a_verifier_that_blocked_is_ended_by_a_tick_and_a_failed_ending_is_told_once_and_tried_again() {
        use crate::repair::Pass;
        use crate::verification::{Ending, State as V};

        let (_env, store) = scratch("verdictblocked");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        let _ = spawned();
        crate::verification::name_seat(&store, "m-1", "cpd-9");
        crate::verification::record(&store, "m-1", V::Blocks, "the second half does not hold", Some("cpd-9"), None).unwrap();
        let _ = drained(&TOLD);

        END_FAIL.with(|f| *f.borrow_mut() = Some("cpd-9 is still standing: no answer".into()));
        crate::repair::tick(&store, &Exits, &mut Pass::new());
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()], "having said what has to change is the end of its job");
        assert!(matches!(&passes_on(&store, "m-1")[0].ending, Ending::Failed(why) if why.contains("no answer")));
        let told = drained(&TOLD);
        assert_eq!(told.iter().filter(|t| t.contains("could not end the verifier of m-1")).count(), 1, "{told:?}");

        crate::repair::tick(&store, &Exits, &mut Pass::new());
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()], "the seat is still there, so the next tick tries again");
        assert!(!drained(&TOLD).iter().any(|t| t.contains("could not end")), "and does not tell again");
        assert!(matches!(&passes_on(&store, "m-1")[0].ending, Ending::Failed(_)), "still failed");
        let lines = || std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert_eq!(lines().matches("could not end cpd-9").count(), 1, "nor log it again: {}", lines());

        END_FAIL.with(|f| *f.borrow_mut() = None);
        crate::repair::tick(&store, &Exits, &mut Pass::new());
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()], "the race is over and the end goes through");
        assert_eq!(passes_on(&store, "m-1")[0].ending, Ending::Ended, "and the pass says so");
        assert!(lines().contains("ended cpd-9, which verified m-1 (blocks), on a later try"), "{}", lines());

        crate::repair::tick(&store, &Exits, &mut Pass::new());
        assert!(drained(&ENDED).is_empty(), "ended once: nothing is standing any more");
    }

    /// `wsp-158` item 2, and the one nothing could find. A governor spawns a
    /// helper — the agent that installs a binary mid-group — and that row is a
    /// member of nothing and a verifier of nothing, so every group left one more
    /// seat standing.
    ///
    /// **Ownership is a fact written when the spawn is issued**, carried in the
    /// spawn's environment and put on the row's log. A pass reads it back rather
    /// than guessing from a title, and the test writes the marker the way the
    /// claim does so it is asserting the reader and not a fiction.
    #[test]
    fn a_pass_ends_a_helper_the_run_opened_which_no_group_text_names() {
        let (_env, store) = scratch("helper");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        let _ = spawned();
        // The helper, spawned while group 1 was going and never named by it.
        let mut helper = Task::new("Install master and drive delivery live", "h-1");
        crate::log::note(&store, &mut helper, &format!("{} run group 1", OWNED)).unwrap();
        store.set_claim("h-1", serde_json::json!({ "workspace": "w" }));
        // And a helper of another group, which this pass must not touch.
        let mut other = Task::new("Something else entirely", "h-2");
        crate::log::note(&store, &mut other, &format!("{} run group 2", OWNED)).unwrap();
        store.set_claim("h-2", serde_json::json!({ "workspace": "w2" }));

        end_group(&store, &Exits, &w, 1);
        let mut was = tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>());
        was.sort();
        assert_eq!(
            was,
            vec!["h-1".to_string()],
            "the helper of the group that passed — and not the one two groups on"
        );
    }

    /// `wsp-158` item 3. A barrier that closes leaves nothing open, and a `hold`
    /// and a `done` are how a run closes when the plan was wrong or the work is
    /// over early — so both end what the run opened, rather than leaving the
    /// commonest ending tidied by hand.
    #[test]
    fn closing_a_list_over_what_the_run_opened_ends_it_and_says_so() {
        let (_env, store) = scratch("closed");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        crate::verification::name_seat(&store, "m-1", "cpd-9");
        let _ = spawned();
        let mut check = Task::new(&barrier_title("run", 1), "b-1");
        check.tags = vec![BARRIER_TAG.into()];
        check.set_status(Status::Doing);
        store.save_task(&check).unwrap();
        let mut helper = Task::new("Install master", "h-1");
        crate::log::note(&store, &mut helper, &format!("{} run group 1", OWNED)).unwrap();
        for id in ["m-1", &check.id, "h-1"] {
            store.set_claim(id, serde_json::json!({ "workspace": "w" }));
        }
        let _ = drained(&TOLD);

        end_what_the_run_opened(&store, &Exits, "run", Some(1), Closing::Done);
        let mut was = tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>());
        was.sort();
        let mut expected = vec!["b-1".to_string(), "h-1".to_string(), "m-1".to_string(), "cpd-9".to_string()];
        expected.sort();
        assert_eq!(was, expected, "a member, a verifier's seat, the barrier check and a helper: nothing the group opened");
        assert_eq!(passes_on(&store, "m-1")[0].ending, crate::verification::Ending::Ended, "and the pass records it");
        assert!(
            drained(&TOLD).iter().any(|t| t.contains("had nothing left to do")),
            "and the seat is told, because ending an agent on work somebody may return to is not done quietly"
        );
    }

    /// `wsp-207`. Closing a whole list — `done` with no group — ended every
    /// barrier check in the store, so `wsp worklist done wsp-unattended` went
    /// for wsp-process's group 7 check while that run was still going. What
    /// a run opened is its own barriers and its own `OWNED` records, and
    /// nothing another run started.
    #[test]
    fn closing_one_list_leaves_another_runs_barrier_and_helper_standing() {
        let (_env, store) = scratch("two-runs");
        task(&store, "m-1", Status::Review);
        list(&store, &[(&["m-1"], "claude")]);
        let _ = spawned();
        let mut ours = Task::new(&barrier_title("run", 1), "b-1");
        ours.tags = vec![BARRIER_TAG.into()];
        crate::log::note(&store, &mut ours, &format!("{} run group 1", OWNED)).unwrap();
        // The other run's check, and a helper of the same group number: the
        // first was what got ended, the second shares every word but the list.
        let mut theirs = Task::new(&barrier_title("other", 7), "b-2");
        theirs.tags = vec![BARRIER_TAG.into()];
        crate::log::note(&store, &mut theirs, &format!("{} other group 7", OWNED)).unwrap();
        let mut helper = Task::new("Install master", "h-2");
        crate::log::note(&store, &mut helper, &format!("{} other group 1", OWNED)).unwrap();
        for id in ["b-1", "b-2", "h-2"] {
            store.set_claim(id, serde_json::json!({ "workspace": "w" }));
        }

        end_what_the_run_opened(&store, &Exits, "run", None, Closing::Done);
        let was = tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>());
        assert_eq!(was, vec!["b-1".to_string()], "this run's barrier, and nothing the other run started");
    }

    /// `wsp-209`. **A pass does not end the agent whose `go` it is.** `wsp spawn`
    /// stamps the barrier check as opened by the run, like any row the run
    /// starts, so reading ownership named it — and the `advance` its own `go`
    /// pokes ended it in the same second, before the `wsp review` its order
    /// ends on. Every agent-checked barrier from `wsp-158` on stood at `doing`.
    ///
    /// The check behind is still ended: that is the next pass's to do.
    #[test]
    fn a_pass_leaves_its_own_barrier_check_to_review_its_row_and_ends_the_one_behind() {
        let (_env, store) = scratch("ownfront");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        for (at, id) in [(1, "b-1"), (2, "b-2")] {
            let mut check = Task::new(&barrier_title("run", at), id);
            check.tags = vec![BARRIER_TAG.into()];
            check.set_status(Status::Doing);
            // What the claim writes, from the environment `start` gave it.
            crate::log::note(&store, &mut check, &format!("{} run group {at}", OWNED)).unwrap();
            store.set_claim(id, serde_json::json!({ "workspace": "w" }));
        }
        set(&store, "b-1", Status::Review);
        let _ = drained(&ENDED);

        end_group(&store, &Exits, &w, 1);
        assert!(drained(&ENDED).is_empty(), "group 1's check is running the `go` that passed group 1");

        end_group(&store, &Exits, &w, 2);
        assert_eq!(drained(&ENDED), vec!["b-1".to_string()], "the next pass ends it, and spares its own check in turn");
    }

    /// `wsp-209`, the other verdict. A `hold` is the barrier check's own turn as
    /// often as a governor's, and `wsp-158` ended that check from inside it — so
    /// a held barrier's row stayed at `doing` too. A hold stops nothing in
    /// flight; the check finishes, reviews its row, and a tick ends it.
    #[test]
    fn a_hold_ends_what_the_run_opened_but_not_the_check_whose_verdict_it_is() {
        let (_env, store) = scratch("holdfront");
        task(&store, "m-1", Status::Review);
        let _w = list(&store, &[(&["m-1"], "claude")]);
        let mut check = Task::new(&barrier_title("run", 1), "b-1");
        check.tags = vec![BARRIER_TAG.into()];
        check.set_status(Status::Doing);
        crate::log::note(&store, &mut check, &format!("{} run group 1", OWNED)).unwrap();
        let mut helper = Task::new("Install master", "h-1");
        crate::log::note(&store, &mut helper, &format!("{} run group 1", OWNED)).unwrap();
        for id in ["b-1", "h-1"] {
            store.set_claim(id, serde_json::json!({ "workspace": "w" }));
        }
        let _ = drained(&ENDED);

        end_what_the_run_opened(&store, &Exits, "run", Some(1), Closing::Held);
        assert_eq!(drained(&ENDED), vec!["h-1".to_string()], "the helper, and not the check holding the barrier");
    }

    /// A seat on `scope`, in the shape `wsp govern` writes one.
    fn seated(store: &Store, scope: &str, pane: &str) {
        store.set_governor(
            scope,
            serde_json::json!({ "workspace": pane, "pane": pane, "host": util::hostname(), "kind": "claude" }),
        );
    }

    /// A run of one group under project `p`, its barrier passed, with the check
    /// whose `go` it was still holding its claim at `doing`, and a seat on the
    /// list and another on the project.
    fn passed_with_seats(tag: &str) -> (util::Isolated, Store, Worklist) {
        let (env, store) = scratch(tag);
        let mut m = Task::new("m-1", "m-1");
        m.status_raw = Status::Review.as_str().into();
        m.project = Some("p".into());
        store.save_task(&m).unwrap();
        let mut w = list(&store, &[(&["m-1"], "claude")]);
        let mut g = w.groups();
        g[0].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        let mut check = Task::new(&barrier_title("run", 1), "b-1");
        check.tags = vec![BARRIER_TAG.into()];
        check.set_status(Status::Doing);
        crate::log::note(&store, &mut check, &format!("{} run group 1", OWNED)).unwrap();
        store.set_claim("b-1", serde_json::json!({ "workspace": "w" }));
        seated(&store, "run", "cpd-7");
        seated(&store, "p", "cpd-340");
        let _ = drained(&ENDED);
        let _ = drained(&ROTATED);
        (env, store, w)
    }

    /// `wsp-208`. On `wsp-unattended` the last pass ended members, verifiers
    /// and barriers, and left two things to a person: the `wsp worklist done`
    /// its governor typed by hand, and that governor, cpd-390, open and named
    /// in `governors.json` minutes after it said it was stopping.
    ///
    /// Past the last barrier wsp closes the list itself and stands the list's
    /// own seat down, ending it. The project's seat answers for more than this
    /// run and is not touched; the check whose `go` this is is left to review
    /// its row; and nothing is rotated, because there is nothing left to govern.
    #[test]
    fn the_last_pass_closes_the_list_and_ends_its_own_seat_and_not_the_projects() {
        let (_env, store, w) = passed_with_seats("lastpass");

        after_pass(&store, &Exits, &w, 1);

        assert_eq!(store.worklist("run").unwrap().status(), WorklistStatus::Done, "closed by wsp, not by a hand verb");
        let ended = drained(&ENDED);
        assert!(ended.contains(&"cpd-7".to_string()), "the list's own seat is ended: {ended:?}");
        assert!(!ended.contains(&"cpd-340".to_string()), "the project's seat is not the list's: {ended:?}");
        assert!(!ended.contains(&"b-1".to_string()), "the check whose `go` this is reviews its row first: {ended:?}");
        assert!(drained(&ROTATED).is_empty(), "no successor for a run that is over");
        let governors = store.governors();
        assert!(crate::governors::seat_of_scope("run", &governors).is_none(), "the list's seat is vacated");
        assert!(
            crate::governors::stood_at(&governors, "run").is_some(),
            "and recorded as stood down, so the reconciler does not fill it again"
        );
        assert!(crate::governors::seat_of_scope("p", &governors).is_some(), "the project's seat is still held");
    }

    /// The record a failed reseat leaves on a list's post: its markers and no
    /// workspace — `wsp-acceptance-fixes`' own on 2026-10-06, down to the count.
    fn a_failed_reseat(store: &Store, scope: &str) {
        store.set_governor(
            scope,
            serde_json::json!({
                "reseat_why": "cpd-420 did not take the work order",
                "reseating": "2026-10-06T13:16:42Z",
                "reseating_failed": "2026-10-06T13:16:55Z",
                "unseated": 1,
            }),
        );
    }

    /// `wsp-216`. `wsp-acceptance-fixes` closed past its last barrier with its
    /// post holding a failed reseat's record and nobody in it. The close
    /// returned at the seat lookup, so the post was never stood down; the
    /// closing verdict was then held on the list's own scope, with nobody coming
    /// to it, and reached nobody.
    ///
    /// Now the empty post is stood down at the close, and the verdict goes on a
    /// member, whose notice reaches the project's seat — saying which list's it
    /// was and why the project has it.
    #[test]
    fn a_close_over_a_post_nobody_is_in_stands_it_down_and_the_verdict_reaches_the_seat_above() {
        let (_env, store, w) = passed_with_seats("emptypost");
        a_failed_reseat(&store, "run");
        let _ = drained(&SAID);

        after_pass(&store, &Exits, &w, 1);

        let governors = store.governors();
        assert!(crate::governors::stood_at(&governors, "run").is_some(), "the empty post is stood down: {governors:?}");
        let said = drained(&SAID);
        assert!(
            said.iter().any(|l| l == "run is closing — stood its own seat down; nobody was sitting in it"),
            "and says so: {said:?}"
        );
        assert!(drained(&ENDED).iter().all(|s| s != "cpd-7"), "there was nobody in it to end");

        let now = store.worklist("run").unwrap();
        assert_eq!(nobody_coming(&store, "run").as_deref(), Some("the list is done"));
        assert!(!hand_it_to_the_governor(&store, &now, "Barrier 1 of run passed."), "not held on its own scope");
        assert!(tell_now(&store, &now, "Barrier 1 of run passed."), "somebody has it");
        assert!(crate::wake::messages_held(&store, "run").is_empty(), "nothing waits on the list's post");
        let to_p = crate::wake::messages_held(&store, "p");
        assert!(
            to_p.iter().any(|(_, _, t)| t.starts_with("Meant for the run list's own seat, and nobody is coming to it: the list is done.")
                && t.contains("Barrier 1 of run passed.")),
            "the project's seat has it, and is told whose it was: {to_p:?}"
        );
    }

    /// `wsp-216`, the governor's rule: a member's ask waits on its list's post
    /// while a successor is on the way, and goes to the project's seat once
    /// nobody is coming.
    #[test]
    fn a_members_ask_waits_on_a_post_being_reseated_and_goes_up_once_nobody_is_coming() {
        let (_env, store) = scratch("ask-route");
        let mut m = Task::new("m-1", "m-1");
        m.project = Some("p".into());
        m.set_status(Status::Doing);
        store.save_task(&m).unwrap();
        list(&store, &[(&["m-1"], "claude")]);
        seated(&store, "p", "cpd-340");
        let ask = |store: &Store| crate::cmd_govern::answering_seat(store, &store.find_task("m-1").unwrap());

        // Its governor died and the reconciler has claimed the reseat.
        store.set_governor(
            "run",
            serde_json::json!({ "vacated": util::now_iso(), "last": { "workspace": "cpd-7" }, "reseating": util::now_iso() }),
        );
        let s = ask(&store).expect("a seat answers");
        assert_eq!((s.scope.as_str(), s.vacant), ("run", true), "it waits on the list's post, empty and being filled");

        // Not yet claimed: the reconciler has counted the vacancy, and is coming.
        store.set_governor("run", serde_json::json!({ "unseated": 1 }));
        assert_eq!(ask(&store).map(|s| s.scope).as_deref(), Some("run"));

        // The reseat failed and backed off: nobody is coming.
        a_failed_reseat(&store, "run");
        let s = ask(&store).expect("a seat answers");
        assert_eq!((s.scope.as_str(), s.vacant), ("p", false), "the project's seat, which is somebody");
        let now = store.worklist("run").unwrap();
        assert!(nobody_coming(&store, "run").is_some_and(|why| why.starts_with("its reseat failed")));
        assert!(!hand_it_to_the_governor(&store, &now, "a verdict"), "nor does the run's own tell wait there");

        // Retried: a claim after the failure is a successor on the way again.
        store.edit_governor("run", |rec| {
            rec["reseating"] = serde_json::json!("2026-10-06T13:40:00Z");
            true
        });
        assert_eq!(ask(&store).map(|s| s.scope).as_deref(), Some("run"));

        // Stood down, and done: nobody, and the list is not a running one.
        crate::cmd_govern::mark_stood_down(&store, "run");
        assert_eq!(ask(&store).map(|s| s.scope).as_deref(), Some("p"));
        let mut w = store.worklist("run").unwrap();
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        store.edit_governor("run", |rec| rec.as_object_mut().is_some_and(|o| o.remove(crate::governors::STOOD_DOWN).is_some()));
        assert_eq!(ask(&store).map(|s| s.scope).as_deref(), Some("p"), "a done list answers for nothing");
    }

    /// `wsp-216`'s live repair: what was already held on a done list's post
    /// when it closed — `wsp-acceptance-fixes`' two, the barrier-3 verdict among
    /// them — is handed on by a tick, and taken off the post. A running list's
    /// held sentences are its own, and stay.
    #[test]
    fn a_tick_hands_on_what_a_done_lists_post_was_holding() {
        let (_env, store, _w) = passed_with_seats("handon");
        a_failed_reseat(&store, "run");
        crate::wake::say(&store, "run", "m-1 is at doing with nothing working on it", None);
        crate::wake::say(&store, "run", "Barrier 1 of run passed.", None);
        let mut w = store.worklist("run").unwrap();
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        let mut other = Worklist::new("other", "other");
        other.set_status(WorklistStatus::Running);
        other.set_groups(&[Group { members: vec!["m-2".into()], agent: "claude".into(), ..Group::default() }]);
        store.save_worklist(&other).unwrap();
        crate::wake::say(&store, "other", "for a list still running", None);
        let _ = drained(&SAID);

        hand_on_what_done_lists_held(&store);

        assert!(crate::wake::messages_held(&store, "run").is_empty(), "taken off the done list's post");
        assert_eq!(crate::wake::messages_held(&store, "other").len(), 1, "a running list keeps its own");
        let to_p: Vec<String> = crate::wake::messages_held(&store, "p").into_iter().map(|(_, _, t)| t).collect();
        assert_eq!(to_p.len(), 2, "{to_p:?}");
        let today = util::local_ymd(&util::now_iso());
        assert!(
            to_p.iter().all(|t| t.contains("Meant for the run list's own seat") && t.contains(&format!("(Held on its scope since {today} "))),
            "dated, since a done list's backlog is days old as often as not: {to_p:?}"
        );
        assert!(to_p[1].contains("Barrier 1 of run passed."), "{to_p:?}");
        let said = drained(&SAID);
        assert!(said.iter().any(|l| l.contains("handed on 2 of the 2")), "{said:?}");

        hand_on_what_done_lists_held(&store);
        assert_eq!(crate::wake::messages_held(&store, "p").len(), 2, "once");
    }

    /// `wsp-151`. cr-linux closed on a machine whose project had no seat, and
    /// all 26 sentences held on its post went to the person as flags —
    /// started, parked, resumed, verified, none asking anything. With nobody
    /// above to keep current, a sentence that asks nothing is let go; one that
    /// asks still reaches the person.
    #[test]
    fn a_done_list_with_no_seat_above_lets_go_what_asks_nothing() {
        let (_env, store, _w) = passed_with_seats("letgo");
        a_failed_reseat(&store, "run");
        assert!(store.clear_governor("p"), "nobody above the list");
        crate::wake::say(&store, "run", "The run list was parked: nothing starts until it is resumed · wsp worklist show run", None);
        crate::wake::say(&store, "run", "The run list was resumed where it was parked · wsp worklist show run", None);
        crate::wake::say(&store, "run", "m-1 has been blocked for 40 min and waits on: does the schema keep its old column?", None);
        let mut w = store.worklist("run").unwrap();
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        let before: Vec<String> = crate::message::all(&store).into_iter().map(|m| m.id).collect();
        let _ = drained(&SAID);

        hand_on_what_done_lists_held(&store);

        assert!(crate::wake::messages_held(&store, "run").is_empty(), "nothing is left on the done list's post");
        let raised: Vec<String> =
            crate::message::all(&store).into_iter().filter(|m| !before.contains(&m.id)).map(|m| m.text).collect();
        assert_eq!(raised.len(), 1, "only the one that asks: {raised:?}");
        assert!(raised[0].contains("does the schema keep its old column?"), "{raised:?}");
        let said = drained(&SAID);
        assert!(
            said.iter().any(|l| l.contains("handed on 1 of the 3") && l.contains("let go 2 that asked nothing")),
            "{said:?}"
        );

        hand_on_what_done_lists_held(&store);
        assert_eq!(crate::message::all(&store).len(), before.len() + 1, "once");
    }

    /// `wsp-216`: `go` on the last barrier of a group wsp ran is the close, and
    /// says so rather than offering `done` or promising a seat. A barrier short
    /// of the end, or a last group run by hand, is not.
    #[test]
    fn only_the_last_barrier_of_a_group_wsp_ran_closes_the_list() {
        let (_env, store) = scratch("closes");
        let w = list(&store, &[(&["a"], "claude"), (&["b"], "claude")]);
        assert!(!closes_on_pass(&w, 1));
        assert!(closes_on_pass(&w, 2));
        let w = list(&store, &[(&["a"], "claude"), (&["b"], crate::model::MANUAL)]);
        assert!(!closes_on_pass(&w, 2), "a hand-run last group is closed by whoever runs it");
    }

    /// A pass that is not the last rotates the list's seat and closes nothing.
    #[test]
    fn a_pass_short_of_the_end_closes_nothing_and_stands_no_seat_down() {
        let (_env, store, mut w) = passed_with_seats("notlast");
        let mut g = w.groups();
        g.push(Group { members: vec!["m-1".into()], agent: "claude".into(), ..Group::default() });
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();

        after_pass(&store, &Exits, &w, 1);

        assert_eq!(drained(&ROTATED), vec!["run".to_string()], "a fresh governor for the next group");
        assert_eq!(store.worklist("run").unwrap().status(), WorklistStatus::Running);
        assert!(!drained(&ENDED).contains(&"cpd-7".to_string()), "the seat still has a barrier to answer for");
        assert!(crate::governors::seat_of_scope("run", &store.governors()).is_some());
    }

    /// A hold is waiting on a decision, and the list's seat is who makes it; a
    /// `done` — typed by a person or by that governor itself, as
    /// `wsp-unattended`'s was — leaves the seat nothing to answer for.
    #[test]
    fn a_hold_keeps_the_lists_own_seat_and_a_done_ends_it() {
        let (_env, store, _w) = passed_with_seats("holdseat");

        end_what_the_run_opened(&store, &Exits, "run", Some(1), Closing::Held);
        assert!(!drained(&ENDED).contains(&"cpd-7".to_string()), "a held run keeps its governor");
        assert!(crate::governors::seat_of_scope("run", &store.governors()).is_some());

        end_what_the_run_opened(&store, &Exits, "run", None, Closing::Done);
        let ended = drained(&ENDED);
        assert!(ended.contains(&"cpd-7".to_string()), "{ended:?}");
        assert!(!ended.contains(&"cpd-340".to_string()), "{ended:?}");
    }

    /// The other half of [`Closing::Passed`]: the check it spared is ended by
    /// a tick once it stands at `review`, on a list that is now `done` — which
    /// the tick never used to read at all.
    #[test]
    fn a_closed_lists_last_check_is_ended_by_a_tick_once_it_has_reviewed_its_row() {
        use crate::repair::Pass;
        let (_env, store, w) = passed_with_seats("closedcheck");
        after_pass(&store, &Exits, &w, 1);
        let _ = drained(&ENDED);

        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert!(!drained(&ENDED).contains(&"b-1".to_string()), "still at `doing`, still deciding");

        set(&store, "b-1", Status::Review);
        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert!(drained(&ENDED).contains(&"b-1".to_string()), "reviewed, and nothing left for it to decide");
    }

    /// `wsp-324`. A landed member whose project declares checks gets no verifier
    /// until a check run for that landing has passed: the step writes the run's
    /// entry and waits, the same step again waits on it, and the pass opens the
    /// way. A project with no checks — every other test here — is untouched.
    #[test]
    fn a_member_with_checks_gets_its_verifier_only_after_the_run_has_passed() {
        let (_env, store) = scratch("checks-gate");
        let repo = _env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&repo).args(args).env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        crate::testkit::repo(&repo);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "--quiet", "-m", "one"]);
        let sha = git(&["rev-parse", "HEAD"]);
        let mut p = crate::model::Project::new("m");
        p.roots = vec![repo.display().to_string()];
        p.body = "## Checks\n- exists: test -f f\n".into();
        store.save_project(&p).unwrap();
        task(&store, "m-1", Status::Review);
        let mut m = store.find_task("m-1").unwrap();
        m.project = Some("m".into());
        crate::log::note(&store, &mut m, &format!("{} {sha} on master", crate::repair::LANDED)).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "no verifier while the check is owed");
        let runs = crate::verification::runs(&store.find_task("m-1").unwrap());
        assert_eq!(runs.len(), 1, "the step wrote the run that keeps a second trigger from starting another");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty() && crate::verification::runs(&store.find_task("m-1").unwrap()).len() == 1);

        let mut t = store.find_task("m-1").unwrap();
        let mut done = crate::verification::runs(&t);
        done[0].state = crate::verification::RunState::Passed;
        crate::verification::save_runs(&store, &mut t, &done).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1 · verifying".into(), "claude".into())], "the check passed, and now the verifier");
    }

    /// `wsp-341`: a member reviewed ready is owed a verifier whose pass reads
    /// the ready commit, the step says nothing about it being unlanded, and the
    /// barrier does not open on it. A hold on it does not re-verify it.
    #[test]
    fn a_ready_member_gets_a_verifier_reading_its_commit_and_no_unlanded_note() {
        use crate::verification::State as V;
        let (env, store) = scratch("ready");
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = wsp_base::calls::Command::new("git")
                .arg("-C").arg(&repo).args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
                .output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        crate::testkit::repo(&repo);
        git(&["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&["branch", "m-1"]);
        git(&["checkout", "-q", "m-1"]);
        git(&["commit", "-q", "--allow-empty", "-m", "work"]);
        git(&["checkout", "-q", "master"]);
        let tip = git(&["rev-parse", "m-1"]);
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        task(&store, "m-1", Status::Review);
        let mut m = store.find_task("m-1").unwrap();
        m.project = Some("p".into());
        store.save_task(&m).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);

        step(&store, &w, &Blind);
        assert!(passes_on(&store, "m-1").is_empty(), "reviewed without a ready line: today's told-to-land path");

        let mut m = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m, &format!("{} {tip} on m-1", crate::repair::READY)).unwrap();
        step(&store, &w, &Blind);
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.len(), 1, "a ready member owes a verifier");
        assert_eq!(ps[0].read.as_deref(), Some(&tip[..ps[0].read.as_deref().unwrap().len()]));
        let pos = crate::worklist::position(&store, &w, crate::worklist::Reading::Landed);
        assert!(!pos.at_barrier(), "ready is not landed, so the barrier waits");

        verdict_all(&store, V::Holds);
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 1, "a hold on the same ready commit is not re-verified");
        let _ = spawned();
    }

    /// A project over a repository with member `m-1` in its own tree, one commit
    /// ahead of a trunk that has since gained `trunk_file` (`wsp-341`).
    fn ready_tree(tag: &str, member_file: &str, trunk_file: &str) -> (util::Isolated, Store, std::path::PathBuf, crate::model::Worklist) {
        let (env, store) = scratch(tag);
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let out = wsp_base::calls::Command::new("git")
                .arg("-C").arg(dir).args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
                .output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        crate::testkit::repo(&repo);
        std::fs::write(repo.join("f"), "base\n").unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let tree = repo.join(crate::cmd_checkout::WORKTREES).join("m-1");
        git(&repo, &["worktree", "add", "-q", "-b", "m-1", &tree.display().to_string()]);
        std::fs::write(tree.join(member_file), "member\n").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-q", "-m", "work"]);
        std::fs::write(repo.join(trunk_file), "trunk\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "trunk moved"]);
        let tip = git(&tree, &["rev-parse", "HEAD"]);
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        task(&store, "m-1", Status::Review);
        let mut m = store.find_task("m-1").unwrap();
        m.project = Some("p".into());
        crate::log::note(&store, &mut m, &format!("{} {tip} on m-1", crate::repair::READY)).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        (env, store, tree, w)
    }

    /// `wsp-341`: a hold lands the branch rebased onto a trunk that moved again,
    /// so the commit that reached the trunk is not the one the pass read. The
    /// pass says so, and the next tick reads it as the same work.
    #[test]
    fn a_rebased_land_is_not_verified_again() {
        let (_env, store, tree, w) = ready_tree("rebased-land", "g", "h");
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 1);
        let repo = tree.parent().unwrap().parent().unwrap().to_path_buf();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let out = wsp_base::calls::Command::new("git").arg("-C").arg(dir).args(args).env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
                .output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        std::fs::write(repo.join("later"), "later\n").unwrap();
        git(&repo, &["add", "later"]);
        git(&repo, &["commit", "-q", "-m", "the trunk moved again"]);
        let from = store.root.join("verdict.md");
        std::fs::write(&from, "holds").unwrap();
        let from = from.display().to_string();
        let args = crate::Args::synth("verified", &["m-1", "--from", &from], &[("holds", "true"), ("caption", "it holds")]);
        assert_eq!(crate::verification::cmd_verified(&store, &args), 0);
        let m = store.find_task("m-1").unwrap();
        let p = crate::verification::passes(&m).pop().unwrap();
        assert!(p.landed.is_some(), "read and landed differ: {:?}", p);
        step(&store, &w, &Blind);
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 1, "the landing is the work the pass read, rebased");
        let _ = spawned();
    }

    /// `wsp-341`: the trunk moved after the member went ready. The step rebases
    /// the member's tree onto it, writes a fresh ready line, and the pass reads
    /// the rebased commit; a second tick changes nothing.
    #[test]
    fn a_pass_opens_on_the_rebased_commit() {
        let (_env, store, tree, w) = ready_tree("rebase-clean", "g", "h");
        let before = crate::repair::ready(&store.find_task("m-1").unwrap()).unwrap();
        step(&store, &w, &Blind);
        let head = String::from_utf8(wsp_base::calls::Command::new("git").arg("-C").arg(&tree).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
        let head = head.trim();
        assert!(!crate::verification::same_commit(head, &before), "the branch moved");
        let m = store.find_task("m-1").unwrap();
        assert!(crate::verification::same_commit(&crate::repair::ready(&m).unwrap(), head), "a fresh ready line names the rebased head");
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.len(), 1);
        assert!(crate::verification::same_commit(ps[0].read.as_deref().unwrap(), head), "the pass reads the rebased commit");
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1").len(), 1, "an idempotent tick");
        assert_eq!(store.find_task("m-1").unwrap().section("Log").unwrap().matches(crate::repair::READY).count(), 2, "one new ready line, no more");
        let _ = spawned();
    }

    /// `wsp-341`, end to end: a project whose `## Checks` are `checks` (`RAN` is a
    /// file the test reads back), a member ready in its own tree, the step that
    /// rebases it and opens the pass, then `wsp verified`.
    struct Run {
        _env: util::Isolated,
        store: Store,
        repo: std::path::PathBuf,
        tree: std::path::PathBuf,
        w: crate::model::Worklist,
        ran: std::path::PathBuf,
    }

    fn run_ready(tag: &str, checks: &str, member_file: &str, trunk_file: &str) -> Run {
        // The land a hold performs reruns checks through the engine's answers, which `main` installs.
        crate::cmd_checkout::install_engine(crate::cmd_checkout::Engine {
            passed: |_| Default::default(),
            poke_task: |_, _, _| {},
            runs: |_, _| false,
            after_land: crate::check::after_land,
            rerun: crate::check::rerun,
            held: |_| None,
            refused: |store, row| crate::hub_journal::refusal(store, row, "land"),
        });
        let (env, store, tree, w) = ready_tree(tag, member_file, trunk_file);
        let ran = env.path("ran.log");
        let mut p = store.project("p").unwrap();
        p.body = format!("## Checks\n{}\n", checks.replace("RAN", &ran.display().to_string()));
        store.save_project(&p).unwrap();
        let repo = tree.parent().unwrap().parent().unwrap().to_path_buf();
        Run { _env: env, store, repo, tree, w, ran }
    }

    impl Run {
        fn commit_on_trunk(&self, file: &str, text: &str) {
            let git = |args: &[&str]| {
                let out = wsp_base::calls::Command::new("git").arg("-C").arg(&self.repo).args(args).env_remove("GIT_INDEX_FILE")
                    .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
                    .output().unwrap();
                assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
                String::from_utf8(out.stdout).unwrap().trim().to_string()
            };
            let at = self.repo.join(file);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(&at, text).unwrap();
            git(&["add", file]);
            git(&["commit", "-q", "-m", "the trunk moved again"]);
        }
        fn trunk(&self) -> String {
            let out = wsp_base::calls::Command::new("git").arg("-C").arg(&self.repo).args(["rev-parse", "master"]).output().unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }
        fn verdict(&self, flag: &str) {
            let from = self.store.root.join("verdict.md");
            let cause = if flag == "blocks" { "\ncause: work — it does not do what was asked" } else { "" };
            std::fs::write(&from, format!("read in the member's tree{cause}")).unwrap();
            let from = from.display().to_string();
            let args = crate::Args::synth("verified", &["m-1", "--from", &from], &[(flag, "true"), ("caption", "read in its tree")]);
            assert_eq!(crate::verification::cmd_verified(&self.store, &args), 0);
        }
        fn reran(&self) -> String {
            std::fs::read_to_string(&self.ran).unwrap_or_default()
        }
        /// The step, the gate's run (a process of its own in the daemon, here in
        /// line), and the step again that opens the pass behind it.
        fn open_the_pass(&self) {
            step(&self.store, &self.w, &Blind);
            if let Some(run) = crate::verification::runs(&self.store.find_task("m-1").unwrap()).pop() {
                assert_eq!(crate::check::check(&self.store, &crate::Args::synth("check", &["m-1"], &[("auto", "true"), ("run", &run.at)])), 0);
            }
            step(&self.store, &self.w, &Blind);
        }
        fn latest(&self) -> crate::verification::Pass {
            passes_on(&self.store, "m-1").pop().unwrap()
        }
    }

    /// Holds: ready → rebased and checked in the tree → pass → `--holds` lands
    /// the row with nothing rerun (the trunk did not move since), and the next
    /// tick neither verifies nor checks it again.
    #[test]
    fn a_ready_member_is_verified_in_its_tree_and_a_hold_lands_it() {
        let r = run_ready("e2e-holds", "- always: echo always >> RAN", "g", "h");
        r.open_the_pass();
        assert_eq!(r.latest().state, crate::verification::State::Running);
        assert_eq!(r.reran(), "always\n", "the gate ran once, in the member's tree");
        assert!(crate::repair::landed(&r.store.find_task("m-1").unwrap()).is_none(), "nothing on the trunk before the verdict");
        r.verdict("holds");
        let m = r.store.find_task("m-1").unwrap();
        assert_eq!(r.trunk(), crate::repair::landed(&m).unwrap());
        assert!(r.repo.join("g").exists() && r.repo.join("h").exists(), "the member's work and the trunk's, together");
        assert_eq!(r.reran(), "always\n", "an unmoved trunk reruns nothing");
        assert!(r.latest().landed.is_none());
        step(&r.store, &r.w, &Blind);
        step(&r.store, &r.w, &Blind);
        assert_eq!(passes_on(&r.store, "m-1").len(), 1);
        assert_eq!(r.reran(), "always\n");
        let _ = spawned();
    }

    /// Blocks: the verdict sends the row back and nothing reaches the trunk.
    #[test]
    fn a_block_on_a_ready_member_leaves_the_trunk_as_it_was() {
        let r = run_ready("e2e-blocks", "", "g", "h");
        r.open_the_pass();
        let before = r.trunk();
        r.verdict("blocks");
        assert_eq!(r.trunk(), before);
        assert!(!r.repo.join("g").exists());
        let m = r.store.find_task("m-1").unwrap();
        assert_eq!(m.status(), Status::Doing);
        assert!(crate::repair::landed(&m).is_none());
        assert_eq!(r.latest().state, crate::verification::State::Blocks);
        let _ = spawned();
    }

    /// Moved trunk: between the pass opening and the hold the trunk gains a
    /// commit; land rebases again and reruns the check that commit can affect
    /// and not the one it cannot.
    #[test]
    fn a_trunk_that_moves_during_the_pass_reruns_only_the_affected_checks() {
        let checks = "- src on src/**: echo src >> RAN\n- docs on docs/**: echo docs >> RAN\n- always: echo always >> RAN";
        let r = run_ready("e2e-moved", checks, "g", "h");
        r.open_the_pass();
        assert_eq!(r.reran(), "always\n", "the gate ran the every-landing step: the member changed no path a rule names");
        r.commit_on_trunk("src/a.txt", "theirs\n");
        r.verdict("holds");
        assert_eq!(r.reran(), "always\nsrc\nalways\n", "src reran for the path the rebase pulled in, and the every-landing step; docs did not");
        let m = r.store.find_task("m-1").unwrap();
        assert_eq!(r.trunk(), crate::repair::landed(&m).unwrap());
        let p = r.latest();
        assert!(p.landed.is_some(), "the commit that reached the trunk is not the one read");
        step(&r.store, &r.w, &Blind);
        assert_eq!(passes_on(&r.store, "m-1").len(), 1, "and it is not verified again");
        let _ = spawned();
    }

    /// Moved trunk, red rerun: the affected check fails after the second
    /// rebase, so the hold is a block and nothing lands.
    #[test]
    fn a_red_rerun_after_the_trunk_moved_blocks_a_ready_member() {
        let r = run_ready("e2e-red", "- src on src/**: test ! -f src/a.txt", "g", "h");
        r.open_the_pass();
        let before = r.trunk();
        r.commit_on_trunk("src/a.txt", "theirs\n");
        let moved = r.trunk();
        r.verdict("holds");
        assert_eq!(r.trunk(), moved, "nothing of the member's reached the trunk");
        assert!(!r.repo.join("g").exists() && before != moved);
        assert_eq!(r.latest().state, crate::verification::State::Blocks);
        assert_eq!(r.store.find_task("m-1").unwrap().status(), Status::Doing);
        let _ = spawned();
    }

    /// Moved trunk, conflict: the second rebase does not apply, so the hold is a
    /// block, the tree is left as the member had it, and nothing lands.
    #[test]
    fn a_conflict_after_the_trunk_moved_blocks_a_ready_member() {
        let r = run_ready("e2e-conflict", "", "g", "h");
        r.open_the_pass();
        r.commit_on_trunk("g", "the trunk wrote g too\n");
        let moved = r.trunk();
        r.verdict("holds");
        assert_eq!(r.trunk(), moved);
        assert_eq!(r.latest().state, crate::verification::State::Blocks);
        assert!(r.latest().text.contains("does not apply"), "{}", r.latest().text);
        let status = wsp_base::calls::Command::new("git").arg("-C").arg(&r.tree).args(["status", "--porcelain"]).output().unwrap();
        assert!(status.stdout.is_empty(), "the tree is not left mid-rebase");
        let _ = spawned();
    }

    /// `wsp-341`: a rebase that does not apply is aborted, the member is sent
    /// back with the reason, and no pass opens.
    #[test]
    fn a_rebase_that_does_not_apply_sends_the_member_back_and_spends_no_agent() {
        let (_env, store, tree, w) = ready_tree("rebase-conflict", "f", "f");
        step(&store, &w, &Blind);
        let m = store.find_task("m-1").unwrap();
        assert_eq!(m.status(), Status::Doing, "sent back");
        assert!(m.section("Log").unwrap().contains("does not apply"), "{}", m.section("Log").unwrap());
        assert!(passes_on(&store, "m-1").is_empty(), "no verifier pass");
        assert!(spawned().iter().all(|(label, _)| !label.contains("verifying")), "no verifier agent");
        let st = String::from_utf8(wsp_base::calls::Command::new("git").arg("-C").arg(&tree).args(["status", "--porcelain"]).output().unwrap().stdout).unwrap();
        assert!(st.trim().is_empty(), "the rebase was aborted: {st}");
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
        use crate::verification::State as V;
        let (_env, store) = scratch("note");
        task(&store, "m-1", Status::Review);
        let mut m = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m, &format!("{} abc1234 on master", crate::repair::LANDED)).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        assert_eq!(passes_on(&store, "m-1")[0].read.as_deref(), Some("abc1234"), "the first pass, which read abc1234");
        verdict_all(&store, V::Blocks);
        set(&store, "m-1", Status::Review);

        step(&store, &w, &Blind);
        let mut m = store.find_task("m-1").unwrap();
        m.log("the verifier is right about the second half");
        m.touch();
        store.save_task(&m).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(
            passes_on(&store, "m-1").len(),
            1,
            "a note moved `updated` and nothing else: still the same work, still one pass"
        );

        // The member comes back with a fix, which is a new commit on the trunk.
        let mut m = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m, &format!("{} def5678 on master", crate::repair::LANDED)).unwrap();
        step(&store, &w, &Blind);
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.len(), 2, "a new landing is new work, and a fresh agent reads it");
        assert_eq!(ps[1].read.as_deref(), Some("def5678"));
        let _ = spawned();
    }

    /// `wsp-188`, and wsp-176 item 2. **A second landing supersedes the verifier
    /// still reading the first**: its seat is ended at once, its pass stays on
    /// the record marked superseded, a fresh pass reads the new commit — and a
    /// verdict the old one sends afterwards is refused rather than read as the
    /// newest.
    #[test]
    fn a_second_landing_supersedes_the_verifier_still_reading_the_first() {
        use crate::verification::State as V;
        let (_env, store) = scratch("supersede");
        task(&store, "m-1", Status::Review);
        let mut m = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m, &format!("{} abc1234 on master", crate::repair::LANDED)).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        crate::verification::name_seat(&store, "m-1", "cpd-1");
        assert_eq!(drained(&SPAWNED).len(), 1);

        step(&store, &w, &Blind);
        assert!(drained(&SPAWNED).is_empty() && drained(&ENDED).is_empty(), "the same landing: one pass, left reading");

        let mut m = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m, &format!("{} def5678 on master", crate::repair::LANDED)).unwrap();
        step(&store, &w, &Exits);
        assert_eq!(drained(&ENDED), vec!["cpd-1".to_string()], "the verifier reading the old commit is ended at once");
        assert_eq!(drained(&SPAWNED), vec![("m-1 · verifying".to_string(), "claude".to_string())], "and a fresh one starts");
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.iter().map(|p| p.state).collect::<Vec<_>>(), vec![V::Superseded, V::Running]);
        assert_eq!(ps[1].read.as_deref(), Some("def5678"));

        let late = crate::verification::record(&store, "m-1", V::Holds, "abc1234 holds", Some("cpd-1"), None);
        assert!(late.unwrap_err().contains("superseded"), "a verdict on the old commit is refused");
        assert!(!verified(&store.tasks(), "m-1"), "and the barrier still waits on the fresh pass");
        crate::verification::name_seat(&store, "m-1", "cpd-2");
        crate::verification::record(&store, "m-1", V::Holds, "def5678 holds", Some("cpd-2"), None).unwrap();
        assert!(verified(&store.tasks(), "m-1"), "the newest pass is what the barrier reads");
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
    fn a_held_barrier_is_checked_again_by_a_fresh_check_beside_the_one_that_held() {
        let (_env, store) = scratch("recheck");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        // A barrier opens on every member's *verdict*, so settle this one first
        // — the group is otherwise still waiting on the verifier, not on the
        // barrier, and this test is about the barrier.
        hold_all(&store);
        step(&store, &w, &Blind);
        assert_eq!(checks(&store, 1).len(), 1, "the first check");
        let _ = spawned();
        check_seated(&store, 1, "cpd-1");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "and one check is enough while it stands");

        // Held: the entry says so, and a resume is a fresh check.
        crate::barrier::settle(&store, "run", 1, crate::barrier::State::Holds);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("run · barrier 1".to_string(), "claude".into())]);
        let c = checks(&store, 1);
        assert_eq!(c.len(), 2, "a resume is a fresh check, not the old one");
        assert!(c[0].state == crate::barrier::State::Holds && c[1].open(), "the hold is kept beside it: {c:?}");

        // And once that one has held too, a third.
        check_seated(&store, 1, "cpd-2");
        crate::barrier::settle(&store, "run", 1, crate::barrier::State::Holds);
        step(&store, &w, &Blind);
        let _ = spawned();
        assert_eq!(checks(&store, 1).len(), 3, "each hold is its own check");
    }

    /// The other half of that: a check whose agent never arrived is **not** a
    /// check that finished. Treating it as one would add a check for a barrier
    /// nobody read; it is started again once it has waited long enough.
    #[test]
    fn a_check_whose_agent_never_arrived_is_retaken_rather_than_superseded() {
        let (_env, store) = scratch("recheckwedge");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a start in flight is waited for");
        let mut l = store.worklist("run").unwrap();
        let mut g = l.groups();
        g[0].checks[0].at = "2026-01-01T00:00:00Z".into();
        l.set_groups(&g);
        store.save_worklist(&l).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(checks(&store, 1).len(), 1, "the same check, started again — a start that never arrived is not a check that finished");
        assert_eq!(spawned(), vec![("run · barrier 1".to_string(), "claude".into())]);
    }

    /// And a check whose agent has gone — its seat is on no backend — is not
    /// waited on for ever: it is marked abandoned and a fresh one reads the
    /// group. Before `wsp-230` a row in that state held the run until somebody
    /// noticed.
    #[test]
    fn a_check_whose_seat_has_gone_is_abandoned_and_a_fresh_one_opens() {
        struct Gone;
        impl Seats for Gone {
            fn state(&self, _seat: &str) -> Option<State> {
                None
            }
            fn absent(&self, seat: &str) -> bool {
                seat == "cpd-dead"
            }
        }
        let (_env, store) = scratch("checkgone");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        check_seated(&store, 1, "cpd-dead");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a reader that cannot see the seat waits");
        step(&store, &w, &Gone);
        let c = checks(&store, 1);
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(c[0].state, crate::barrier::State::Abandoned);
        assert!(c[1].open());
        assert_eq!(spawned(), vec![("run · barrier 1".to_string(), "claude".into())]);
    }

    /// `wsp-516`, wsp-513's third part: after a cut the running group's barrier
    /// reads the members the stored list names now. Group 1 is `m-1 m-2 m-3`
    /// with `m-1` blocked and `m-2` resting on it; the verb cuts them into a new
    /// group 2, the barrier of group 1 opens and passes on `m-3` alone, the run
    /// goes on to the cut group, and the old group 2 is group 3.
    #[test]
    fn a_cut_leaves_the_barrier_to_read_the_members_that_remain_and_the_run_goes_on_to_the_cut_group() {
        let (_env, store) = scratch("cutbarrier");
        task(&store, "m-1", Status::Blocked);
        task(&store, "m-2", Status::Todo);
        task(&store, "m-3", Status::Review);
        task(&store, "n-1", Status::Todo);
        let mut t = store.find_task("m-2").unwrap();
        t.after = vec!["m-1".into()];
        t.tags = vec!["kept".into()];
        store.save_task(&t).unwrap();
        let w = list(&store, &[(&["m-1", "m-2", "m-3"], "claude"), (&["n-1"], "claude")]);

        // The pass before the cut reads the whole group: the barrier is owed on
        // three members and the blocked one holds it.
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        assert!(checks(&store, 1).is_empty(), "m-1 is blocked, so group 1's barrier is not owed");

        let flags = [("why", "m-1 waits on a person")];
        assert_eq!(
            crate::cmd_worklist::cut_by(&store, &Args::synth("worklist", &["cut", "run", "m-1"], &flags), &[], None),
            0
        );
        let after_cut = store.worklist("run").unwrap().groups();
        let members: Vec<&[String]> = after_cut.iter().map(|g| g.members.as_slice()).collect();
        assert_eq!(members, [&["m-3".to_string()][..], &["m-1".to_string(), "m-2".to_string()][..], &["n-1".to_string()][..]]);

        // The same `w` the pass was handed before the cut: nothing is read from it.
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("run · barrier 1".to_string(), "claude".to_string())], "the barrier opens on what is left");
        assert_eq!(checks(&store, 1).len(), 1);
        let order = check_order(&store, "run", 1).expect("an order");
        assert!(order.contains("m-3") && !order.contains("m-1") && !order.contains("m-2"), "the check reads m-3 alone: {order}");
        assert_eq!(crate::barrier_run::owed(&store, &store.worklist("run").unwrap(), 1), crate::barrier_run::owed(&store, &w, 1));

        // It passes, and the run stands at the cut group, which has nothing to start
        // while its first row is blocked.
        crate::barrier::settle(&store, "run", 1, crate::barrier::State::Passes);
        let mut l = store.worklist("run").unwrap();
        let mut g = l.groups();
        g[0].verdict = "2026-08-20T09:00:00Z passed".into();
        l.set_groups(&g);
        store.save_worklist(&l).unwrap();
        let l = store.worklist("run").unwrap();
        assert_eq!(worklist::position(&store, &l, Reading::Settled).at, Some(2));
        let _ = spawned();
        step(&store, &l, &Blind);
        assert!(spawned().is_empty(), "m-1 is blocked, m-2 rests on it, and n-1 is two groups on");
        let (m1, m2) = (store.find_task("m-1").unwrap(), store.find_task("m-2").unwrap());
        assert_eq!((m1.status(), m2.after.clone(), m2.tags.clone()), (Status::Blocked, vec!["m-1".to_string()], vec!["kept".to_string()]));

        // The blocked row is answered: group 2 runs, is verified, and its barrier passes.
        set(&store, "m-1", Status::Todo);
        step(&store, &l, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "m-1 starts; m-2 still rests on it");
        set(&store, "m-1", Status::Review);
        step(&store, &l, &Blind);
        assert!(spawned().iter().any(|(id, _)| id == "m-2"), "m-2 starts once m-1 has landed");
        hold_all(&store);
        step(&store, &l, &Blind);
        let _ = spawned();
        set(&store, "m-2", Status::Review);
        step(&store, &l, &Blind);
        let _ = spawned();
        hold_all(&store);
        step(&store, &l, &Blind);
        assert_eq!(spawned(), vec![("run · barrier 2".to_string(), "claude".to_string())], "group 2's own barrier");
        let order = check_order(&store, "run", 2).expect("an order");
        assert!(order.contains("m-1") && order.contains("m-2") && !order.contains("m-3"), "{order}");
        crate::barrier::settle(&store, "run", 2, crate::barrier::State::Passes);
        let mut l2 = store.worklist("run").unwrap();
        let mut g = l2.groups();
        g[1].verdict = "2026-08-20T10:00:00Z passed".into();
        l2.set_groups(&g);
        store.save_worklist(&l2).unwrap();
        assert_eq!(worklist::position(&store, &l2, Reading::Settled).at, Some(3), "and the run is on the old group 2");
        step(&store, &l2, &Blind);
        assert_eq!(spawned(), vec![("n-1".to_string(), "claude".to_string())]);
    }

    /// The barrier of a one-member run, open and with its first start taken.
    fn barrier_open(tag: &str) -> (util::Isolated, Store, Worklist) {
        let (env, store) = scratch(tag);
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        assert!(spawned().iter().any(|(id, _)| id == "run · barrier 1"), "the check's first start");
        (env, store, w)
    }

    /// Make the open check on group 1 as old as it would be `secs` after its
    /// last start or failure.
    fn age_check(store: &Store, secs: i64) {
        let mut l = store.worklist("run").unwrap();
        let mut g = l.groups();
        g[0].checks.last_mut().unwrap().at = util::iso_at(util::epoch_secs() - secs);
        l.set_groups(&g);
        store.save_worklist(&l).unwrap();
    }

    /// `wsp-328`, seen on `verification` group 2: `claude started but never
    /// became ready` left the pane named on the entry, idle, and the run waited
    /// on it until a person ended it and spawned the check again.
    #[test]
    fn a_check_that_never_started_loses_its_pane_and_is_started_again_on_a_backoff() {
        let (_env, store, w) = barrier_open("checkretry");
        check_seated(&store, 1, "cpd-9");
        let _ = drained(&TOLD);
        check_start_failed(&store, &w, 1, "claude started but never became ready for input");
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()], "the pane the start left is ended");
        let c = checks(&store, 1);
        assert_eq!((c.len(), c[0].pane.clone(), c[0].tries), (1, None, 1), "off the entry, so it is not read as in flight");
        let told = drained(&TOLD);
        assert!(told.len() == 1 && told[0].contains("try 1 of 3") && told[0].contains("never became ready"), "{told:?}");

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "not again at once — the backoff");
        age_check(&store, 4 * 60);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("run · barrier 1".to_string(), "claude".into())], "the same entry, started again");
        let c = checks(&store, 1);
        assert_eq!((c.len(), c[0].tries), (1, 2), "a second start and not a second check");

        // It comes up this time and nothing more is owed.
        check_seated(&store, 1, "cpd-10");
        age_check(&store, 60 * 60);
        step(&store, &w, &Fixed::new(&[("cpd-10", State::Working)]));
        assert!(spawned().is_empty() && drained(&ENDED).is_empty(), "a check that is running is left alone");
    }

    /// The wake a failed start on `run` scheduled, as [`wake_after`] launches it.
    fn woken_after(secs: &str) -> Vec<String> {
        ["worklist", "advance", "run", "--event", RETRY_EVENT, "--after", secs].iter().map(|a| a.to_string()).collect()
    }

    /// wsp-087, m1-fixes-3's wsp-079: a member's start failed "never ready",
    /// the run said "starting it again in 30 s", and the retry came on the
    /// daemon's next pass, 74 s later. The failure now schedules a step for one
    /// second after the delay its line named, and that step finds the start free.
    #[test]
    fn a_member_start_never_ready_is_retried_at_the_delay_its_line_names() {
        let (_env, store) = scratch("start-wake");
        task(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let _ = (drained(&SAID), drained(&WAKES));
        FAIL.with(|f| *f.borrow_mut() = Some("claude started but never became ready for input within 60 s (no seat.started heard)".into()));
        step(&store, &w, &Blind);
        FAIL.with(|f| *f.borrow_mut() = None);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "the start that failed");

        let said = drained(&SAID);
        let line = said.iter().find(|l| l.contains("m-1 did not start on try 1 of 3")).unwrap_or_else(|| panic!("{said:?}"));
        assert!(line.ends_with("starting it again in 30 s"), "{line}");
        assert_eq!(drained(&WAKES), vec![woken_after("31")], "a step for just past the delay the line named, not the next pass");

        age_start(&store, "m-1", 29);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "inside the 30 s it is still held");
        age_start(&store, "m-1", 31);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "the woken step starts it again");
    }

    /// And the barrier check's seat, the other start the run retries on a told
    /// delay: the same wake, and the step at it finds the entry due.
    #[test]
    fn a_check_start_never_ready_is_retried_at_the_delay_its_line_names() {
        let (_env, store, w) = barrier_open("checkwake");
        check_seated(&store, 1, "cpd-9");
        let _ = (drained(&SAID), drained(&WAKES), drained(&TOLD));
        check_start_failed(&store, &w, 1, "claude started but never became ready for input within 60 s (last read Idle)");
        let said = drained(&SAID);
        assert!(said.iter().any(|l| l.contains("its check did not start on try 1 of 3") && l.ends_with("starting it again in 30 s")), "{said:?}");
        assert_eq!(drained(&WAKES), vec![woken_after("31")]);

        age_check(&store, 29);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "inside the 30 s it waits");
        age_check(&store, 31);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("run · barrier 1".to_string(), "claude".into())], "the woken step starts it again");
    }

    #[test]
    fn a_spawn_that_fails_in_the_step_is_retried_and_the_person_is_flagged_when_the_tries_run_out() {
        let (_env, store, w) = barrier_open("checkspent");
        let _ = drained(&TOLD);
        let raised = || crate::message::raised(&store).into_iter().filter(|m| m.about.scope() == Some("run")).collect::<Vec<_>>();

        // Tries 2 and 3 fail inside the step itself, as the live spawn did.
        FAIL.with(|f| *f.borrow_mut() = Some("claude started but never became ready for input".into()));
        check_start_failed(&store, &w, 1, "the first try's error");
        assert!(raised().is_empty());
        for (try_, wait) in [(2, 4 * 60), (3, 6 * 60)] {
            age_check(&store, wait);
            step(&store, &w, &Blind);
            assert_eq!(spawned().len(), 1, "try {try_}");
            assert_eq!(checks(&store, 1)[0].tries, try_);
        }
        FAIL.with(|f| *f.borrow_mut() = None);

        let c = checks(&store, 1);
        assert!(c[0].gave_up && c[0].pane.is_none() && c[0].open(), "{c:?}");
        let flags = raised();
        assert_eq!(flags.len(), 1, "one flag on the list");
        assert_eq!(flags[0].about, crate::message::About::Group { scope: "run".into(), group: 1 });
        assert!(flags[0].text.contains("after 3 tries") && flags[0].text.contains("never became ready"), "{}", flags[0].text);
        assert!(drained(&TOLD).iter().any(|t| t.contains("person is flagged")), "the governor hears it too");

        // Spent: no fourth start however long it waits, and no second flag.
        age_check(&store, 60 * 60);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
        assert_eq!(raised().len(), 1);

        // A person's `advance` takes it from the top: the entry is abandoned and a fresh one starts.
        assert!(crate::barrier::try_again(&store, "run"));
        step(&store, &w, &Blind);
        let c = checks(&store, 1);
        assert_eq!((c.len(), c[0].state, c[1].tries), (2, crate::barrier::State::Abandoned, 1), "{c:?}");
        assert_eq!(spawned().len(), 1);
    }

    /// wsp-600 (retro-2 group 4): the check never started, the barrier run was
    /// green and every member landed with its verifier holding, and the list
    /// waited for a person to find that out. The notice names the evidence and
    /// says the stop's mechanical parts are met.
    #[test]
    fn a_check_that_gave_up_names_the_green_run_and_the_held_members() {
        let (_env, store, w) = barrier_open("checkevidence");
        crate::barrier::note_run(&store, "run", 1, crate::barrier::Suite::Green, None);
        let _ = drained(&TOLD);
        check_gave_up(&store, &w, 1, 3, "claude did not start in cpd-1: cpd-1 is gone");
        let flags = crate::message::raised(&store);
        assert_eq!(flags.len(), 1, "one notice to the person");
        let text = &flags[0].text;
        assert!(text.contains("the barrier run is green") && text.contains("all 1 members landed with their verifiers holding"), "{text}");
        assert!(text.contains("passes it in one step"), "{text}");
        let told = drained(&TOLD).join("\n");
        assert!(told.contains("the barrier run is green") && told.contains("passes it in one step"), "the governor hears the same: {told}");

        // A member whose verifier does not hold is named, and `go` is not offered as the answer.
        let mut t = store.find_task("m-1").unwrap();
        t.log("sent back: something");
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        let (said_, passable) = barrier_evidence(&store, &store.worklist("run").unwrap(), 1);
        assert!(!passable && said_.contains("held: m-1 (not landed)"), "{said_}");
    }

    /// wsp-600 (retro-2's wsp-565): a member reopened while its group runs sat
    /// at `doing` with no seat because a start that had spent its tries before
    /// the reopen made the run step aside. The sent-back line is a fresh ask.
    #[test]
    fn a_member_reopened_in_a_running_group_is_seated_in_one_step_whatever_an_earlier_start_spent() {
        let (_env, store) = scratch("reopened-seat");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        for _ in 0..crate::barrier::TRIES {
            crate::starts::missed_under(&store, "m-1", None);
        }
        assert!(crate::starts::spent(&store, "m-1"));
        left_unseated(&store, "m-1", &[&format!("{SENT_BACK} the layering check is red on master")], 60);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "seated within one tick");
        assert!(!crate::starts::spent(&store, "m-1"), "the earlier tries were not this ask's");
    }

    /// wsp-600, the real shape of retro-2's wsp-565 (cycle test fake, not the
    /// sandbox): an `x2` group with three members sent back at `doing`, none
    /// seated. Each used to count the other two as in flight (`2 >= 2`), so
    /// none started and nothing was said. Two are seated in one step, the
    /// third as a place frees, and a third that waits says it is the cap.
    #[test]
    fn three_sent_back_members_of_an_x2_group_are_seated_two_at_a_time_and_the_third_as_a_place_frees() {
        let (_env, store) = scratch("x2-sent-back");
        for m in ["m-1", "m-2", "m-3"] {
            task(&store, m, Status::Doing);
            left_unseated(&store, m, &[&format!("{SENT_BACK} the layering check is red on master")], 60);
        }
        let mut w = list(&store, &[(&["m-1", "m-2", "m-3"], "claude")]);
        let mut g = w.groups();
        g[0].cap = Some(2);
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        let _ = spawned();

        step(&store, &w, &Blind);
        let started = spawned();
        assert_eq!(started.len(), 2, "two places, two seated in one step: {started:?}");
        assert_eq!(started.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(), ["m-1", "m-2"]);

        // The third is held by the group's cap, and its notice says so.
        let mut t = store.find_task("m-3").unwrap();
        t.updated = util::iso_at(util::epoch_secs() - 400);
        store.save_task(&t).unwrap();
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the group is full");
        name_silent(&store, &w);
        let line = said(&store);
        assert!(line.contains("m-3") && line.contains("waits for a place in its group's x2"), "{line}");
        assert!(!line.contains("may not have arrived"), "{line}");

        // A place frees: the third is seated.
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        let after = spawned();
        assert!(after.iter().any(|(m, _)| m == "m-3"), "the third is seated: {after:?}");
    }

    /// wsp-600: a start that gave up under a throttle or memory hold is tried
    /// again when the hold lets go, with nobody running `advance`.
    #[test]
    fn a_start_that_gave_up_under_a_hold_is_retried_when_the_hold_lets_go() {
        let (_env, store) = scratch("retry-after-hold");
        task(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        for _ in 0..crate::barrier::TRIES {
            crate::starts::missed_under(&store, "m-1", Some("throttle: memory"));
        }
        assert!(crate::starts::spent(&store, "m-1"));
        // Still held: left alone.
        assert!(crate::starts::forgive_after_hold(&store, &["m-1".to_string()], true).is_empty());
        assert!(crate::starts::spent(&store, "m-1"));
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "the hold is not on, so the step starts it");
        assert!(said(&store).contains("the hold has let go"), "{}", said(&store));
    }

    /// wsp-600: and a start that gave up with no hold on is a person's, as before.
    #[test]
    fn a_start_that_gave_up_with_no_hold_on_waits_for_a_person() {
        let (_env, store) = scratch("no-retry-without-hold");
        task(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        for _ in 0..crate::barrier::TRIES {
            crate::starts::missed_under(&store, "m-1", None);
        }
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
    }

    /// wsp-600: "its start may not have arrived" is counted, not repeated: the
    /// third notice raises the row to the governor and the person's panel.
    #[test]
    fn a_silent_member_is_named_three_times_and_then_raised() {
        let (_env, store) = scratch("silent-raises");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let _ = drained(&TOLD);
        let raised = || crate::message::raised(&store);
        for n in 1..=3 {
            left_unseated(&store, "m-1", &[], 400);
            name_silent(&store, &w);
            assert!(said(&store).contains(&format!("notice {n} of 3")), "{}", said(&store));
            assert_eq!(raised().len(), usize::from(n == 3), "raised on the last notice only (after {n})");
        }
        assert!(drained(&TOLD).iter().any(|t| t.contains("m-1") && t.contains("person is flagged")));
        // Once: a fourth notice says nothing more.
        left_unseated(&store, "m-1", &[], 400);
        name_silent(&store, &w);
        assert_eq!(raised().len(), 1);
    }

    /// The spawn killed before it could say it failed: no seat, no report, and
    /// the last try. Nothing else would ever see it.
    #[test]
    fn a_last_try_that_never_reported_back_is_flagged_by_the_tick() {
        let (_env, store, w) = barrier_open("checklost");
        let mut l = store.worklist("run").unwrap();
        let mut g = l.groups();
        g[0].checks[0].tries = crate::barrier::TRIES;
        l.set_groups(&g);
        store.save_worklist(&l).unwrap();

        age_check(&store, 10);
        step(&store, &w, &Blind);
        assert!(crate::message::raised(&store).is_empty(), "not yet: a start may be in flight");
        age_check(&store, 4 * 60);
        step(&store, &w, &Blind);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "no fourth start");
        let flags: Vec<_> = crate::message::raised(&store).into_iter().filter(|m| m.about.scope() == Some("run")).collect();
        assert_eq!(flags.len(), 1, "said once");
        assert!(checks(&store, 1)[0].gave_up);
    }

    /// A check whose pane reads empty or still starting is not coming up, and
    /// the tick says so without waiting for the spawn that left it to report.
    #[test]
    fn a_check_seat_that_is_not_up_after_five_minutes_is_ended_and_started_again_on_the_backoff() {
        let (_env, store, w) = barrier_open("checknotup");
        check_seated(&store, 1, "cpd-9");
        let starting = Fixed::new(&[("cpd-9", State::Starting)]);
        age_check(&store, 4 * 60);
        step(&store, &w, &starting);
        assert!(drained(&ENDED).is_empty(), "a slow start inside five minutes is left alone");
        age_check(&store, 6 * 60);
        step(&store, &w, &starting);
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()]);
        let c = checks(&store, 1);
        assert_eq!((c[0].pane.clone(), c[0].tries), (None, 1));
        assert!(spawned().is_empty(), "its backoff has begun, and it is not the same pass's start");
        age_check(&store, 4 * 60);
        step(&store, &w, &starting);
        assert_eq!(spawned().len(), 1);
        assert_eq!(checks(&store, 1)[0].tries, 2);
    }

    #[test]
    fn the_tries_and_the_give_up_survive_the_checks_block() {
        let mut c = crate::barrier::Check::opened(Some("abc".into()), None);
        c.tries = 3;
        c.gave_up = true;
        let text = crate::barrier::text(&[c.clone()]);
        let mut back = Vec::new();
        crate::barrier::push(&mut back, &text);
        assert_eq!(back, vec![c]);
    }

    /// **A verifier whose seat has gone is superseded, and a fresh one starts**
    /// (`wsp-252`, ux-kit group 2). A park closed cpd-615 and cpd-608 under
    /// their passes and wrote nothing on them; both read `running` on panes
    /// that no longer existed, and advance, resume and `spawn --verify` all
    /// started nothing. The check has had this since `wsp-230` (the test
    /// above); this is the verifier's half.
    #[test]
    fn a_verifier_whose_seat_has_gone_is_superseded_and_a_fresh_one_starts() {
        use crate::verification::{Ending, State as V};
        struct Gone;
        impl Seats for Gone {
            fn state(&self, _seat: &str) -> Option<State> {
                None
            }
            fn absent(&self, seat: &str) -> bool {
                seat == "cpd-615"
            }
        }
        let (_env, store) = scratch("verifiergone");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        crate::verification::name_seat(&store, "m-1", "cpd-615");
        assert_eq!(spawned().len(), 1);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a reader that cannot see the seat waits");

        step(&store, &w, &Gone);
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.iter().map(|p| p.state).collect::<Vec<_>>(), vec![V::Superseded, V::Running]);
        assert_eq!(ps[0].ending, Ending::Ended, "nothing is left to end, and nothing tries");
        assert!(drained(&ENDED).is_empty(), "a seat that went is not despawned");
        assert_eq!(spawned(), vec![("m-1 · verifying".to_string(), "claude".to_string())]);
    }

    /// **A verifier whose agent went but whose seat still stands is ended, not
    /// written off** (`wsp-085`, m1-fixes-3 group 2). wsp-080's verifier cpd-419
    /// read `Gone` after one turn and no verdict; its pass was written `ended`
    /// as if its pane had closed, so nothing ever ended it, and the list closed
    /// with cpd-419 still standing in wsp-080's tree. Now it is superseded and
    /// ended at once, the fresh verifier's verdict is the row's record, and the
    /// group's end finds nothing of it left to end.
    #[test]
    fn a_verifier_whose_agent_went_in_a_seat_still_standing_is_ended_and_the_next_one_verifies() {
        use crate::verification::{Ending, State as V};
        let (_env, store) = scratch("verifierdeadseat");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        crate::verification::name_seat(&store, "m-1", "cpd-419");
        assert_eq!(spawned().len(), 1);
        let _ = drained(&ENDED);

        let _ = drained(&SAID);
        step(&store, &w, &Fixed::new(&[("cpd-419", State::Gone)]));
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.iter().map(|p| p.state).collect::<Vec<_>>(), vec![V::Superseded, V::Running]);
        assert_eq!(drained(&ENDED), vec!["cpd-419".to_string()], "the seat that still stands is ended at once");
        assert_eq!(ps[0].ending, Ending::Ended, "by that ending, not written off");
        assert_eq!(spawned(), vec![("m-1 · verifying".to_string(), "claude".to_string())], "and a fresh one starts");
        let said = drained(&SAID);
        assert!(said.iter().any(|l| l.contains("cpd-419 still stands with its agent gone")), "{said:?}");

        crate::verification::name_seat(&store, "m-1", "cpd-1000");
        crate::verification::record(&store, "m-1", V::Holds, "holds", Some("cpd-1000"), None).unwrap();
        assert!(verified(&store.tasks(), "m-1"), "the restart's verdict is the row's verified record");

        end_group(&store, &Fixed::new(&[("cpd-419", State::Gone), ("cpd-1000", State::Idle)]), &store.worklist("run").unwrap(), 1);
        let ended = drained(&ENDED);
        assert!(!ended.contains(&"cpd-419".to_string()), "nothing of the lost seat is left at close: {ended:?}");
    }

    /// A seat that is idle, has taken no turn, and answers what its prompt holds.
    struct Untaken {
        typed: Option<bool>,
        pressed: RefCell<Vec<String>>,
        resent: RefCell<Vec<(String, String)>>,
    }

    impl Untaken {
        fn holding(typed: Option<bool>) -> Untaken {
            Untaken { typed, pressed: RefCell::new(Vec::new()), resent: RefCell::new(Vec::new()) }
        }
    }

    impl Seats for Untaken {
        fn state(&self, _seat: &str) -> Option<State> {
            Some(State::Idle)
        }
        fn turn_began_since(&self, _store: &Store, _seat: &str, _since: i64) -> Option<bool> {
            Some(false)
        }
        fn typed(&self, _seat: &str) -> Option<bool> {
            self.typed
        }
        fn press(&self, seat: &str) -> bool {
            self.pressed.borrow_mut().push(seat.to_string());
            true
        }
        fn resend(&self, seat: &str, text: &str) -> Option<crate::place::Result<crate::place::Delivery>> {
            self.resent.borrow_mut().push((seat.to_string(), text.to_string()));
            Some(Ok(crate::place::Delivery::Unconfirmed))
        }
        /// A pid that is not running, so the ending is confirmed.
        fn pids(&self, _seat: &str) -> Option<Vec<u32>> {
            Some(vec![u32::MAX - 1])
        }
    }

    /// A member under review with its verifier seated in `seat`, the pass
    /// opened five minutes ago.
    fn verifier_waiting(store: &Store, seat: &str) -> Worklist {
        task(store, "m-1", Status::Review);
        let w = list(store, &[(&["m-1"], "claude")]);
        step(store, &w, &Blind);
        crate::verification::name_seat(store, "m-1", seat);
        let _ = spawned();
        let mut m = store.find_task("m-1").unwrap();
        let mut ps = crate::verification::passes(&m);
        ps[0].at = util::iso_at(util::epoch_secs() - 300);
        crate::verification::write(&mut m, &ps);
        store.save_task(&m).unwrap();
        w
    }

    /// Every `cycle.log` line dated five minutes back, which is what the
    /// next step waits out.
    fn age_the_log(store: &Store) {
        let path = store.state_file("cycle.log");
        let then = util::iso_at(util::epoch_secs() - 300);
        let aged: String = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(|l| format!("{then} {}\n", l.split_once(' ').map_or(l, |(_, r)| r)))
            .collect();
        std::fs::write(&path, aged).unwrap();
    }

    /// **An order sitting typed in a seat the run is waiting on is pressed —
    /// once — and a seat that still takes no turn is replaced** (`wsp-252`).
    /// compound-415's verifier sat with its order in the prompt from 04:56
    /// until Ed pressed Enter at 06:17; here the run presses it at the first
    /// look past two minutes, does not press again while the press is fresh,
    /// and ends the seat and starts another when the press bought nothing.
    #[test]
    fn an_order_typed_and_unsent_is_pressed_once_and_then_the_seat_is_replaced() {
        use crate::verification::State as V;
        let (_env, store) = scratch("untakenpress");
        let w = verifier_waiting(&store, "cpd-627");
        let seats = Untaken::holding(Some(true));

        step(&store, &w, &seats);
        assert_eq!(*seats.pressed.borrow(), vec!["cpd-627".to_string()], "pressed");
        assert!(spawned().is_empty() && drained(&ENDED).is_empty(), "and nothing else");
        step(&store, &w, &seats);
        assert_eq!(seats.pressed.borrow().len(), 1, "not again while the press is fresh");

        age_the_log(&store);
        step(&store, &w, &seats);
        assert_eq!(seats.pressed.borrow().len(), 1, "a press is taken once");
        assert_eq!(drained(&ENDED), vec!["cpd-627".to_string()], "the seat that took nothing is ended");
        assert_eq!(spawned(), vec![("m-1 · verifying".to_string(), "claude".to_string())], "and another starts");
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.iter().map(|p| p.state).collect::<Vec<_>>(), vec![V::Superseded, V::Running]);
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap();
        assert!(log.contains("pressed submit in cpd-627,") && log.contains("replacing cpd-627:"), "{log}");
    }

    /// An empty prompt and no turn is an order that never arrived: the run
    /// sends it again, the verifier's own order, to the seat — once.
    #[test]
    fn an_order_that_never_reached_the_prompt_is_sent_again_once() {
        let (_env, store) = scratch("untakenresend");
        let w = verifier_waiting(&store, "cpd-609");
        let seats = Untaken::holding(Some(false));
        step(&store, &w, &seats);
        step(&store, &w, &seats);
        let resent = seats.resent.borrow();
        assert_eq!(resent.len(), 1, "once: {resent:?}");
        assert_eq!(resent[0].0, "cpd-609", "to the seat, not to the member's holder");
        assert!(resent[0].1.contains("m-1"), "the verifier's own order: {}", resent[0].1);
        assert!(seats.pressed.borrow().is_empty(), "nothing in the prompt to press");
    }

    /// A prompt nobody can read is left alone: no press, no resend, no line.
    #[test]
    fn a_seat_whose_prompt_cannot_be_read_is_left_alone() {
        let (_env, store) = scratch("untakenblind");
        let w = verifier_waiting(&store, "cpd-1");
        let seats = Untaken::holding(None);
        step(&store, &w, &seats);
        age_the_log(&store);
        step(&store, &w, &seats);
        assert!(seats.pressed.borrow().is_empty() && seats.resent.borrow().is_empty());
        assert!(drained(&ENDED).is_empty() && spawned().is_empty());
    }

    /// **Under `WSP_ORDERS=store` a verifier's seat is replaced once its first
    /// order is `failed`, and nothing is pressed or resent** (wsp-126): the
    /// order's retries are the orders machine's, so an order typed and unsent
    /// is left to them, and the replacement waits on the order's state, not on
    /// a press's line in `cycle.log`.
    #[test]
    fn under_store_a_failed_first_order_replaces_the_verifier_once_and_nothing_is_pressed() {
        use crate::verification::State as V;
        let (_env, store) = scratch("untakenstored");
        let w = verifier_waiting(&store, "cpd-640");
        let seats = Untaken::holding(Some(true));

        crate::untaken::as_stored(&[]);
        step(&store, &w, &seats);
        age_the_log(&store);
        step(&store, &w, &seats);
        assert!(seats.pressed.borrow().is_empty() && seats.resent.borrow().is_empty(), "no press, no resend");
        assert!(drained(&ENDED).is_empty() && spawned().is_empty(), "and no replacement while the order is not failed");

        crate::untaken::as_stored(&[("cpd-640", "the prompt did not take it")]);
        step(&store, &w, &seats);
        assert_eq!(drained(&ENDED), vec!["cpd-640".to_string()], "the seat whose order failed is ended");
        assert_eq!(spawned(), vec![("m-1 · verifying".to_string(), "claude".to_string())], "and another starts");
        let ps = passes_on(&store, "m-1");
        assert_eq!(ps.iter().map(|p| p.state).collect::<Vec<_>>(), vec![V::Superseded, V::Running]);

        step(&store, &w, &seats);
        assert!(drained(&ENDED).is_empty() && spawned().is_empty(), "replaced once");
        assert!(seats.pressed.borrow().is_empty() && seats.resent.borrow().is_empty());
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap();
        assert!(log.contains("did not receive its first order") && log.contains("replacing cpd-640:"), "{log}");
        assert!(!log.contains("pressed submit in cpd-640,"), "{log}");
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
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        check_seated(&store, 1, "cpd-last");

        // Still deciding, so nothing is ended: the check is not finished work,
        // it is a barrier somebody may still pass.
        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert!(
            tests::ENDED.with(|e| e.borrow().is_empty()),
            "a check still deciding is nobody's to end"
        );

        // Its `go` settles it, and the run has nothing left to govern.
        crate::barrier::settle(&store, "run", 1, crate::barrier::State::Passes);
        let mut l = store.worklist("run").unwrap();
        let mut g = l.groups();
        g[0].verdict = "passed".into();
        l.set_groups(&g);
        store.save_worklist(&l).unwrap();
        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert_eq!(
            tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>()),
            vec!["cpd-last".to_string()],
            "and the run's last barrier check is ended, whatever its own verb arranged"
        );

        // And once, because the entry records it.
        crate::repair::tick(&store, &Blind, &mut Pass::new());
        assert!(tests::ENDED.with(|e| e.borrow().is_empty()), "an ended check is not ended twice");
        assert_eq!(checks(&store, 1)[0].ending, crate::barrier::Ending::Ended);
    }

    /// A list closed by `done` while its check was still reading: nothing is
    /// left for the check to decide, so its seat is ended though it never
    /// settled — or it would stand for ever, since only a verb settles one.
    #[test]
    fn a_check_still_reading_when_its_list_is_done_is_ended_by_the_tick() {
        let (_env, store) = scratch("donecheck");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        check_seated(&store, 1, "cpd-open");
        assert!(checks_finished(&store, &Blind).is_empty(), "a running list's open check is still deciding");
        let mut l = store.worklist("run").unwrap();
        l.set_status(WorklistStatus::Done);
        store.save_worklist(&l).unwrap();
        assert_eq!(checks_finished(&store, &Blind), vec!["cpd-open".to_string()]);
    }

    /// `wsp-385`: a second check seated while the first settled the barrier
    /// stands open on a group that has passed. Nothing is left for it to decide,
    /// so the tick ends its seat by its pane and writes the entry off — and the
    /// closing tick neither tells it to close nor writes it a `go`.
    #[test]
    fn a_check_open_on_a_barrier_already_passed_is_ended_and_not_told_to_close() {
        let (_env, store) = scratch("passedcheck");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        check_seated(&store, 1, "cpd-second");
        assert!(checks_finished(&store, &Blind).is_empty(), "an open check on an open barrier is deciding");
        let mut l = store.worklist("run").unwrap();
        let mut g = l.groups();
        g[0].verdict = "2026-11-05T15:49:26Z go".into();
        l.set_groups(&g);
        store.save_worklist(&l).unwrap();
        tests::TOLD.with(|t| t.borrow_mut().clear());
        crate::closing::tick_at(&store, &Blind, util::epoch_secs() + 3600);
        assert!(tests::TOLD.with(|t| t.borrow().is_empty()), "a passed barrier's check is not told to close");
        assert_eq!(checks_finished(&store, &Blind), vec!["cpd-second".to_string()]);
        let c = &checks(&store, 1)[0];
        assert_eq!(c.ending, crate::barrier::Ending::Ended);
        assert_eq!(c.state, crate::barrier::State::Abandoned);
        assert!(checks_finished(&store, &Blind).is_empty(), "ended once");
    }

    /// Panes the backends hold, all idle, each with its label.
    struct Panes(Vec<(&'static str, &'static str)>);

    impl Seats for Panes {
        fn state(&self, seat: &str) -> Option<State> {
            self.0.iter().any(|(s, _)| *s == seat).then_some(State::Idle)
        }
        fn census(&self) -> Option<Vec<(String, String)>> {
            Some(self.0.iter().map(|(s, l)| (s.to_string(), l.to_string())).collect())
        }
    }

    /// `wsp-340`: the first try's pane of a check whose retry passed the
    /// barrier is named by no entry, and stood open for ever.
    #[test]
    fn a_check_pane_no_entry_names_is_ended_once_its_barrier_settled() {
        let (_env, store) = scratch("orphancheck");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        check_seated(&store, 1, "cpd-retry");
        let panes = Panes(vec![("cpd-first", "run · barrier 1"), ("cpd-retry", "run · barrier 1"), ("cpd-other", "run · barrier 2"), ("cpd-1", "m-1 · work")]);

        assert!(checks_orphaned(&store, &panes).is_empty(), "the check is still open: the pane may be reading it");

        crate::barrier::settle(&store, "run", 1, crate::barrier::State::Passes);
        assert_eq!(checks_orphaned(&store, &panes), vec!["cpd-first".to_string()], "not the recorded seat, not another group's, not a member's");
        assert_eq!(tests::ENDED.with(|e| e.borrow_mut().drain(..).collect::<Vec<_>>()), vec!["cpd-first".to_string()]);
        assert!(checks_orphaned(&store, &Panes(vec![("cpd-retry", "run · barrier 1")])).is_empty(), "the recorded seat is checks_finished's");
    }

    #[test]
    fn an_orphan_check_pane_mid_turn_or_unlisted_is_left() {
        let (_env, store) = scratch("orphanturn");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        let _ = spawned();
        crate::barrier::settle(&store, "run", 1, crate::barrier::State::Passes);
        assert!(checks_orphaned(&store, &Blind).is_empty(), "nobody answered the census");
        let working = Fixed::new(&[("cpd-first", State::Working)]);
        struct Mid(Fixed);
        impl Seats for Mid {
            fn state(&self, seat: &str) -> Option<State> {
                self.0.state(seat)
            }
            fn census(&self) -> Option<Vec<(String, String)>> {
                Some(vec![("cpd-first".into(), "run · barrier 1".into())])
            }
        }
        assert!(checks_orphaned(&store, &Mid(working)).is_empty(), "a turn in flight is not ended");
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

    /// `wsp-629`: a group left `manual` after its person's row moved out is
    /// turned on while it runs, and the run then starts what has not started —
    /// and goes on leaving a member already started to the agent it has.
    #[test]
    fn a_running_groups_line_changed_from_manual_makes_the_run_start_its_unstarted_members() {
        let (_env, store) = scratch("turnon");
        task(&store, "m-1", Status::Todo);
        task(&store, "m-2", Status::Doing);
        let w = list(&store, &[(&["m-1", "m-2"], "manual")]);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "manual: nothing starts");

        let bare = Args::synth("worklist", &["group", "run", "1"], &[("agent", "opencode some/model")]);
        assert_eq!(crate::cmd_worklist::group(&store, &bare), 2, "a reason is owed, as for the stop");
        assert_eq!(store.worklist("run").unwrap().groups()[0].agent, "manual");

        let said = Args::synth("worklist", &["group", "run", "1"], &[("agent", "opencode some/model"), ("why", "its person's row moved out")]);
        assert_eq!(crate::cmd_worklist::group(&store, &said), 0);
        let now = store.worklist("run").unwrap();
        assert_eq!(now.groups()[0].agent, "opencode some/model");
        assert!(now.body.contains("agent line changed while it runs — why: its person's row moved out"), "{}", now.body);

        step(&store, &now, &Blind);
        assert_eq!(spawned(), vec![("m-1".into(), "opencode".into())], "the member not started starts, on the new line");
        assert_eq!(store.find_task("m-2").unwrap().status(), Status::Doing, "and the one started is not touched");

        let back = Args::synth("worklist", &["group", "run", "1"], &[("agent", "manual"), ("why", "the person has it again")]);
        assert_eq!(crate::cmd_worklist::group(&store, &back), 0);
        task(&store, "m-3", Status::Todo);
        let mut w = store.worklist("run").unwrap();
        let mut g = w.groups();
        g[0].members.push("m-3".into());
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "an agent kind to manual stops new starts");
    }

    /// `wsp-629`: a `manual` group whose members have all landed and whose
    /// stop an agent can check is not left "waiting on Ed".
    #[test]
    fn a_manual_barrier_with_a_checkable_stop_and_every_member_landed_starts_a_check_seat() {
        let (_env, store) = scratch("manualcheck");
        task(&store, "m-1", Status::Review);
        task(&store, "g-1", Status::Review);
        let mut w = list(&store, &[(&["m-1"], "manual"), (&["g-1"], "claude sonnet high")]);
        let mut g = w.groups();
        g[0].stop = "the full check is green on the trunk".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();

        step(&store, &w, &Blind);
        assert_eq!(
            spawned(),
            vec![(crate::barrier::seat_label("run", 1), "claude".into())],
            "the check starts, on the list's usual line, and nothing of the member"
        );
        assert_eq!(checks(&store, 1).len(), 1);
    }

    /// The other side of it: a stop that names the person, one not written, a
    /// member that has not landed, and an empty (hand-run) line all stay put.
    #[test]
    fn a_manual_barrier_waits_for_its_person_when_the_stop_names_one_or_a_member_is_out() {
        let cases: [(&str, &str, Status); 4] = [
            ("manual", "waiting on Ed's results", Status::Review),
            ("manual", "", Status::Review),
            ("manual", "the full check is green", Status::Doing),
            ("", "the full check is green", Status::Review),
        ];
        for (i, (agent, stop, status)) in cases.iter().enumerate() {
            let (_env, store) = scratch(&format!("manualwait{i}"));
            task(&store, "m-1", *status);
            let mut w = list(&store, &[(&["m-1"], agent), (&["m-2"], "claude")]);
            let mut g = w.groups();
            g[0].stop = stop.to_string();
            w.set_groups(&g);
            store.save_worklist(&w).unwrap();
            step(&store, &w, &Blind);
            assert!(spawned().is_empty(), "case {i} ({agent:?}, {stop:?}, {status:?}): nothing starts");
            assert!(checks(&store, 1).is_empty(), "case {i}: and no check is opened");
        }
    }

    /// The cap is on the work, and a member already going counts against it.
    #[test]
    fn a_group_start_with_no_pack_builder_makes_no_spawn_attempt_and_names_it() {
        let (_env, store) = scratch("nopack");
        for m in ["m-1", "m-2"] {
            task(&store, m, Status::Todo);
        }
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        PACK_MISSING.with(|p| *p.borrow_mut() = Some("the pack builder is not built: set WSP_PACK_BIN".into()));
        step(&store, &w, &Blind);
        PACK_MISSING.with(|p| *p.borrow_mut() = None);
        assert!(spawned().is_empty(), "nothing is started while a seat cannot be given a packet");
        let w = store.worklist(&w.id).unwrap();
        assert_eq!(w.status(), WorklistStatus::Parked);
        assert!(w.section("Log").unwrap_or_default().contains("the pack builder is not built"), "{}", w.section("Log").unwrap_or_default());
    }

    #[test]
    fn a_list_wide_spawn_refusal_parks_the_list_after_one_try() {
        let (_env, store) = scratch("listwide");
        for m in ["m-1", "m-2"] {
            task(&store, m, Status::Todo);
        }
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        FAIL.with(|f| *f.borrow_mut() = Some("m-1 is not given a packet: the pack builder is not built".into()));
        step(&store, &w, &Blind);
        FAIL.with(|f| *f.borrow_mut() = None);
        assert_eq!(spawned().len(), 1, "one try, and the second row is not tried");
        assert_eq!(store.worklist(&w.id).unwrap().status(), WorklistStatus::Parked);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Todo);
    }

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

    // ---- the seat's cap (`wsp-231`) --------------------------------------

    fn ids(started: Vec<(String, String)>) -> Vec<String> {
        started.into_iter().map(|(id, _)| id).collect()
    }

    fn said_capped() -> Vec<String> {
        drained(&SAID).into_iter().filter(|l| l.contains("the seat is at its cap")).collect()
    }

    /// A second running list beside `run`, for the cases where two share the
    /// machine.
    fn list_named(store: &Store, id: &str, members: &[&str], agent: &str) -> Worklist {
        let mut w = Worklist::new(id, id);
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group { members: members.iter().map(|x| x.to_string()).collect(), agent: agent.into(), ..Group::default() }]);
        store.save_worklist(&w).unwrap();
        w
    }

    /// gpui-migration's group 10, under Ed's four: seven members and no `xN`
    /// start four, and the rest start as members finish — after the verifier
    /// each finished member is owed, which goes first. What is held back is
    /// said once in `cycle.log` and not on every pass.
    #[test]
    fn under_the_seats_cap_a_seven_member_group_starts_four_and_a_verifier_goes_before_the_fifth() {
        let (_env, store) = scratch("seat-cap");
        let members = ["m-1", "m-2", "m-3", "m-4", "m-5", "m-6", "m-7"];
        for m in members {
            task(&store, m, Status::Todo);
        }
        store.set_seat_cap(Some(4)).unwrap();
        let w = list(&store, &[(&members, "claude")]);
        let _ = drained(&SAID);

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1", "m-2", "m-3", "m-4"], "four, in the order the group names them");
        let said = said_capped();
        assert_eq!(said.len(), 3, "each member held back is said: {said:?}");
        assert!(said[0].contains("m-5 waits — the seat is at its cap of 4: m-1 starting"), "{said:?}");

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "four are going and the seat bears four");
        assert!(said_capped().is_empty(), "and what was said is not said again");

        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · verifying"], "the finished member's verifier takes its place, not m-5");

        hold_all(&store);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-5"], "and the verdict frees the place for the next member");
    }

    // ---- light rows count half a place (`wsp-601`) ------------------------

    /// A row of project `docs`, carrying `tag`.
    fn docs_row(store: &Store, id: &str, tag: &str) {
        let mut t = Task::new(id, id);
        t.status_raw = Status::Todo.as_str().into();
        t.project = Some("docs".into());
        t.tags = vec![tag.into()];
        store.save_task(&t).unwrap();
    }

    /// Project `docs`, declaring `light` as its light tags.
    fn declare_light(store: &Store, light: &str) {
        let mut p = crate::model::Project::new("docs");
        p.light = light.into();
        store.save_project(&p).unwrap();
    }

    /// One group of `members` run on claude, carrying `weight` and `own` (member, word).
    fn weighed_list(store: &Store, members: &[&str], weight: &str, own: &[(&str, &str)]) -> Worklist {
        let w = list(store, &[(members, "claude")]);
        let mut g = w.groups();
        g[0].weight = weight.into();
        g[0].member_weights = own.iter().map(|(m, x)| (m.to_string(), x.to_string())).collect();
        let mut w = w;
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        w
    }

    /// Done when: a cap of 4 admits eight light rows and refuses the ninth, and
    /// the cap line says the weights in use.
    #[test]
    fn a_cap_of_four_admits_eight_light_rows_and_refuses_the_ninth() {
        let (_env, store) = scratch("light-eight");
        let members: Vec<String> = (1..=9).map(|n| format!("r-{n}")).collect();
        for m in &members {
            task(&store, m, Status::Todo);
        }
        store.set_seat_cap(Some(4)).unwrap();
        let refs: Vec<&str> = members.iter().map(String::as_str).collect();
        let w = weighed_list(&store, &refs, "light", &[]);
        let _ = drained(&SAID);

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()).len(), 8, "eight halves are four places");
        let said = said_capped();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("r-9 waits — the seat is at its cap of 4 (4 in use — 0 whole, 8 light at ½; the start waiting counts ½): r-1 starting"), "{said:?}");
    }

    /// Four build rows fill four places; two build and four light do the same.
    #[test]
    fn four_build_rows_or_two_build_and_four_light_fill_a_cap_of_four() {
        let (_env, store) = scratch("light-mix");
        let members = ["b-1", "b-2", "b-3", "b-4", "b-5"];
        for m in members {
            task(&store, m, Status::Todo);
        }
        store.set_seat_cap(Some(4)).unwrap();
        let w = weighed_list(&store, &members, "", &[]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()).len(), 4, "four build rows, the fifth refused");

        drop(_env);
        let (_env, store) = scratch("light-mix-2");
        let members = ["b-1", "b-2", "l-1", "l-2", "l-3", "l-4", "l-5"];
        for m in members {
            task(&store, m, Status::Todo);
        }
        store.set_seat_cap(Some(4)).unwrap();
        let own: Vec<(&str, &str)> = ["l-1", "l-2", "l-3", "l-4", "l-5"].iter().map(|m| (*m, "light")).collect();
        let w = weighed_list(&store, &members, "", &own);
        let _ = drained(&SAID);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["b-1", "b-2", "l-1", "l-2", "l-3", "l-4"], "2 + 4 at those weights, then the next is refused");
        let said = said_capped();
        assert!(said[0].contains("l-5 waits — the seat is at its cap of 4 (4 in use — 2 whole, 4 light at ½;"), "{said:?}");
    }

    /// A project's `light:` tags make its rows light; a row without one is whole.
    #[test]
    fn a_projects_declaration_is_honoured() {
        let (_env, store) = scratch("light-project");
        declare_light(&store, "retro plan");
        for n in 1..=4 {
            docs_row(&store, &format!("r-{n}"), "retro");
        }
        docs_row(&store, "r-5", "build");
        store.set_seat_cap(Some(1)).unwrap();
        let w = weighed_list(&store, &["r-1", "r-2", "r-3", "r-4", "r-5"], "", &[]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["r-1", "r-2"], "two retros fill a place; the third is refused, and r-5 behind it");

        drop(_env);
        let (_env, store) = scratch("light-project-none");
        docs_row(&store, "r-1", "retro");
        docs_row(&store, "r-2", "retro");
        store.set_seat_cap(Some(1)).unwrap();
        let w = weighed_list(&store, &["r-1", "r-2"], "", &[]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["r-1"], "a project that declares nothing has no light rows");
    }

    /// A member's line beats its group's, and the group's beats the project's,
    /// either way.
    #[test]
    fn a_row_override_and_a_group_override_beat_the_projects_declaration() {
        // The group says heavy over a project that says light: one place, one row.
        let (_env, store) = scratch("light-group-heavy");
        declare_light(&store, "retro");
        docs_row(&store, "r-1", "retro");
        docs_row(&store, "r-2", "retro");
        store.set_seat_cap(Some(1)).unwrap();
        let w = weighed_list(&store, &["r-1", "r-2"], "heavy", &[]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["r-1"], "the group's heavy beats the project's light");

        // The group says light over a project that declares nothing.
        drop(_env);
        let (_env, store) = scratch("light-group-light");
        docs_row(&store, "r-1", "build");
        docs_row(&store, "r-2", "build");
        store.set_seat_cap(Some(1)).unwrap();
        let w = weighed_list(&store, &["r-1", "r-2"], "light", &[]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["r-1", "r-2"], "the group's light beats the project's silence");

        // A member heavy in a light group takes a whole place; one light in a
        // heavy group takes half.
        drop(_env);
        let (_env, store) = scratch("light-member-heavy");
        for m in ["r-1", "r-2"] {
            docs_row(&store, m, "build");
        }
        store.set_seat_cap(Some(1)).unwrap();
        let w = weighed_list(&store, &["r-1", "r-2"], "light", &[("r-1", "heavy")]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["r-1"], "the member's heavy beats the group's light");

        drop(_env);
        let (_env, store) = scratch("light-member-light");
        for m in ["r-1", "r-2", "r-3"] {
            docs_row(&store, m, "build");
        }
        store.set_seat_cap(Some(1)).unwrap();
        let w = weighed_list(&store, &["r-1", "r-2", "r-3"], "heavy", &[("r-1", "light"), ("r-2", "light")]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["r-1", "r-2"], "the member's light beats the group's heavy");
    }

    /// The seat's number and a group's `xN` are both caps, and the smaller
    /// wins: a group asking for two gets two on a seat that bears four.
    #[test]
    fn a_groups_cap_under_the_seats_is_the_smaller_of_the_two() {
        let (_env, store) = scratch("seat-cap-x2");
        for m in ["m-1", "m-2", "m-3"] {
            task(&store, m, Status::Todo);
        }
        store.set_seat_cap(Some(4)).unwrap();
        let mut w = list(&store, &[(&["m-1", "m-2", "m-3"], "claude")]);
        let mut g = w.groups();
        g[0].cap = Some(2);
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 2);
        assert!(said_capped().is_empty(), "the group's own cap held the third, not the seat's");
    }

    /// Two lists running together share the seat's number, and a barrier check
    /// owed on one goes ahead of a new member on the other.
    #[test]
    fn two_lists_share_the_seats_cap_and_a_barrier_check_goes_before_a_new_member() {
        let (_env, store) = scratch("seat-cap-two");
        task(&store, "m-1", Status::Todo);
        for n in ["n-1", "n-2", "n-3"] {
            task(&store, n, Status::Todo);
        }
        let run = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        task(&store, "m-2", Status::Todo);
        step(&store, &run, &Blind);
        set(&store, "m-1", Status::Review);
        step(&store, &run, &Blind);
        hold_all(&store);
        let _ = spawned();

        // `run` is at its barrier, owed a check; `two` has three members.
        store.set_seat_cap(Some(2)).unwrap();
        let two = list_named(&store, "two", &["n-1", "n-2", "n-3"], "claude");
        let _ = drained(&SAID);
        step(&store, &two, &Blind);
        assert_eq!(
            ids(spawned()),
            ["run · barrier 1".to_string(), "n-1".to_string()],
            "the other list's check first, then one member: two places between the two lists"
        );
        let said = said_capped();
        assert!(said.iter().any(|l| l.starts_with("two group 1: n-2 waits")), "{said:?}");

        step(&store, &run, &Blind);
        assert!(spawned().is_empty(), "the check is already open: nothing owed twice");
    }

    /// A verifier owed at a full seat waits: no pass is written, it is said
    /// once, and it goes ahead of the next member when a place frees.
    #[test]
    fn a_verifier_owed_at_a_full_seat_waits_and_writes_no_pass() {
        let (_env, store) = scratch("seat-cap-verifier-waits");
        for m in ["m-1", "m-2", "m-3"] {
            task(&store, m, Status::Todo);
        }
        for (h, s) in [("h-1", "w2:p1"), ("h-2", "w2:p2")] {
            task(&store, h, Status::Doing);
            store.set_binding(s, serde_json::json!({ "task_id": h }));
        }
        store.set_seat_cap(Some(4)).unwrap();
        let w = list(&store, &[(&["m-1", "m-2", "m-3"], "claude")]);
        let seats = Fixed::new(&[("w2:p1", State::Working), ("w2:p2", State::Working), ("w2:p3", State::Working)]);
        let _ = drained(&SAID);
        step(&store, &w, &seats);
        assert_eq!(ids(spawned()), ["m-1", "m-2"]);
        set(&store, "m-1", Status::Review);
        task(&store, "h-3", Status::Doing);
        store.set_binding("w2:p3", serde_json::json!({ "task_id": "h-3" }));
        let _ = said_capped();
        step(&store, &w, &seats);
        assert!(spawned().is_empty(), "full: the verifier waits");
        let said = said_capped();
        assert!(said.iter().any(|l| l.contains("m-1 · verifying waits")), "{said:?}");
        let m1 = store.find_task("m-1").unwrap();
        assert!(crate::verification::passes(&m1).is_empty(), "no pass written for a held verifier");
        let seats = Fixed::new(&[("w2:p1", State::Working), ("w2:p2", State::Working), ("w2:p3", State::Gone)]);
        step(&store, &w, &seats);
        assert_eq!(ids(spawned()), ["m-1 · verifying"], "a place frees, the verifier goes before m-3");
    }

    /// A barrier check owed at a full seat waits: no check is written, it is
    /// said once, and it opens when a place frees.
    #[test]
    fn a_barrier_check_owed_at_a_full_seat_waits_and_writes_no_check() {
        let (_env, store) = scratch("seat-cap-check-waits");
        task(&store, "m-1", Status::Todo);
        task(&store, "m-2", Status::Todo);
        let run = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        step(&store, &run, &Blind);
        set(&store, "m-1", Status::Review);
        step(&store, &run, &Blind);
        hold_all(&store);
        let _ = spawned();
        let mut pairs = Vec::new();
        for (h, s) in [("h-1", "w2:p1"), ("h-2", "w2:p2"), ("h-3", "w2:p3"), ("h-4", "w2:p4")] {
            task(&store, h, Status::Doing);
            store.set_binding(s, serde_json::json!({ "task_id": h }));
            pairs.push((s, State::Working));
        }
        store.set_seat_cap(Some(4)).unwrap();
        let _ = drained(&SAID);
        step(&store, &run, &Fixed::new(&pairs));
        assert!(spawned().is_empty(), "full: the check waits");
        let said = said_capped();
        assert!(said.iter().any(|l| l.contains("barrier 1 waits")), "{said:?}");
        assert!(store.worklist("run").unwrap().groups()[0].checks.is_empty(), "no check written for a held one");
        step(&store, &run, &Fixed::new(&pairs));
        assert!(said_capped().is_empty(), "said once");
        pairs[0].1 = State::Gone;
        step(&store, &run, &Fixed::new(&pairs));
        assert_eq!(ids(spawned()), ["run · barrier 1"]);
    }

    /// A governor is not one of the four: a seat it sits in, even one holding
    /// a row, leaves the count where it was. A helper `wsp spawn`ed onto a row
    /// is one of them.
    #[test]
    fn a_governor_seat_is_not_counted_and_a_helper_is() {
        let (_env, store) = scratch("seat-cap-gov");
        let members = ["m-1", "m-2", "m-3", "m-4", "m-5"];
        for m in members {
            task(&store, m, Status::Todo);
        }
        task(&store, "h-1", Status::Doing);
        task(&store, "g-1", Status::Doing);
        store.set_governor("run", serde_json::json!({ "workspace": "w9", "pane": "w9:p1" }));
        store.set_binding("w9:p1", serde_json::json!({ "task_id": "g-1" }));
        store.set_binding("w2:p1", serde_json::json!({ "task_id": "h-1" }));
        store.set_seat_cap(Some(4)).unwrap();
        let w = list(&store, &[(&members, "claude")]);
        let seats = Fixed::new(&[("w9:p1", State::Working), ("w2:p1", State::Working)]);

        step(&store, &w, &seats);
        assert_eq!(ids(spawned()), ["m-1", "m-2", "m-3"], "the helper takes a place and the governor takes none");
    }

    /// `after:` (`wsp-256`, §19.8 Q-7): a member resting on one of its own
    /// group is held until that one has **landed** — not reviewed, not held by
    /// its verifier — and is started by the step after the land.
    #[test]
    fn a_member_is_not_started_until_the_same_group_member_it_rests_on_has_landed() {
        let (_env, store) = scratch("after-hold");
        task(&store, "m-1", Status::Todo);
        task(&store, "m-2", Status::Todo);
        let mut t = store.find_task("m-2").unwrap();
        t.after = vec!["m-1".into()];
        store.save_task(&t).unwrap();
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);

        step(&store, &w, &Blind);
        let ids: Vec<String> = spawned().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["m-1"], "m-2 rests on m-1");
        assert_eq!(store.find_task("m-2").unwrap().status(), Status::Todo, "held, so left as it was");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "m-1 is going and has not landed");

        let mut m1 = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m1, &format!("{} 1234567abc on master", crate::repair::LANDED)).unwrap();
        step(&store, &w, &Blind);
        let ids: Vec<String> = spawned().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["m-2"], "landed is what frees it");
    }

    /// A held member is not running, so it does not take a place under the
    /// group's `parallel`: with a cap of two and one held, two others start.
    #[test]
    fn a_held_member_does_not_count_against_the_groups_parallel() {
        let (_env, store) = scratch("after-cap");
        for m in ["m-1", "m-2", "m-3"] {
            task(&store, m, Status::Todo);
        }
        let mut t = store.find_task("m-2").unwrap();
        t.after = vec!["m-1".into()];
        store.save_task(&t).unwrap();
        let mut w = list(&store, &[(&["m-2", "m-1", "m-3"], "claude")]);
        let mut g = w.groups();
        g[0].cap = Some(2);
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        step(&store, &w, &Blind);
        let mut ids: Vec<String> = spawned().into_iter().map(|(id, _)| id).collect();
        ids.sort();
        assert_eq!(ids, ["m-1", "m-3"]);
    }

    fn capped_group(store: &Store, kind: &str, n: usize, cap: usize) -> Worklist {
        let ids: Vec<String> = (1..=n).map(|i| format!("m-{i}")).collect();
        for m in &ids {
            task(store, m, Status::Todo);
        }
        let mut w = list(store, &[(&ids.iter().map(String::as_str).collect::<Vec<_>>(), "claude")]);
        let mut g = w.groups();
        g[0].cap = Some(cap);
        g[0].kind = kind.into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        w
    }

    /// `wsp-471`: a components group of six at x2 with four free places runs four.
    #[test]
    fn a_components_group_at_x2_runs_as_many_as_the_seat_has_free_places() {
        let (_env, store) = scratch("raised-cap");
        store.set_seat_cap(Some(4)).unwrap();
        let w = capped_group(&store, "components", 6, 2);
        let _ = drained(&SAID);
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 4, "x2 raised to the seat's four");
        assert!(drained(&SAID).join("\n").contains("x2 raised"), "and it says so");
    }

    #[test]
    fn a_group_that_is_not_components_keeps_its_cap() {
        let (_env, store) = scratch("kept-cap");
        store.set_seat_cap(Some(4)).unwrap();
        let w = capped_group(&store, "wiring", 6, 2);
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 2);
    }

    #[test]
    fn a_components_group_with_no_seat_cap_keeps_its_own() {
        let (_env, store) = scratch("kept-cap-no-seat");
        let w = capped_group(&store, "components", 6, 2);
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 2, "no seat cap, no free places to speak of");
    }

    // ---- a member is started once, however many picks race (wsp-607) ----------

    /// The ids the passes in `run` started, from every thread they ran on.
    fn sorted(mut ids: Vec<String>) -> Vec<String> {
        ids.sort();
        ids
    }

    /// Two passes of the start loop in one tick: the second reads the first's
    /// start and starts nothing again.
    #[test]
    fn two_passes_in_one_tick_start_each_member_once() {
        let (_env, store) = scratch("twice-one-tick");
        for m in ["m-1", "m-2"] {
            task(&store, m, Status::Todo);
        }
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        step(&store, &w, &Blind);
        step(&store, &w, &Blind);
        let ids = sorted(spawned().into_iter().map(|(id, _)| id).collect());
        assert_eq!(ids, ["m-1", "m-2"], "one spawn for each row");
    }

    /// A pick that comes while another start of the same row is under way —
    /// its mark taken, its `doing` not yet read — starts nothing, and says so.
    #[test]
    fn a_row_whose_start_is_under_way_is_not_started_again() {
        let (_env, store) = scratch("under-way");
        for m in ["m-1", "m-2"] {
            task(&store, m, Status::Todo);
        }
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        assert!(crate::starts::begin(&store, "m-1"), "the first start takes the mark");
        let _ = drained(&SAID);
        step(&store, &w, &Blind);
        let ids: Vec<String> = spawned().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["m-2"]);
        assert!(drained(&SAID).iter().any(|l| l.contains("m-1 is already being started")), "said, not silent");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Todo, "and the row is left as the first start has it");
        crate::starts::ended(&store, "m-1");
        step(&store, &w, &Blind);
        assert_eq!(spawned().into_iter().map(|(id, _)| id).collect::<Vec<_>>(), ["m-1"], "started once its mark is let go");
        assert!(!crate::starts::under_way(&store, "m-1"), "and the spawn's return lets its own mark go");
    }

    /// Two steps at once — the daemon's tick and an `advance` by hand — with the
    /// store lock held elsewhere past its two seconds' patience, as under
    /// compounding-428's load, and each pick stalling between its read and its
    /// write as that one did (the row's log kept only the second start's line:
    /// it was written over the first). Each row is spawned once.
    ///
    /// Since `wsp-663` a step does not carry on without the lock: it waits for
    /// the holder, and the second step finds its list held and marks it rather
    /// than stepping beside it.
    fn race(tag: &str, kind: &str, n: usize, cap: usize, seat_cap: Option<usize>) -> (Vec<String>, usize) {
        let (_env, store) = scratch(tag);
        store.set_seat_cap(seat_cap).unwrap();
        let w = capped_group(&store, kind, n, cap);
        let lock = store.state.join(".lock");
        let (root, state) = (store.root.clone(), store.state.clone());
        let go = std::sync::Barrier::new(2);
        // Held elsewhere from before either step starts, for longer than a
        // loose `locked` waits, and then let go.
        std::fs::write(&lock, "1 elsewhere\n").unwrap();
        let held = std::time::Instant::now();
        let ids: Vec<String> = std::thread::scope(|sc| {
            sc.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(2500));
                let _ = std::fs::remove_file(&lock);
            });
            let runs: Vec<_> = (0..2)
                .map(|_| {
                    let (root, state, w, go) = (root.clone(), state.clone(), w.clone(), &go);
                    sc.spawn(move || {
                        let store = Store::at(root, state);
                        STALL.with(|s| s.set(Some(std::time::Duration::from_millis(300))));
                        go.wait();
                        step(&store, &w, &Blind);
                        spawned().into_iter().map(|(id, _)| id).collect::<Vec<_>>()
                    })
                })
                .collect();
            runs.into_iter().flat_map(|r| r.join().unwrap()).collect()
        });
        assert!(held.elapsed() >= std::time::Duration::from_millis(2500), "the steps waited for the holder");
        assert!(!lock.exists(), "and gave the lock back");
        let unique = ids.iter().collect::<std::collections::BTreeSet<_>>().len();
        (sorted(ids), unique)
    }

    #[test]
    fn an_advance_during_a_tick_spawns_each_member_once() {
        let (ids, unique) = race("advance-in-tick", "wiring", 3, 3, None);
        assert_eq!(ids, ["m-1", "m-2", "m-3"], "one spawn for each row");
        assert_eq!(unique, 3);
    }

    /// The path that raises a components group's `xN` to the seat's free places
    /// picks through the same guard.
    #[test]
    fn the_free_places_path_spawns_each_member_once_under_a_race() {
        let (ids, unique) = race("free-places-race", "components", 6, 2, Some(4));
        assert_eq!(unique, ids.len(), "no row twice: {ids:?}");
        assert!(ids.len() <= 6, "{ids:?}");
        assert!(ids.len() >= 4, "x2 raised to the seat's four still starts them: {ids:?}");
    }

    /// Two seats on one claimed member: the one without the claim is ended,
    /// its tree kept for the holder, and the holder is told.
    #[test]
    fn a_second_seat_on_a_claimed_member_is_ended_and_the_claimant_told() {
        let (_env, store) = scratch("second-seat");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        store.set_binding("cpd-2", serde_json::json!({ "task_id": "m-1" }));
        store.set_agent("a-1", serde_json::json!({ "seat": "cpd-1" }));
        store.set_agent("a-2", serde_json::json!({ "seat": "cpd-2" }));
        store.set_claim("m-1", serde_json::json!({ "agent_id": "a-2", "pane": "cpd-2" }));
        let _ = drained(&ENDED);
        let _ = drained(&KEPT);
        let _ = drained(&MEMBER_TOLD);
        step(&store, &w, &Fixed::new(&[("cpd-1", State::Working), ("cpd-2", State::Working)]));
        assert_eq!(drained(&ENDED), ["cpd-1"], "the seat without the claim is ended");
        assert_eq!(drained(&KEPT), ["cpd-1"], "and the tree is left to the holder");
        let told = drained(&MEMBER_TOLD);
        assert!(told.iter().any(|(to, text)| to == "cpd-2" && text.contains("cpd-1") && text.contains("m-1")), "{told:?}");
        assert!(spawned().is_empty(), "nothing is started");
    }

    /// Of two seats where nobody can say which holds the claim, neither is ended.
    #[test]
    fn two_seats_with_no_holder_named_are_left_alone() {
        let (_env, store) = scratch("second-seat-unknown");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        store.set_binding("cpd-2", serde_json::json!({ "task_id": "m-1" }));
        store.set_claim("m-1", serde_json::json!({ "agent_id": "a-9" }));
        let _ = drained(&ENDED);
        step(&store, &w, &Fixed::new(&[("cpd-1", State::Working), ("cpd-2", State::Working)]));
        assert!(drained(&ENDED).is_empty());
    }

    /// `wsp-471`: a member blocked for 20 minutes is flagged, told to the governor and
    /// marked once; before that nothing is said.
    #[test]
    fn a_member_blocked_for_twenty_minutes_raises_a_flag_once() {
        let (_env, store) = scratch("blocked-escalates");
        task(&store, "m-1", Status::Blocked);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let blocked_at = |ago: i64| {
            let mut t = store.find_task("m-1").unwrap();
            t.body = format!("## Log\n- {} blocked: which one?\n", util::iso_at(util::epoch_secs() - ago));
            store.save_task(&t).unwrap();
        };
        blocked_at(10 * 60);
        let _ = drained(&TOLD);
        step(&store, &w, &Blind);
        assert!(flags_on(&store, "m-1").is_empty() && drained(&TOLD).is_empty(), "ten minutes is not yet");
        blocked_at(25 * 60);
        step(&store, &w, &Blind);
        let flags = flags_on(&store, "m-1");
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert!(flags[0].text.contains("which one?"));
        let told = drained(&TOLD);
        assert!(told.len() == 1 && told[0].contains("which one?") && told[0].contains("25 min"), "{told:?}");
        assert!(told[0].contains("wsp worklist cut run m-1 --why"), "the held line names the move: {told:?}");
        assert!(flags[0].text.contains("wsp worklist cut run m-1 --why"), "{:?}", flags[0].text);
        assert!(store.find_task("m-1").unwrap().section("Log").unwrap().contains(ESCALATED));
        step(&store, &w, &Blind);
        assert_eq!(flags_on(&store, "m-1").len(), 1, "once per block");
        assert!(drained(&TOLD).is_empty());
    }

    /// `wsp-635` (react-pass-7-plan, 2026-11-14): a member blocked, its seat
    /// ended, and the answer arrived as `wsp decide`, which leaves the row
    /// blocked. The run reads the decision as the answer: the row is `doing`,
    /// it is seated in the same step, and the row, the list and cycle.log say why.
    #[test]
    fn a_decision_made_since_a_block_reopens_the_member_and_seats_it() {
        let (_env, store) = scratch("decided-reopens");
        task(&store, "m-1", Status::Blocked);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let stamp = |ago: i64| util::iso_at(util::epoch_secs() - ago);
        let mut t = store.find_task("m-1").unwrap();
        t.body = format!("## Log\n- {} blocked: which one?\n", stamp(300));
        store.save_task(&t).unwrap();
        // A decision from before the block answers nothing.
        let mut t = store.find_task("m-1").unwrap();
        t.body.push_str(&format!("\n## Decisions\n- {} (d1) an older choice\n", stamp(900)));
        store.save_task(&t).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Blocked, "an older decision is not the answer");
        assert!(spawned().is_empty());
        // The governor's answer, after the block.
        let mut t = store.find_task("m-1").unwrap();
        crate::decisions::decide_task(&mut t, "take the second shape", &[]);
        store.save_task(&t).unwrap();
        let _ = std::fs::write(store.state_file("cycle.log"), "");
        step(&store, &w, &Blind);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Doing, "reopened with no person asked");
        let log = t.section("Log").unwrap();
        assert!(log.contains(&format!("{SENT_BACK} {ANSWERED_BY} d2 to its block (which one?): take the second shape")), "{log}");
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "seated in the same step");
        assert!(said(&store).contains("m-1 was blocked and decision d2 answers it"), "{}", said(&store));
        let list_log = store.worklist("run").unwrap().section("Log").unwrap_or_default();
        assert!(list_log.contains("reopens m-1") && list_log.contains("decision d2"), "{list_log}");
        let order = sent_back_order(&t).unwrap();
        assert!(order.contains("take the second shape") && !order.contains("verifier"), "{order}");
        assert!(flags_on(&store, "m-1").is_empty(), "nobody was flagged");
        // And once: the row is `doing` now, and a later block is a new question.
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
    }

    /// A block asked after the decision is a new question, not answered by it.
    #[test]
    fn a_decision_older_than_the_latest_block_answers_nothing() {
        let (_env, store) = scratch("decided-before-block");
        task(&store, "m-1", Status::Blocked);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let mut t = store.find_task("m-1").unwrap();
        t.body = format!(
            "## Decisions\n- {} (d1) first answer\n\n## Log\n- {} blocked: and now this?\n",
            util::iso_at(util::epoch_secs() - 600),
            util::iso_at(util::epoch_secs() - 60)
        );
        store.save_task(&t).unwrap();
        assert!(answer_since_block(&store.find_task("m-1").unwrap()).is_none());
        step(&store, &w, &Blind);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Blocked);
    }

    /// An edge to a task the list does not hold is carried and never held on:
    /// the run cannot see that task land, and the barrier is not its to read.
    #[test]
    fn an_edge_off_the_list_holds_nothing() {
        let (_env, store) = scratch("after-off");
        task(&store, "m-1", Status::Todo);
        task(&store, "x-9", Status::Todo);
        let mut t = store.find_task("m-1").unwrap();
        t.after = vec!["x-9".into()];
        store.save_task(&t).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 1);
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
        end_behind(&store, &Exits, &w, 2);
        let mut ended = ENDED.with(|s| s.borrow_mut().drain(..).collect::<Vec<_>>());
        // Sorted, because the list is built from several walks and the order it
        // comes out in is not a fact anybody should be asserting.
        ended.sort();
        assert_eq!(ended, vec!["b-1", "m-2", "v-1"]);
    }

    /// The process a settling verb launches (`wsp-230`): the check is ended
    /// through `despawn`, said in `cycle.log`, and only while it still holds
    /// its claim — a check somebody ended first is left alone, and said so.
    #[test]
    fn an_arranged_check_ending_despawns_the_check_and_says_so_once_and_only_while_it_is_claimed() {
        let (_env, store) = scratch("end-check");
        let mut b = Task::new(&barrier_title("run", 1), "b-1");
        b.tags = vec![BARRIER_TAG.into()];
        store.save_task(&b).unwrap();
        store.set_claim("b-1", serde_json::json!({ "workspace": "w" }));
        drained(&ENDED);
        drained(&SAID);
        let args = Args::synth("worklist", &["advance", "run"], &[("end-check", "b-1")]);

        assert_eq!(advance(&store, &args), 0);
        assert_eq!(drained(&ENDED), vec!["b-1"]);
        assert!(drained(&SAID).iter().any(|l| l.contains("b-1 settled its barrier") && l.contains("ending it")));
        assert!(drained(&SPAWNED).is_empty(), "an ending and nothing else: the run's step was the verb's own poke");

        store.clear_claim("b-1");
        assert_eq!(advance(&store, &args), 0);
        assert!(drained(&ENDED).is_empty(), "already gone");
        assert!(drained(&SAID).iter().any(|l| l.contains("already gone")));
    }

    /// `wsp-666`: an advance does not sit the daemon's 30 s on a `deliver` to a
    /// supervisor that never answers — the wait it makes is the cap, under the
    /// 5 s the done-when allows a `compound-sup` child, and the whole path is
    /// the place's own pending-and-asked-again (`a_capped_process_leaves_a_delivery_pending_at_the_cap`).
    #[test]
    fn an_advance_caps_its_deliver_wait_well_under_the_daemons() {
        let (_env, store) = scratch("advance-cap");
        let args = Args::synth("worklist", &["advance", "run"], &[("end-check", "gone")]);
        assert_eq!(advance(&store, &args), 0);
        let wait = crate::place_compound::deliver_wait();
        assert_eq!(wait, ADVANCE_DELIVER_CAP);
        assert!(wait < std::time::Duration::from_secs(5), "{wait:?}");
    }

    /// `wsp-662`: a task's trigger steps the list that holds it and no other,
    /// and none where no list does; a list named, or no task, keeps its reach.
    #[test]
    fn a_tasks_advance_steps_only_the_list_that_holds_it() {
        let (_env, store) = scratch("stepped");
        for id in ["a-1", "b-1", "loose"] {
            task(&store, id, Status::Review);
        }
        let mut v = Task::new("verify a-1", "va-1");
        v.tags = vec![VERIFY_TAG.into()];
        v.parent = Some("a-1".into());
        store.save_task(&v).unwrap();
        for (id, member) in [("list-a", "a-1"), ("list-b", "b-1")] {
            let mut w = Worklist::new(id, id);
            w.set_status(WorklistStatus::Running);
            w.set_groups(&[Group { members: vec![member.into()], agent: "claude".into(), ..Group::default() }]);
            store.save_worklist(&w).unwrap();
        }
        let cases: &[(Option<&str>, Option<&str>, &[&str], &str)] = &[
            (None, Some("a-1"), &["list-a"], "a member of A steps A and not B"),
            (None, Some("b-1"), &["list-b"], "a member of B steps B and not A"),
            (None, Some("va-1"), &["list-a"], "a verifier under A's member steps A"),
            (None, Some("loose"), &[], "a task no list holds steps no list"),
            (None, Some("nobody"), &[], "a task that is not there steps no list"),
            (Some("list-b"), Some("a-1"), &["list-b"], "a list named is the one stepped"),
            (None, None, &["list-a", "list-b"], "the bare form steps every running list"),
        ];
        for (named, id, want, why) in cases {
            let mut got: Vec<String> = stepped(&store, *named, *id).into_iter().map(|w| w.id).collect();
            got.sort();
            assert_eq!(got, *want, "{why}");
        }
    }

    pub(crate) fn drained<T>(k: &'static std::thread::LocalKey<RefCell<Vec<T>>>) -> Vec<T> {
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
        let gate = crate::room::Gate::new(&store, &Blind);
        let took = |cap| take_member(&store, &m2, "run", 1, &members, cap, &gate).map(|(why, _)| why);
        assert_eq!(took(Some(1)), None, "one going, cap one");
        assert_eq!(took(Some(2)), Some("new"));
        assert_eq!(took(Some(2)), None, "taken already");
    }

    /// `wsp-663`: two advances arrive while a step of the list runs — the
    /// 2026-11-17 race that spawned compounding-709 twice. Neither steps the
    /// list; the holder steps it exactly once more for both, and the member is
    /// started once.
    #[test]
    fn two_advances_during_a_step_mark_it_once_and_start_nothing_twice() {
        let (_env, store) = scratch("onestep");
        task(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        fn two_advances(store: &Store, w: &Worklist) {
            assert!(step(store, w, &Blind).is_empty(), "the list is held: marked, not stepped");
            assert!(step(store, w, &Blind).is_empty(), "and marked again, not queued");
        }
        STEPS.with(|n| n.set(0));
        DURING_STEP.with(|d| d.set(Some(two_advances)));
        let started = step(&store, &w, &Blind);
        assert_eq!(STEPS.with(|n| n.get()), 2, "one step, then exactly one follow-up");
        assert_eq!(spawned(), vec![("m-1".into(), "claude".into())], "and one start: no double spawn");
        assert_eq!(started.len(), 1, "{started:?}");
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert_eq!(log.matches("marked to step again").count(), 2, "{log}");
        assert!(log.contains("stepped 1 more time(s)"), "{log}");
        assert!(!store.state.join("stepping/run.lock").exists(), "and the list is let go");

        step(&store, &w, &Blind);
        assert_eq!(STEPS.with(|n| n.get()), 3, "a later step is one step");
        assert!(spawned().is_empty());
    }

    /// A start that fails puts the member back and says so; one that never
    /// arrived is taken again once it has had its minute.
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

        // Held for its backoff first (`wsp-335`), then started again.
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "not at once");
        age_start(&store, "m-1", 4 * 60);

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

        // And a pass whose agent never came: no seat on it, a minute on.
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        let _ = spawned();
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
        let mut m = store.find_task("m-1").unwrap();
        let mut ps = crate::verification::passes(&m);
        ps[0].at = "2026-01-01T00:00:00Z".into();
        crate::verification::write(&mut m, &ps);
        store.save_task(&m).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1 · verifying".into(), "claude".into())], "the same pass, started again");
        assert_eq!(passes_on(&store, "m-1").len(), 1, "and not a second one");
    }

    #[test]
    fn a_barrier_row_filed_before_wsp_230_finds_the_run_it_belongs_to() {
        let (_env, store) = scratch("belong");
        task(&store, "m-1", Status::Review);
        list(&store, &[(&["m-1"], "claude")]);
        let mut b = Task::new(&barrier_title("run", 1), "b-1");
        b.tags = vec![BARRIER_TAG.into()];
        store.save_task(&b).unwrap();
        assert_eq!(list_of(&store, &b).map(|w| w.id), Some("run".into()));
    }

    /// `wsp-286`. **A held run's member, reopened, is the way back that is not
    /// the pass.** The check holds at barrier 1 because m-1 still owes its
    /// install; m-1's agent is gone. `wsp reopen` takes the run back to group 1
    /// with no verdict, the step seats m-1 once, and when m-1 is back at review
    /// the barrier is checked afresh beside the check that held.
    #[test]
    fn a_held_runs_member_reopened_is_reseated_and_the_barrier_checked_afresh() {
        let (_env, store) = scratch("reopenheld");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        check_seated(&store, 1, "cpd-chk");
        let _ = spawned();

        let hold = Args::synth("worklist", &["hold", "run", "m-1 is not installed"], &[]);
        assert_eq!(crate::cmd_worklist::hold(&store, &hold), 0);
        assert_eq!(store.worklist("run").unwrap().status(), WorklistStatus::Held);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "held: nothing starts");
        let _ = drained(&TOLD);
        let _ = drained(&HEARD);

        let reopen = Args::synth("reopen", &["m-1", "install it and drive it live"], &[]);
        assert_eq!(crate::cmd_task::reopen(&store, &reopen), 0);
        let now = store.worklist("run").unwrap();
        assert_eq!(now.status(), WorklistStatus::Running, "the reopen takes the run back");
        assert!(now.groups()[0].verdict.trim().is_empty(), "and records no verdict: {:?}", now.groups()[0].verdict);
        assert!(now.body.contains("started again · m-1 sent back"), "{}", now.body);
        assert_eq!(worklist::position(&store, &now, Reading::Settled).at, Some(1), "at the group it stood at");
        let told = drained(&TOLD);
        assert!(told.iter().any(|t| t.contains("m-1") && t.contains("no verdict")), "{told:?}");
        assert!(drained(&HEARD).iter().any(|t| t.contains("sent back")), "and the governor above hears it");

        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".into(), "claude".into())], "the member is reseated, and m-2 is not started");
        let m1 = store.find_task("m-1").unwrap();
        assert_eq!(m1.status(), Status::Doing);
        assert!(m1.section("Log").unwrap().lines().last().unwrap().contains(STARTED_BY), "{}", m1.body);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "once: the start is the record a second step finds");
        assert_eq!(checks(&store, 1).len(), 1, "no check while m-1 owes work");

        let mut t = store.find_task("m-1").unwrap();
        t.set_status(Status::Review);
        crate::log::note(&store, &mut t, "review: installed and driven live").unwrap();
        step(&store, &w, &Blind);
        let c = checks(&store, 1);
        assert_eq!(
            c.iter().map(|c| c.state).collect::<Vec<_>>(),
            [crate::barrier::State::Holds, crate::barrier::State::Running],
            "a fresh check beside the one that held"
        );
        assert_eq!(spawned(), vec![(crate::barrier::seat_label("run", 1), "claude".into())], "and its agent started");
    }

    /// A reopen is not a word about a hold somebody took for their own reasons:
    /// with no check that held, the run stays held and `go` is its way back.
    #[test]
    fn a_member_reopened_under_a_hold_no_check_made_leaves_the_run_held() {
        let (_env, store) = scratch("reopenhand");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let hold = Args::synth("worklist", &["hold", "run", "stop for now"], &[]);
        assert_eq!(crate::cmd_worklist::hold(&store, &hold), 0);
        set(&store, "m-1", Status::Review);

        let mut t = store.find_task("m-1").unwrap();
        t.log("claimed by pane cpd-1 · spawned at opus/high");
        crate::log::note(&store, &mut t, "released after 5m").unwrap();

        let reopen = Args::synth("reopen", &["m-1", "one more thing"], &[]);
        assert_eq!(crate::cmd_task::reopen(&store, &reopen), 0);
        assert_eq!(store.worklist("run").unwrap().status(), WorklistStatus::Held);
        assert!(crate::cmd_task::tests::reseated().is_empty(), "nor does the reopen seat it past the hold");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "{:?}", spawned());
    }

    /// `wsp-304`, beside `wsp-286`: the reopen of a lone row seats an agent
    /// itself, and the reopen of a running group's member leaves it to the
    /// step — one seat between them, never two.
    #[test]
    fn a_running_members_reopen_is_seated_by_the_run_and_not_by_the_reopen_too() {
        let (_env, store) = scratch("reopenrunning");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Todo);
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        step(&store, &w, &Blind);
        let _ = spawned();
        let mut t = store.find_task("m-1").unwrap();
        t.log("claimed by pane cpd-1 · spawned at opus/high");
        crate::log::note(&store, &mut t, "released after 5m").unwrap();

        let reopen = Args::synth("reopen", &["m-1", "the regression"], &[]);
        assert_eq!(crate::cmd_task::reopen(&store, &reopen), 0);
        assert!(crate::cmd_task::tests::reseated().is_empty(), "the reopen leaves the member to its run");
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".into(), "claude".into())], "and the run seats it, once");
    }

    /// `wsp-188`. A verdict that holds leaves the member at `review` and tells
    /// nobody; one that blocks sends the member back the way `wsp reopen` does —
    /// its pane told, the reason on its log — and is typed to the governor
    /// **once**, as the decision it is. The verb triggers one advance, and the
    /// steps and ticks after it read the block without saying it again.
    #[test]
    fn a_holds_verdict_stays_at_review_and_a_block_sends_the_member_back_and_reaches_the_governor_once() {
        use crate::verification::State as V;
        let (_env, store) = scratch("verdicts");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Review);
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        step(&store, &w, &Blind);
        let _ = spawned();
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-2" }));
        let _ = drained(&TOLD);

        crate::verification::record(&store, "m-1", V::Holds, "both halves hold", None, None).unwrap();
        told_about_task(&store, "m-1", "verified");
        let m1 = store.find_task("m-1").unwrap();
        assert_eq!(m1.status(), Status::Review, "a holds verdict leaves the member where it is");
        assert!(m1.section("Verification").unwrap().contains("holds"), "{}", m1.body);
        assert!(m1.section("Verification").unwrap().contains("both halves hold"), "and in the verifier's words");
        assert!(drained(&TOLD).is_empty(), "and is nobody's news");

        let verdict = "the second half does not hold\n1. wake::say must spool first";
        crate::verification::record(&store, "m-2", V::Blocks, verdict, None, None).unwrap();
        crate::cmd_task::deliver_reason(&store, "m-2", verdict);
        told_about_task(&store, "m-2", "verified");
        let m2 = store.find_task("m-2").unwrap();
        assert_eq!(m2.status(), Status::Doing, "a block sends the member back");
        assert!(m2.section("Log").unwrap().contains(SENT_BACK), "with what is owed on its log, as reopen writes it");
        assert!(
            m2.section("Verification").unwrap().contains("  1. wake::say must spool first"),
            "and the whole verdict, with its structure, on the row: {}",
            m2.body
        );
        let member_told = drained(&MEMBER_TOLD);
        assert!(member_told.iter().any(|(id, t)| id == "m-2" && t.contains("does not hold")), "{member_told:?}");
        let told = drained(&TOLD);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("m-2") && told[0].contains("sent back"), "{}", told[0]);

        step(&store, &w, &Blind);
        crate::repair::tick(&store, &Blind, &mut crate::repair::Pass::new());
        assert!(!drained(&TOLD).iter().any(|t| t.contains("sent back")), "told once, not on every pass after");
        assert!(tagged(&store, VERIFY_TAG).is_empty(), "and no row was filed for either verdict");
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
        crate::testkit::repo(&repo);
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
        assert!(spawned().is_empty() && reading(&store).is_empty(), "no verifier on unlanded work");
        let told = drained(&TOLD);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("m-1 is at review with 1 commit not on master"), "{}", told[0]);
        assert!(told[0].contains("`wsp review m-1`") && !told[0].contains("`wsp land m-1`"), "{}", told[0]);

        git(&["merge", "--ff-only", "--quiet", "m-1"]);
        told_about_task(&store, "m-1", "review");
        step(&store, &w, &Blind);
        assert_eq!(reading(&store), vec!["m-1".to_string()], "landed: the verifier starts");
        assert!(drained(&TOLD).is_empty(), "and a landed review is the next step, not news");
        let _ = spawned();
    }

    /// **One call is one line in `cycle.log`** (`wsp-217`). It was two for any
    /// process this module launched: `log_line` echoed to stdout, and that
    /// child's stdout *is* the file. The echo is still there for a reader, so
    /// what keeps the file at one line is [`a_reader`] saying no to a stdout
    /// that is a file — asserted on `cycle.log`'s own metadata, which is what a
    /// launched child's stdout reads as.
    #[test]
    fn a_line_said_through_log_line_is_in_cycle_log_once_and_not_echoed_into_a_log() {
        let (_env, store) = scratch("logonce");
        log_line(&store, "core: the reseat failed — the agent never came up");

        let path = store.state_file("cycle.log");
        let log = std::fs::read_to_string(&path).unwrap();
        assert_eq!(log.matches("the reseat failed").count(), 1, "{log:?}");
        let meta = std::fs::metadata(&path).unwrap();
        assert!(!a_reader(Some(&meta)), "a launched child's stdout is cycle.log, so an echo is a second copy");
        let null = std::fs::metadata("/dev/null").unwrap();
        assert!(a_reader(Some(&null)), "a stdout that is not a file is not a log: echo, as `stamp` did to a terminal");
        assert!(a_reader(None), "and one that cannot be read is echoed to, which costs nothing");
    }

    /// **A run step the daemon's tick takes inline says itself in `cycle.log`**
    /// (`wsp-222`). The tick calls [`step`] in-process, and `step` said
    /// everything to stdout, which in the daemon is `daemon.log`: on `wsp-212`'s
    /// live proof the first verifier a tick started had its `started` line there
    /// and nowhere a governor reads. Asserted on the file, not on `SAID`, since
    /// the record is in memory whichever writer said it.
    #[test]
    fn a_verifier_a_tick_starts_inline_is_said_in_cycle_log() {
        let (_env, store) = scratch("tickinline");
        task(&store, "m-1", Status::Review);
        let mut t = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut t, &format!("{} 323faba83e4ce0ea6ae7253674a1e919cee27f78", crate::repair::LANDED)).unwrap();
        let _ = list(&store, &[(&["m-1"], "claude")]);

        crate::repair::tick(&store, &Blind, &mut crate::repair::Pass::new());

        assert_eq!(spawned().len(), 1, "the tick started the member's verifier itself");
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert!(
            log.lines().any(|l| l.contains("run group 1: started m-1")),
            "the start is in cycle.log, not only on the daemon's stdout: {log:?}"
        );
    }

    // ---- legs (wsp-293) ---------------------------------------------------------

    /// `legs::*` applied to the row's record, as the verb that does it would.
    fn edit_legs(store: &Store, id: &str, f: impl FnOnce(&[crate::legs::Leg]) -> Result<Vec<crate::legs::Leg>, String>) {
        let mut t = store.find_task(id).unwrap();
        let new = f(&crate::legs::legs(&t)).unwrap();
        crate::legs::write(&mut t, &new);
        store.save_task(&t).unwrap();
    }

    /// A row staged with `plan` alone, as `wsp leg add` writes it.
    fn staged(store: &Store, id: &str, status: Status) {
        task(store, id, status);
        edit_legs(store, id, |_| Ok(vec![crate::legs::Leg::pending("plan", "decide how it splits")]));
    }

    fn add_leg(store: &Store, id: &str, name: &str, after: &[&str], before: Option<&str>, actor: Option<&str>) {
        edit_legs(store, id, |old| {
            crate::legs::add(
                old,
                crate::legs::New {
                    name: name.into(),
                    order: format!("do {name}"),
                    after: Some(after.iter().map(|a| a.to_string()).collect()),
                    before: before.map(str::to_string),
                    ..Default::default()
                },
                actor,
            )
        });
    }

    /// The seat a spawn gives a leg that opened, as `wsp spawn --leg` writes it.
    fn seat_leg(store: &Store, id: &str, name: &str, seat: &str) {
        let stand = crate::leg_seat::Standing { room: None, cwd: None, kind: "claude" };
        assert!(crate::leg_seat::attach(store, id, name, seat, &stand), "{name} was not open to seat");
    }

    /// `wsp leg done` from the leg's seat.
    fn leg_done(store: &Store, id: &str, name: &str) {
        edit_legs(store, id, |old| crate::legs::finish(old, name, "handed on", "did it", "2026-10-18T10:00:00Z"));
    }

    fn leg_state(store: &Store, id: &str, name: &str) -> crate::legs::State {
        crate::legs::legs(&store.find_task(id).unwrap()).into_iter().find(|l| l.name == name).unwrap().state
    }

    /// `wsp-499`: legs a planner files on a todo row with `wsp leg add` — a build,
    /// two drives that wait on it, a last leg — are started by the run when the
    /// row starts, with no working seat to stage them: the build first, and the
    /// drives together once it is done.
    #[test]
    fn legs_a_planner_filed_on_a_todo_row_start_when_the_row_starts() {
        let (env, store) = scratch("legs-planner");
        task(&store, "m-1", Status::Todo);
        let file = env.home().join("order.md");
        std::fs::write(&file, "Do it.").unwrap();
        let path = file.to_string_lossy().to_string();
        let add = |name: &str, extra: &[&str]| {
            let mut line = vec!["leg", "add", "m-1", name, "--order", path.as_str()];
            line.extend_from_slice(extra);
            let args = Args::parse(line.iter().map(|s| s.to_string()).collect());
            assert_eq!(crate::legs::run(&store, &args, Some("cpd-planner")), 0, "{name}");
        };
        add("build", &[]);
        add("finish", &["--after", "build"]);
        add("drive-a", &["--after", "build", "--before", "finish"]);
        add("drive-b", &["--after", "build", "--before", "finish"]);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Todo, "filed on a todo row, nothing started");
        let w = list(&store, &[(&["m-1"], "claude")]);

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · build".to_string()], "the first leg, and no whole-row worker");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing);
        seat_leg(&store, "m-1", "build", "cpd-1");
        leg_done(&store, "m-1", "build");
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · drive-a".to_string(), "m-1 · drive-b".to_string()], "the drives side by side");
    }

    /// The row's legs, one staged member, run from the first start to a barrier
    /// with no hand step: the plan fans out into two siblings and a join, every
    /// leg is opened once however often the step runs, the row's own `review` is
    /// the last leg's, and a verifier and the barrier follow.
    #[test]
    fn a_staged_member_runs_every_leg_to_verified_with_no_hand_step() {
        let (_env, store) = scratch("legs-run");
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · plan".to_string()], "the leg that waits on nothing, and no whole-row worker");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing, "the row is begun by its first leg");
        assert!(store.claims().is_empty(), "a leg claims nothing");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a second advance starts nothing twice");

        // The plan seats, fans out into two siblings and the join, and ends.
        seat_leg(&store, "m-1", "plan", "cpd-1");
        add_leg(&store, "m-1", "finish", &["plan"], None, Some("plan"));
        add_leg(&store, "m-1", "a", &["plan"], Some("finish"), Some("plan"));
        add_leg(&store, "m-1", "b", &["plan"], Some("finish"), Some("plan"));
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "nothing is ready while the plan runs");
        leg_done(&store, "m-1", "plan");
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · a".to_string(), "m-1 · b".to_string()], "both siblings, in record order");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "and once");

        seat_leg(&store, "m-1", "a", "cpd-2");
        seat_leg(&store, "m-1", "b", "cpd-3");
        leg_done(&store, "m-1", "a");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the join waits for both");
        leg_done(&store, "m-1", "b");
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · finish".to_string()]);

        // The last leg types `review`.
        seat_leg(&store, "m-1", "finish", "cpd-4");
        crate::leg_seat::on_review(&store, "m-1", "all of it done", Some("cpd-4")).unwrap();
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · verifying".to_string()], "the verifier runs once, on the row");
        let order = verifier_order(&store, &store.find_task("m-1").unwrap());
        assert!(order.contains("ran in legs") && order.contains("name the leg whose work fails") && order.contains("- finish"), "{order}");

        hold_all(&store);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["run · barrier 1".to_string()], "and the barrier follows");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
    }

    /// The verifier's order carries each piece's done-when and the result of its
    /// check lines, and asks for the pieces a block is about to be named.
    #[test]
    fn the_verifiers_order_carries_each_pieces_done_when_and_check_result() {
        let (_env, store) = scratch("piecesorder");
        task(&store, "m-1", Status::Review);
        let mut t = store.find_task("m-1").unwrap();
        let mut rec = crate::pieces::Record::default();
        for (n, label) in [(1, "types"), (2, "board")] {
            let mut p = crate::pieces::Piece::pending(n, label, 1);
            p.state = crate::pieces::State::Done;
            p.done_when = format!("the {label} read back");
            p.checks = vec![format!("{label} (1m): true {label}")];
            rec.pieces.push(p);
        }
        let mut p = crate::pieces::Piece::pending(3, "extra", 2);
        p.state = crate::pieces::State::Dropped;
        p.why = Some("not needed".into());
        rec.pieces.push(p);
        crate::pieces::write(&mut t, &rec);
        store.save_task(&t).unwrap();
        let order = verifier_order(&store, &store.find_task("m-1").unwrap());
        assert!(order.contains("ran as pieces") && order.contains("name the pieces whose work fails"), "{order}");
        for want in ["`m-1-1` · types", "done-when: the types read back", "1.types · not green", "`m-1-2` · board", "done-when: the board read back", "2.board · not green", "`m-1-3` · extra · dropped: not needed"] {
            assert!(order.contains(want), "{want}: {order}");
        }
        let plain = Task::new("plain", "m-2");
        store.save_task(&plain).unwrap();
        assert!(!verifier_order(&store, &store.find_task("m-2").unwrap()).contains("ran as pieces"));
    }

    /// The tick ends a done leg's seat once, from `/`, and records it on the entry.
    #[test]
    fn the_tick_ends_a_done_legs_seat_once_and_records_it_on_the_entry() {
        let (_env, store) = scratch("legs-end");
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        seat_leg(&store, "m-1", "plan", "cpd-1");
        assert!(legs_finished(&store, &Exits).is_empty(), "a running leg's seat is not ended");
        leg_done(&store, "m-1", "plan");
        let _ = drained(&ENDED);
        assert_eq!(legs_finished(&store, &Exits), ["cpd-1".to_string()]);
        assert_eq!(drained(&ENDED), ["cpd-1".to_string()]);
        let plan = crate::legs::legs(&store.find_task("m-1").unwrap()).remove(0);
        assert_eq!(plan.ending, crate::verification::Ending::Ended);
        assert!(legs_finished(&store, &Exits).is_empty(), "once");
        assert!(drained(&ENDED).is_empty());
    }

    /// The manager host's fold of a seat's start for `row` (`wsp-028`), as
    /// event `id`: the record that wrote a claim for every leg's seat until
    /// `wsp-334`.
    fn fold_start(store: &Store, row: &str, seat: &str, id: u64) {
        use wsp_host_api::{Epoch, EventId, Frame, Key, Message, RowId, SeatId, SeatStarted};
        let started = SeatStarted { key: Key { row: RowId(row.into()), attempt: 1 }, seat: SeatId(seat.into()), epoch: Epoch(1), pid: 4242, sockets: vec![] };
        assert!(crate::fold::fold(store, "manager", &Frame { id: Some(EventId(id)), req: None, msg: Message::SeatStarted(started) }));
    }

    /// [`seat_leg`] in `cwd`, a directory no repository holds, so the leg's own
    /// `wsp leg done` ([`crate::leg_seat::finish`]) reads no tree as unclean.
    fn seat_leg_in(store: &Store, id: &str, name: &str, seat: &str, cwd: &std::path::Path) {
        let cwd = cwd.display().to_string();
        let stand = crate::leg_seat::Standing { room: None, cwd: Some(&cwd), kind: "claude" };
        assert!(crate::leg_seat::attach(store, id, name, seat, &stand), "{name} was not open to seat");
    }

    /// The fake clock, in the seat's own terms: every seat idle, its turn ended
    /// an hour ago, so far past the member's idle window (`repair::STALLED_AFTER`).
    /// Every process is gone once looked for. This is a leg's seat after its
    /// `wsp leg done`, as wsp-030's read when they were told to finish.
    struct IdleAnHour;

    impl Seats for IdleAnHour {
        fn state(&self, _seat: &str) -> Option<State> {
            Some(State::Idle)
        }
        fn turn_end(&self, _seat: &str) -> Option<crate::waiting::TurnEnd> {
            Some(crate::waiting::TurnEnd { at: util::epoch_secs() - 3600, words: None, denied: false })
        }
        fn pids(&self, _seat: &str) -> Option<Vec<u32>> {
            Some(vec![1])
        }
        fn running(&self, _pids: &[u32]) -> Vec<u32> {
            Vec::new()
        }
    }

    /// What a leg's seat must never be told or flagged as, read off `cycle.log`
    /// and the row's log: the member's idle tell, its end-and-restart and the
    /// close that flags the person (`wsp-334`).
    fn no_member_word(store: &Store, id: &str) {
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        for word in ["told to finish", "needs a person", "with nothing working on it", "open and not closing"] {
            assert!(!log.contains(word), "{word:?} in cycle.log: {log}");
        }
        let row = store.find_task(id).unwrap().section("Log").unwrap_or_default();
        assert!(!row.contains("told it to finish"), "{row}");
    }

    /// `wsp-334`: a row with two legs, under the manager host, whose fold of the
    /// first seat's start wrote a claim on the row. The first leg calls `wsp leg
    /// done`, and its seat then reads idle an hour. **One tick** ends that seat
    /// and starts the second leg: no "told to finish", no "needs a person". It
    /// holds both for a run that folds after the fix (no claim) and for a claim
    /// a fold wrote before it, which the ending takes with the seat.
    #[test]
    fn a_done_legs_seat_is_ended_and_the_next_leg_started_in_one_tick_with_no_member_tell() {
        for stale in [false, true] {
            let (env, store) = scratch(if stale { "legs-tick-stale" } else { "legs-tick" });
            staged(&store, "m-1", Status::Todo);
            add_leg(&store, "m-1", "second", &["plan"], None, None);
            let w = list(&store, &[(&["m-1"], "claude")]);
            step(&store, &w, &Blind);
            assert_eq!(ids(spawned()), ["m-1 · plan".to_string()]);
            seat_leg_in(&store, "m-1", "plan", "cpd-1", &env.home());
            fold_start(&store, "m-1", "cpd-1", 1);
            assert!(store.claims().is_empty(), "a leg's seat start claims nothing: {:?}", store.claims());
            if stale {
                store.set_claim("m-1", serde_json::json!({ "workspace_id": "cpd-1", "host": "manager", "epoch": 1 }));
            }

            crate::leg_seat::finish(&store, "m-1", "plan", "cpd-1", "handed on", "did it", None).unwrap();
            let _ = drained(&ENDED);
            crate::repair::tick(&store, &IdleAnHour, &mut crate::repair::Pass::new());

            assert_eq!(drained(&ENDED), ["cpd-1".to_string()], "stale {stale}: the done leg's seat is ended by this tick");
            assert_eq!(ids(spawned()), ["m-1 · second".to_string()], "stale {stale}: and the second leg is started in it");
            assert_eq!(leg_state(&store, "m-1", "second"), crate::legs::State::Running);
            assert!(store.claims().is_empty(), "stale {stale}: {:?}", store.claims());
            no_member_word(&store, "m-1");
        }
    }

    /// `wsp-334` over wsp-030's five leg boundaries (06:43:38 to 07:08:44 on
    /// 2026-11-20, `leg-done` each). The run is driven for real: each leg seated,
    /// its start folded as the manager host folds it, `wsp leg done`, then the
    /// poke's step and as many ticks as it takes for the next leg to start. The
    /// starts go into the fixture at the done time, plus a hand-off for the
    /// poke and plus [`crate::timeline`]'s tick for each tick taken. The
    /// timeline's gap before each leg is under 30 s. The log as it happened
    /// (two ticks a boundary) is read the same way for comparison: every gap
    /// is "nothing the logs name", and 614 s in all.
    #[test]
    fn wsp_030s_five_leg_boundaries_each_cost_under_30s() {
        const NAMES: [&str; 6] = ["wire-inproc", "daemon-tick", "verbs", "tests-docs", "finish", "rework"];
        let at = |hms: &str| util::epoch_of(&format!("2026-11-20T{hms}Z"));
        let done = ["06:43:38", "06:57:40", "07:02:46", "07:05:52", "07:08:44"].map(at);
        let then = ["06:45:26", "06:59:43", "07:04:50", "07:07:55", "07:11:00"].map(at);

        let (env, store) = scratch("legs-030");
        task(&store, "m-1", Status::Todo);
        edit_legs(&store, "m-1", |_| Ok(NAMES.iter().map(|n| crate::legs::Leg::pending(n, &format!("do {n}"))).collect()));
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · wire-inproc".to_string()]);
        let mut ticks = Vec::new();
        for (k, name) in NAMES.iter().take(5).enumerate() {
            let seat = format!("cpd-{}", 161 + k);
            seat_leg_in(&store, "m-1", name, &seat, &env.home());
            fold_start(&store, "m-1", &seat, k as u64 + 1);
            crate::leg_seat::finish(&store, "m-1", name, &seat, "handed on", "did it", None).unwrap();
            let next = format!("m-1 · {}", NAMES[k + 1]);
            // The poke `leg done` sends is a step; a tick after it if it took none.
            step(&store, &w, &Blind);
            let mut n = 0;
            while !ids(spawned()).contains(&next) {
                n += 1;
                assert!(n <= 3, "{next} never started: {:?}", store.claims());
                crate::repair::tick(&store, &IdleAnHour, &mut crate::repair::Pass::new());
            }
            ticks.push(n);
        }
        assert_eq!(ticks, [0; 5], "each next leg is started by the poke of the leg done before it");
        crate::repair::tick(&store, &IdleAnHour, &mut crate::repair::Pass::new());
        no_member_word(&store, "m-1");

        let gaps = |starts: &[i64]| {
            let mut i = crate::timeline::Inputs { id: "wsp-030".into(), ..Default::default() };
            i.legs = NAMES.iter().map(|n| crate::timeline::LegIn { name: n.to_string(), ..Default::default() }).collect();
            i.cycle.push((at("06:35:26"), "m1-agent-manager group 8: starting leg wire-inproc of wsp-030".into()));
            for k in 0..5 {
                i.events.push((done[k], "leg-done".into(), serde_json::json!({ "id": "wsp-030", "leg": NAMES[k] })));
                i.cycle.push((starts[k], format!("m1-agent-manager group 8: starting leg {} of wsp-030", NAMES[k + 1])));
            }
            crate::timeline::build(&i).into_iter().filter(|p| p.gap && p.label.starts_with("gap before leg")).collect::<Vec<_>>()
        };
        let fixed: Vec<i64> = (0..5).map(|k| done[k] + crate::timeline::HAND_OFF / 2 + ticks[k] as i64 * crate::timeline::TICK).collect();
        let now = gaps(&fixed);
        assert_eq!(now.len(), 5, "{now:?}");
        for g in &now {
            assert!(g.seconds() < 30, "{g:?}");
            assert!(!g.waiting_for.as_deref().unwrap_or_default().contains("did not run for longer than a tick"), "{g:?}");
        }
        let was = gaps(&then);
        assert_eq!(was.len(), 5, "{was:?}");
        assert!(was.iter().all(|g| g.waiting_for.as_deref().unwrap_or_default().contains("the step of wsp-030 did not run for longer than a tick")), "{was:?}");
        assert_eq!(was.iter().map(|g| g.seconds()).sum::<i64>(), 614);
    }

    /// A row at `doing`: the plan done, two siblings and their join.
    fn fanned_out(store: &Store, id: &str) {
        staged(store, id, Status::Doing);
        edit_legs(store, id, |old| {
            let after = |on: &[&str], l: crate::legs::Leg| crate::legs::Leg { after: Some(on.iter().map(|s| s.to_string()).collect()), ..l };
            let mut v = old.to_vec();
            v.push(after(&["plan"], crate::legs::Leg::pending("a", "do a")));
            v.push(after(&["plan"], crate::legs::Leg::pending("b", "do b")));
            v.push(after(&["a", "b"], crate::legs::Leg::pending("finish", "join")));
            Ok(v)
        });
        seat_leg_open(store, id, "plan", "cpd-1");
        leg_done(store, id, "plan");
    }

    /// `plan` opened and seated by hand, which is how a row staged in a test
    /// has its first leg run.
    fn seat_leg_open(store: &Store, id: &str, name: &str, seat: &str) {
        crate::leg_seat::open(store, id, name).unwrap();
        seat_leg(store, id, name, seat);
    }

    /// With the seat capped at two and a second member ready, the siblings take
    /// the places ahead of the new member, and the third agent waits.
    #[test]
    fn siblings_take_places_ahead_of_a_new_member_under_the_seats_cap() {
        let (_env, store) = scratch("legs-cap");
        store.set_seat_cap(Some(2)).unwrap();
        fanned_out(&store, "m-1");
        task(&store, "m-2", Status::Todo);
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);
        let _ = drained(&SAID);

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · a".to_string(), "m-1 · b".to_string()], "the row begun finishes before a new member fans out");
        assert_eq!(store.find_task("m-2").unwrap().status(), Status::Todo, "the third agent waits");
        let said = said_capped();
        assert!(said.iter().any(|l| l.contains("m-2 waits")), "said once: {said:?}");

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "nothing starts twice, and the wait is not said again");
        assert!(said_capped().is_empty(), "said once and not on every tick");

        // A place frees when a sibling is done and its seat is not seen working.
        seat_leg(&store, "m-1", "a", "cpd-2");
        seat_leg(&store, "m-1", "b", "cpd-3");
        leg_done(&store, "m-1", "a");
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-2".to_string()], "now the new member takes the place");
    }

    /// A leg that does not fit stays pending; the cap counts each running leg as
    /// one agent, and a leg opened with no seat yet as one starting.
    #[test]
    fn each_running_leg_is_one_agent_and_a_leg_that_does_not_fit_waits_pending() {
        let (_env, store) = scratch("legs-count");
        store.set_seat_cap(Some(1)).unwrap();
        fanned_out(&store, "m-1");
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · a".to_string()], "one place, one sibling");
        assert_eq!(leg_state(&store, "m-1", "b"), crate::legs::State::Pending, "b waits, written as nothing");
        let gate = crate::room::Gate::new(&store, &Blind);
        let now = gate.working();
        assert_eq!(now.iter().map(|c| c.what.clone()).collect::<Vec<_>>(), ["m-1 · a".to_string()], "{now:?}");

        seat_leg(&store, "m-1", "a", "cpd-2");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a seated, still running: the place is taken");
        leg_done(&store, "m-1", "a");
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · b".to_string()], "a is done, so b has the place");
    }

    // ---- pieces (wsp-506) ------------------------------------------------------------

    /// A row with pieces: `(n, batch)` each, pending, under `cap:`.
    fn pieced(store: &Store, id: &str, status: Status, spec: &[(usize, usize)], cap: usize) {
        task(store, id, status);
        let mut t = store.find_task(id).unwrap();
        let mut rec = crate::pieces::Record { cap, ..Default::default() };
        for (n, batch) in spec {
            let mut p = crate::pieces::Piece::pending(*n, &format!("part {n}"), *batch);
            p.overview = format!("do part {n}");
            p.done_when = format!("part {n} is done");
            rec.pieces.push(p);
        }
        crate::pieces::write(&mut t, &rec);
        store.save_task(&t).unwrap();
    }

    fn piece_rec(store: &Store, id: &str) -> crate::pieces::Record {
        crate::pieces::record(&store.find_task(id).unwrap())
    }

    fn piece_of(store: &Store, id: &str, n: usize) -> crate::pieces::Piece {
        piece_rec(store, id).pieces.into_iter().find(|p| p.n == n).unwrap()
    }

    /// The seat a spawn gives a piece that opened, as `wsp spawn --piece` writes it.
    fn seat_piece(store: &Store, id: &str, n: usize, seat: &str) {
        let stand = crate::piece_seat::Standing { room: None, cwd: None, kind: "claude", spawned: "" };
        assert!(crate::piece_seat::attach(store, id, n, seat, &stand), "piece {n} was not open to seat");
    }

    /// `wsp piece done` from the piece's seat, as the record has it after.
    fn piece_done(store: &Store, id: &str, n: usize) {
        let mut t = store.find_task(id).unwrap();
        let mut rec = crate::pieces::record(&t);
        let p = rec.pieces.iter_mut().find(|p| p.n == n).unwrap();
        assert_eq!(p.state, crate::pieces::State::Running, "piece {n} is not running");
        p.state = crate::pieces::State::Done;
        p.caption = Some(format!("part {n} landed"));
        crate::pieces::write(&mut t, &rec);
        store.save_task(&t).unwrap();
    }

    fn closes() -> Vec<String> {
        drained(&CLOSED)
    }

    /// The packet's first done-when: one member, pieces in a batch of two and a
    /// batch of one, run through both batches, both closes and the row's review
    /// with no hand step, every piece opened once however often the step runs.
    #[test]
    fn a_member_with_pieces_runs_both_batches_the_closes_and_the_review_with_no_hand_step() {
        let (_env, store) = scratch("pieces-run");
        pieced(&store, "m-1", Status::Todo, &[(1, 1), (2, 1), (3, 2)], 3);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let _ = closes();

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-1".to_string(), "m-1-2".to_string()], "batch 1, in record order, and no whole-row worker");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing, "a planned row is begun by its first batch");
        assert!(store.claims().is_empty(), "a piece claims nothing");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a second advance starts nothing twice");

        seat_piece(&store, "m-1", 1, "cpd-1");
        seat_piece(&store, "m-1", 2, "cpd-2");
        piece_done(&store, "m-1", 1);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty() && closes().is_empty(), "batch 1 waits for both its pieces");

        piece_done(&store, "m-1", 2);
        step(&store, &w, &Blind);
        assert_eq!(closes(), ["m-1 batch 1".to_string()], "the last piece's end closes its batch");
        assert_eq!(ids(spawned()), ["m-1-3".to_string()], "and batch 2 opens in the same step");
        assert_eq!(piece_rec(&store, "m-1").closed, [1]);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty() && closes().is_empty(), "a closed batch is not closed again");

        seat_piece(&store, "m-1", 3, "cpd-3");
        piece_done(&store, "m-1", 3);
        step(&store, &w, &Blind);
        assert_eq!(closes(), ["m-1 batch 2".to_string()]);
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Review, "the run typed the row's review");
        let log = t.section("Log").unwrap_or_default();
        assert!(log.contains(PIECES_REVIEWING), "{log}");
        assert!(log.contains("part 3 landed") && log.contains("batch 2"), "the account is the pieces table and captions: {log}");
        assert_eq!(piece_rec(&store, "m-1").closed, [1, 2]);
        assert!(spawned().iter().all(|(id, _)| !id.starts_with("m-1-")), "no piece opens after the last batch");
    }

    /// With the room capped at 2 and a second member ready, the pieces of the row
    /// begun take the places ahead of the new member.
    #[test]
    fn pieces_take_places_ahead_of_a_new_member_under_the_seats_cap() {
        let (_env, store) = scratch("pieces-cap");
        store.set_seat_cap(Some(2)).unwrap();
        pieced(&store, "m-1", Status::Doing, &[(1, 1), (2, 1)], 3);
        task(&store, "m-2", Status::Todo);
        let w = list(&store, &[(&["m-1", "m-2"], "claude")]);

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-1".to_string(), "m-1-2".to_string()], "the row begun goes before a new member");
        assert_eq!(store.find_task("m-2").unwrap().status(), Status::Todo, "the new member waits");
        let gate = crate::room::Gate::new(&store, &Blind);
        let now: Vec<String> = gate.working().into_iter().map(|c| c.what).collect();
        assert_eq!(now, ["m-1-1".to_string(), "m-1-2".to_string()], "each piece starting is one place");

        seat_piece(&store, "m-1", 1, "cpd-1");
        seat_piece(&store, "m-1", 2, "cpd-2");
        let gate = crate::room::Gate::new(&store, &Blind);
        let now: Vec<String> = gate.working().into_iter().map(|c| c.what).collect();
        assert_eq!(now, ["m-1-1".to_string(), "m-1-2".to_string()], "counted off the entry's seat, once each, not as the row");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the room is full");

        piece_done(&store, "m-1", 1);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-2".to_string()], "a done piece's seat not seen working frees its place");
    }

    /// `cap: 1` runs batch 1 one piece at a time, and a held piece is written as
    /// nothing: it stays pending.
    #[test]
    fn a_row_capped_at_one_runs_its_batch_one_piece_at_a_time() {
        let (_env, store) = scratch("pieces-cap1");
        pieced(&store, "m-1", Status::Todo, &[(1, 1), (2, 1)], 1);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let _ = closes();
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-1".to_string()]);
        assert_eq!(piece_of(&store, "m-1", 2).state, crate::pieces::State::Pending);
        seat_piece(&store, "m-1", 1, "cpd-1");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the row's own cap holds the second");
        piece_done(&store, "m-1", 1);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-2".to_string()], "the first ended, so the second runs");
        assert!(closes().is_empty(), "and the batch is not closed with a piece still to run");
    }

    /// What a batch that did not merge does: files `merge batch N` in a batch of
    /// its own right after, as `piece_close::close` would.
    fn conflicted(store: &Store, row: &str, batch: usize) -> crate::piece_close::Closed {
        let added = store.locked(|| {
            let mut t = store.find_task(row).unwrap();
            let rec = crate::pieces::record(&t);
            if let Some(p) = rec.pieces.iter().find(|p| p.title == format!("merge batch {batch}")) {
                return p.n;
            }
            let new = crate::pieces::New {
                title: format!("merge batch {batch}"),
                overview: "merge the branches by hand".into(),
                done_when: "the row's tree is clean at a commit".into(),
                at: crate::pieces::At::Next,
                checks: Vec::new(),
                agent: None,
                why: None,
            };
            let (rec, n) = crate::pieces::insert_batch_after(&rec, batch, new, t.status()).unwrap();
            crate::pieces::write(&mut t, &rec);
            store.save_task(&t).unwrap();
            n
        });
        crate::piece_close::Closed::Conflict { branches: vec![format!("{row}-2")], files: vec!["src/a.rs".into()], added }
    }

    /// A conflicting batch gets its `merge batch` piece, which opens next, and the
    /// run goes on to the batch after it once that closes.
    #[test]
    fn a_conflicting_batch_gets_its_merge_piece_and_the_run_goes_on() {
        let (_env, store) = scratch("pieces-conflict");
        pieced(&store, "m-1", Status::Doing, &[(1, 1), (2, 1), (3, 2)], 3);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let _ = closes();
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-1".to_string(), "m-1-2".to_string()]);
        seat_piece(&store, "m-1", 1, "cpd-1");
        seat_piece(&store, "m-1", 2, "cpd-2");
        piece_done(&store, "m-1", 1);
        piece_done(&store, "m-1", 2);

        CLOSE_AS.with(|f| f.set(Some(conflicted)));
        step(&store, &w, &Blind);
        CLOSE_AS.with(|f| f.set(None));
        assert_eq!(closes(), ["m-1 batch 1".to_string()]);
        assert_eq!(ids(spawned()), ["m-1-4".to_string()], "the merge piece opens, ahead of the batch it was put before");
        let merge = piece_of(&store, "m-1", 4);
        assert_eq!((merge.title.as_str(), merge.batch), ("merge batch 1", 2));
        assert_eq!(piece_of(&store, "m-1", 3).batch, 3, "the batch after moved up behind it");
        let log = store.find_task("m-1").unwrap().section("Log").unwrap_or_default();
        assert!(log.contains("batch 1 did not merge") && log.contains("piece 4"), "{log}");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty() && closes().is_empty(), "nothing twice");

        seat_piece(&store, "m-1", 4, "cpd-4");
        piece_done(&store, "m-1", 4);
        step(&store, &w, &Blind);
        assert_eq!(closes(), ["m-1 batch 2".to_string()]);
        assert_eq!(ids(spawned()), ["m-1-3".to_string()], "and the run goes on");
    }

    /// A piece's start that never takes is put back and retried on the backoff,
    /// keyed by the piece; a start written down and never seated is retaken.
    #[test]
    fn a_piece_start_that_never_takes_is_retried_on_the_backoff() {
        let (_env, store) = scratch("pieces-retry");
        pieced(&store, "m-1", Status::Doing, &[(1, 1)], 3);
        let w = list(&store, &[(&["m-1"], "claude")]);
        FAIL.with(|f| *f.borrow_mut() = Some("claude started but never became ready for input".into()));
        step(&store, &w, &Blind);
        FAIL.with(|f| *f.borrow_mut() = None);
        assert_eq!(ids(spawned()), ["m-1-1".to_string()]);
        assert_eq!(piece_of(&store, "m-1", 1).state, crate::pieces::State::Pending, "given back with no seat");
        assert!(crate::starts::held(&store, "m-1-1"), "keyed by the piece");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "its wait is not over");
        age_start(&store, "m-1-1", 31);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-1".to_string()], "tried again on the backoff");

        // Opened and never seated, past the minute: the start is taken again.
        let mut t = store.find_task("m-1").unwrap();
        let mut rec = crate::pieces::record(&t);
        rec.pieces[0].started = Some(util::iso_at(util::epoch_secs() - 2 * WEDGED_AFTER));
        crate::pieces::write(&mut t, &rec);
        store.save_task(&t).unwrap();
        crate::starts::forgive(&store, "m-1-1");
        let _ = drained(&SAID);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-1".to_string()]);
        assert!(drained(&SAID).iter().any(|l| l.contains("starting piece 1 of m-1 again")));
    }

    /// The tick ends a done piece's seat once, and records it on the entry.
    #[test]
    fn the_tick_ends_a_done_pieces_seat_once_and_records_it_on_the_entry() {
        let (_env, store) = scratch("pieces-end");
        pieced(&store, "m-1", Status::Todo, &[(1, 1)], 3);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        seat_piece(&store, "m-1", 1, "cpd-1");
        assert!(pieces_finished(&store, &Exits).is_empty(), "a running piece's seat is not ended");
        piece_done(&store, "m-1", 1);
        let _ = drained(&ENDED);
        assert_eq!(pieces_finished(&store, &Exits), ["cpd-1".to_string()]);
        assert_eq!(drained(&ENDED), ["cpd-1".to_string()]);
        assert_eq!(piece_of(&store, "m-1", 1).ending, crate::verification::Ending::Ended);
        assert!(pieces_finished(&store, &Exits).is_empty(), "once");
    }

    /// Outside a list: `piece done`'s launch opens what is ready, and the tick
    /// catches it for a begun row and never starts an unbegun one.
    #[test]
    fn outside_a_list_the_launch_and_the_tick_start_a_begun_rows_pieces() {
        let (_env, store) = scratch("pieces-hand");
        pieced(&store, "h-1", Status::Doing, &[(1, 1), (2, 2)], 3);
        let _ = closes();
        assert!(pieces_owed(&store, &Blind).is_empty(), "staging a row is not starting it");
        assert_eq!(start_inside(&store, "h-1", &Blind), ["h-1-1".to_string()], "the launch opens batch 1");
        let _ = spawned();
        seat_piece(&store, "h-1", 1, "cpd-1");
        piece_done(&store, "h-1", 1);
        assert_eq!(pieces_owed(&store, &Blind), ["h-1-2".to_string()], "a lost launch: the tick closes batch 1 and opens batch 2");
        assert_eq!(closes(), ["h-1 batch 1".to_string()]);
        assert!(pieces_owed(&store, &Blind).is_empty(), "and once");
    }

    /// A seat holding the row's claim keeps every piece shut.
    #[test]
    fn no_piece_opens_while_a_seat_holds_the_rows_claim() {
        let (_env, store) = scratch("pieces-claimed");
        pieced(&store, "m-1", Status::Doing, &[(1, 1)], 3);
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        assert!(spawned().iter().all(|(id, _)| id != "m-1-1"), "the planner still holds it");
        store.clear_claim("m-1");
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1-1".to_string()]);
    }

    // ---- starts that did not take (wsp-335) --------------------------------------

    /// Make the misses of `label` as old as they would be `secs` after the last.
    fn age_start(store: &Store, label: &str, secs: i64) {
        store.update_starts(|m| {
            m.get_mut(label).unwrap()["at"] = serde_json::json!(util::epoch_secs() - secs);
        });
    }

    fn flags_on(store: &Store, row: &str) -> Vec<crate::message::Message> {
        crate::message::raised(store).into_iter().filter(|m| m.about.task() == Some(row)).collect()
    }

    fn leg_of(store: &Store, id: &str, name: &str) -> crate::legs::Leg {
        crate::legs::legs(&store.find_task(id).unwrap()).into_iter().find(|l| l.name == name).unwrap()
    }

    /// `wsp-335`, seen on `compound-956`: `wsp spawn` named the pane on the leg,
    /// the agent never came up, and the entry stayed `running` on a pane that
    /// was gone, which nothing reset and `spawn --leg` refused to open again.
    #[test]
    fn a_leg_whose_agent_never_came_up_loses_its_seat_and_starts_again_on_the_backoff() {
        let (_env, store) = scratch("legs-never-up");
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        seat_leg_open(&store, "m-1", "plan", "cpd-9");
        let _ = (drained(&TOLD), drained(&ENDED));

        leg_start_failed(&store, Some(&w), "m-1", "plan", "claude started but never became ready for input");

        let leg = leg_of(&store, "m-1", "plan");
        assert_eq!((leg.state, leg.pane.clone(), leg.started.clone()), (crate::legs::State::Pending, None, None), "{leg:?}");
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()], "the pane the start left is ended");
        assert!(store.bindings().get("cpd-9").is_none(), "and no longer says it is the row's");
        let told = drained(&TOLD);
        assert!(told.len() == 1 && told[0].contains("try 1 of 3") && told[0].contains("never became ready"), "{told:?}");
        let log = store.find_task("m-1").unwrap().section("Log").unwrap_or_default().to_string();
        assert!(log.contains("could not spawn leg plan (try 1 of 3)"), "{log}");

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "not at once: the backoff");
        age_start(&store, "m-1 · plan", 4 * 60);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · plan".to_string()], "started again, by the ordinary step");
    }

    /// The step's own start failing is the same road: no pane had to be named
    /// for the leg to be put back and held.
    #[test]
    fn a_leg_start_that_fails_in_the_step_is_retried_and_the_person_is_flagged_when_the_tries_run_out() {
        let (_env, store) = scratch("legs-spent");
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        FAIL.with(|f| *f.borrow_mut() = Some("claude started but never became ready for input".into()));

        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 1);
        assert_eq!(leg_state(&store, "m-1", "plan"), crate::legs::State::Pending);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "held for its three minutes");
        assert!(flags_on(&store, "m-1").is_empty());

        for wait in [4 * 60, 6 * 60] {
            age_start(&store, "m-1 · plan", wait);
            step(&store, &w, &Blind);
            assert_eq!(spawned().len(), 1);
        }
        FAIL.with(|f| *f.borrow_mut() = None);

        let flags = flags_on(&store, "m-1");
        assert_eq!(flags.len(), 1, "one flag, on the row");
        assert!(flags[0].text.contains("after 3 tries") && flags[0].text.contains("never became ready"), "{}", flags[0].text);
        assert!(flags[0].text.contains("wsp leg reset m-1 plan"), "it says how to go on: {}", flags[0].text);

        age_start(&store, "m-1 · plan", 24 * 60 * 60);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "spent: no fourth start however long it waits");
        assert_eq!(flags_on(&store, "m-1").len(), 1, "and no second flag");

        // A person's `wsp worklist advance <list>` takes it from the top.
        assert_eq!(forgive_spent_starts(&store, "run"), 1);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · plan".to_string()]);
    }

    /// The cap counted the dead leg and held the next one back. Reset, it holds
    /// no place at all.
    #[test]
    fn a_leg_that_never_came_up_holds_no_place_under_the_seats_cap() {
        let (_env, store) = scratch("legs-cap-dead");
        store.set_seat_cap(Some(1)).unwrap();
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        seat_leg_open(&store, "m-1", "plan", "cpd-9");
        let starting = Fixed::new(&[("cpd-9", State::Starting)]);
        let held = crate::room::Gate::new(&store, &starting).working();
        assert_eq!(held.len(), 1, "a seat coming up counts: {held:?}");

        leg_start_failed(&store, Some(&w), "m-1", "plan", "never ready");
        assert!(crate::room::Gate::new(&store, &starting).working().is_empty(), "reset, it is nobody's place");
    }

    /// A member's failed start leaves a claim on a pane whose agent never came
    /// up, with the row put back at `todo` where `repair` does not look.
    #[test]
    fn a_member_whose_agent_never_came_up_is_ended_and_started_again_and_the_person_is_flagged_at_the_end() {
        let (_env, store) = scratch("member-never-up");
        task(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        FAIL.with(|f| *f.borrow_mut() = Some("claude started but never became ready for input".into()));

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1".to_string()]);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Todo, "put back");
        assert_eq!(drained(&ENDED), vec!["m-1".to_string()], "and what the start left is ended");
        let told = drained(&TOLD);
        assert!(told.iter().any(|t| t.contains("try 1 of 3") && t.contains("never became ready")), "{told:?}");

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "held for its wait");
        for wait in [4 * 60, 6 * 60] {
            age_start(&store, "m-1", wait);
            step(&store, &w, &Blind);
            assert_eq!(ids(spawned()), ["m-1".to_string()]);
        }
        FAIL.with(|f| *f.borrow_mut() = None);
        assert_eq!(flags_on(&store, "m-1").len(), 1);
        age_start(&store, "m-1", 24 * 60 * 60);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "spent");

        assert_eq!(forgive_spent_starts(&store, "run"), 1);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1".to_string()]);
    }

    /// The spawn that was killed before it could say it failed: the pane reads
    /// `starting` for good, and the tick says so.
    #[test]
    fn a_leg_seat_that_is_not_up_after_five_minutes_is_ended_and_the_leg_started_again() {
        let (_env, store) = scratch("legs-not-up");
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        seat_leg_open(&store, "m-1", "plan", "cpd-9");
        let starting = Fixed::new(&[("cpd-9", State::Starting)]);
        let opened_ago = |secs: i64| edit_legs(&store, "m-1", |old| {
            let mut v = old.to_vec();
            v[0].started = Some(util::iso_at(util::epoch_secs() - secs));
            Ok(v)
        });

        opened_ago(4 * 60);
        step(&store, &w, &starting);
        assert!(drained(&ENDED).is_empty(), "a slow start inside five minutes is left alone");
        assert_eq!(leg_state(&store, "m-1", "plan"), crate::legs::State::Running);

        opened_ago(6 * 60);
        step(&store, &w, &starting);
        assert_eq!(drained(&ENDED), vec!["cpd-9".to_string()]);
        assert_eq!(leg_state(&store, "m-1", "plan"), crate::legs::State::Pending);
        assert!(drained(&TOLD).iter().any(|t| t.contains("try 1 of 3")));
        step(&store, &w, &starting);
        assert!(spawned().is_empty(), "and tried again on the backoff, not at once");
    }

    /// `wsp leg reset`'s half: the record goes back and the misses are forgiven,
    /// so the leg is not held by a count it earned before.
    #[test]
    fn a_running_leg_reset_by_hand_is_pending_with_its_misses_forgiven() {
        let (_env, store) = scratch("legs-reset");
        staged(&store, "m-1", Status::Todo);
        seat_leg_open(&store, "m-1", "plan", "cpd-9");
        for _ in 0..3 {
            crate::starts::missed(&store, "m-1 · plan");
        }
        assert_eq!(crate::leg_seat::reset(&store, "m-1", "plan"), Some(Some("cpd-9".to_string())));
        assert!(crate::starts::forgive(&store, "m-1 · plan"));
        assert!(!crate::starts::held(&store, "m-1 · plan"));
        let leg = leg_of(&store, "m-1", "plan");
        assert_eq!((leg.state, leg.pane), (crate::legs::State::Pending, None));
        assert_eq!(crate::leg_seat::reset(&store, "m-1", "plan"), None, "a pending leg is not reset");
    }

    /// A start that never seated is taken again once it is old enough, and not
    /// before.
    #[test]
    fn a_leg_opened_and_never_seated_is_started_again_once_it_is_wedged() {
        let (_env, store) = scratch("legs-wedged");
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · plan".to_string()]);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "a start in flight is left alone");

        edit_legs(&store, "m-1", |old| {
            let mut v = old.to_vec();
            v[0].started = Some("2020-01-01T00:00:00Z".into());
            Ok(v)
        });
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · plan".to_string()], "a minute on with no seat: taken again");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "and the new opening is the key");
    }

    /// A failed start gives the leg back, and says so on the row.
    #[test]
    fn a_leg_whose_spawn_failed_goes_back_to_pending_with_the_reason_on_the_row() {
        let (_env, store) = scratch("legs-fail");
        staged(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        FAIL.with(|f| *f.borrow_mut() = Some("no room".into()));
        step(&store, &w, &Blind);
        FAIL.with(|f| *f.borrow_mut() = None);
        let _ = spawned();
        assert_eq!(leg_state(&store, "m-1", "plan"), crate::legs::State::Pending);
        let log = store.find_task("m-1").unwrap().section("Log").unwrap_or_default().to_string();
        assert!(log.contains("could not spawn leg plan (try 1 of 3): no room"), "{log}");
    }

    /// A start's note about a clone it could not make is not why it failed
    /// (wsp-427): the real reason is what is left.
    #[test]
    fn a_spawn_failure_names_its_cause_and_not_the_note_about_a_dependency_clone() {
        let err = "wsp: spikes/canvas/target not cloned: this tree has no spikes/canvas/Cargo.lock — install as usual\nwsp: claude started but never became ready\n";
        assert_eq!(why_not(err), "wsp: claude started but never became ready");
        assert_eq!(why_not("wsp: no room"), "wsp: no room");
    }

    /// A `doing` row with no seat, left quiet for `secs`, with `lines` on its log
    /// and nothing said of it in `cycle.log`.
    fn left_unseated(store: &Store, id: &str, lines: &[&str], secs: i64) {
        let mut t = store.find_task(id).unwrap();
        for l in lines {
            t.log(l);
        }
        t.updated = util::iso_at(util::epoch_secs() - secs);
        store.save_task(&t).unwrap();
        std::fs::write(store.state_file("cycle.log"), "").unwrap();
    }

    fn said(store: &Store) -> String {
        std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default()
    }

    /// wsp-549 (compounding-200, wsp-502): a start the seat's cap holds is queued
    /// and uses no try. A row whose last start went quiet, passed over for many
    /// passes at the cap, is not given up on; when a slot frees it starts, and
    /// that start is the first miss and not the third.
    #[test]
    fn a_start_held_at_the_cap_uses_no_try_and_starts_when_a_slot_frees() {
        let (_env, store) = scratch("queued-at-cap");
        task(&store, "m-1", Status::Doing);
        task(&store, "h-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        store.set_seat_cap(Some(1)).unwrap();
        left_unseated(&store, "m-1", &[&format!("{STARTED_BY} run group 1")], 100);
        // Another start in flight holds the one place.
        let mut h = store.find_task("h-1").unwrap();
        h.log(&format!("{STARTED_BY} other group 1"));
        store.save_task(&h).unwrap();

        for _ in 0..6 {
            step(&store, &w, &Blind);
            assert!(spawned().is_empty(), "the seat is full");
            assert!(!crate::starts::held(&store, "m-1"), "and the wait is not a miss");
            assert!(store.starts().get("m-1").is_none(), "no try used");
        }
        assert!(!said(&store).contains("giving up"), "{}", said(&store));
        assert!(!said(&store).contains("starting again, try"), "{}", said(&store));
        assert!(store.capped().get("run").is_some(), "queued, for wip to read");

        set(&store, "h-1", Status::Done);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "started as soon as a slot freed");
        assert_eq!(store.starts().get("m-1").and_then(|e| e["tries"].as_u64()), Some(1), "one try, the launched one");
    }

    /// wsp-584: an x3 first group that is not together, with one of four places
    /// free, starts exactly one member; the next starts when a place frees.
    #[test]
    fn an_x3_group_not_together_starts_one_member_with_one_place_free_and_the_next_as_places_free() {
        let (_env, store) = scratch("x3-one-free");
        for id in ["m-1", "m-2", "m-3"] {
            task(&store, id, Status::Todo);
        }
        for id in ["h-1", "h-2", "h-3"] {
            task(&store, id, Status::Doing);
            let mut h = store.find_task(id).unwrap();
            h.log(&format!("{STARTED_BY} other group 1"));
            store.save_task(&h).unwrap();
        }
        let w = list(&store, &[(&["m-1", "m-2", "m-3"], "claude")]);
        let mut w2 = store.worklist("run").unwrap();
        let mut groups = w2.groups();
        groups[0].cap = Some(3);
        groups[0].together = false;
        w2.set_groups(&groups);
        store.save_worklist(&w2).unwrap();
        store.set_seat_cap(Some(4)).unwrap();

        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 1, "one place free: exactly one member starts");

        set(&store, "h-1", Status::Done);
        step(&store, &w, &Blind);
        assert_eq!(spawned().len(), 1, "a place freed: the next member starts");
    }

    /// wsp-524, the first case: a start that failed its three tries while the
    /// seat was at its cap is taken again once there is room, at the member's
    /// own agent line, and once.
    #[test]
    fn a_start_given_up_at_the_cap_is_reseated_once_there_is_room() {
        let (_env, store) = scratch("reseat-spent");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        left_unseated(&store, "m-1", &[&format!("{STARTED_BY} run group 1")], 400);
        store.update_starts(|m| {
            m.insert("m-1".into(), serde_json::json!({ "tries": 3, "at": util::epoch_secs() - 400, "spent": true }));
        });
        assert!(crate::starts::held(&store, "m-1"), "given up, and held until a person says");
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "reseated at its own agent line");
        assert!(said(&store).contains("started again: no seat"), "{}", said(&store));
        assert!(!crate::starts::spent(&store, "m-1"), "and its spent count is forgiven");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "once per silence");
    }

    /// wsp-550: a member already flagged (its tries spent, wsp-524's reseat) while
    /// the seat is at its cap waits for room and uses no try: nothing started,
    /// no "starting again" said, no second flag, its spent count kept until the
    /// reseat. Once a slot frees it is reseated, once.
    #[test]
    fn a_flagged_member_at_the_cap_waits_for_room_uses_no_try_and_is_reseated_once() {
        let (_env, store) = scratch("reseat-flagged-cap");
        task(&store, "m-1", Status::Doing);
        task(&store, "h-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        store.set_seat_cap(Some(1)).unwrap();
        // Quiet past the room's own window, so its old start holds no place.
        left_unseated(&store, "m-1", &[&format!("{STARTED_BY} run group 1")], 700);
        store.update_starts(|m| {
            m.insert("m-1".into(), serde_json::json!({ "tries": 3, "at": util::epoch_secs() - 700, "spent": true }));
        });
        let mut h = store.find_task("h-1").unwrap();
        h.log(&format!("{STARTED_BY} other group 1"));
        store.save_task(&h).unwrap();

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the seat is full");
        assert!(store.capped().get("run").is_some(), "queued, for wip and next to read: {}", said(&store));
        assert!(said(&store).contains("m-1 waits — the seat is at its cap of 1"), "{}", said(&store));
        // The wait is said once (it is a line said of the row, so the next
        // silence is five minutes on); every pass after reads the same.
        for _ in 0..5 {
            step(&store, &w, &Blind);
            assert!(spawned().is_empty(), "still full");
            assert!(crate::starts::spent(&store, "m-1"), "still flagged, and not forgiven while it waits");
            assert_eq!(store.starts().get("m-1").and_then(|e| e["tries"].as_u64()), Some(3), "no try used");
        }
        assert!(!said(&store).contains("starting again"), "{}", said(&store));
        assert!(!said(&store).contains("started again"), "{}", said(&store));
        assert!(!said(&store).contains("flagged the person"), "{}", said(&store));
        let log = store.find_task("m-1").unwrap().section("Log").unwrap_or_default().to_string();
        assert_eq!(log.matches(super::RESEATED).count(), 0, "no reseat written while it waits: {log}");

        // A slot frees and the row has been silent again for the window.
        set(&store, "h-1", Status::Done);
        std::fs::write(store.state_file("cycle.log"), "").unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())], "reseated once there is room");
        assert!(!crate::starts::spent(&store, "m-1"), "and its spent count is forgiven then");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "once");
    }

    /// The second case: the verifier blocked it, the run's start did not take,
    /// the governor answered that starting one is the run's job and wrote
    /// nothing else. The run starts it.
    #[test]
    fn a_blocked_member_the_governor_declined_to_start_is_reseated() {
        let (_env, store) = scratch("reseat-block");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        left_unseated(
            &store,
            "m-1",
            &[
                &format!("{SENT_BACK} the verifier blocked: something"),
                &format!("{STARTED_BY} run group 1"),
                "governor: starting one is the run's job",
            ],
            400,
        );
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())]);
        assert!(said(&store).contains("started again: no seat"), "{}", said(&store));
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "once");
    }

    /// The third case: a governor's despawn writes the word the step reads, and
    /// the seat is reseated at once and not after five quiet minutes.
    #[test]
    fn a_member_a_governor_despawned_is_reseated_by_the_next_step() {
        let (_env, store) = scratch("reseat-despawn");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        let mut t = store.find_task("m-1").unwrap();
        t.log(RESEAT_ASKED);
        store.save_task(&t).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".to_string(), "claude".to_string())]);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "once");
    }

    /// Two reseats that never reached a first turn are the most: the third silence
    /// starts nothing and raises one item, and a fourth raises no second.
    #[test]
    fn a_third_failed_reseat_raises_one_item_and_starts_nothing() {
        let (_env, store) = scratch("reseat-cap");
        task(&store, "m-1", Status::Doing);
        let w = list(&store, &[(&["m-1"], "claude")]);
        for round in 1..=4 {
            left_unseated(&store, "m-1", &["governor: nothing to add"], 400);
            step(&store, &w, &Blind);
            let started = spawned().len();
            assert_eq!(started, usize::from(round <= 2), "round {round}");
        }
        let log = store.find_task("m-1").unwrap().section("Log").unwrap_or_default().to_string();
        assert_eq!(log.matches(RESEAT_STOPPED).count(), 1, "one stop, said once: {log}");
        assert_eq!(log.matches(super::RESEATED).count(), 2, "{log}");
        assert!(!said(&store).contains("started again"), "the last rounds started nothing: {}", said(&store));
    }

    /// A start that has not claimed within a minute is taken again, and a
    /// younger one is left alone (wsp-469).
    #[test]
    fn a_start_with_no_claim_is_wedged_after_a_minute_and_not_before() {
        let (_env, store) = scratch("wedged-minute");
        task(&store, "m-1", Status::Doing);
        let mut t = store.find_task("m-1").unwrap();
        t.updated = util::iso_at(util::epoch_secs() - 30);
        assert!(!wedged(&t, &store.claims()), "30 s old: still in flight");
        t.updated = util::iso_at(util::epoch_secs() - 90);
        assert!(wedged(&t, &store.claims()), "90 s with no claim: taken again");
    }

    /// A row at `doing` with no seat and nothing said of it for five minutes is
    /// named, in the log and for `wsp wip` (wsp-427).
    #[test]
    fn a_row_with_no_seat_and_no_line_for_five_minutes_is_named_once_something_is_said_of_it() {
        let (_env, store) = scratch("silent-row");
        task(&store, "m-1", Status::Doing);
        let ids = vec!["m-1".to_string()];
        assert!(silent(&store, &ids).is_empty(), "just written");
        let mut t = store.find_task("m-1").unwrap();
        t.updated = util::iso_at(util::epoch_secs() - 400);
        store.save_task(&t).unwrap();
        let got = silent(&store, &ids);
        assert_eq!(got.len(), 1);
        assert!(got[0].1 >= 400);
        log_line(&store, "list-1: m-1 has no seat and no log line for 6m");
        assert!(silent(&store, &ids).is_empty(), "the line is a line said of it");
    }

    /// A staged row is never given a whole-row worker beside its legs, by any of
    /// the repairs that seat a row at `doing` with nobody on it.
    #[test]
    fn a_staged_row_with_no_claim_is_left_to_its_legs_by_the_whole_row_repairs() {
        let (_env, store) = scratch("legs-exclusion");
        fanned_out(&store, "m-1");
        let mut t = store.find_task("m-1").unwrap();
        t.log(&format!("{STARTED_BY} run group 1"));
        t.log(&format!("{SENT_BACK} the verifier blocked: something"));
        t.updated = "2026-10-18T08:00:00Z".into();
        store.save_task(&t).unwrap();
        let t = store.find_task("m-1").unwrap();
        assert!(!wedged(&t, &store.claims()), "unclaimed for hours, and not wedged: it has no claim by design");
        assert!(!sent_back_unseated(&store, &t, &store.claims()));
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · a".to_string(), "m-1 · b".to_string()], "its legs and not the row");
    }

    /// A member's seat that finds its row bigger than one seat stages the rest
    /// and releases (`wsp-311`): no leg opens while it still holds the row,
    /// because the first one would stand in its tree, and every ready leg opens
    /// on the step after the release.
    #[test]
    fn a_seat_that_stages_the_rest_of_its_own_row_hands_it_to_the_legs_by_releasing() {
        let (_env, store) = scratch("legs-handed-over");
        task(&store, "m-1", Status::Doing);
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        let w = list(&store, &[(&["m-1"], "claude")]);
        // From the claim-holder's seat, which is no leg's and so is judged as a
        // person's: two builds side by side and a finish that joins them.
        edit_legs(&store, "m-1", |old| {
            crate::legs::add(old, crate::legs::New { name: "build-a".into(), order: "do a".into(), ..Default::default() }, None)
        });
        add_leg(&store, "m-1", "finish", &["build-a"], None, None);
        add_leg(&store, "m-1", "build-b", &[], Some("finish"), None);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "the seat still holds the row, so nothing opens beside it");
        assert_eq!(leg_state(&store, "m-1", "build-a"), crate::legs::State::Pending);

        assert!(store.clear_claim("m-1"), "`wsp release` ends the claim");
        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · build-a".to_string(), "m-1 · build-b".to_string()], "the ready legs, and not the finish");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "and each once");
    }

    /// `wsp-311`, compound-758's shape: one member at doing, whose seat filed
    /// four sub-tasks under it with titles only and built them all itself, then
    /// landed about 4,000 lines in one commit. Surfaced on `worklist show` while
    /// the member is worked; at its landing, to the governor once and in the
    /// verifier's order; and the barrier's follow-up, which is a member, never.
    #[test]
    fn a_member_split_into_sub_tasks_nothing_ran_is_surfaced_on_show_to_the_governor_and_to_its_verifier() {
        let (_env, store) = scratch("unrun-758");
        let mut m = Task::new("The Review tab, the Board as a document, the N at review door and do view board", "c-758");
        m.status_raw = "doing".into();
        crate::model::set_section_in(
            &mut m.body,
            "Overview",
            "Size M-L. Split it if larger.\n\nDone when: the Review tab lists rows at review; the Board renders as a document; \
             the door counts rows at review; `do view board` opens it.",
        );
        store.save_task(&m).unwrap();
        store.set_claim("c-758", serde_json::json!({ "workspace": "w" }));
        for (id, title) in [("c-782", "The Review tab"), ("c-783", "The Board as a document"), ("c-784", "The N at review door"), ("c-785", "do view board")] {
            let mut s = Task::new(title, id);
            s.parent = Some("c-758".into());
            store.save_task(&s).unwrap();
        }
        // A barrier's follow-up under the same member is a member of the list,
        // so the run starts it and it is not counted.
        let mut f = Task::new("a follow-up the barrier attached", "c-790");
        f.parent = Some("c-758".into());
        store.save_task(&f).unwrap();
        let w = list(&store, &[(&["c-758"], "claude"), (&["c-790"], "claude")]);
        let _ = drained(&TOLD);

        // While it is worked: on `worklist show`, in text and in JSON.
        let found = |store: &Store| {
            let tasks = store.tasks_shared();
            let run = worklist::Running::read(store);
            let w = store.worklist("run").unwrap();
            let lines = crate::cmd_worklist::unrun_lines(&util::Paint::plain(), &crate::split::on_list(&tasks, &w, &run));
            (lines.join("\n"), crate::split::json(&crate::split::on_list(&tasks, &w, &run)))
        };
        let (text, json) = found(&store);
        assert!(text.contains("unrun") && text.contains("c-758 is at doing with 4 sub-task(s)"), "{text}");
        assert!(text.contains("c-782 c-783 c-784 c-785") && !text.contains("c-790"), "{text}");
        assert!(text.contains("stage the rest as legs"), "{text}");
        assert_eq!(json[0]["member"], "c-758");
        assert_eq!(json[0]["subtasks"].as_array().map(Vec::len), Some(4), "{json}");

        // The landing: one commit of about 4,000 lines, and the member to review.
        let mut m = store.find_task("c-758").unwrap();
        m.log(&format!("{} 13488cc6 on master · 85 files changed, 3912 insertions(+), 88 deletions(-)", crate::repair::LANDED));
        m.set_status(Status::Review);
        store.save_task(&m).unwrap();
        store.clear_claim("c-758");
        step(&store, &w, &Blind);
        assert_eq!(reading(&store), vec!["c-758".to_string()], "its verifier opened");
        let told = drained(&TOLD);
        let about: Vec<&String> = told.iter().filter(|t| t.contains("no run starts or verifies")).collect();
        assert_eq!(about.len(), 1, "the governor is told once: {told:?}");
        assert!(about[0].contains("c-758 is at review with 4 sub-task(s)") && about[0].contains("wsp leg add"), "{}", about[0]);

        let order = verifier_order(&store, &store.find_task("c-758").unwrap());
        assert!(order.contains("c-758 was split into sub-tasks that never ran"), "{order}");
        for id in ["c-782", "c-783", "c-784", "c-785"] {
            assert!(order.contains(&format!("- {id} (")), "{id} is named: {order}");
        }
        assert!(!order.contains("c-790"), "the follow-up is a member and is run: {order}");
        assert!(order.contains("Done when** item by item"), "{order}");

        step(&store, &w, &Blind);
        assert!(drained(&TOLD).iter().all(|t| !t.contains("no run starts or verifies")), "and not again on the next tick");
        let (text, _) = found(&store);
        assert!(text.contains("c-758 is at review"), "still on show while it waits on its verdict: {text}");

        // A member with no sub-tasks gains nothing in its order.
        task(&store, "m-2", Status::Review);
        assert!(!verifier_order(&store, &store.find_task("m-2").unwrap()).contains("sub-tasks"));
    }

    /// A blocking verdict appends exactly one `rework` leg after the last, which
    /// the step starts; a verdict recorded twice is still one.
    #[test]
    fn a_blocking_verdict_appends_one_rework_leg_after_the_last_and_the_step_starts_it() {
        let (_env, store) = scratch("legs-rework");
        staged(&store, "m-1", Status::Todo);
        add_leg(&store, "m-1", "build", &["plan"], None, None);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w, &Blind);
        seat_leg(&store, "m-1", "plan", "cpd-1");
        leg_done(&store, "m-1", "plan");
        step(&store, &w, &Blind);
        seat_leg(&store, "m-1", "build", "cpd-2");
        leg_done(&store, "m-1", "build");
        set(&store, "m-1", Status::Review);
        step(&store, &w, &Blind);
        let _ = spawned();

        let verdict = "the wiring is untested; the build leg stopped before the second caller";
        crate::verification::record(&store, "m-1", crate::verification::State::Blocks, verdict, Some("cpd-9"), None).unwrap();
        let all = crate::legs::legs(&store.find_task("m-1").unwrap());
        assert_eq!(all.iter().filter(|l| l.rework.is_some()).count(), 1, "one rework leg: {all:?}");
        let rework = all.last().unwrap();
        assert_eq!(rework.name, "rework");
        assert_eq!(crate::legs::waits(&all).last().unwrap(), &vec!["build".to_string()], "it waits on the last leg");
        assert_eq!(crate::legs::lasts(&all), ["rework".to_string()], "and is the last leg now");
        assert_eq!(rework.rework.as_deref(), Some(&["build".to_string()][..]), "naming the leg the verdict named");
        assert!(rework.order.contains(verdict), "its order is the verdict");
        assert!(crate::legs::is_default(&rework.policy) && rework.why.is_none(), "Sonnet: build had no why");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing, "the row is back at doing");

        // A second recording of the same send-back appends nothing more.
        crate::verification::record(&store, "m-1", crate::verification::State::Blocks, verdict, None, None).unwrap();
        assert_eq!(crate::legs::legs(&store.find_task("m-1").unwrap()).len(), 3, "plan, build, and the one rework");

        step(&store, &w, &Blind);
        assert_eq!(ids(spawned()), ["m-1 · rework".to_string()], "the rework leg, and no whole-row reseat");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
    }

    /// The tier rule: a named leg that had a `why` is inherited, `rework of …:`
    /// prefixed; several named take the dearest; none named gives the default.
    #[test]
    fn a_rework_leg_inherits_the_dearest_named_tier_and_its_why() {
        let opus = |name: &str, why: &str| crate::legs::Leg {
            policy: crate::model::Policy::parse("claude opus high").unwrap(),
            why: Some(why.into()),
            after: Some(Vec::new()),
            ..crate::legs::Leg::pending(name, "x")
        };
        let mid = crate::legs::Leg {
            policy: crate::model::Policy::parse("claude sonnet high").unwrap(),
            why: Some("hard to review".into()),
            after: Some(vec!["design".into()]),
            ..crate::legs::Leg::pending("mid", "x")
        };
        let plain = crate::legs::Leg { after: Some(vec!["mid".into()]), ..crate::legs::Leg::pending("build", "x") };
        let old = vec![opus("design", "decides the layer"), mid, plain];

        let both = crate::legs::rework(&old, "design is wrong and mid is thin").unwrap().unwrap();
        let r = both.last().unwrap();
        assert_eq!(r.policy, crate::model::Policy::parse("claude opus high").unwrap(), "the dearest of the two");
        assert_eq!(r.why.as_deref(), Some("rework of design: decides the layer"));
        assert_eq!(r.rework.as_deref(), Some(&["design".to_string(), "mid".to_string()][..]));

        let default = crate::legs::rework(&old, "build left a path untested").unwrap().unwrap();
        assert!(crate::legs::is_default(&default.last().unwrap().policy), "a default leg named gives no reason");

        let none = crate::legs::rework(&old, "it does not hold").unwrap().unwrap();
        let r = none.last().unwrap();
        assert!(crate::legs::is_default(&r.policy) && r.why.is_none());
        assert_eq!(r.rework.as_deref(), Some(&[][..]), "a list, empty, for a verdict that named none");

        // And a name inside a longer word is not the leg.
        let embedded = crate::legs::rework(&old, "the redesigned part").unwrap().unwrap();
        assert!(embedded.last().unwrap().rework.as_deref().unwrap().is_empty());
    }

    /// A row at the leg cap has no room for a rework leg: it says so on its row,
    /// the verdict still reaches whoever is on the row, and the governor is told
    /// there is no rework leg rather than that one was added.
    #[test]
    fn a_row_at_the_leg_cap_gets_no_rework_leg_and_its_governor_and_pane_are_told_the_truth() {
        let (_env, store) = scratch("legs-rework-cap");
        task(&store, "m-1", Status::Review);
        let mut all: Vec<crate::legs::Leg> = (0..crate::legs::MAX_LEGS)
            .map(|n| crate::legs::Leg { state: crate::legs::State::Done, ..crate::legs::Leg::pending(&format!("l{n}"), "x") })
            .collect();
        all[0].after = Some(Vec::new());
        edit_legs(&store, "m-1", |_| Ok(all.clone()));
        store.set_binding("cpd-7", serde_json::json!({ "task_id": "m-1" }));
        let w = list(&store, &[(&["m-1"], "claude")]);
        let _ = drained(&TOLD);
        let _ = drained(&MEMBER_TOLD);

        crate::verification::record(&store, "m-1", crate::verification::State::Blocks, "the second caller is untested", None, None).unwrap();
        let t = store.find_task("m-1").unwrap();
        assert_eq!(crate::legs::legs(&t).len(), crate::legs::MAX_LEGS);
        assert!(!crate::legs::rework_open(&crate::legs::legs(&t)));
        let log = t.section("Log").unwrap_or_default().to_string();
        assert!(log.contains("no rework leg was added") && log.contains("governor decides"), "{log}");

        crate::verification::deliver_unless_reworked(&store, "m-1", "the second caller is untested");
        assert_eq!(drained(&MEMBER_TOLD).len(), 1, "nothing carries the verdict, so the pane is told it");

        told_verdict(&store, &w, &t);
        let told = drained(&TOLD).join("\n");
        assert!(told.contains("NO rework leg") && told.contains("the second caller is untested"), "{told}");
        assert!(!told.contains("appended a `rework` leg"), "and it does not claim one: {told}");
    }

    /// And with a rework leg, the leg carries the verdict: the pane is not told twice
    /// and the governor is told one was appended.
    #[test]
    fn with_a_rework_leg_the_verdict_is_not_also_delivered_to_the_pane() {
        let (_env, store) = scratch("legs-rework-told");
        staged(&store, "m-1", Status::Review);
        leg_done_by_hand(&store);
        store.set_binding("cpd-7", serde_json::json!({ "task_id": "m-1" }));
        let w = list(&store, &[(&["m-1"], "claude")]);
        let _ = drained(&TOLD);
        let _ = drained(&MEMBER_TOLD);
        crate::verification::record(&store, "m-1", crate::verification::State::Blocks, "it is thin", None, None).unwrap();
        crate::verification::deliver_unless_reworked(&store, "m-1", "it is thin");
        assert!(drained(&MEMBER_TOLD).is_empty());
        told_verdict(&store, &w, &store.find_task("m-1").unwrap());
        assert!(drained(&TOLD).join("\n").contains("appended a `rework` leg"));
    }

    /// `wsp-360`: legs a (done) and b (skipped, last). Skipping b leaves nothing
    /// running or pending, so the row is finished — at review, said on its log,
    /// its governor told — once; and the repair tick finishes a record that got
    /// there by hand.
    #[test]
    fn a_row_whose_last_leg_is_skipped_is_finished_and_its_governor_told() {
        let (_env, store) = scratch("legs-skip-out");
        staged(&store, "m-1", Status::Doing);
        add_leg(&store, "m-1", "b", &["plan"], None, None);
        let _ = list(&store, &[(&["m-1"], "claude")]);
        leg_done_by_hand(&store);
        let _ = drained(&TOLD);
        assert!(!crate::leg_seat::finish_skipped_out(&store, "m-1"), "a pending leg remains");
        edit_legs(&store, "m-1", |old| crate::legs::skip(old, "b", "split into rows of their own", None));
        assert!(crate::leg_seat::finish_skipped_out(&store, "m-1"));
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Review);
        assert!(t.section("Log").unwrap().contains("the last, b, was skipped"), "{}", t.section("Log").unwrap());
        let told = drained(&TOLD).join("\n");
        assert!(told.contains("m-1") && told.contains("was skipped"), "{told}");
        assert!(!crate::leg_seat::finish_skipped_out(&store, "m-1"), "once");
        assert!(drained(&TOLD).is_empty());
    }

    /// The same record written by hand: the repair tick finds it.
    #[test]
    fn the_repair_tick_finishes_a_row_whose_last_leg_was_skipped_by_hand() {
        let (_env, store) = scratch("legs-skip-out-tick");
        staged(&store, "m-1", Status::Doing);
        add_leg(&store, "m-1", "b", &["plan"], None, None);
        leg_done_by_hand(&store);
        edit_legs(&store, "m-1", |old| crate::legs::skip(old, "b", "by hand", None));
        let _ = list(&store, &[(&["m-1"], "claude")]);
        let _ = drained(&TOLD);
        crate::repair::finish_skipped_out(&store);
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Review);
    }

    fn leg_done_by_hand(store: &Store) {
        edit_legs(store, "m-1", |old| {
            let mut v = old.to_vec();
            v[0].state = crate::legs::State::Done;
            Ok(v)
        });
    }

    /// A row staged and spawned by hand has no list to step it: the launch from
    /// `leg done` starts what is ready through the same gate. A list's member is
    /// left to its list.
    #[test]
    fn the_launch_from_leg_done_starts_a_hand_staged_rows_legs_through_the_gate() {
        let (_env, store) = scratch("legs-hand");
        store.set_seat_cap(Some(1)).unwrap();
        fanned_out(&store, "m-1");
        assert_eq!(start_inside(&store, "m-1", &Blind), ["m-1 · a".to_string()], "one place");
        assert_eq!(ids(spawned()), ["m-1 · a".to_string()]);
        assert!(start_inside(&store, "m-1", &Blind).is_empty(), "and once");
        let held = |said: Vec<String>| said.iter().filter(|l| l.contains("m-1 · b waits")).count();
        assert_eq!(held(drained(&SAID)), 1, "the held leg is said once while it lasts, not on every tick");
    }

    #[test]
    fn a_member_of_a_running_list_is_left_to_the_list_by_the_launch_from_leg_done() {
        let (_env, store) = scratch("legs-hand-listed");
        fanned_out(&store, "m-1");
        let _ = list(&store, &[(&["m-1"], "claude")]);
        assert!(start_inside(&store, "m-1", &Blind).is_empty(), "a member of a running list is that list's step's");
        assert!(spawned().is_empty());
    }

    /// `wsp-356`: a parked or held list starts nothing, its members' legs
    /// included. `leg done` on the first of two chained legs starts nothing and
    /// says why in one line, the repair tick leaves the row alone, and resuming
    /// the list starts the next leg through its step.
    #[test]
    fn a_parked_or_held_lists_member_starts_no_legs_until_the_list_is_resumed() {
        for stopped in [WorklistStatus::Parked, WorklistStatus::Held] {
            let (_env, store) = scratch(&format!("legs-stopped-{}", stopped.as_str()));
            staged(&store, "m-1", Status::Doing);
            add_leg(&store, "m-1", "build", &["plan"], None, None);
            let mut w = list(&store, &[(&["m-1"], "claude")]);
            w.set_status(stopped);
            store.save_worklist(&w).unwrap();
            seat_leg_open(&store, "m-1", "plan", "cpd-1");
            leg_done(&store, "m-1", "plan");
            let _ = drained(&SAID);
            let _ = spawned();

            assert!(start_inside(&store, "m-1", &Blind).is_empty(), "{stopped:?}: the launch from leg done starts nothing");
            assert!(spawned().is_empty());
            let said = drained(&SAID);
            let why: Vec<_> = said.iter().filter(|l| l.contains("m-1's legs wait")).collect();
            assert_eq!(why.len(), 1, "{said:?}");
            assert!(why[0].contains(&format!("its list run is {}", stopped.as_str())), "{}", why[0]);
            assert!(legs_owed(&store, &Blind).is_empty(), "{stopped:?}: nor does the repair tick");
            assert!(spawned().is_empty());
            assert_eq!(leg_state(&store, "m-1", "build"), crate::legs::State::Pending);

            w.set_status(WorklistStatus::Running);
            store.save_worklist(&w).unwrap();
            step(&store, &w, &Blind);
            assert_eq!(ids(spawned()), ["m-1 · build".to_string()], "{stopped:?}: resumed, the list starts it");
        }
    }

    /// The repair tick catches a launch that was lost: a begun row with legs ready
    /// and nothing starting them. A row nothing has begun is not started by it.
    #[test]
    fn an_idle_seat_that_handed_its_row_to_legs_holds_no_place_so_a_held_leg_starts_at_the_next_tick() {
        let (_env, store) = scratch("legs-owed-idle-seat");
        store.set_seat_cap(Some(2)).unwrap();
        task(&store, "other", Status::Doing);
        store.set_binding("cpd-8", serde_json::json!({ "task_id": "other" }));
        fanned_out(&store, "begun");
        store.set_binding("cpd-9", serde_json::json!({ "task_id": "begun" }));
        let busy_idle = Fixed::new(&[("cpd-8", State::Working), ("cpd-9", State::Idle)]);
        let _ = spawned();

        let owed = legs_owed(&store, &busy_idle);
        assert_eq!(owed, ["begun · a".to_string()], "the idle first seat holds nothing, so one place is free and not two");
        assert!(legs_owed(&store, &busy_idle).is_empty(), "and the second leg waits for a real place");
        let waiting = store.capped();
        assert!(waiting.get("begun").is_some_and(|v| v.get("begun · b").is_some()), "{waiting:?}");
    }

    #[test]
    fn the_tick_starts_legs_owed_on_a_begun_hand_staged_row_and_not_an_unbegun_one() {
        let (_env, store) = scratch("legs-owed");
        fanned_out(&store, "begun");
        staged(&store, "fresh", Status::Doing);
        let owed = legs_owed(&store, &Blind);
        assert_eq!(owed, ["begun · a".to_string(), "begun · b".to_string()]);
        assert_eq!(ids(spawned()), ["begun · a".to_string(), "begun · b".to_string()]);
    }

    // ---- a member's prerequisite (wsp-495) ---------------------------------------

    /// `m-1` of a group run at `x1` is going when it files a row and rests on it
    /// (`wsp after`), and then releases its seat to wait.
    fn waiting_on_its_prerequisite(tag: &str) -> (util::Isolated, Store, Worklist) {
        let (env, store) = scratch(tag);
        task(&store, "m-1", Status::Todo);
        let mut w = list(&store, &[(&["m-1"], "claude")]);
        let mut g = w.groups();
        g[0].cap = Some(1);
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("m-1".into(), "claude".into())]);
        store.set_claim("m-1", serde_json::json!({ "workspace_id": "cpd-9" }));
        task(&store, "pre", Status::Todo);
        let mut m = store.find_task("m-1").unwrap();
        m.after = vec!["pre".into()];
        store.save_task(&m).unwrap();
        (env, store, w)
    }

    /// The whole route (`wsp-495`): a running member files a row nobody lists and
    /// sets `after`; the run adds it to the member's group, starts it ahead of
    /// the member under the group's `x1` once the member has let its seat go,
    /// holds the member (silent as it is) while it runs, seats it again when it
    /// has landed, and the barrier waits on both.
    #[test]
    fn a_prerequisite_a_running_member_names_is_added_started_ahead_and_the_member_is_seated_again_once_it_lands() {
        let (_env, store, w) = waiting_on_its_prerequisite("prerequisite-route");

        // Still seated: the row joins the group, but the member holds the one place.
        step(&store, &w, &Blind);
        let members = store.worklist("run").unwrap().groups()[0].members.clone();
        assert_eq!(members, vec!["pre", "m-1"], "the row joins the group being run, ahead of the member");
        assert!(spawned().is_empty(), "x1 is held by the member while it has its seat");
        let m = store.find_task("m-1").unwrap();
        assert!(m.section("Log").unwrap_or_default().contains("wsp: prerequisite pre added to run group 1"), "the member's row says who and why");
        let p = store.find_task("pre").unwrap();
        assert!(p.section("Log").unwrap_or_default().contains("m-1 names it with after:"), "and so does the row");
        assert!(
            store.worklist("run").unwrap().section("Log").unwrap_or_default().contains("takes in pre for m-1"),
            "and the list"
        );

        // It lets the seat go: the member is no longer in flight, so the row starts.
        store.clear_claim("m-1");
        left_unseated(&store, "m-1", &[], 900);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("pre".into(), "claude".into())], "started with no person, under x1");
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "nothing starts twice, and the quiet member is not reseated while it waits");
        assert!(!said(&store).contains("m-1 started again"), "{}", said(&store));

        // The row lands: the member is seated again, on the same row.
        let mut p = store.find_task("pre").unwrap();
        p.set_status(Status::Review);
        p.log(&format!("{} 1234567abc on master", crate::repair::LANDED));
        store.save_task(&p).unwrap();
        step(&store, &w, &Blind);
        let started = spawned();
        assert!(started.contains(&("m-1".into(), "claude".into())), "seated again once the prerequisite landed: {started:?}");
        let m = store.find_task("m-1").unwrap();
        assert_eq!(m.status(), Status::Doing);
        assert!(m.section("Log").unwrap_or_default().contains("wsp: prerequisite landed, starting again"));
        step(&store, &w, &Blind);
        assert!(!spawned().iter().any(|(id, _)| id == "m-1"), "and only once");

        // The barrier is owed on both.
        hold_all(&store);
        step(&store, &w, &Blind);
        assert!(checks(&store, 1).is_empty(), "the prerequisite is verified and the member is not: no barrier");
        let mut m = store.find_task("m-1").unwrap();
        m.set_status(Status::Review);
        m.log(&format!("{} 7654321cba on master", crate::repair::LANDED));
        store.save_task(&m).unwrap();
        step(&store, &w, &Blind);
        hold_all(&store);
        step(&store, &w, &Blind);
        assert_eq!(checks(&store, 1).len(), 1, "both landed and verified: the barrier opens");
    }

    /// A row another list holds is not pulled in, a done row owes nothing, and a
    /// row named by a task outside the group is nobody's prerequisite there. The
    /// member's row says which, once.
    #[test]
    fn a_prerequisite_another_list_holds_or_a_non_member_names_is_not_added_and_the_row_says_so() {
        let (_env, store) = scratch("prerequisite-refused");
        task(&store, "m-1", Status::Todo);
        task(&store, "held", Status::Todo);
        task(&store, "fin", Status::Done);
        task(&store, "outside", Status::Todo);
        task(&store, "free", Status::Todo);
        let mut m = store.find_task("m-1").unwrap();
        m.after = vec!["held".into(), "fin".into()];
        store.save_task(&m).unwrap();
        let mut o = store.find_task("outside").unwrap();
        o.after = vec!["free".into()];
        store.save_task(&o).unwrap();
        let other = {
            let mut e = Worklist::new("elsewhere", "elsewhere");
            e.set_status(WorklistStatus::Draft);
            e.set_groups(&[Group { members: vec!["held".into()], agent: "claude".into(), ..Group::default() }]);
            store.save_worklist(&e).unwrap();
            e
        };
        let w = list(&store, &[(&["m-1"], "claude")]);

        step(&store, &w, &Blind);
        step(&store, &w, &Blind);
        assert_eq!(store.worklist("run").unwrap().groups()[0].members, vec!["m-1"], "nothing joined");
        assert_eq!(store.worklist(&other.id).unwrap().groups()[0].members, vec!["held"], "and the other list is as it was");
        let log = store.find_task("m-1").unwrap().section("Log").unwrap_or_default().to_string();
        assert_eq!(log.matches("wsp: not adding held to run group 1: elsewhere holds it").count(), 1, "said once: {log}");
        assert!(!log.contains("fin"), "a done row owes nothing, so nothing is said: {log}");
        assert!(!log.contains("wsp: prerequisite"), "{log}");
    }

    // ---- the drive lease (wsp-467) ------------------------------------------------

    /// `HOME` pointed at a spoke's login for the test, put back however it ends.
    struct Home(Option<std::ffi::OsString>);

    impl Home {
        fn at(home: &std::path::Path) -> Home {
            let was = std::env::var_os("HOME");
            std::env::set_var("HOME", home);
            Home(was)
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            match self.0.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// A served hub with `ed` and `jesper` in `shared:team`; this store a spoke
    /// logged in as `me`; three shared rows in `todo` and the `run` list over
    /// them, shared.
    fn two_spokes(env: &util::Isolated, store: &Store, me: &str) -> (wsp_hub::serve::Running, std::collections::BTreeMap<String, String>, Home) {
        let dir = env.path("hub");
        std::fs::create_dir_all(&dir).unwrap();
        let (running, tokens) = crate::hub_lease::tests::serve_users(&dir, &["ed", "jesper"]);
        let home = Home::at(&crate::hub_lease::tests::login(&dir, &running, &tokens, me));
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            let mut t = Task::new(id, id);
            t.workspace = "shared:team".parse().unwrap();
            store.save_task(&t).unwrap();
        }
        list(store, &[(&["wsp-1", "wsp-2", "wsp-3"], "claude")]);
        let mut w = store.worklist("run").unwrap();
        w.workspace = "shared:team".parse().unwrap();
        store.save_worklist(&w).unwrap();
        (running, tokens, home)
    }

    /// Done-when 1 (wsp-467): Ed's spoke drives a shared list of three rows;
    /// Jesper's spoke leases one; Ed's run spawns the other two and never the
    /// leased one, neither reseats nor verifies it, and the barrier opens once
    /// Jesper's verdict and landing arrive. The drive lease holds Jesper's spoke
    /// off the list, and once it expires a spoke of Ed's takes it again.
    #[test]
    fn drive_a_shared_list_is_driven_by_eds_spoke_and_the_member_jespers_spoke_leased_runs_there() {
        let (env, store) = scratch("drive-two");
        let (running, tokens, _home) = two_spokes(&env, &store, "ed");
        let mut jesper = crate::hub_lease::tests::dial(&running, &tokens, "jesper");
        crate::hub_lease::take(&mut jesper, "shared:team", "wsp-2").unwrap();
        let w = store.worklist("run").unwrap();

        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("wsp-1".into(), "claude".into()), ("wsp-3".into(), "claude".into())], "the two Ed could lease");
        assert_eq!(store.find_task("wsp-2").unwrap().status(), Status::Todo, "never the leased one");
        assert_eq!(crate::hub_lease::running_elsewhere(&store, "wsp-2").as_deref(), Some("jesper"));
        assert_eq!(crate::hub_lease::driven_by(&store, &w).as_deref(), Some("ed"));
        assert!(crate::hub_lease::Book::read(&store).drives.contains_key("run"), "Ed's spoke holds drive:run");
        let why = crate::hub_lease::take(&mut jesper, "shared:team", &crate::hub_lease::drive_row("run")).unwrap_err();
        assert!(why.contains("ed"), "Jesper's spoke cannot take the drive: {why}");
        // Ed's spoke holds the two it started: Jesper cannot take them.
        assert!(crate::hub_lease::take(&mut jesper, "shared:team", "wsp-1").unwrap_err().contains("ed"));

        // Jesper's work arrives through the sync: `doing`, with no seat here.
        set(&store, "wsp-2", Status::Doing);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "not reseated: it runs on Jesper's wsp");

        // Ed's two land and are verified here; Jesper's is not.
        set(&store, "wsp-1", Status::Review);
        set(&store, "wsp-3", Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("wsp-1 · verifying".into(), "claude".into()), ("wsp-3 · verifying".into(), "claude".into())]);
        hold_all(&store);
        step(&store, &w, &Blind);
        assert!(checks(&store, 1).is_empty(), "the barrier waits for Jesper's landing");

        // Jesper's landing arrives, and then the verdict his spoke's verifier wrote.
        set(&store, "wsp-2", Status::Review);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "no verifier on Ed's spoke for Jesper's row");
        assert!(checks(&store, 1).is_empty(), "and no barrier before its verdict");
        // A verdict that blocks is not counted; one that holds is, as pulled (wsp-486).
        let hold = serde_json::json!({ "commit": "abc1234", "verdict": "holds" });
        crate::hub_lease::take_verdict(&store, "wsp-2", &hold);
        crate::hub_lease::take_verdict(&store, "wsp-2", &serde_json::json!({ "commit": "abc1234", "verdict": "blocks" }));
        step(&store, &w, &Blind);
        assert!(checks(&store, 1).is_empty(), "a later blocks clears the hold: the barrier waits");
        crate::hub_lease::take_verdict(&store, "wsp-2", &hold);
        step(&store, &w, &Blind);
        assert_eq!(checks(&store, 1).len(), 1, "the barrier opens");
        assert_eq!(spawned(), vec![("run · barrier 1".into(), "claude".into())]);

        // The drive lease expires on the hub: Ed's spoke lets it go, and the
        // next step (any spoke of Ed's) takes it again with a higher token.
        let token = crate::hub_lease::Book::read(&store).drives["run"].token;
        running.hub().clock().advance(std::time::Duration::from_secs(8 * 86_400));
        let expired = running.hub().expire_leases().unwrap();
        let drive = expired.iter().find(|e| e.row.0 == "drive:run").expect("drive:run expired");
        let payload = serde_json::json!({ "row": drive.row.0, "token": drive.token.0 });
        assert!(crate::hub_lease::follow_expired(&store, &payload));
        assert!(!crate::hub_lease::Book::read(&store).drives.contains_key("run"));
        step(&store, &w, &Blind);
        assert!(crate::hub_lease::Book::read(&store).drives["run"].token > token, "taken again");
        running.stop();
    }

    /// wsp-486: Jesper's spoke, which does not drive Ed's list, opens the
    /// verifier of the member it holds, and of no other.
    #[test]
    fn drive_the_holder_spoke_opens_a_verifier_for_a_member_of_a_list_it_does_not_drive() {
        let (env, store) = scratch("drive-held");
        let (running, _tokens, _home) = two_spokes(&env, &store, "jesper");
        let mut w = store.worklist("run").unwrap();
        w.for_user = Some("ed".into());
        store.save_worklist(&w).unwrap();
        for id in ["wsp-2", "wsp-3"] {
            set(&store, id, Status::Review);
        }
        store.set_claim("wsp-2", serde_json::json!({ "hub_token": 5, "expires": crate::util::iso_at(crate::util::epoch_secs() + 3600) }));
        let before = std::fs::read_to_string(store.worklist_path("run")).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("wsp-2 · verifying".into(), "claude".into())], "only the member held here");
        assert_eq!(std::fs::read_to_string(store.worklist_path("run")).unwrap(), before, "the list untouched");
        running.stop();
    }

    /// Done-when 2 (wsp-467): a spoke without the drive lease never advances
    /// the list — no spawn, no list change — whether another user's spoke holds
    /// the lease or the list is allocated to another user.
    #[test]
    fn drive_a_spoke_without_the_drive_lease_never_advances_the_list() {
        let (env, store) = scratch("drive-none");
        let (running, tokens, _home) = two_spokes(&env, &store, "jesper");
        let mut ed = crate::hub_lease::tests::dial(&running, &tokens, "ed");
        crate::hub_lease::take(&mut ed, "shared:team", &crate::hub_lease::drive_row("run")).unwrap();
        let w = store.worklist("run").unwrap();
        let file = || std::fs::read_to_string(store.worklist_path("run")).unwrap();
        let before = file();

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "nothing spawned");
        assert_eq!(file(), before, "nothing written to the list");
        assert!(["wsp-1", "wsp-2", "wsp-3"].iter().all(|r| store.find_task(r).unwrap().status() == Status::Todo));
        assert_eq!(crate::hub_lease::driven_by(&store, &w).as_deref(), Some("ed"));
        assert!(crate::hub_lease::Book::read(&store).drives.is_empty());

        // Allocated to ed: Jesper's spoke does not even ask the hub.
        let mut w = store.worklist("run").unwrap();
        w.for_user = Some("ed".into());
        store.save_worklist(&w).unwrap();
        let before = file();
        assert_eq!(crate::hub_lease::drive(&store, &w), crate::hub_lease::Drives::Elsewhere("ed".into()));
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
        assert_eq!(file(), before);
        running.stop();
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    /// **wsp-125: under the orders machine a barrier check is told by an
    /// order**, keyed per list, group and seat: the same tell on the next tick
    /// is the same order, another group is another, and nothing runs `wsp tell`.
    #[test]
    fn under_the_orders_machine_a_barrier_check_told_twice_is_one_order() {
        let env = util::isolated("cycle-tell-orders");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let opened = crate::refusals::tests::open(&store, &["cpd-9"]);
        let orders = || opened.0.orders_for(&wsp_store_contract::SeatId("cpd-9".into()), &[]).unwrap().len();
        tell_seat(&store, "cpd-9", "the group moved", "the group moved under it", "barrier:run:2");
        tell_seat(&store, "cpd-9", "the group moved", "the group moved under it", "barrier:run:2");
        assert_eq!(orders(), 1, "one order across two ticks");
        tell_seat(&store, "cpd-9", "the group moved", "the group moved under it", "barrier:run:3");
        assert_eq!(orders(), 2, "another group is another order");
        assert!(tests::MEMBER_TOLD.with(|t| t.borrow().is_empty()), "no `wsp tell` past the orders machine");
    }
}
