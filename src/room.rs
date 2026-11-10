//! How many agents are working on this seat, against the number it will bear:
//! `wsp-231`.
//!
//! Ed, 2026-10-07: **at most four working agents at once on this laptop, and a
//! governor does not count toward the four.** gpui-migration's group 10 had
//! seven members and no `xN`, the run started all seven, and with two
//! verifiers and an idle barrier seat on top the load average was 182 on 8
//! cores. Ed believed a cap was in place. It never was: `cycle` passed no
//! machine number to [`crate::model::Group::parallelism`], the seat had no
//! record to carry one, and a group's `xN` counts that group's members and
//! nothing else.
//!
//! # What counts
//!
//! Every agent wsp started on this machine and has a record of — a member or a
//! `wsp spawn`ed helper through its claim or its binding, a verifier through
//! its pass, a barrier check through its entry — across every list and every
//! project, plus a start written down and not yet seated. A verifier counts
//! because it builds and runs tests like a member does.
//!
//! **A governor does not**, nor a seat that is gone, nor one whose work is
//! finished and is not seen working: a member at `review`, a pass with its
//! verdict, a check that has settled. Finished work that a screen still shows
//! working is counted — that is a pane still building — and finished work
//! nobody can read is not, because a record the run has not yet ended would
//! otherwise hold the cap for as long as it stood.
//!
//! **A seat whose verdict is recorded is not counted at all** (`wsp-385`), seen
//! working or not: a verifier with its verdict, a check that has settled, and a
//! check still open on a barrier that was passed without it (a second one, seated
//! while the first settled). It is closing, and the tick ends it by its pane.
//!
//! **A seat that holds nothing is not counted unless it is seen working**
//! (`wsp-353`): the seat of a row run as legs or pieces, which handed the row to
//! them (the legs' and pieces' own seats are counted as theirs), and any seat of a member of a
//! parked list. Each sat in the cap for ten minutes at `compounding-048`
//! while the starts it blocked waited.
//!
//! **A seat whose send-back it did not take is not counted unless it is seen
//! working** (`compounding-285`): the row is owed work, but the seat cannot take
//! it and the run ends it and starts the row afresh, so it holds no place a queued
//! start is waiting for. 207's idle seat held one for 42 minutes at retro-1.
//!
//! **Unfinished work nobody can read is counted.** A backend that does not
//! answer is the moment the machine is least visible, and the direction to be
//! wrong in there is starting one agent fewer.
//!
//! # Under the lock
//!
//! The run's three starts each write their record under the store lock — the
//! member moved to `doing`, the pass written, the check opened — and the count
//! is taken in the same section, through [`Gate::admits`], so two advances
//! racing at a pass cannot both take the last place. The seats' states are read
//! *before* the lock, because asking a backend is not a thing to do while every
//! wsp verb on the machine waits: a seat that appears in between is read as
//! unknown, and counted.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::cycle::Seats;
use crate::model::{Status, Task, Worklist};
use crate::place::State;
use crate::store::Store;
use crate::util;

/// How long a start written down may go unseated before it stops counting: the
/// run's own [`crate::cycle`] ten minutes, after which it is taken again.
const WEDGED_AFTER: i64 = 10 * 60;

/// How long a seat may sit in its own open `wsp ask --person` before it holds no
/// place under the cap (`wsp-495`): a question that may wait all night is the
/// person's, and the seat holding it open in a tool call was costing a start.
/// The ask stays open, the seat stays waiting and is never nudged or ended; it
/// only stops counting as an agent working.
pub(crate) const HELD_ASK_AFTER: i64 = 10 * 60;

/// How a seat reads, for this count and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    /// Positively not there, or there with nothing running in it.
    Gone,
    Working,
    /// An agent still coming up in it. Counted while it has had time to, and
    /// not after ([`crate::barrier::NOT_UP_AFTER`]): a seat reading this that
    /// long is a start that never took, `wsp-335`.
    Starting,
    /// Idle, on a prompt: there, and not seen working.
    Quiet,
    /// Nobody can say.
    Unknown,
}

impl Look {
    pub(crate) fn of(seats: &dyn Seats, seat: &str) -> Look {
        if seats.absent(seat) {
            return Look::Gone;
        }
        Look::of_state(seats.state(seat))
    }

