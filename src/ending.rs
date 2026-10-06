//! A member whose verification holds has its agent ended. `wsp-193`.
//!
//! Ed, 2026-10-06: once a member's verification is complete and agreed, nothing
//! ended its agent. The member's pane and the verifier's stayed open until
//! somebody closed them by hand — every member of every group, every night.
//!
//! # The trigger, and why it is the verdict and not the row
//!
//! The member is at `review`, still holds its claim, and the newest pass in
//! its `## Verification` section holds, on the commit it last landed. A pass
//! still running, a blocks, a superseded pass, or a holds on a commit a later
//! landing moved past are all "not yet". `review` alone is not enough: a
//! member at `review` with no verdict yet is the agent a blocks verdict sends
//! work back to, and ending it would throw away the context the fix is made
//! from.
//!
//! The verdict is read through [`crate::verification::latest`], `wsp-188`'s
//! record, and nowhere else. The verifier's pane is `wsp-188`'s too:
//! [`crate::cycle::passes_finished`] ends every pass with its verdict in, once,
//! and runs before [`tick`] in the same repair pass — so this module ends the
//! member and only the member. What it lends the verifier's ending is the
//! confirmation: [`verifier_pids`] before the despawn and [`confirm_gone`]
//! after it, read through the same [`Seats`], so a pass is `ended` on the same
//! evidence a member is.
//!
//! # What is never ended, and how each is read
//!
//! - **A seat in a turn, on a prompt, or behind its own open `wsp ask`.** That
//!   is [`crate::waiting::reading`] — the one reading every other reader asks —
//!   plus the plain fact of a turn in flight, which `waiting` deliberately does
//!   not count as waiting. Checked again on the next tick, and said nowhere: it
//!   passes on its own.
//! - **A seat a person has typed to since the verdict.** A turn that began
//!   after the verdict is somebody using the agent, and the agent is theirs to
//!   end now. Read through [`Seats::turn_began_since`], and **a backend that
//!   cannot say holds too** — the cost of holding is a pane left open, the cost
//!   of guessing is a conversation a person was in.
//! - **A governor seat.** Whatever row it is bound to, a seat in
//!   `governors.json` is a post, and ending it is a reseat nobody asked for.
//!
//! The three that will not pass by themselves — typed, governor, and a seat
//! with no pid to confirm by — are said once on the row and in `cycle.log`,
//! so a pane left open has a sentence beside it saying why.
//!
//! # Ended means gone
//!
//! `wsp despawn` closes the seat and releases the claim, and an agent was
//! found alive at `ppid=1` after exactly that, still holding a conversation Ed
//! was in. So the agent's whole process tree is read **before** the despawn —
//! afterwards the parent links are gone — and the ending is recorded only once
//! none of it is running. A seat with no pid to read is not ended at all:
//! nothing could confirm it.
//!
//! The record is one line on the member's `## Log`, [`ENDED`] and the commit.
//! A despawn that fails, or leaves a pid running, is [`NOT_ENDED`] on the row,
//! a line in `cycle.log` and one sentence to the governor — and **not retried
//! on the next tick**, which is `wsp-176`'s shape: a failure tried once a
//! minute is a governor told once a minute about one pane.
//!
//! The row stays at `review`. Ending its agent is not `done`, which is Ed's.

use crate::cycle::Seats;
use crate::model::{Status, Task, Worklist, WorklistStatus};
use crate::place::State;
use crate::store::Store;
use crate::util;

/// The line on a member's row saying its agent was ended, and on which verdict.
pub(crate) const ENDED: &str = "ended: verification holds at";

/// The line on a member's row saying an ending was tried and failed.
const NOT_ENDED: &str = "wsp: could not end";

/// The line on a member's row saying its agent is being left running for a
/// reason that will not pass by itself.
const LEFT: &str = "wsp: verification holds and the agent is left running —";

/// The line on a member's row saying its verifier's pane is left running.
const VERIFIER_LEFT: &str = "wsp: the verifier is left running —";

