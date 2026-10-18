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

/// How a seat reads, for this count and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    /// Positively not there, or there with nothing running in it.
    Gone,
    Working,
    /// Idle, starting, on a prompt: there, and not seen working.
    Quiet,
    /// Nobody can say.
    Unknown,
}

impl Look {
    pub(crate) fn of(seats: &dyn Seats, seat: &str) -> Look {
        if seats.absent(seat) {
            return Look::Gone;
        }
        match seats.state(seat) {
            Some(State::Gone | State::Empty) => Look::Gone,
            Some(State::Working) => Look::Working,
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
            Look::Quiet | Look::Unknown => !finished,
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
}

impl Records {
    pub(crate) fn read(store: &Store) -> Records {
        Records {
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

/// Every agent working on this machine, by the rules in the module docs.
/// `look` answers for a seat; the caller decides how it was read.
pub(crate) fn working(r: &Records, look: &dyn Fn(&str) -> Look) -> Vec<Counted> {
    let now = util::epoch_secs();
    let fresh = |at: &str| now - util::epoch_of(at) <= WEDGED_AFTER;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let governor = |seat: &str| crate::cmd_agent::governs_seat(&r.governors, seat);
    let mut take = |out: &mut Vec<Counted>, seat: Option<&str>, what: String, finished: bool| {
        if let Some(s) = seat {
            if !here(s) || governor(s) || !seen.insert(s.to_string()) || !look(s).counts(finished) {
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
                Some(seat) => take(&mut out, Some(seat), what, !running),
                None if running && p.ending == crate::verification::Ending::Standing && fresh(&p.at) => {
                    take(&mut out, None, what, false)
                }
                None => {}
            }
        }
    }

    // A barrier check, by its entry on the group.
    for w in &r.lists {
        for (i, g) in w.groups().iter().enumerate() {
            for c in &g.checks {
                let what = crate::barrier::seat_label(&w.id, i + 1);
                match (c.standing(), &c.pane) {
                    (Some(seat), _) => take(&mut out, Some(seat), what, !c.open()),
                    (None, None) if c.open() && fresh(&c.at) => take(&mut out, None, what, false),
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
        for l in crate::legs::legs(t) {
            let what = crate::legs::label(&t.id, &l.name);
            let running = l.state == crate::legs::State::Running;
            match &l.pane {
                Some(seat) => take(&mut out, Some(seat), what, !running || l.ending != crate::verification::Ending::Standing),
                None if running && l.started.as_deref().is_some_and(fresh) => take(&mut out, None, what, false),
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
        take(&mut out, Some(&seat), id, finished(s));
    }

    // A member the run moved to `doing` whose agent has not claimed it yet:
    // the start in flight that two racing advances would otherwise both miss.
    for t in &r.tasks {
        let ours = t
            .section("Log")
            .and_then(|l| l.lines().last().map(|x| x.contains(crate::cycle::STARTED_BY)))
            .unwrap_or(false);
        if t.status() == Status::Doing && ours && !r.claims.contains_key(&t.id) && fresh(&t.updated) {
            take(&mut out, None, t.id.clone(), false);
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

    pub(crate) fn take_held(&self) -> Vec<(String, Vec<Counted>)> {
        self.held.borrow_mut().drain(..).collect()
    }
}

/// The working agents on this machine and its cap, when there is a cap and it
/// is reached — what `wsp spawn` by hand says before it starts anyway.
pub(crate) fn full(store: &Store, seats: &dyn Seats) -> Option<(usize, Vec<Counted>)> {
    let gate = Gate::new(store, seats);
    let cap = gate.cap?;
    let now = gate.working();
    (now.len() >= cap).then_some((cap, now))
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
        assert_eq!(counted(&r, &[("w3:p1", Look::Gone), ("w3:p3", Look::Working)]), ["m-2 · verifying", "m-3 · verifying"]);
    }

    struct AllWorking;

    impl Seats for AllWorking {
        fn state(&self, _seat: &str) -> Option<State> {
            Some(State::Working)
        }
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
}
