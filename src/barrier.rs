//! A barrier check is an entry on the list's group: `wsp-230`.
//!
//! Every barrier used to be filed as a row of its own, "Barrier: <list> group
//! N", whose overview was the check's work order. Ed, 2026-10-07: each row then
//! sat at `review` waiting for a `done` nobody owed it, and the backlog grew by
//! one per group — compound-365, -367, -369 and -371 on `gpui-migration` alone.
//! `wsp-188` stopped doing this for verifiers, and the barrier is the same kind
//! of step.
//!
//! So the check is recorded where its verdict is read: on the group, under a
//! `checks:` block beside the `verdict:` it writes. One entry per check, oldest
//! first, and a re-check after a blocking follow-up adds one:
//!
//! ```text
//!   checks: 2026-10-07T12:04:11Z passes read:55c4839abcde pane:cpd-484
//!           on:claude+opus/high ended · 2026-10-07T13:00:00Z running
//! ```
//!
//! **Tokens, like `landed:`**, because every value is whitespace-free — an
//! instant, a word, a hash, a seat id, a tier with its spaces turned to `+` —
//! so a wrapped block is the same walk run again and nothing a writer does can
//! split an entry. An entry starts at an instant; every token after it until
//! the next instant is one of its clauses, keyed by the word before the colon.
//! The prose stays where it already was: a pass's sentence is the group's
//! `verdict:`, a hold's and a follow-up's are on the list's `## Log`.
//!
//! # The entry is the record, and the only one
//!
//! A check has no row and holds no claim. The `running` entry is written under
//! the store lock *before* its agent is spawned — the idempotence key the row's
//! title used to be — and `wsp spawn <list> --check N` writes the seat onto it.
//! Everything that asks about a check asks the entry: the step that opens one
//! ([`open`]), the verbs that settle one ([`settle`]), the ending
//! ([`crate::cycle::end_checks_when_idle`] and the tick), `wsp wip` naming the
//! seat against its list ([`seats`]), and the brief it is given.
//!
//! # The rows that came before
//!
//! A Barrier row wsp filed is still read by the run while it is open — its
//! agent may be mid-check when this binary is installed, and the verbs it was
//! told to finish with still take it — and [`migrate`] moves it onto its group
//! once it has settled.

use std::collections::BTreeMap;

use crate::model::{Group, Status, Task, Worklist};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;
#[cfg(test)]
use crate::checks::{ps_says, push, text};
pub(crate) use crate::checks::{wait_after, RETRY_BACKOFF, TRIES, instant, Check, Ending, Run, State, Suite};

/// What `wsp wip` and the seat's own label call a check: against its list,
/// since there is no row to name it by.
pub(crate) fn seat_label(list: &str, at: usize) -> String {
    format!("{list} · barrier {at}")
}

/// The list and group a seat's label names, if it is a check's ([`seat_label`]).
pub(crate) fn parse_label(label: &str) -> Option<(&str, usize)> {
    let (list, at) = label.trim().split_once(" · barrier ")?;
    Some((list, at.trim().parse().ok().filter(|at| *at >= 1)?))
}

/// The name a check's brief file and tree are kept under, as a verifier's is
/// `<member>-verify`.
pub(crate) fn subject(list: &str, at: usize) -> String {
    format!("{list}-check-{at}")
}

/// The list and group a [`subject`] names, if it is one.
pub(crate) fn parse_subject(name: &str) -> Option<(&str, usize)> {
    crate::util::parse_subject(name)
}

/// Every standing check seat, by pane: the list and the group it is reading.
///
/// Every list, and not only running ones: a check that held its list is still
/// seated until the tick ends it, and `wip` must not draw it as holding
/// nothing in the meantime.
pub(crate) fn seats(lists: &[Worklist]) -> BTreeMap<String, (String, usize)> {
    let mut out = BTreeMap::new();
    for w in lists {
        for (i, g) in w.groups().iter().enumerate() {
            for c in &g.checks {
                if let Some(p) = c.standing() {
                    out.insert(p.to_string(), (w.id.clone(), i + 1));
                }
            }
        }
    }
    out
}

/// Every standing check seat, by pane, to what its entry records it ran on —
/// the policy's label with its spaces back — for `wip`'s kind, model and
/// effort (`wsp-248`). A seat whose entry recorded nothing is absent.
pub(crate) fn ran_on(lists: &[Worklist]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for g in lists.iter().flat_map(|w| w.groups()) {
        for c in &g.checks {
            if let (Some(p), Some(on)) = (c.standing(), &c.on) {
                out.insert(p.to_string(), on.clone());
            }
        }
    }
    out
}

/// The list and group a seat is checking, if it is one.
pub(crate) fn seat_of(store: &Store, seat: &str) -> Option<(String, usize)> {
    seats(&store.worklists()).remove(seat)
}

/// The open check on a group, if there is one.
pub(crate) fn running(g: &Group) -> Option<&Check> {
    g.checks.iter().rev().find(|c| c.open())
}

/// Write a change to group `at` of `list` under the lock, reading the list
/// afresh, and say whether it was made. `f` answers whether it changed anything.
fn change(store: &Store, list: &str, at: usize, f: impl FnOnce(&mut Group) -> bool) -> bool {
    let wrote = store.locked(|| {
        let Some(mut w) = store.worklist(list) else { return false };
        let mut groups = w.groups();
        let Some(g) = groups.get_mut(at - 1) else { return false };
        if !f(g) {
            return false;
        }
        w.set_groups(&groups);
        store.save_worklist(&w).is_ok()
    });
    // A check's state — an attempt marked `ended` among it — is committed with
    // the write (`wsp-433`); the daemon's tick that makes it commits none.
    if wrote {
        store.git_commit(&format!("wsp: {list} barrier {at}: a check moved"));
    }
    wrote
}