/// How long a despawned agent's processes are given to exit before the ending
/// is called failed. `compound`'s own `stop` already waits out `TERM` and sends
/// `KILL`, so this is the margin after that, not the whole grace.
const GONE_WITHIN_MS: u64 = if cfg!(test) { 0 } else { 5_000 };

/// A verification that holds and still stands — the trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Holds {
    /// The commit the verifier read, which is what the ending is recorded
    /// against: a later landing is a new verdict, and a new ending.
    pub(crate) commit: String,
    /// When the verdict was recorded. A turn since then is somebody's.
    pub(crate) at: i64,
}

/// The member's newest verification, if it holds and nothing has moved past it.
///
/// **The one place this module reads a verdict.** A pass at `holds` whose
/// `read` is not the member's latest landing was overtaken by one: that landing
/// has not been read, and it is a new verdict that ends the agent, if any does.
/// The time is the pass's `decided`; a pass written before `decided` existed
/// falls back to when it was opened, which only widens what counts as typed.
fn holds(tasks: &[Task], member: &Task) -> Option<Holds> {
    use crate::verification::{latest, same_commit, State as Verdict};
    let p = latest(tasks, &member.id)?;
    if p.state != Verdict::Holds {
        return None;
    }
    let now = crate::repair::landed(member);
    if let (Some(read), Some(now)) = (p.read.as_deref(), now.as_deref()) {
        if !same_commit(read, now) {
            return None;
        }
    }
    let commit = p.read.clone().or(now).unwrap_or_else(|| p.at.clone());
    Some(Holds { commit, at: util::epoch_of(p.decided.as_deref().unwrap_or(&p.at)) })
}

/// Why an agent is left running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Left {
    /// Nobody can read the seat this pass.
    Unreadable,
    /// A turn in flight, or an agent still coming up.
    Turn(State),
    /// On a prompt, or behind its own open `wsp ask`.
    Waiting(crate::waiting::Wait),
    /// A turn began after the verdict.
    Typed,
    /// The backend cannot say whether a turn began after the verdict.
    Untold,
    /// The seat holds a governor post.
    Governor(String),
    /// Nothing names a process to confirm the ending by.
    NoPid,
}

impl Left {
    /// Whether this passes by itself, and is checked again silently next tick.
    fn passes(&self) -> bool {
        matches!(self, Left::Unreadable | Left::Turn(_) | Left::Waiting(_))
    }

    fn sentence(&self) -> String {
        match self {
            Left::Unreadable => "nobody can read its seat".into(),
            Left::Turn(s) => format!("its seat reads {}", s.as_str()),
            Left::Waiting(w) => w.sentence(),
            Left::Typed => "a turn began at its seat after the verdict, so somebody is using it and it is theirs to end".into(),
            Left::Untold => "its seat cannot say whether anybody has typed to it since the verdict".into(),
            Left::Governor(scope) => format!("its seat governs {scope}"),
            Left::NoPid => "nothing names its process, so an ending could not be confirmed".into(),
        }
    }
}

/// What the seat is asked, gathered so [`left`] can be decided without one.
pub(crate) struct Reading {
    pub(crate) state: Option<State>,
    pub(crate) wait: Option<crate::waiting::Wait>,
    pub(crate) governs: Option<String>,
    pub(crate) typed: Option<bool>,
    pub(crate) pids: Option<Vec<u32>>,
}

/// **The decision.** `None` is end it; `Some` is why not.
///
/// The governor first, because it is never ended whatever else is true. A seat
/// that reads `Gone` or `Empty` has no agent left in it to be using: there is
/// nobody to have typed and nothing to confirm, and ending it is releasing the
/// claim and closing the pane.
pub(crate) fn left(r: &Reading) -> Option<Left> {
    if let Some(scope) = &r.governs {
        return Some(Left::Governor(scope.clone()));
    }
    let Some(state) = r.state else { return Some(Left::Unreadable) };
    if let Some(w) = &r.wait {
        return Some(Left::Waiting(w.clone()));
    }
    match state {
        State::Gone | State::Empty => return None,
        State::Idle => {}
        other => return Some(Left::Turn(other)),
    }
    match r.typed {
        Some(true) => return Some(Left::Typed),
        None => return Some(Left::Untold),
        Some(false) => {}
    }
    match &r.pids {
        Some(p) if !p.is_empty() => None,
        _ => Some(Left::NoPid),
    }
}