    fn of_state(state: Option<State>) -> Look {
        match state {
            Some(State::Gone | State::Empty) => Look::Gone,
            Some(State::Working) => Look::Working,
            Some(State::Starting) => Look::Starting,
            Some(State::Unknown) | None => Look::Unknown,
            Some(_) => Look::Quiet,
        }
    }

    /// Whether an agent in a seat reading this, on work `finished` or not, is
    /// one of the machine's working agents. See the module docs.
    fn counts(self, finished: bool) -> bool {
        match self {
            Look::Gone => false,
            Look::Working => true,
            Look::Starting | Look::Quiet | Look::Unknown => !finished,
        }
    }
}

/// One agent the count found: its seat, when it has one yet, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Counted {
    pub seat: Option<String>,
    pub what: String,
}

impl Counted {
    pub(crate) fn said(&self) -> String {
        match &self.seat {
            Some(s) => format!("{} in {s}", self.what),
            None => format!("{} starting", self.what),
        }
    }
}

/// The records the count is read off, gathered once.
pub(crate) struct Records {
    pub tasks: Vec<Task>,
    pub lists: Vec<Worklist>,
    pub bindings: BTreeMap<String, Value>,
    pub claims: BTreeMap<String, Value>,
    pub agents: BTreeMap<String, Value>,
    pub governors: BTreeMap<String, Value>,
    /// The panes and tasks of seats standing in an open ask to the person older
    /// than [`HELD_ASK_AFTER`].
    pub held_asks: BTreeSet<String>,
}

/// The panes and tasks waiting on a person's answer for longer than `after`
/// seconds, off the open questions.
fn held_asks(store: &Store, after: i64) -> BTreeSet<String> {
    let now = util::epoch_secs();
    let mut out = BTreeSet::new();
    for m in store.messages().into_values() {
        if !(m.wants_answering() && !m.is_reply() && m.for_person) || now - util::epoch_of(&m.at) < after {
            continue;
        }
        if let Some(w) = &m.waiting {
            out.extend([w.pane.clone(), w.task.clone()].into_iter().filter(|x| !x.is_empty()));
        }
    }
    out
}

impl Records {
    pub(crate) fn read(store: &Store) -> Records {
        Records {
            held_asks: held_asks(store, HELD_ASK_AFTER),
            tasks: store.tasks(),
            lists: store.worklists(),
            bindings: store.bindings(),
            claims: store.claims(),
            agents: store.agents_held(),
            governors: store.governors(),
        }
    }

    /// Every seat on this machine the count might ask about: the ones whose
    /// state [`Gate::new`] reads before the lock.
    ///
    /// **The claimed agents' seats, and not every agent's**: `agents.json`
    /// keeps every agent ever minted — 514 on this machine on 2026-10-14 — and
    /// a backend asked about each would be a reading per seat that ever was.
    fn seats(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self.bindings.keys().cloned().collect();
        out.extend(self.claimed().into_iter().map(|(seat, _)| seat));
        out.extend(crate::verification::seats(&self.tasks).into_keys());
        out.extend(crate::barrier::seats(&self.lists).into_keys());
        out.retain(|s| here(s));
        out
    }
}

impl Records {
    /// (seat, task) for every claim, through the agent that made it.
    fn claimed(&self) -> Vec<(String, String)> {
        self.claims
            .iter()
            .filter_map(|(id, c)| {
                let agent = self.agents.get(c.get("agent_id")?.as_str()?)?;
                Some((agent.get("seat")?.as_str()?.to_string(), id.clone()))
            })
            .collect()
    }
}

/// On this machine: a seat qualified `@machine` is that machine's to count.
fn here(seat: &str) -> bool {
    crate::herdr::host_of(seat).is_none()
}

/// A member of a list somebody parked: nothing of it starts or is read until
/// the list is resumed, so a seat of its that is not seen working holds no place.
fn in_parked_list(r: &Records, task: &str) -> bool {
    crate::worklist::open_list_of(&r.lists, task).is_some_and(|w| w.status() == crate::model::WorklistStatus::Parked)
}