/// Whether opening a check on `g` would write anything now: there is none open,
/// the open one's seat has gone, or it has no seat and has waited out its start.
/// Asked before the plan is read, so a barrier that is only being waited on does
/// not read the repositories every tick.
pub(crate) fn would_open(g: &Group, gone: &dyn Fn(&str) -> bool) -> bool {
    match running(g) {
        None => true,
        Some(c) => match c.pane.as_deref() {
            Some(p) => gone(p),
            None => c.wait_before_next_try().is_some_and(|w| util::epoch_secs() - util::epoch_of(&c.at) > w),
        },
    }
}

/// A check opened on group `at`, or none owed.
pub(crate) struct Opened {
    /// It took over an open entry nobody's agent ever reached.
    pub retaken: bool,
    /// Which start of the entry this is, the first being 1.
    pub tries: u32,
    /// Where the mechanical run of the entry it took over had got to, so a
    /// start that failed after the suite finished does not run the suite again.
    pub suite: Option<Suite>,
    /// The pid of the run it took over, when that run had died without a result.
    pub died: Option<u32>,
}

#[cfg(test)]
mod retry_waits {
    use super::*;

    #[test]
    fn a_start_that_failed_is_tried_again_in_thirty_seconds_then_two_minutes_and_the_third_is_the_last() {
        assert_eq!(wait_after(0), Some(30));
        assert_eq!(wait_after(1), Some(30));
        assert_eq!(wait_after(2), Some(120));
        assert_eq!(wait_after(TRIES), None);
        assert_eq!((span(30), span(120)), ("30 s".to_string(), "2 min".to_string()));
    }
}

/// A wait as a person reads it: `30 s`, `2 min`.
pub(crate) fn span(secs: i64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", secs / 60)
    }
}

/// How long a seat may read empty or still starting before it is not a check
/// that is coming up but one that never will. `claude started but never became
/// ready` is the spawn's own verdict after about a minute; this is for the one
/// that never even got to say so.
pub(crate) const NOT_UP_AFTER: i64 = 5 * 60;

/// Whether wsp is to run the mechanical part for a check it opens.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mechanical {
    No,
    Yes,
}

/// Open a check on group `at`, if one is owed, under the lock. `gone` answers
/// whether a seat has nobody in it any more.
///
/// **One open entry is the key**, as a row's title used to be. An open entry
/// with a seat is a check in flight and nothing is owed — unless that seat has
/// gone, when it is marked abandoned and a fresh one opens, rather than the run
/// waiting on an agent that will never answer. An open entry with no seat after
/// [`RETRY_BACKOFF`] is a start that never arrived, and is taken again — on the
/// backoff, [`TRIES`] times in all, and then not at all: [`give_up`] says so.
#[cfg(test)]
pub(crate) fn open(store: &Store, w: &Worklist, at: usize, read: Option<String>, on: Option<String>, gone: &dyn Fn(&str) -> bool) -> Option<Opened> {
    open_within(store, w, at, read, on, Mechanical::No, gone, &|| true)
}

/// [`open`], asking `admit` whether the machine has room for one more agent
/// (`wsp-231`) — once a check is owed and before it is written, inside the
/// same lock, so a check the seat's cap holds back leaves no record behind.
pub(crate) fn open_within(
    store: &Store,
    w: &Worklist,
    at: usize,
    read: Option<String>,
    on: Option<String>,
    mech: Mechanical,
    gone: &dyn Fn(&str) -> bool,
    admit: &dyn Fn() -> bool,
) -> Option<Opened> {
    let mut opened = None;
    let changed = change(store, &w.id, at, |g| {
        if let Some(c) = g.checks.iter_mut().rev().find(|c| c.open()) {
            match c.pane.as_deref() {
                Some(p) if gone(p) => {
                    if !admit() {
                        return false;
                    }
                    c.state = State::Abandoned;
                    c.ending = Ending::Ended;
                }
                Some(_) => return false,
                None if c.wait_before_next_try().is_some_and(|w| util::epoch_secs() - util::epoch_of(&c.at) > w) => {
                    if !admit() {
                        return false;
                    }
                    let before = c.suite;
                    let died = c.run.as_ref().filter(|_| c.run_died()).map(|r| r.pid);
                    c.at = util::now_iso();
                    c.run = None;
                    // A suite that never finished, or one not asked for until
                    // now, is a run and not another try of the agent's start;
                    // a finished suite's start is what is retried. A run that
                    // died is a try as well, so one that dies every time ends
                    // in `give_up` and not in a loop.
                    if before == Some(Suite::Running) || (before.is_none() && mech == Mechanical::Yes) {
                        c.suite = (mech == Mechanical::Yes).then_some(Suite::Running);
                        if died.is_some() {
                            c.tries += 1;
                        }
                    } else {
                        c.tries += 1;
                    }
                    opened = Some(Opened { retaken: true, tries: c.tries, suite: before, died });
                    return true;
                }
                None => return false,
            }
        } else if !admit() {
            return false;
        }
        let mut c = Check::opened(read, on);
        if mech == Mechanical::Yes {
            c.suite = Some(Suite::Running);
        }
        g.checks.push(c);
        opened = Some(Opened { retaken: false, tries: 1, suite: None, died: None });
        true
    });
    changed.then_some(()).and(opened)
}

/// What [`open_unless_held`] came to.
pub(crate) enum Opening {
    /// The open check's run reads as dead and the tree lock is held by a live
    /// process: the run is alive (or its result is being written), and a copy
    /// started now would only wait behind it. Nothing was written.
    Held,
    Opened(Opened),
    Nothing,
}