/// One pass over every member of every open list.
///
/// **Open is running, held or parked**, the population
/// [`crate::cycle::verdicts_recorded`] argues for: a hold stops nothing that
/// has finished, and a held list's verified members were otherwise nobody's.
pub(crate) fn tick(store: &Store, seats: &dyn Seats) {
    let lists: Vec<Worklist> = store
        .worklists()
        .into_iter()
        .filter(|w| matches!(w.status(), WorklistStatus::Running | WorklistStatus::Held | WorklistStatus::Parked))
        .collect();
    let mut tasks = None;
    let mut asks = None;
    for w in &lists {
        for id in w.groups().iter().flat_map(|g| g.members.clone()) {
            let Some(m) = store.find_task(&id) else { continue };
            if m.status() != Status::Review {
                continue;
            }
            let tasks = tasks.get_or_insert_with(|| store.tasks());
            let Some(h) = holds(tasks, &m) else { continue };
            if ended(&m, &h.commit) || failed(&m, &h.commit) {
                continue;
            }
            let asks = asks.get_or_insert_with(|| crate::waiting::Asks::read(store));
            if store.claims().contains_key(&m.id) && end_one(store, seats, w, &m.id, &h, asks) {
                record_ended(store, &m.id, &h.commit);
            }
        }
    }
}

/// Whether this verdict's ending is already recorded on the member.
fn ended(m: &Task, commit: &str) -> bool {
    let line = format!("{ENDED} {commit}");
    m.section("Log").is_some_and(|l| l.lines().any(|x| x.ends_with(&line)))
}

/// Whether ending the member for this verdict has already been tried and
/// failed — the record that makes a failure told once and not retried.
fn failed(m: &Task, commit: &str) -> bool {
    let (who, at) = (format!("{NOT_ENDED} {} (", m.id), format!(" at {commit} — "));
    m.section("Log").is_some_and(|l| l.lines().any(|x| x.contains(&who) && x.contains(&at)))
}

/// End the member's agent if nothing says leave it. `true` when it was ended
/// and confirmed gone.
fn end_one(store: &Store, seats: &dyn Seats, w: &Worklist, member: &str, h: &Holds, asks: &crate::waiting::Asks) -> bool {
    let Some(seat) = store.panes_for_task(member).first().cloned() else {
        // Held, and bound to no seat: nothing here can be asked or confirmed,
        // and `repair`'s gone-member pass is the one that says so.
        return false;
    };
    let state = seats.state(&seat);
    let reading = Reading {
        state,
        wait: crate::waiting::reading(state, asks, &seat, member),
        governs: crate::cmd_govern::governs(&store.governors(), &crate::place::Seat::new(seat.as_str())),
        typed: seats.turn_began_since(&seat, h.at),
        pids: seats.pids(&seat),
    };
    if let Some(why) = left(&reading) {
        if !why.passes() {
            say_left(store, w, member, &seat, &why, &h.commit);
        }
        return false;
    }
    let pids = reading.pids.unwrap_or_default();
    let outcome = crate::cycle::despawned(store, member).and_then(|()| confirm_gone(seats, &pids));
    match outcome {
        Ok(()) => {
            crate::cycle::log_line(store, &format!(
                "{} {member} ({seat}) ended — verification holds at {}, and no pid of it is running",
                w.id, h.commit
            ));
            true
        }
        Err(why) => {
            not_ended(store, w, member, &seat, &h.commit, &why);
            false
        }
    }
}