/// Every agent working on this machine, by the rules in the module docs.
/// `look` answers for a seat; the caller decides how it was read.
pub(crate) fn working(r: &Records, look: &dyn Fn(&str) -> Look) -> Vec<Counted> {
    let now = util::epoch_secs();
    let fresh = |at: &str| now - util::epoch_of(at) <= WEDGED_AFTER;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let governor = |seat: &str| crate::cmd_agent::governs_seat(&r.governors, seat);
    // `since` is when the record opened, where one says: a seat still reading
    // `starting` that long after is not an agent that is coming up.
    // **`settled`: a verdict is recorded** (`wsp-385`) — a verifier's, or a check's
    // (its own, or its barrier's having passed). Such a seat is closing and holds
    // no place whatever its screen shows; the tick ends it by its pane.
    let mut take = |out: &mut Vec<Counted>, seat: Option<&str>, what: String, finished: bool, settled: bool, since: Option<&str>| {
        if let Some(s) = seat {
            if !here(s) || governor(s) || r.held_asks.contains(s) || !seen.insert(s.to_string()) || settled || !look(s).counts(finished) {
                return;
            }
            let never_up = look(s) == Look::Starting && since.is_some_and(|at| now - util::epoch_of(at) > crate::barrier::NOT_UP_AFTER);
            if never_up {
                return;
            }
        }
        out.push(Counted { seat: seat.map(str::to_string), what });
    };

    // A verifier, by its pass on the member.
    for t in r.tasks.iter().filter(|t| t.body.contains("## Verification")) {
        for p in crate::verification::passes(t) {
            let what = crate::verification::seat_label(&t.id);
            let running = p.state == crate::verification::State::Running;
            match &p.pane {
                Some(_) if !p.standing() => {}
                Some(seat) => take(&mut out, Some(seat), what, !running, !running, Some(&p.at)),
                None if running && p.ending == crate::verification::Ending::Standing && fresh(&p.at) => {
                    take(&mut out, None, what, false, false, None)
                }
                None => {}
            }
        }
    }

    // A barrier check, by its entry on the group.
    for w in &r.lists {
        let groups = w.groups();
        for (i, g) in groups.iter().enumerate() {
            let moot = crate::barrier::moot(w, &groups, i);
            for c in &g.checks {
                let what = crate::barrier::seat_label(&w.id, i + 1);
                match (c.standing(), &c.pane) {
                    (Some(seat), _) => take(&mut out, Some(seat), what, !c.open() || moot, !c.open() || moot, Some(&c.at)),
                    (None, None) if c.open() && fresh(&c.at) => take(&mut out, None, what, false, false, None),
                    _ => {}
                }
            }
        }
    }

    // A leg, by the seat on its entry: its pane is bound to the row, so the
    // binding below would count it too, once and as the row; read here first it
    // is counted as the leg, and a seat counted is not counted twice. A leg
    // that has finished is counted only while its seat is seen working.
    for t in r.tasks.iter().filter(|t| t.body.contains("## Legs")) {
        let parked = in_parked_list(r, &t.id);
        for l in crate::legs::legs(t) {
            let what = crate::legs::label(&t.id, &l.name);
            let running = l.state == crate::legs::State::Running;
            match &l.pane {
                Some(seat) => take(&mut out, Some(seat), what, parked || !running || l.ending != crate::verification::Ending::Standing, false, l.started.as_deref()),
                None if running && l.started.as_deref().is_some_and(fresh) => take(&mut out, None, what, false, false, None),
                None => {}
            }
        }
    }

    // A piece, the same: by the seat on its entry, a start not yet seated
    // counted while it is fresh, and a done piece only while its seat is seen
    // working (`docs/pieces.md`, *The run*, "Places").
    for t in r.tasks.iter().filter(|t| t.body.contains("## Pieces")) {
        let parked = in_parked_list(r, &t.id);
        for p in crate::pieces::record(t).pieces {
            let what = p.id(&t.id);
            let running = p.state == crate::pieces::State::Running;
            match &p.pane {
                Some(seat) => take(&mut out, Some(seat), what, parked || !running || p.ending != crate::verification::Ending::Standing, false, p.started.as_deref()),
                None if running && p.started.as_deref().is_some_and(fresh) => take(&mut out, None, what, false, false, None),
                None => {}
            }
        }
    }

    // A member or a helper: the seat its binding names, and the seat of the
    // agent its claim names — a binding is free to be lost and a claim is not.
    let status = |id: &str| r.tasks.iter().find(|t| t.id == id).map(|t| t.status());
    let finished = |s: Status| matches!(s, Status::Review | Status::Done);
    let mut held: Vec<(String, String)> = r
        .bindings
        .iter()
        .filter_map(|(seat, b)| Some((seat.clone(), b.get("task_id")?.as_str()?.to_string())))
        .collect();
    held.extend(r.claimed());
    for (seat, id) in held {
        let Some(s) = status(&id) else { continue };
        // **A sent-back row whose seat took no tell holds nothing owed to the cap**
        // (`compounding-285`): the run is about to end that seat and start the row
        // on a fresh one, and until it does the seat counts only if seen working.
        let holds_nothing = in_parked_list(r, &id)
            || r.tasks.iter().any(|t| t.id == id && (crate::inner::run_inside(t) || crate::repair::undelivered_reason(t).is_some()));
        take(&mut out, Some(&seat), id, finished(s) || holds_nothing, false, None);
    }

    // A member the run moved to `doing` whose agent has not claimed it yet:
    // the start in flight that two racing advances would otherwise both miss.
    for t in &r.tasks {
        let ours = t
            .section("Log")
            .and_then(|l| l.lines().last().map(|x| x.contains(crate::cycle::STARTED_BY)))
            .unwrap_or(false);
        if t.status() == Status::Doing && ours && !r.claims.contains_key(&t.id) && fresh(&t.updated) {
            take(&mut out, None, t.id.clone(), false, false, None);
        }
    }
    out
}