/// [`open_within`], except that a mechanical run that reads as dead while
/// `held` says a live process has its tree lock is left alone. **Whoever holds
/// the lock counts**, not only the pid the entry records: a copy that did start
/// has replaced that pid with its own (`note_pid`), and the lock is still the
/// first run's.
pub(crate) fn open_unless_held(
    store: &Store,
    w: &Worklist,
    at: usize,
    read: Option<String>,
    on: Option<String>,
    mech: Mechanical,
    gone: &dyn Fn(&str) -> bool,
    admit: &dyn Fn() -> bool,
    held: &dyn Fn() -> bool,
) -> Opening {
    let died = store.worklist(&w.id).is_some_and(|w| w.groups().get(at - 1).is_some_and(run_died));
    if died && held() {
        return Opening::Held;
    }
    match open_within(store, w, at, read, on, mech, gone, admit) {
        Some(o) => Opening::Opened(o),
        None => Opening::Nothing,
    }
}

/// Name the seat of the open check on group `at`. `wsp spawn --check` calls it
/// once the seat exists and before the agent starts, because the brief it
/// reads at start-up finds it by this seat.
pub(crate) fn name_seat(store: &Store, list: &str, at: usize, seat: &str) -> bool {
    change(store, list, at, |g| match g.checks.iter_mut().rev().find(|c| c.open()) {
        Some(c) => {
            c.pane = Some(seat.to_string());
            true
        }
        None => false,
    })
}

/// What [`unseat`] took from an entry.
pub(crate) struct Unseated {
    /// The seat that held no agent, for the caller to end: after this the
    /// entry names nobody, so `despawn`'s own `mark_ended` finds nothing to mark.
    pub seat: Option<String>,
    pub tries: u32,
    /// This was the last try and the person has to be told, once.
    pub spent: bool,
}

/// A start that did not take: take the seat off the open entry on group `at`
/// and start its backoff from now (`wsp-328`). The next [`open_within`] after
/// the wait starts the agent again, until [`TRIES`] have been had; the caller
/// ends the seat returned and, when `spent`, flags the person.
///
/// **The seat comes off the entry rather than being left on it**, because an
/// entry naming a pane is read as a check in flight and nothing is owed. That
/// is the shape this row found: `claude started but never became ready` in
/// cpd-2511, the pane named on the entry and idle, and the run waiting on it
/// until somebody ended it by hand.
pub(crate) fn unseat(store: &Store, list: &str, at: usize) -> Option<Unseated> {
    let mut out = None;
    change(store, list, at, |g| {
        let Some(c) = g.checks.iter_mut().rev().find(|c| c.open()) else { return false };
        let seat = c.pane.take();
        c.ending = Ending::Standing;
        c.at = util::now_iso();
        out = Some(Unseated { seat, tries: c.tries, spent: c.spent() && !std::mem::replace(&mut c.gave_up, true) });
        true
    });
    out
}

/// The entry on group `at` has had its last try and nobody seated it, and the
/// start never reported failing — a spawn killed before it could (`wsp-328`).
/// Marks it given up and answers how many tries it had, once; [`unseat`] says
/// the same for a start that did report.
pub(crate) fn give_up(store: &Store, list: &str, at: usize) -> Option<u32> {
    let mut tries = None;
    change(store, list, at, |g| {
        let Some(c) = g.checks.iter_mut().rev().find(|c| c.open()) else { return false };
        let waited = util::epoch_secs() - util::epoch_of(&c.at) > RETRY_BACKOFF[0];
        if !c.spent() || c.gave_up || !waited {
            return false;
        }
        c.gave_up = true;
        tries = Some(c.tries);
        true
    });
    tries
}

/// Somebody asked for the start again (`wsp worklist advance <list>` by hand)
/// after the tries ran out: the spent entry is abandoned, so the same step
/// opens a fresh one with a fresh count. Whether there was one to abandon.
pub(crate) fn try_again(store: &Store, list: &str) -> bool {
    let mut any = false;
    let n = store.worklist(list).map_or(0, |w| w.groups().len());
    for at in 1..=n {
        any |= change(store, list, at, |g| {
            let Some(c) = g.checks.iter_mut().rev().find(|c| c.open() && c.gave_up) else { return false };
            c.state = State::Abandoned;
            c.ending = Ending::Ended;
            true
        });
    }
    any
}

/// Record where the open check's mechanical run got to, and — with `by` — that
/// wsp, and no agent, is settling it. Written before the verb that settles, so
/// the entry reads whole. `Some("")` takes the claim back, for a pass that
/// could not be recorded.
pub(crate) fn note_run(store: &Store, list: &str, at: usize, suite: Suite, by: Option<&str>) -> bool {
    change(store, list, at, |g| match g.checks.iter_mut().rev().find(|c| c.open()) {
        Some(c) => {
            c.suite = Some(suite);
            if suite != Suite::Running {
                c.run = None;
            }
            if let Some(b) = by {
                c.by = Some(b.to_string()).filter(|b| !b.is_empty());
            }
            true
        }
        None => false,
    })
}

/// Record that this process is making the open check's mechanical run, so a
/// run that is killed is seen to be dead and not believed for hours. Only while
/// the entry reads `suite:running`.
pub(crate) fn note_pid(store: &Store, list: &str, at: usize) -> bool {
    let Some(run) = Run::this() else { return false };
    change(store, list, at, |g| match g.checks.iter_mut().rev().find(|c| c.open() && c.suite == Some(Suite::Running)) {
        Some(c) => {
            c.run = Some(run);
            true
        }
        None => false,
    })
}

/// Whether the open check on `g` is a mechanical run whose process has died.
pub(crate) fn run_died(g: &Group) -> bool {
    running(g).is_some_and(Check::run_died)
}

/// The open check's suite state on group `at`, if it has one.
pub(crate) fn suite_of(store: &Store, list: &str, at: usize) -> Option<Suite> {
    let w = store.worklist(list)?;
    let groups = w.groups();
    running(groups.get(at - 1)?)?.suite
}