/// `Ok` once none of `pids` is running, waiting [`GONE_WITHIN_MS`] at most.
/// The member's check, and the verifier's in [`crate::cycle::end_passes`].
pub(crate) fn confirm_gone(seats: &dyn Seats, pids: &[u32]) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(GONE_WITHIN_MS);
    loop {
        let running = seats.running(pids);
        if running.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let list = running.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ");
            return Err(format!("despawned, and pid {list} is still running"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// A verifier's seat, read before [`crate::cycle::end_passes`] closes it: the
/// pids its ending is confirmed by, or why it is left.
///
/// **The member's rule, less what does not apply to a verifier.** A seat that
/// reads `Gone` or `Empty` has nothing in it to confirm; one nobody can read is
/// left silently and asked again next tick; one with no pid is [`Left::NoPid`],
/// left and said, because closing a pane that nothing confirms is how the
/// `ppid=1` survivor became invisible. No turn, prompt or typed check: a
/// verifier is ended on its own verdict, or because a later landing made its
/// reading worthless, and both stand whatever its screen shows.
pub(crate) fn verifier_pids(seats: &dyn Seats, seat: &str) -> Result<Vec<u32>, Left> {
    if let Some(pids) = seats.pids(seat).filter(|p| !p.is_empty()) {
        return Ok(pids);
    }
    match seats.state(seat) {
        Some(State::Gone | State::Empty) => Ok(Vec::new()),
        None => Err(Left::Unreadable),
        Some(_) => Err(Left::NoPid),
    }
}

/// A verifier left running for a reason that will not pass: said once per
/// pass, on the member's row, in `cycle.log`, and to the run's governor.
pub(crate) fn say_verifier_left(store: &Store, w: Option<&Worklist>, member: &str, seat: &str, opened: &str, why: &Left) {
    if why.passes() {
        return;
    }
    let line = format!("{VERIFIER_LEFT} {member} ({seat}, opened {opened}): {}", why.sentence());
    if !once(store, member, &line) {
        return;
    }
    let by = w.map_or(member, |w| w.id.as_str());
    crate::cycle::log_line(store, &format!("{by} {member}: {}", line.trim_start_matches("wsp: ")));
    if let Some(w) = w {
        crate::cycle::tell(store, w, &format!(
            "The verifier of {member} in {seat} is left running: {}. \
             `wsp despawn --pane {seat}` is the hand version.",
            why.sentence()
        ));
    }
}

/// The ending, on the member's row, once — re-read under the lock so two
/// passes racing write it once.
fn record_ended(store: &Store, id: &str, commit: &str) {
    let line = format!("{ENDED} {commit}");
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        if ended(&t, commit) {
            return false;
        }
        t.log(&line);
        t.touch();
        store.save_task(&t).is_ok()
    });
}

/// A failed ending: on the row, in `cycle.log`, and to the governor — once,
/// because the row's line is what the next pass reads as "already tried".
fn not_ended(store: &Store, w: &Worklist, member: &str, seat: &str, commit: &str, why: &str) {
    let line = format!("{NOT_ENDED} {member} ({seat}) at {commit} — {}", util::truncate(why, 160));
    if !once(store, member, &line) {
        return;
    }
    crate::cycle::log_line(store, &format!("{} {member}: {}", w.id, line.trim_start_matches("wsp: ")));
    crate::cycle::tell(store, w, &format!(
        "{member}'s verification holds at {commit}, and ending its agent ({seat}) failed: {}. \
         It will not be tried again for this verdict; `wsp despawn {member}` is the hand version.",
        util::truncate(why, 160)
    ));
}

/// An agent left running for a reason that will not pass: said once per
/// reason per verdict.
fn say_left(store: &Store, w: &Worklist, member: &str, seat: &str, why: &Left, commit: &str) {
    let line = format!("{LEFT} {member} ({seat}) at {commit}: {}", why.sentence());
    if !once(store, member, &line) {
        return;
    }
    crate::cycle::log_line(store, &format!("{} {member}: {}", w.id, line.trim_start_matches("wsp: ")));
    crate::cycle::tell(store, w, &format!(
        "{member}'s verification holds at {commit}, and its agent ({seat}) is left running: {}.",
        why.sentence()
    ));
}

/// Write `line` on the row unless it is already there; whether it was written.
fn once(store: &Store, id: &str, line: &str) -> bool {
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        if t.section("Log").is_some_and(|l| l.lines().any(|x| x.ends_with(line))) {
            return false;
        }
        t.log(line);
        t.touch();
        store.save_task(&t).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cycle::tests::{DESPAWN_FAILS, ENDED as DESPAWNED, SAID, TOLD};
    use crate::model::Group;
    use crate::verification::Ending;
    use std::collections::BTreeMap;

    /// Seats scripted per seat: what each reads, whether a turn began since the
    /// verdict, its pids — and which pids are still running after an ending.
    #[derive(Default)]
    struct Fake {
        state: BTreeMap<String, State>,
        typed: BTreeMap<String, bool>,
        pids: BTreeMap<String, Vec<u32>>,
        survives: Vec<u32>,
    }

    impl Fake {
        /// `cpd-1` the member and `cpd-2` the verifier, both idle, nobody has
        /// typed to either, each with a pid that exits when it is ended.
        fn at_rest() -> Fake {
            let mut f = Fake::default();
            for (seat, pid) in [("cpd-1", 101), ("cpd-2", 202)] {
                f.state.insert(seat.into(), State::Idle);
                f.typed.insert(seat.into(), false);
                f.pids.insert(seat.into(), vec![pid, pid + 1]);
            }
            f
        }

        fn with(mut self, seat: &str, s: State) -> Fake {
            self.state.insert(seat.into(), s);
            self
        }
    }

    impl Seats for Fake {
        fn state(&self, seat: &str) -> Option<State> {
            self.state.get(seat).copied()
        }
        fn turn_began_since(&self, seat: &str, _since: i64) -> Option<bool> {
            self.typed.get(seat).copied()
        }
        fn pids(&self, seat: &str) -> Option<Vec<u32>> {
            self.pids.get(seat).cloned()
        }
        fn running(&self, pids: &[u32]) -> Vec<u32> {
            pids.iter().copied().filter(|p| self.survives.contains(p)).collect()
        }
    }

    fn drain<T>(cell: &'static std::thread::LocalKey<std::cell::RefCell<Vec<T>>>) -> Vec<T> {
        cell.with(|c| c.borrow_mut().drain(..).collect())
    }

    /// A running list whose one member `m-1` landed `abc1234` and is at
    /// `review`, claimed on `cpd-1`, with one pass in its `## Verification`
    /// that read that commit and holds — its verifier still standing in
    /// `cpd-2`, as `wsp-188` leaves it until the next tick.
    fn verified(tag: &str) -> (util::Isolated, Store) {
        drain(&DESPAWNED);
        drain(&SAID);
        drain(&TOLD);
        DESPAWN_FAILS.with(|f| *f.borrow_mut() = None);
        let env = util::isolated(&format!("ending-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut m = Task::new("m-1", "m-1");
        m.set_status(Status::Review);
        m.log(&format!("{} abc1234", crate::repair::LANDED));
        store.save_task(&m).unwrap();
        set_pass(&store, crate::verification::State::Holds);
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group { members: vec!["m-1".into()], agent: "claude".into(), ..Group::default() }]);
        store.save_worklist(&w).unwrap();
        store.set_claim("m-1", serde_json::json!({ "workspace": "cpd-1" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        (env, store)
    }

    /// `m-1`'s one pass, at `state`.
    fn set_pass(store: &Store, state: crate::verification::State) {
        let mut m = store.find_task("m-1").unwrap();
        let mut p = crate::verification::Pass::opened(Some("abc1234".into()), None);
        p.at = "2026-10-06T09:00:00Z".into();
        p.pane = Some("cpd-2".into());
        p.state = state;
        p.decided = (state != crate::verification::State::Running).then(|| "2026-10-06T09:30:00Z".into());
        crate::verification::write(&mut m, &[p]);
        store.save_task(&m).unwrap();
    }

    /// One repair pass, in its order: `wsp-188` ends the verifier seats whose
    /// verdict is in, and then this module ends the members.
    fn pass(store: &Store, seats: &Fake) {
        crate::cycle::passes_finished(store, seats);
        tick(store, seats);
    }

    fn log_of(store: &Store, id: &str) -> String {
        store.find_task(id).unwrap().section("Log").unwrap_or_default().to_string()
    }

    fn member_ended() -> bool {
        drain(&DESPAWNED).contains(&"m-1".to_string())
    }

    #[test]
    fn a_holds_verdict_ends_the_member_and_the_verifier_exactly_once() {
        let (_env, store) = verified("holds");
        let seats = Fake::at_rest();
        pass(&store, &seats);
        assert_eq!(drain(&DESPAWNED), vec!["cpd-2".to_string(), "m-1".to_string()], "the verifier's pane and the member's agent");
        assert_eq!(verifier_ending(&store), Ending::Ended, "pids 202 and 203 read before, and neither running after");
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches("ended: verification holds at abc1234").count(), 1, "{log}");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Review, "ending its agent is not done, which is Ed's");

        // `wsp despawn` would have released the claim; a second pass that
        // still finds it — a despawn that lied — ends nothing a second time.
        pass(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "the row's record and the pass's are what make it once");
        assert_eq!(log_of(&store, "m-1").matches("ended: verification holds").count(), 1);
    }

    #[test]
    fn a_blocks_verdict_leaves_the_member_alone() {
        let (_env, store) = verified("blocks");
        set_pass(&store, crate::verification::State::Blocks);
        pass(&store, &Fake::at_rest());
        assert!(!member_ended(), "the member is where a fix is made from");
    }

    #[test]
    fn a_verdict_a_later_landing_superseded_ends_nothing() {
        let (_env, store) = verified("superseded");
        let mut m = store.find_task("m-1").unwrap();
        m.log(&format!("{} def5678", crate::repair::LANDED));
        store.save_task(&m).unwrap();
        pass(&store, &Fake::at_rest());
        assert!(!member_ended(), "abc1234 held, and def5678 has not been read");

        set_pass(&store, crate::verification::State::Superseded);
        pass(&store, &Fake::at_rest());
        assert!(!member_ended(), "a pass wsp-188 marked superseded is no verdict at all");
    }

    #[test]
    fn a_verifier_still_running_ends_nothing() {
        let (_env, store) = verified("running");
        set_pass(&store, crate::verification::State::Running);
        pass(&store, &Fake::at_rest());
        assert!(drain(&DESPAWNED).is_empty());
    }

    /// "Typed since the verdict" is measured from when the verdict was
    /// recorded: a turn between opening the pass and its verdict is the member
    /// answering its own verifier, not a person picking the agent up.
    #[test]
    fn typed_since_is_measured_from_the_verdict_not_from_the_opening() {
        let (_env, store) = verified("decided");
        let m = store.find_task("m-1").unwrap();
        let h = holds(&store.tasks(), &m).unwrap();
        assert_eq!(h.at, util::epoch_of("2026-10-06T09:30:00Z"));
        assert_eq!(h.commit, "abc1234");
    }

    #[test]
    fn a_member_mid_turn_is_left_and_ended_on_the_tick_after_it_settles() {
        let (_env, store) = verified("turn");
        pass(&store, &Fake::at_rest().with("cpd-1", State::Working));
        assert_eq!(drain(&DESPAWNED), vec!["cpd-2".to_string()], "only the verifier, whose turn is over");
        assert!(drain(&TOLD).is_empty(), "a turn passes by itself and nobody is told about it");
        pass(&store, &Fake::at_rest());
        assert_eq!(drain(&DESPAWNED), vec!["m-1".to_string()], "the verifier was ended once, on the tick before");
    }

    #[test]
    fn a_member_on_a_prompt_is_left_alone() {
        let (_env, store) = verified("prompt");
        tick(&store, &Fake::at_rest().with("cpd-1", State::Blocked));
        assert!(!member_ended(), "a dialog only a person can answer");
    }

    #[test]
    fn a_member_behind_its_own_open_ask_is_left_alone() {
        let (_env, store) = verified("ask");
        let q = crate::message::Message::question(
            crate::message::Party::pane("cpd-1", ""),
            crate::message::Kind::Note,
            "which of the two?",
            crate::message::Waiting::new("cpd-1", "m-1"),
        )
        .about(crate::message::About::Task("m-1".into()));
        store.save_message(&q).unwrap();
        let asks = crate::waiting::Asks::read(&store);
        assert!(asks.of("cpd-1", "m-1").is_some(), "the fixture is an open ask the one reading sees");
        tick(&store, &Fake::at_rest());
        assert!(!member_ended(), "idle on screen, and waiting on an answer");
    }

    #[test]
    fn a_seat_somebody_typed_to_since_the_verdict_is_left_and_said_once() {
        let (_env, store) = verified("typed");
        let mut seats = Fake::at_rest();
        seats.typed.insert("cpd-1".into(), true);
        tick(&store, &seats);
        tick(&store, &seats);
        assert!(!member_ended());
        let told = drain(&TOLD);
        assert_eq!(told.len(), 1, "said once, not every minute: {told:?}");
        assert!(told[0].contains("theirs to end"), "{}", told[0]);
    }

    #[test]
    fn a_seat_that_cannot_say_whether_it_was_typed_to_is_left() {
        let (_env, store) = verified("untold");
        let mut seats = Fake::at_rest();
        seats.typed.remove("cpd-1");
        tick(&store, &seats);
        assert!(!member_ended(), "holding costs a pane, guessing costs a conversation");
    }

    #[test]
    fn a_governor_seat_is_never_ended_whatever_row_it_is_bound_to() {
        let (_env, store) = verified("governor");
        store.set_governor("run", serde_json::json!({ "workspace": "cpd-1", "pane": "cpd-1" }));
        tick(&store, &Fake::at_rest());
        assert!(!member_ended());
    }

    #[test]
    fn a_despawn_that_fails_is_logged_and_told_once_and_not_retried() {
        let (_env, store) = verified("fails");
        DESPAWN_FAILS.with(|f| *f.borrow_mut() = Some("no such seat".into()));
        let seats = Fake::at_rest();
        tick(&store, &seats);
        tick(&store, &seats);
        tick(&store, &seats);
        let told = drain(&TOLD);
        assert_eq!(told.iter().filter(|t| t.contains("ending its agent")).count(), 1, "{told:?}");
        assert!(drain(&SAID).iter().any(|l| l.contains("could not end m-1") && l.contains("no such seat")));
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches("could not end m-1").count(), 1, "{log}");
        assert!(!log.contains("ended: verification holds"), "a failure is not an ending");
    }

    #[test]
    fn an_ending_is_recorded_only_once_no_pid_of_the_agent_is_running() {
        let (_env, store) = verified("pid");
        let mut seats = Fake::at_rest();
        seats.survives = vec![102];
        tick(&store, &seats);
        let log = log_of(&store, "m-1");
        assert!(!log.contains("ended: verification holds"), "the seat closed and pid 102 did not: {log}");
        assert!(log.contains("pid 102 is still running"), "{log}");
        assert_eq!(drain(&TOLD).iter().filter(|t| t.contains("102")).count(), 1);
    }

    fn verifier_ending(store: &Store) -> Ending {
        crate::verification::passes(&store.find_task("m-1").unwrap())[0].ending.clone()
    }

    #[test]
    fn a_verifier_whose_pid_outlives_its_pane_is_a_failed_ending_told_once() {
        let (_env, store) = verified("vpid");
        let mut seats = Fake::at_rest();
        seats.survives = vec![203];
        pass(&store, &seats);
        assert!(
            matches!(verifier_ending(&store), Ending::Failed(why) if why.contains("pid 203 is still running")),
            "the pane closed and 203 did not: that is not an ending"
        );
        assert_eq!(drain(&TOLD).iter().filter(|t| t.contains("verifier of m-1") && t.contains("203")).count(), 1);
        drain(&DESPAWNED);
        pass(&store, &seats);
        assert!(!drain(&DESPAWNED).contains(&"cpd-2".to_string()), "a failure is not tried again every tick");
        assert!(drain(&TOLD).iter().all(|t| !t.contains("verifier of m-1")), "nor told again");
    }

    #[test]
    fn a_verifier_with_no_pid_to_confirm_by_is_left_and_said_once() {
        let (_env, store) = verified("vnopid");
        let mut seats = Fake::at_rest();
        seats.pids.remove("cpd-2");
        pass(&store, &seats);
        assert!(!drain(&DESPAWNED).contains(&"cpd-2".to_string()), "nothing could confirm it, so it is not closed");
        assert_eq!(verifier_ending(&store), Ending::Standing, "and stays standing to be asked again");
        assert_eq!(drain(&TOLD).iter().filter(|t| t.contains("verifier of m-1") && t.contains("left running")).count(), 1);
        pass(&store, &seats);
        assert!(drain(&TOLD).iter().all(|t| !t.contains("verifier of m-1")), "said once, not every tick");
        assert_eq!(log_of(&store, "m-1").matches("the verifier is left running").count(), 1);

        seats.state.insert("cpd-2".into(), State::Gone);
        pass(&store, &seats);
        assert!(drain(&DESPAWNED).contains(&"cpd-2".to_string()), "a seat with nobody left in it has nothing to confirm");
        assert_eq!(verifier_ending(&store), Ending::Ended);
    }

    #[test]
    fn a_seat_with_no_pid_to_confirm_by_is_not_ended() {
        let (_env, store) = verified("nopid");
        let mut seats = Fake::at_rest();
        seats.pids.remove("cpd-1");
        tick(&store, &seats);
        assert!(!member_ended());
    }

    #[test]
    fn a_member_whose_agent_already_exited_is_released_and_recorded() {
        let (_env, store) = verified("gone");
        let mut seats = Fake::at_rest().with("cpd-1", State::Gone);
        seats.pids.remove("cpd-1");
        seats.typed.remove("cpd-1");
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).contains(&"m-1".to_string()), "nobody is left in it to be using it");
        assert!(log_of(&store, "m-1").contains("ended: verification holds at abc1234"));
    }

    #[test]
    fn the_process_tree_is_read_with_its_root_even_when_ps_lists_nothing_under_it() {
        let me = std::process::id();
        assert!(crate::place_super::tree_of(me).contains(&me));
    }

    /// The case the pid check exists for, with real processes: the root of a
    /// seat exits, the agent under it does not, and what was read before the
    /// ending still names the survivor — which is now at `ppid=1`.
    #[test]
    fn a_child_that_outlives_its_root_is_still_named_and_still_running() {
        use std::os::unix::process::CommandExt;
        let mut root = std::process::Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(root.stdout.take().unwrap()), &mut line).unwrap();
        let child: u32 = line.trim().parse().unwrap();
        let tree = crate::place_super::tree_of(root.id());
        assert!(tree.contains(&root.id()) && tree.contains(&child), "{tree:?} names {child}");

        root.kill().unwrap();
        root.wait().unwrap();
        let after = crate::place_super::tree_of(root.id());
        let running = crate::cycle::Fleet.running(&tree);
        // By pid, never by pattern — this test's own sleep and nothing else.
        let _ = std::process::Command::new("kill").arg(child.to_string()).status();
        assert!(after.contains(&child), "still in the group with its parent gone: {after:?}");
        assert_eq!(running, vec![child], "the seat's root is gone and its agent is not");
    }
}