/// The seat's cap and the reading it is held against, for the run's starts.
///
/// **No cap is no work**: [`Gate::new`] reads nothing past the number, and
/// [`Gate::admits`] is `true` — so a seat nobody has given a number runs
/// exactly as it did before there was one.
pub(crate) struct Gate<'a> {
    store: &'a Store,
    pub cap: Option<usize>,
    looks: BTreeMap<String, Look>,
    /// What was turned away this pass, and who was in the room when it was.
    held: RefCell<Vec<(String, Vec<Counted>)>>,
}

impl<'a> Gate<'a> {
    pub(crate) fn new(store: &'a Store, seats: &dyn Seats) -> Gate<'a> {
        let cap = store.seat_cap();
        let looks = match cap {
            None => BTreeMap::new(),
            Some(_) => Records::read(store).seats().into_iter().map(|s| (s.clone(), Look::of(seats, &s))).collect(),
        };
        Gate { store, cap, looks, held: RefCell::new(Vec::new()) }
    }

    /// The agents working on this machine now. Takes the store lock, and is
    /// called inside it by the starts, where the lock is already held.
    pub(crate) fn working(&self) -> Vec<Counted> {
        self.store.locked(|| {
            let r = Records::read(self.store);
            working(&r, &|s| self.looks.get(s).copied().unwrap_or(Look::Unknown))
        })
    }

    /// Whether one more may start, as `what`. Called inside the lock that
    /// writes its record, so the count and the record are one act. A refusal
    /// is kept for [`Gate::take_held`].
    pub(crate) fn admits(&self, what: &str) -> bool {
        let Some(cap) = self.cap else { return true };
        let now = self.working();
        if now.len() < cap {
            return true;
        }
        self.held.borrow_mut().push((what.to_string(), now));
        false
    }

    /// [`Gate::admits`] for a start that replaces one of the same name which
    /// was launched and has made no claim (`wsp-549`): that start holds a place
    /// of its own in the count until [`WEDGED_AFTER`], and the one that
    /// replaces it takes that place over, so it is not counted against itself.
    pub(crate) fn admits_again(&self, what: &str) -> bool {
        let Some(cap) = self.cap else { return true };
        let now: Vec<Counted> = self.working().into_iter().filter(|c| c.seat.is_some() || c.what != what).collect();
        if now.len() < cap {
            return true;
        }
        self.held.borrow_mut().push((what.to_string(), now));
        false
    }

    pub(crate) fn take_held(&self) -> Vec<(String, Vec<Counted>)> {
        self.held.borrow_mut().drain(..).collect()
    }
}

/// The agents working on this machine now, read whatever the cap is (or is not):
/// [`Gate::working`] reads no seat where there is no cap. For the reader that asks
/// whether a list has a seat of its own working (`wsp-435`), not for a start.
pub(crate) fn working_now(store: &Store, seats: &dyn Seats) -> Vec<Counted> {
    let r = Records::read(store);
    let looks: BTreeMap<String, Look> = r.seats().into_iter().map(|s| (s.clone(), Look::of(seats, &s))).collect();
    working(&r, &|s| looks.get(s).copied().unwrap_or(Look::Unknown))
}

/// The working agents on this machine and its cap, when there is a cap and it
/// is reached — what `wsp spawn` by hand says before it starts anyway.
pub(crate) fn full(store: &Store, seats: &dyn Seats) -> Option<(usize, Vec<Counted>)> {
    let gate = Gate::new(store, seats);
    let cap = gate.cap?;
    let now = gate.working();
    (now.len() >= cap).then_some((cap, now))
}

/// What `wsp wip` says of the starts waiting at the cap, off `capped.json`: each
/// start, since when, and the seats holding the places. `seen` is the census
/// `wip` already took, so this asks no backend.
pub(crate) fn waiting_lines(store: &Store, seen: &[crate::place::Seated]) -> Vec<String> {
    waiting_lines_of(store, seen, None)
}

/// [`waiting_lines`] for the starts of one list (`wsp worklist next`, `wsp-550`):
/// the same sentence `wip` says, off the same file. `next` reads and asks no
/// backend, so it passes no census and a holder's seat reads as it does when
/// the screen is not known.
pub(crate) fn waiting_lines_of(store: &Store, seen: &[crate::place::Seated], only: Option<&str>) -> Vec<String> {
    let Some(cap) = store.seat_cap() else { return Vec::new() };
    let waits: Vec<(String, String, String)> = store
        .capped()
        .into_iter()
        .filter(|(owner, _)| only.is_none_or(|o| o == owner))
        .flat_map(|(owner, starts)| {
            starts.as_object().cloned().unwrap_or_default().into_iter().map(move |(what, since)| {
                (what, owner.clone(), since.as_str().unwrap_or_default().to_string())
            })
        })
        .collect();
    if waits.is_empty() {
        return Vec::new();
    }
    let states: BTreeMap<&str, State> = seen.iter().map(|s| (s.seat.as_str(), s.state)).collect();
    // A start waiting here is launched and unseated, so the count reads it as a
    // place of its own; it holds none against itself (`admits_again`), and the
    // line `cycle.log` says of it leaves it out, so this does too (`wsp-550`).
    let holders: Vec<Counted> = working(&Records::read(store), &|seat| Look::of_state(states.get(seat).copied()))
        .into_iter()
        .filter(|c| c.seat.is_some() || !waits.iter().any(|(what, _, _)| *what == c.what))
        .collect();
    let mut out = vec![format!("waiting for a seat slot ({} of {cap}: {}):", holders.len(), names(&holders))];
    for (what, owner, since) in waits {
        let age = match since.is_empty() {
            true => String::new(),
            false => format!(" for {}", util::duration_human(util::since(&since))),
        };
        out.push(format!("  {what} ({owner}){age}"));
    }
    out
}

/// The agents a list of them reads as, for a log line: the first few, and how
/// many more.
pub(crate) fn names(counted: &[Counted]) -> String {
    const SHOWN: usize = 4;
    let mut out: Vec<String> = counted.iter().take(SHOWN).map(Counted::said).collect();
    if counted.len() > SHOWN {
        out.push(format!("{} more", counted.len() - SHOWN));
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str, status: Status) -> Task {
        let mut t = Task::new(id, id);
        t.status_raw = status.as_str().into();
        t
    }

    fn records(tasks: Vec<Task>, bound: &[(&str, &str)]) -> Records {
        Records {
            tasks,
            lists: Vec::new(),
            bindings: bound.iter().map(|(seat, id)| (seat.to_string(), json!({ "task_id": id }))).collect(),
            claims: BTreeMap::new(),
            agents: BTreeMap::new(),
            governors: BTreeMap::new(),
            held_asks: BTreeSet::new(),
        }
    }

    fn counted(r: &Records, looks: &[(&str, Look)]) -> Vec<String> {
        let looks: BTreeMap<&str, Look> = looks.iter().copied().collect();
        working(r, &|s| looks.get(s).copied().unwrap_or(Look::Unknown)).into_iter().map(|c| c.what).collect()
    }

    /// The rules in the module docs, one seat each: what is working counts,
    /// what is gone does not, and finished work counts only while a screen
    /// shows it working.
    #[test]
    fn a_seat_counts_while_its_work_is_unfinished_or_it_is_seen_working() {
        let r = records(
            vec![
                task("working", Status::Doing),
                task("idle-doing", Status::Doing),
                task("unread", Status::Doing),
                task("gone", Status::Doing),
                task("idle-review", Status::Review),
                task("busy-review", Status::Review),
                task("unread-review", Status::Review),
                task("far", Status::Doing),
            ],
            &[
                ("w1:p1", "working"),
                ("w1:p2", "idle-doing"),
                ("w1:p3", "unread"),
                ("w1:p4", "gone"),
                ("w1:p5", "idle-review"),
                ("w1:p6", "busy-review"),
                ("w1:p7", "unread-review"),
                ("w1:p1@mb2", "far"),
            ],
        );
        let got = counted(
            &r,
            &[
                ("w1:p1", Look::Working),
                ("w1:p2", Look::Quiet),
                ("w1:p4", Look::Gone),
                ("w1:p5", Look::Quiet),
                ("w1:p6", Look::Working),
                ("w1:p1@mb2", Look::Working),
            ],
        );
        assert_eq!(got, ["working", "idle-doing", "unread", "busy-review"]);
    }

    /// `wsp-335`: a seat still reading `starting` long after its record opened
    /// is an agent that never came up, and holds no place under the cap.
    #[test]
    fn a_leg_seat_still_starting_after_five_minutes_is_not_counted() {
        use crate::legs::{Leg, State as LegState};
        let leg_at = |name: &str, ago: i64| Leg {
            state: LegState::Running,
            pane: Some(format!("w1:{name}")),
            started: Some(util::iso_at(util::epoch_secs() - ago)),
            ..Leg::pending(name, "do it")
        };
        let mut t = task("m-1", Status::Doing);
        crate::legs::write(&mut t, &[leg_at("fresh", 60), leg_at("stale", 6 * 60)]);
        let r = records(vec![t], &[]);
        let looks = [("w1:fresh", Look::Starting), ("w1:stale", Look::Starting)];
        assert_eq!(counted(&r, &looks), ["m-1 · fresh"]);
        // Working is working however old the record is.
        let looks = [("w1:fresh", Look::Starting), ("w1:stale", Look::Working)];
        assert_eq!(counted(&r, &looks), ["m-1 · fresh", "m-1 · stale"]);
    }

    /// `wsp-353`: the idle seat of a row run as legs and of a parked list's
    /// member holds no place; the same seats seen working still do.
    #[test]
    fn an_idle_seat_that_handed_its_row_to_legs_or_sits_in_a_parked_list_holds_no_place() {
        use crate::legs::{Leg, State as LegState};
        let mut legged = task("legged", Status::Doing);
        crate::legs::write(&mut legged, &[Leg { state: LegState::Pending, ..Leg::pending("build", "do it") }]);
        let mut r = records(vec![legged, task("parked", Status::Doing), task("plain", Status::Doing)], &[("w1:p1", "legged"), ("w1:p2", "parked"), ("w1:p3", "plain")]);
        let mut w = Worklist::new("l", "l");
        w.set_groups(&[crate::model::Group { members: vec!["parked".into()], ..Default::default() }]);
        w.set_status(crate::model::WorklistStatus::Parked);
        r.lists.push(w);
        let idle = [("w1:p1", Look::Quiet), ("w1:p2", Look::Quiet), ("w1:p3", Look::Quiet)];
        assert_eq!(counted(&r, &idle), ["plain"]);
        let busy = [("w1:p1", Look::Working), ("w1:p2", Look::Working), ("w1:p3", Look::Working)];
        assert_eq!(counted(&r, &busy), ["legged", "parked", "plain"]);
    }

    /// `compounding-285`: a sent-back row whose seat took no tell holds no place
    /// unless the seat is seen working; one that took it still does.
    #[test]
    fn an_idle_seat_that_did_not_take_its_send_back_holds_no_place() {
        let mut refused = task("m-1", Status::Doing);
        refused.log("sent back: the verifier blocked: x");
        refused.log(crate::repair::TELL_PENDING);
        let mut taken = task("m-2", Status::Doing);
        taken.log("sent back: the verifier blocked: x");
        let r = records(vec![refused, taken], &[("w1:p1", "m-1"), ("w1:p2", "m-2")]);
        assert_eq!(counted(&r, &[("w1:p1", Look::Quiet), ("w1:p2", Look::Quiet)]), ["m-2"]);
        assert_eq!(counted(&r, &[("w1:p1", Look::Working), ("w1:p2", Look::Quiet)]), ["m-1", "m-2"]);
    }

    /// A governor is not counted, by its pane, even holding a row.
    #[test]
    fn a_governor_seat_is_never_counted() {
        let mut r = records(vec![task("g-1", Status::Doing), task("h-1", Status::Doing)], &[("w9:p1", "g-1"), ("w2:p1", "h-1")]);
        r.governors.insert("wsp".into(), json!({ "workspace": "w9", "pane": "w9:p1" }));
        assert_eq!(counted(&r, &[("w9:p1", Look::Working), ("w2:p1", Look::Working)]), ["h-1"]);
    }

    /// A claim names its seat through the agent that made it, which is how a
    /// member whose binding was lost is still counted — and once only.
    #[test]
    fn a_claim_counts_through_its_agents_seat_and_a_seat_counts_once() {
        let mut r = records(vec![task("m-1", Status::Doing), task("m-2", Status::Doing)], &[("w1:p1", "m-1")]);
        r.agents.insert("a-1".into(), json!({ "seat": "w1:p1" }));
        r.agents.insert("a-2".into(), json!({ "seat": "w1:p2" }));
        r.claims.insert("m-1".into(), json!({ "agent_id": "a-1" }));
        r.claims.insert("m-2".into(), json!({ "agent_id": "a-2" }));
        assert_eq!(counted(&r, &[]), ["m-1", "m-2"]);
    }

    /// A verifier counts while its pass is reading, seated or not yet, and
    /// stops once its verdict is in and its seat is quiet.
    #[test]
    fn a_verifier_counts_until_its_verdict_and_a_start_not_yet_seated_counts_too() {
        use crate::verification::{self as v, Pass};
        let mut seated = task("m-1", Status::Review);
        v::write(&mut seated, &[Pass { pane: Some("w3:p1".into()), ..Pass::opened(None, None) }]);
        let mut unseated = task("m-2", Status::Review);
        v::write(&mut unseated, &[Pass::opened(None, None)]);
        let mut decided = task("m-3", Status::Review);
        v::write(&mut decided, &[Pass { pane: Some("w3:p3".into()), state: v::State::Holds, ..Pass::opened(None, None) }]);
        let r = records(vec![seated, unseated, decided], &[]);
        assert_eq!(
            counted(&r, &[("w3:p1", Look::Quiet), ("w3:p3", Look::Quiet)]),
            ["m-1 · verifying", "m-2 · verifying"]
        );
        // A decided verifier is closing: not counted even if its screen shows work (`wsp-385`).
        assert_eq!(counted(&r, &[("w3:p1", Look::Gone), ("w3:p3", Look::Working)]), ["m-2 · verifying"]);
    }

    /// `wsp-385`: a check whose verdict is recorded, or whose barrier was passed
    /// without it, holds no place however its screen reads; an undecided one does.
    #[test]
    fn a_check_with_its_verdict_recorded_is_not_counted_even_working() {
        use crate::barrier::{Check, State as CheckState};
        let mut w = Worklist::new("run", "A run");
        let group = |checks: Vec<Check>, verdict: &str| crate::model::Group { checks, verdict: verdict.into(), ..Default::default() };
        let seated = |seat: &str, state| Check { pane: Some(seat.into()), state, ..Check::opened(None, None) };
        w.set_groups(&[
            group(vec![seated("w1:p1", CheckState::Passes), seated("w1:p2", CheckState::Running)], "2026-11-05T15:49:26Z go"),
            group(vec![seated("w1:p3", CheckState::Running)], ""),
        ]);
        let mut r = records(Vec::new(), &[]);
        r.lists.push(w);
        let looks = [("w1:p1", Look::Working), ("w1:p2", Look::Working), ("w1:p3", Look::Working)];
        assert_eq!(counted(&r, &looks), ["run · barrier 2"]);
    }

    struct AllWorking;

    impl Seats for AllWorking {
        fn state(&self, _seat: &str) -> Option<State> {
            Some(State::Working)
        }
    }

    /// `wsp-353`: `wip` names each start waiting at the cap and the seats holding
    /// the places, and says nothing when nothing waits.
    #[test]
    fn wip_names_what_waits_at_the_cap_and_who_holds_the_places() {
        let env = util::isolated("room-waiting");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        store.save_task(&task("h-1", Status::Doing)).unwrap();
        store.set_binding("w2:p1", json!({ "task_id": "h-1" }));
        let seat = |s: &str| crate::place::Seated { seat: crate::place::Seat::new(s), state: State::Working, ..Default::default() };
        let seen = [seat("w2:p1")];

        assert!(waiting_lines(&store, &seen).is_empty(), "no cap");
        store.set_seat_cap(Some(1)).unwrap();
        assert!(waiting_lines(&store, &seen).is_empty(), "nothing waits");
        store.set_capped("run", Some(json!({ "m-2": util::now_iso() })));
        let lines = waiting_lines(&store, &seen);
        assert_eq!(lines[0], "waiting for a seat slot (1 of 1: h-1 in w2:p1):");
        assert!(lines[1].starts_with("  m-2 (run)"), "{lines:?}");
    }

    /// wsp-550: `next` reads the same sentence for its own list only.
    #[test]
    fn next_reads_the_waiting_of_its_own_list_in_wips_words() {
        let env = util::isolated("room-waiting-next");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        store.save_task(&task("h-1", Status::Doing)).unwrap();
        store.set_binding("w2:p1", json!({ "task_id": "h-1" }));
        store.set_seat_cap(Some(1)).unwrap();
        store.set_capped("run", Some(json!({ "m-2": util::now_iso() })));
        store.set_capped("other", Some(json!({ "o-1": util::now_iso() })));
        let lines = waiting_lines_of(&store, &[], Some("run"));
        assert!(lines[0].starts_with("waiting for a seat slot (1 of 1: h-1"), "{lines:?}");
        assert_eq!(lines.len(), 2, "only this list's start: {lines:?}");
        assert!(lines[1].starts_with("  m-2 (run)"), "{lines:?}");
        assert!(waiting_lines_of(&store, &[], Some("none")).is_empty());
        // The start that waits is launched and unseated, and is not a holder
        // of the places it waits for.
        let mut m = task("m-2", Status::Doing);
        m.log(&format!("{} run group 1", crate::cycle::STARTED_BY));
        store.save_task(&m).unwrap();
        let lines = waiting_lines_of(&store, &[], Some("run"));
        assert!(lines[0].starts_with("waiting for a seat slot (1 of 1: h-1"), "{lines:?}");
    }

    /// What `wsp spawn` by hand reads before it starts anyway: nothing on a
    /// seat with no cap, and the cap and who fills it once it is reached.
    #[test]
    fn full_answers_only_when_there_is_a_cap_and_it_is_reached() {
        let env = util::isolated("room-full");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        store.save_task(&task("h-1", Status::Doing)).unwrap();
        store.set_binding("w2:p1", json!({ "task_id": "h-1" }));

        assert!(full(&store, &AllWorking).is_none(), "no cap, nothing to be full");
        store.set_seat_cap(Some(2)).unwrap();
        assert!(full(&store, &AllWorking).is_none(), "one of two");
        store.set_seat_cap(Some(1)).unwrap();
        let (cap, now) = full(&store, &AllWorking).expect("one of one");
        assert_eq!((cap, names(&now)), (1, "h-1 in w2:p1".to_string()));
    }
    /// `wsp-495`: a seat standing in its own `wsp ask --person` holds a place
    /// until the ask is old, holds none after, and holds one again once answered
    /// is moot (the ask is closed and the seat is working on).
    #[test]
    fn held_ask_older_than_the_threshold_holds_no_place_and_an_answered_one_never_did() {
        use crate::message::{self, About, Kind, Message, Party, Waiting};
        let env = util::isolated("room-held-ask");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        store.save_task(&task("h-1", Status::Doing)).unwrap();
        store.set_binding("w2:p1", json!({ "task_id": "h-1" }));
        store.set_seat_cap(Some(1)).unwrap();

        let mut q = Message::question(Party::pane("w2:p1", ""), Kind::Note, "Land now?", Waiting::new("w2:p1", "h-1"))
            .about(About::Task("h-1".into()))
            .for_the_person();
        q.at = util::iso_at(util::epoch_secs() - (HELD_ASK_AFTER - 60));
        message::raise(&store, &q).unwrap();
        assert!(full(&store, &AllWorking).is_some(), "a young ask holds the place");

        let mut old = store.message(&q.id).unwrap();
        old.at = util::iso_at(util::epoch_secs() - (HELD_ASK_AFTER + 60));
        store.save_message(&old).unwrap();
        assert!(full(&store, &AllWorking).is_none(), "an old ask holds none");
        assert!(store.message(&q.id).unwrap().wants_answering(), "and it is still open");

        message::answer(&store, &q.id, &Party::Human, "yes").unwrap();
        assert!(full(&store, &AllWorking).is_some(), "answered, the seat is an agent working again");
    }
}