/// Settle the open check on group `at`. Returns the seat it ran in, for the
/// ending, when there was one to settle.
///
/// **Whoever settles it**: the check itself, nearly always, but a governor's
/// `go` over a check still reading also ends that check's job, and the entry
/// says what the barrier came to either way.
pub(crate) fn settle(store: &Store, list: &str, at: usize, state: State) -> Option<String> {
    let mut seat = None;
    change(store, list, at, |g| match g.checks.iter_mut().rev().find(|c| c.open()) {
        Some(c) => {
            c.state = state;
            seat = c.pane.clone();
            true
        }
        None => false,
    });
    seat
}

/// Record the ending of the check that ran in `seat`, wherever it is.
/// `despawn` calls it for every seat it ends, so the record follows the
/// ending however it came about.
pub(crate) fn mark_ended(store: &Store, seat: &str, ok: bool) -> bool {
    let Some((list, at)) = seat_of(store, seat) else { return false };
    change(store, &list, at, |g| {
        let mut any = false;
        for c in g.checks.iter_mut().filter(|c| c.standing() == Some(seat)) {
            c.ending = if ok { Ending::Ended } else { Ending::Failed };
            any = true;
        }
        any
    })
}

/// Whether group `i` (0-based) has nothing left for a check to decide: its list
/// is done, or its barrier — or one past it — has a verdict written. A check
/// still open on such a group is a second one, seated while the first settled
/// the barrier (`pre-prep` barrier 2, 2026-11-05: cpd-2999 stood 100 minutes
/// holding one of the four places, and was told to close, and wsp then wrote a
/// `go` for it on a barrier long passed). `wsp-385`.
pub(crate) fn moot(w: &Worklist, groups: &[Group], i: usize) -> bool {
    w.status() == crate::model::WorklistStatus::Done || i < crate::worklist::crossed(groups)
}

/// Group `at`'s standing check seats, settled or not.
pub(crate) fn standing_on(g: &Group) -> Vec<String> {
    g.checks.iter().filter_map(|c| c.standing().map(str::to_string)).collect()
}

/// The one-line readings of a group's checks, for `worklist show --verdicts`:
/// what each said, the commit it read and the seat it ran in.
pub(crate) fn lines(p: &Paint, g: &Group) -> Vec<String> {
    g.checks
        .iter()
        .map(|c| {
            let mut parts = vec![format!("check {}", c.at), c.state.said().to_string()];
            if let Some(r) = &c.read {
                parts.push(format!("read {}", &r[..r.len().min(12)]));
            }
            if let Some(s) = &c.pane {
                parts.push(format!("in {s}"));
            }
            if let Some(o) = &c.on {
                parts.push(format!("on {o}"));
            }
            if let Some(w) = &c.was {
                parts.push(format!("was {w}"));
            }
            if let Some(s) = c.suite {
                parts.push(format!("suite {}", s.as_str()));
            }
            if c.run_died() {
                parts.push("barrier run died".into());
            }
            if let Some(b) = &c.by {
                parts.push(format!("by {b}, no agent"));
            }
            p.dim(&parts.join(" · "))
        })
        .collect()
}

// ---- the rows that came before ----------------------------------------

/// How the barrier work order wsp filed on every row began. What the migration
/// keys on, with the tag: a row somebody wrote by hand about a barrier
/// (compound-291, compound-330) carries neither.
const ORDER_OPENS: &str = "Check the barrier after group ";

/// A Barrier row wsp filed: the tag, and wsp's own work order.
pub(crate) fn filed_by_wsp(t: &Task) -> bool {
    t.tags.iter().any(|g| g == crate::cycle::BARRIER_TAG)
        && t.section("Overview").is_some_and(|o| o.trim_start().starts_with(ORDER_OPENS))
}

/// The list and group a wsp-filed row checked, from its title.
fn placed(t: &Task) -> Option<(String, usize)> {
    let list = crate::cycle::barrier_list(t)?.to_string();
    let rest = t.title.strip_prefix(&format!("{}{list} group ", crate::cycle::BARRIER_TITLE))?;
    let at: usize = rest.split_whitespace().next()?.parse().ok()?;
    crate::cycle::is_barrier(t, &list, at).then_some((list, at))
}

/// The entry a settled row becomes. The state is read off what the run
/// recorded rather than the row's status, which only ever said `review`: a
/// follow-up names the row in the list's log, and otherwise a verdict on the
/// group is a pass — unless a later check on the same barrier exists, in which
/// case this one held.
fn legacy(row: &Task, w: &Worklist, g: &Group, later: bool) -> Check {
    let log = w.section("Log").unwrap_or_default();
    let mark = |how: &str| log.lines().any(|l| l.contains(&format!("follow-up by {} {how}", row.id)));
    let state = if mark("passes") {
        State::Next
    } else if mark("holds") {
        State::Blocking
    } else if !g.verdict.trim().is_empty() && !later {
        State::Passes
    } else {
        State::Holds
    };
    let rlog = row.section("Log").unwrap_or_default();
    let pane = rlog
        .lines()
        .find_map(|l| l.split("claimed by pane ").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string);
    let on = rlog
        .lines()
        .find_map(|l| l.split("spawned at ").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string);
    Check { at: row.created.clone(), state, read: None, pane, on, ending: Ending::Ended, was: Some(row.id.clone()), tries: 1, gave_up: false, suite: None, run: None, by: None }
}

/// What [`migrate`] would do.
pub(crate) struct Plan {
    /// (list, group) -> the rows whose checks go onto it, oldest first.
    pub moves: BTreeMap<(String, usize), Vec<Task>>,
    /// Still open or still holding a claim: an agent may be reading.
    pub open: Vec<String>,
    /// Filed by wsp, but naming a list or group that is not there.
    pub orphans: Vec<String>,
    /// Tagged `barrier`, or titled like one, without wsp's work order.
    pub by_hand: Vec<String>,
}

pub(crate) fn plan(store: &Store) -> Plan {
    let tasks = store.tasks();
    let claims = store.claims();
    let mut plan = Plan { moves: BTreeMap::new(), open: Vec::new(), orphans: Vec::new(), by_hand: Vec::new() };
    for t in &tasks {
        if !filed_by_wsp(t) {
            let looks = t.tags.iter().any(|g| g == crate::cycle::BARRIER_TAG) || t.title.starts_with("Barrier");
            if looks {
                plan.by_hand.push(t.id.clone());
            }
            continue;
        }
        if claims.contains_key(&t.id) || !matches!(t.status(), Status::Review | Status::Done) {
            plan.open.push(t.id.clone());
            continue;
        }
        let Some((list, at)) = placed(t) else {
            plan.orphans.push(t.id.clone());
            continue;
        };
        if store.worklist(&list).map_or(true, |w| w.groups().len() < at) {
            plan.orphans.push(t.id.clone());
            continue;
        }
        plan.moves.entry((list, at)).or_default().push(t.clone());
    }
    for rows in plan.moves.values_mut() {
        rows.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    }
    plan
}

/// `wsp migrate --barrier-rows [-n]` — every settled Barrier row wsp filed,
/// moved onto its list's group as a check and archived.
///
/// **Old ids keep resolving.** The row is archived, not deleted, and `wsp show`
/// reads the archive, so a log or a verdict that cites compound-369 still finds
/// it, and the entry on the group names it with `was:`.
pub(crate) fn migrate(store: &Store, args: &Args) -> i32 {
    let dry = args.has("n") || args.has("dry-run");
    let p = Paint::new();
    let plan = plan(store);
    for ((list, at), rows) in &plan.moves {
        println!(
            "{}  {} check(s) from {}",
            p.bold(&format!("{list} group {at}")),
            rows.len(),
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(" ")
        );
    }
    if !plan.open.is_empty() {
        println!("{}", p.yellow(&format!("left open: {} — an agent may still be reading; run this again once it settles", plan.open.join(" "))));
    }
    if !plan.orphans.is_empty() {
        println!("{}", p.yellow(&format!("left alone: {} — no such list or group", plan.orphans.join(" "))));
    }
    if !plan.by_hand.is_empty() {
        println!("{}", p.dim(&format!("made by hand, left alone: {}", plan.by_hand.join(" "))));
    }
    let n: usize = plan.moves.values().map(Vec::len).sum();
    println!("\n{n} Barrier row(s) onto {} group(s)", plan.moves.len());
    if n == 0 {
        return 0;
    }
    if dry {
        println!("{}", p.dim("nothing written — drop -n to apply"));
        return 0;
    }
    match apply(store, &plan) {
        Ok(moved) => {
            store.git_commit(&format!("wsp: {moved} Barrier row(s) moved onto their groups' checks"));
            println!("moved {moved}");
            0
        }
        Err(e) => {
            eprintln!("wsp: {e}");
            1
        }
    }
}

fn apply(store: &Store, plan: &Plan) -> Result<usize, String> {
    let mut moved = 0;
    for ((list, at), rows) in &plan.moves {
        let wrote = store.locked(|| -> Result<(), String> {
            let mut w = store.worklist(list).ok_or_else(|| format!("{list} went away"))?;
            let mut groups = w.groups();
            let g = groups.get_mut(at - 1).ok_or_else(|| format!("{list} has no group {at}"))?;
            for (i, row) in rows.iter().enumerate() {
                if g.checks.iter().any(|c| c.was.as_deref() == Some(row.id.as_str())) {
                    continue;
                }
                let entry = legacy(row, &w, g, i + 1 < rows.len());
                g.checks.push(entry);
            }
            g.checks.sort_by(|a, b| a.at.cmp(&b.at));
            w.set_groups(&groups);
            store.save_worklist(&w).map_err(|e| format!("{list}: {e}"))
        });
        wrote?;
        for row in rows {
            store.archive_task(row).map_err(|e| format!("{}: {e}", row.id))?;
            moved += 1;
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{parse_groups, render_groups};

    #[test]
    fn only_an_exit_one_with_nothing_printed_from_ps_is_a_pid_that_is_gone() {
        assert_eq!(ps_says(Some(0), b"Tue Nov  4 11:19:05 2026\n"), Ok(Some("Tue_Nov_4_11:19:05_2026".into())));
        assert_eq!(ps_says(Some(1), b""), Ok(None), "no such process");
        assert_eq!(ps_says(Some(1), b"\n"), Ok(None));
        assert_eq!(ps_says(None, b""), Err(()), "killed by a signal says nothing of the pid");
        assert_eq!(ps_says(Some(2), b""), Err(()));
        assert_eq!(ps_says(Some(1), b"Tue Nov  4 11:19:05 2026\n"), Err(()));
    }

    /// `wsp-391`: the run reads dead but holds the tree lock, so the tick starts
    /// no copy of it; the same record with nobody holding the lock is retaken.
    #[test]
    fn a_dead_reading_run_that_holds_the_tree_lock_is_not_started_again() {
        let (_env, store) = scratch("held-run");
        let w = list(&store, &[&["m-1"]]);
        let (_, _retake) = running_with(&store, Run { pid: 2_000_000_000, started: "x".into() });
        let try_it = |held: bool| open_unless_held(&store, &w, 1, None, None, Mechanical::Yes, &|_: &str| false, &|| true, &|| held);
        assert!(matches!(try_it(true), Opening::Held));
        assert_eq!(checks(&store, 1)[0].tries, 1, "nothing was written");
        assert!(run_died(&store.worklist("run").unwrap().groups()[0]));
        assert!(matches!(try_it(false), Opening::Opened(o) if o.retaken && o.died == Some(2_000_000_000) && o.tries == 2));
    }

    /// A copy that did start has put its own pid on the entry; the lock is the
    /// first run's, and the guard asks the lock, not the pid.
    #[test]
    fn a_copys_pid_on_the_entry_does_not_hide_the_first_runs_lock() {
        let (_env, store) = scratch("copy-pid");
        let w = list(&store, &[&["m-1"]]);
        let lock = _env.state().join("lock");
        let (_, _retake) = running_with(&store, Run { pid: 2_000_000_000, started: "x".into() });
        let first = crate::check::TreeLock::take(lock.clone(), std::time::Duration::from_secs(1)).unwrap();
        change(&store, "run", 1, |g| {
            g.checks.last_mut().unwrap().run = Some(Run { pid: 2_000_000_001, started: "y".into() });
            true
        });
        let held = || crate::check::TreeLock::live(&lock);
        let t = |h: &dyn Fn() -> bool| open_unless_held(&store, &w, 1, None, None, Mechanical::Yes, &|_: &str| false, &|| true, h);
        assert!(matches!(t(&held), Opening::Held));
        drop(first);
        assert!(matches!(t(&held), Opening::Opened(_)));
    }

    #[test]
    fn this_process_is_not_gone_and_a_pid_nothing_has_is() {
        let me = Run::this().unwrap();
        assert!(!me.gone());
        let none = Run { pid: 4_000_000, started: me.started.clone() };
        assert!(none.gone());
    }

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        crate::testkit::isolated_store(&format!("barrier-{tag}"))
    }

    fn list(store: &Store, groups: &[&[&str]]) -> Worklist {
        let mut w = Worklist::new("run", "r");
        let gs: Vec<Group> =
            groups.iter().map(|m| Group { members: m.iter().map(|s| s.to_string()).collect(), ..Group::default() }).collect();
        w.set_groups(&gs);
        w.set_status(crate::model::WorklistStatus::Running);
        store.save_worklist(&w).unwrap();
        w
    }

    fn checks(store: &Store, at: usize) -> Vec<Check> {
        store.worklist("run").unwrap().groups()[at - 1].checks.clone()
    }

    /// Every clause comes back as it was written, through the group's own file
    /// and a wrap: the block is tokens, so where the writer breaks the line
    /// cannot split an entry, and a verdict above it that wraps onto a line
    /// beginning `- ` is not read as a group (`wsp-232`).
    #[test]
    fn a_check_reads_back_as_it_was_written_through_the_groups_file_and_a_wrap() {
        let full = Check {
            at: "2026-10-07T12:04:11Z".into(),
            state: State::Next,
            read: Some("55c4839abcdef0123456789abcdef0123456789a".into()),
            pane: Some("cpd-484".into()),
            on: Some("opencode openrouter/some-model high".into()),
            ending: Ending::Failed,
            was: Some("compound-369".into()),
            tries: 3,
            gave_up: true,
            suite: Some(Suite::Red),
            run: Some(Run { pid: 25037, started: "Tue_Nov_4_11:19:05_2026".into() }),
            by: Some("wsp".into()),
        };
        let bare = Check::opened(None, None);
        let g = Group {
            members: vec!["m-1".into(), "m-2".into()],
            verdict: "passes:\n- Keystrokes reach the pane\n- the goldens hold".into(),
            checks: vec![full.clone(), bare.clone(), full.clone()],
            ..Group::default()
        };
        let text = render_groups(&[g.clone(), Group { members: vec!["m-3".into()], ..Group::default() }]);
        assert!(text.contains("  checks: 2026-10-07T12:04:11Z next"), "{text}");
        assert!(text.lines().count() > 6, "and long enough to wrap: {text}");
        let back = parse_groups(&text);
        assert_eq!(back.len(), 2, "no group made of a check's words: {text}");
        assert_eq!(back[0].checks, vec![full.clone(), bare, full], "{text}");
        assert_eq!(back[0].checks[0].on.as_deref(), Some("opencode openrouter/some-model high"), "a model's own `/` survives");
        assert_eq!(back[1].members, vec!["m-3"]);
    }

    /// The mechanical run is on the entry and nowhere else: opened with a
    /// suite running it is not retaken at the ten minutes a bare start is, a
    /// finished one is retaken for its start alone, and one that never began
    /// is run when it is retaken.
    #[test]
    fn an_entry_with_a_suite_running_waits_longer_and_a_finished_one_only_retries_its_start() {
        let (_env, store) = scratch("suite");
        let w = list(&store, &[&["m-1"]]);
        let never = |_: &str| false;
        let open_mech = |m| open_within(&store, &store.worklist("run").unwrap(), 1, None, None, m, &never, &|| true);

        assert!(open_mech(Mechanical::Yes).is_some());
        assert_eq!(suite_of(&store, "run", 1), Some(Suite::Running));
        assert!(open_mech(Mechanical::Yes).is_none(), "one entry is the key");

        let aged = |secs: i64, f: &dyn Fn(&mut Check)| {
            change(&store, "run", 1, |g| {
                let c = g.checks.last_mut().unwrap();
                c.at = util::iso_at(util::epoch_secs() - secs);
                f(c);
                true
            });
        };
        aged(30 * 60, &|_| {});
        assert!(open_mech(Mechanical::Yes).is_none(), "a suite in flight is believed for hours, not minutes");
        assert!(!would_open(&store.worklist("run").unwrap().groups()[0], &never));

        aged(4 * 60 * 60, &|_| {});
        assert!(would_open(&store.worklist("run").unwrap().groups()[0], &never));
        let o = open_mech(Mechanical::Yes).expect("a suite that never finished is taken again");
        assert!(o.retaken && o.suite == Some(Suite::Running));

        assert!(note_run(&store, "run", 1, Suite::Green, None));
        aged(20 * 60, &|_| {});
        let o = open_mech(Mechanical::Yes).expect("a finished suite whose start never arrived");
        assert!(o.retaken && o.suite == Some(Suite::Green), "so its start is retried and the suite is not");
        assert_eq!(suite_of(&store, "run", 1), Some(Suite::Green));
        drop(w);
    }

    /// `wsp-369`: a barrier run killed by hand leaves `suite:running`; the entry
    /// names the run's process, and a gone one is retaken at once.
    fn running_with(store: &Store, run: Run) -> (Option<Opened>, impl Fn() -> Option<Opened> + '_) {
        let never = |_: &str| false;
        assert!(open_within(store, &store.worklist("run").unwrap(), 1, None, None, Mechanical::Yes, &never, &|| true).is_some());
        change(store, "run", 1, |g| {
            g.checks.last_mut().unwrap().run = Some(run);
            true
        });
        (None, move || open_within(store, &store.worklist("run").unwrap(), 1, None, None, Mechanical::Yes, &|_: &str| false, &|| true))
    }

    #[test]
    fn a_run_whose_process_is_gone_is_retaken_at_once_and_counts_a_try() {
        let (_env, store) = scratch("dead-run");
        let _w = list(&store, &[&["m-1"]]);
        let (_, retake) = running_with(&store, Run { pid: 2_000_000_000, started: "Tue_Nov_4_11:19:05_2026".into() });
        let g = store.worklist("run").unwrap().groups()[0].clone();
        assert!(run_died(&g) && would_open(&g, &|_| false));
        let o = retake().expect("a dead run is not waited out");
        assert!(o.retaken && o.died == Some(2_000_000_000) && o.suite == Some(Suite::Running));
        assert_eq!(o.tries, 2);
        assert_eq!(suite_of(&store, "run", 1), Some(Suite::Running));
        assert!(retake().is_none(), "the new entry names no process yet and is believed");
    }

    #[test]
    fn a_run_that_dies_every_time_stops_at_the_tries() {
        let (_env, store) = scratch("dead-run-tries");
        let _w = list(&store, &[&["m-1"]]);
        let dead = Run { pid: 2_000_000_000, started: "x".into() };
        let (_, retake) = running_with(&store, dead.clone());
        let mut n = 1;
        while let Some(o) = retake() {
            n = o.tries;
            change(&store, "run", 1, |g| {
                g.checks.last_mut().unwrap().run = Some(dead.clone());
                true
            });
            assert!(n <= TRIES);
        }
        assert_eq!(n, TRIES, "a loop would have gone on");
    }

    #[test]
    fn a_run_whose_process_is_alive_is_left_alone() {
        let (_env, store) = scratch("live-run");
        let _w = list(&store, &[&["m-1"]]);
        let (_, retake) = running_with(&store, Run::this().expect("ps reads this process"));
        let g = store.worklist("run").unwrap().groups()[0].clone();
        assert!(!run_died(&g) && !would_open(&g, &|_| false));
        assert!(retake().is_none());
    }

    #[test]
    fn a_pid_that_has_been_reused_is_a_dead_run() {
        let (_env, store) = scratch("reused-run");
        let _w = list(&store, &[&["m-1"]]);
        let mut other = Run::this().expect("ps reads this process");
        other.started = "Mon_Jan_1_00:00:00_2001".into();
        let (_, retake) = running_with(&store, other);
        assert!(run_died(&store.worklist("run").unwrap().groups()[0]));
        assert!(retake().is_some_and(|o| o.died == Some(std::process::id())));
    }

    #[test]
    fn the_pid_is_written_and_read_back_and_dropped_when_the_run_has_a_result() {
        let (_env, store) = scratch("pid-text");
        let _w = list(&store, &[&["m-1"]]);
        let (_, _retake) = running_with(&store, Run { pid: 25037, started: "Tue_Nov_4_11:19:05_2026".into() });
        let g = store.worklist("run").unwrap().groups()[0].clone();
        let t = text(&g.checks);
        assert!(t.contains("suite:running pid:25037@Tue_Nov_4_11:19:05_2026"), "{t}");
        let mut back = Vec::new();
        push(&mut back, &t);
        assert_eq!(back, g.checks);
        assert!(note_run(&store, "run", 1, Suite::Green, None));
        assert!(!text(&store.worklist("run").unwrap().groups()[0].checks).contains("pid:"));
    }

    #[test]
    fn a_pass_by_wsp_is_on_the_entry_and_a_claim_can_be_taken_back() {
        let (_env, store) = scratch("by");
        let w = list(&store, &[&["m-1"]]);
        assert!(open_within(&store, &w, 1, None, None, Mechanical::Yes, &|_: &str| false, &|| true).is_some());
        assert!(note_run(&store, "run", 1, Suite::Green, Some("wsp")));
        let c = &checks(&store, 1)[0];
        assert_eq!((c.suite, c.by.as_deref()), (Some(Suite::Green), Some("wsp")));
        assert!(note_run(&store, "run", 1, Suite::Green, Some("")));
        assert_eq!(checks(&store, 1)[0].by, None);
        assert_eq!(settle(&store, "run", 1, State::Passes), None, "no seat to end");
        let text = text(&checks(&store, 1));
        assert!(text.contains("passes") && text.contains("suite:green") && !text.contains("by:"), "{text}");
    }

    /// The lifecycle the run walks: an entry opens before any agent exists,
    /// is the key that stops a second (until it is wedged), takes its seat,
    /// is settled by the verb that decides, and is ended — and only then is
    /// its seat no longer one `wip` and the brief name as a check.
    #[test]
    fn a_check_opens_once_takes_its_seat_is_settled_and_is_ended() {
        let (_env, store) = scratch("life");
        let w = list(&store, &[&["m-1"], &["m-2"]]);
        let never = |_: &str| false;

        assert!(open(&store, &w, 1, Some("abc1234".into()), Some("claude opus high".into()), &never).is_some());
        assert!(open(&store, &w, 1, None, None, &never).is_none(), "one open entry is the key");
        assert!(seat_of(&store, "cpd-1").is_none(), "no seat yet");
        assert!(name_seat(&store, "run", 1, "cpd-1"));
        assert_eq!(seat_of(&store, "cpd-1"), Some(("run".into(), 1)));
        assert_eq!(checks(&store, 1)[0].read.as_deref(), Some("abc1234"), "the commit it was opened against");

        assert_eq!(settle(&store, "run", 1, State::Blocking), Some("cpd-1".into()), "the seat, for its ending");
        assert_eq!(settle(&store, "run", 1, State::Passes), None, "nothing open is left to settle");
        assert_eq!(seat_of(&store, "cpd-1"), Some(("run".into(), 1)), "settled, and still seated until it is ended");

        assert!(open(&store, &w, 1, None, None, &never).is_some(), "a re-check is a fresh entry");
        let c = checks(&store, 1);
        assert_eq!(c.iter().map(|c| c.state).collect::<Vec<_>>(), [State::Blocking, State::Running], "beside the first");

        assert!(mark_ended(&store, "cpd-1", true));
        assert!(seat_of(&store, "cpd-1").is_none(), "an ended check is nobody's seat");
        assert_eq!(checks(&store, 1)[0].ending, Ending::Ended);
        assert!(checks(&store, 2).is_empty(), "and nothing touched the next group");
    }

    /// A Barrier row, filed the way wsp filed them.
    fn row(store: &Store, id: &str, at: usize, created: &str, status: Status, log: &str) -> Task {
        let mut t = Task::new(&crate::cycle::barrier_title("run", at), id);
        t.tags = vec![crate::cycle::BARRIER_TAG.into()];
        t.created = created.into();
        t.status_raw = status.as_str().into();
        t.body = format!(
            "## Overview\n{ORDER_OPENS}{at} of the `run` worklist. Every member has landed.\n\n## Log\n{log}\n"
        );
        store.save_task(&t).unwrap();
        t
    }

    /// `wsp-230`'s migration, shaped on gpui-migration: every settled row wsp
    /// filed goes onto its group, oldest first, with what it decided read off
    /// what the run recorded; the open one and every row a person wrote are
    /// left alone and named; a dry run moves nothing; and the old ids still
    /// resolve, from the archive.
    #[test]
    fn the_migration_moves_every_settled_row_wsp_filed_and_no_hand_made_one() {
        let (_env, store) = scratch("migrate");
        let mut w = list(&store, &[&["m-1"], &["m-2"], &["m-3"]]);
        let mut gs = w.groups();
        gs[0].verdict = "2026-10-07T09:37:36Z go".into();
        gs[1].verdict = "2026-10-07T12:04:11Z passes, with follow-ups m-9 as group 3".into();
        w.set_groups(&gs);
        crate::log::note_to_list(&store, &mut w, "follow-up by c-2 passes: m-9 · group 3 (new), ahead of 1 planned").unwrap();

        row(&store, "c-1", 1, "2026-10-07T09:00:00Z", Status::Review, "- 2026-10-07 claimed by pane cpd-480 · spawned at opus/high");
        row(&store, "c-2", 2, "2026-10-07T11:51:51Z", Status::Review, "- 2026-10-07 claimed by pane cpd-481");
        row(&store, "c-3", 3, "2026-10-07T12:33:20Z", Status::Doing, "");
        store.set_claim("c-3", serde_json::json!({ "workspace": "w" }));
        // Hand-made: tagged without wsp's order, and titled without the tag.
        let mut by_hand = Task::new("Barrier review: run group 1, by a person", "c-8");
        by_hand.tags = vec![crate::cycle::BARRIER_TAG.into()];
        store.save_task(&by_hand).unwrap();
        store.save_task(&Task::new("Barrier 4 re-check: read the goldens", "c-9")).unwrap();

        let p = plan(&store);
        let named: Vec<&str> = p.moves.values().flatten().map(|t| t.id.as_str()).collect();
        assert_eq!(named, ["c-1", "c-2"], "every settled row wsp filed");
        assert_eq!(p.open, ["c-3"], "the one still reading is left for later");
        assert_eq!(p.by_hand, ["c-8", "c-9"], "and nothing a person wrote");

        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("barrier-rows", "true"), ("n", "true")])), 0);
        assert!(store.find_task("c-1").is_some() && checks(&store, 1).is_empty(), "-n moves nothing");

        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("barrier-rows", "true")])), 0);
        let one = checks(&store, 1);
        assert_eq!(one.len(), 1);
        assert_eq!((one[0].state, one[0].was.as_deref()), (State::Passes, Some("c-1")), "a verdict and no follow-up: it passed");
        assert_eq!(one[0].pane.as_deref(), Some("cpd-480"));
        assert_eq!(one[0].on.as_deref(), Some("opus/high"));
        assert_eq!(one[0].ending, Ending::Ended);
        assert_eq!(checks(&store, 2)[0].state, State::Next, "the log names it as passing with follow-ups");
        assert!(store.find_task("c-1").is_none() && store.archived_task("c-1").is_some(), "archived, and still resolvable");
        assert!(store.find_task("c-3").is_some() && store.find_task("c-8").is_some(), "the rest untouched");

        assert_eq!(migrate(&store, &Args::synth("migrate", &[], &[("barrier-rows", "true")])), 0);
        assert_eq!(checks(&store, 1).len(), 1, "and a second run adds nothing");
    }
}
