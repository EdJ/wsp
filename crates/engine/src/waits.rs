//! What a planned list waits on (`wsp-383`): one answer, for each planned
//! list, that is either "may start" or the first reason it may not.
//!
//! Second of three pieces of `wsp-381`, and it **computes and shows; it starts
//! nothing**. The daemon that acts on it is the third piece; `wsp worklist ls`
//! and `wsp worklist next <slug>` are the readers now.
//!
//! # The reasons, in the order the first is told
//!
//! 1. **room** — the seat's agent cap (`wsp-231`), counted as `wsp-353` counts
//!    it ([`crate::room`]: seats holding work only), against what the list's
//!    first group would take: its `--parallel` (or its members) plus one
//!    verifier. No cap is room enough.
//! 2. **throttle** — the load warning `wsp throttle` shows: the 1- and
//!    5-minute averages both over 1.5 times the core count, which holds builds
//!    back today.
//! 3. **after** — the list it is `after` is not yet complete (`complete` or
//!    `closed`).
//! 4. **alone** — a list on the same project is executing. On by default for
//!    wsp's own source, and `--alone` / `--no-alone` on the list say otherwise.
//!    An executing list that waits on a person (a manual group, or a question
//!    only the person can close) with no seat of its own working does not count
//!    as executing (`wsp-435`); it counts again when a seat of it starts.
//! 5. **manual group** — its next group has no `agent:` line, so wsp would not
//!    run it.
//!
//! # The queue
//!
//! `--prio high|normal|low` first, then the time the list was planned, then
//! the slug. **A list is judged on its own**: one blocked on `after` is
//! passed over and does not hold back a later one that may start. What the
//! lists ahead of it that *may* start do take is room — they are going to
//! seat their agents — so a later list is told the room that is left.
//!
//! The decision is [`queue`], a function of plain values, so each reason and
//! the ordering is a test that touches no store. [`read`] gathers the values.

use std::collections::BTreeSet;

use crate::model::Worklist;
use crate::store::Store;
use crate::util;
use crate::worklist::{self, ListStatus, Listed};

/// The project whose lists default to `alone`: wsp's own source, where two
/// runs in one trunk is two runs rebasing over each other's changes.
pub const OWN_SOURCE: &str = "wsp";

/// Where a list stands in the queue, before its slug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prio {
    High,
    Normal,
    Low,
}

impl Prio {
    pub fn parse(word: &str) -> Option<Prio> {
        match word.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Prio::High),
            "normal" | "default" => Some(Prio::Normal),
            "low" => Some(Prio::Low),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Prio::High => "high",
            Prio::Normal => "normal",
            Prio::Low => "low",
        }
    }

    /// A list's priority as it is stored: absent is normal.
    pub fn of(w: &Worklist) -> Prio {
        w.prio.as_deref().and_then(Prio::parse).unwrap_or(Prio::Normal)
    }
}

/// Why a planned list may not start, the first reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// This many more places than there are.
    Room { short: usize },
    /// The machine is held back: which reading, and where it lets go.
    Throttle(crate::throttle::Pause),
    /// The list it is `after` is not complete yet.
    After(String),
    /// A list on the same project is executing, and this one runs alone.
    Alone(String),
    /// Its next group has no `agent:` line.
    Manual,
    /// It has no groups to run.
    Empty,
}

impl Wait {
    /// The sentence `ls` and `next` print.
    pub fn text(&self) -> String {
        match self {
            Wait::Room { short: 1 } => "room: 1 slot short".into(),
            Wait::Room { short } => format!("room: {short} slots short"),
            Wait::Throttle(hold) => hold.text(),
            Wait::After(other) => format!("after {other}"),
            Wait::Alone(other) => format!("alone: {other} executing"),
            Wait::Manual => "manual group".into(),
            Wait::Empty => "no groups".into(),
        }
    }
}

/// One planned list, as the decision reads it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub prio: Prio,
    /// When it was planned, ISO; a list planned by hand-editing the file has
    /// none and queues behind every one that has.
    pub planned: Option<String>,
    /// Every list this one waits on; it starts when all are complete.
    pub after: Vec<String>,
    pub alone: bool,
    /// What `alone` is judged against: the project, or [`OWN_SOURCE`] for
    /// anything under it. `None` for a list with no home, which conflicts with
    /// nothing.
    pub scope: Option<String>,
    /// What the list needs free to start. One, unless its first group is
    /// marked `together` (`wsp-584`): then the members (up to its cap) at once
    /// and a verifier. A group not marked starts its members as places free.
    pub need: usize,
    /// How many members the first group runs at once as places free: its
    /// members up to its cap.
    pub width: usize,
    /// The first group asked to be seated all at once.
    pub together: bool,
    pub empty: bool,
    pub manual: bool,
}

/// The machine, as the decision reads it.
#[derive(Debug, Clone, Default)]
pub struct World {
    /// Places free under the seat's cap; `None` where there is no cap.
    pub free: Option<usize>,
    /// What holds new work back, if anything.
    pub throttled: Option<crate::throttle::Pause>,
    /// Lists complete or closed.
    pub complete: BTreeSet<String>,
    /// (list, scope) of every executing list.
    pub executing: Vec<(String, Option<String>)>,
    /// The executing lists that are waiting on a person (their next group is
    /// manual, or a question only the person can close is open) with no seat of
    /// their own working. They are in `executing` and **do not hold `alone`**
    /// (`wsp-435`): a seat of theirs starting takes them out of here again.
    pub waiting_on_ed: BTreeSet<String>,
}

/// The queue order: priority, then when planned, then slug.
fn order(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    let at = |e: &Entry| e.planned.as_deref().map_or(i64::MAX, util::epoch_of);
    a.prio.cmp(&b.prio).then(at(a).cmp(&at(b))).then(a.id.cmp(&b.id))
}

/// Every planned list in queue order, each with `None` (may start) or the
/// first thing it waits on.
pub fn queue(mut entries: Vec<Entry>, world: &World) -> Vec<(String, Option<Wait>)> {
    entries.sort_by(order);
    let mut free = world.free;
    let mut out = Vec::new();
    for e in entries {
        let wait = first_wait(&e, world, free);
        if wait.is_none() {
            free = free.map(|f| f.saturating_sub(e.need));
        }
        out.push((e.id, wait));
    }
    out
}

fn first_wait(e: &Entry, world: &World, free: Option<usize>) -> Option<Wait> {
    if let Some(free) = free {
        if e.need > free {
            return Some(Wait::Room { short: e.need - free });
        }
    }
    if let Some(hold) = world.throttled {
        return Some(Wait::Throttle(hold));
    }
    let open: Vec<&str> = e.after.iter().filter(|o| !world.complete.contains(*o)).map(String::as_str).collect();
    if !open.is_empty() {
        return Some(Wait::After(open.join(", ")));
    }
    if e.alone {
        if let Some((other, _)) = world
            .executing
            .iter()
            .find(|(id, scope)| *id != e.id && scope.is_some() && *scope == e.scope && !world.waiting_on_ed.contains(id)) {
            return Some(Wait::Alone(other.clone()));
        }
    }
    if e.empty {
        return Some(Wait::Empty);
    }
    if e.manual {
        return Some(Wait::Manual);
    }
    None
}

// ---- reading the store -------------------------------------------------

/// The scope `alone` is judged in: the list's home project, with everything
/// under [`OWN_SOURCE`] counting as it.
fn scope_of(store: &Store, w: &Worklist) -> Option<String> {
    let home = worklist::home(store, w)?.project;
    let projects = store.projects();
    let mut at = Some(home.clone());
    let mut hops = 0;
    while let Some(id) = at {
        if id == OWN_SOURCE {
            return Some(OWN_SOURCE.into());
        }
        hops += 1;
        if hops > 32 {
            break;
        }
        at = projects.iter().find(|p| p.id == id).and_then(|p| p.parent.clone());
    }
    Some(home)
}

fn entry_of(store: &Store, l: &Listed, seat_cap: Option<usize>) -> Entry {
    let w = &l.list;
    let groups = w.groups();
    let scope = scope_of(store, w);
    let width = groups.first().map_or(0, |g| {
        let members = g.members.len();
        g.cap.map_or(members, |c| c.min(members))
    });
    let together = groups.first().is_some_and(|g| g.together);
    let need = groups.first().map_or(0, |g| {
        let want = if g.together { width + 1 } else { 1 };
        seat_cap.map_or(want, |c| want.min(c))
    });
    let next = groups.get(l.at.at.unwrap_or(1).saturating_sub(1)).or(groups.first());
    Entry {
        id: w.id.clone(),
        prio: Prio::of(w),
        planned: w.planned.clone(),
        after: w.after.clone(),
        alone: w.alone.unwrap_or(scope.as_deref() == Some(OWN_SOURCE)),
        scope,
        need,
        width,
        together,
        empty: groups.is_empty(),
        manual: next.is_some_and(|g| g.policy().is_none()),
    }
}

/// Places free under the seat's cap, counted as the run counts them.
fn free_places(store: &Store) -> Option<usize> {
    let gate = crate::room::Gate::new(store, &crate::cycle::Fleet);
    gate.free_places()
}

/// What the machine says right now, the two readings [`queue`] cannot get from
/// the store: the places free under the seat's cap, and the load warning.
/// Separate from [`read_on`] so a test can say what the machine says.
#[derive(Debug, Clone, Copy, Default)]
pub struct Machine {
    /// Places free under the cap; `None` where there is no cap.
    pub free: Option<usize>,
    /// The cap itself, for the sentence that says what room there was.
    pub cap: Option<usize>,
    pub throttled: Option<crate::throttle::Pause>,
}

impl Machine {
    /// The machine as it is: the seats counted as `wsp-353` counts them, and
    /// `wsp throttle`'s own answer.
    pub fn now(store: &Store) -> Machine {
        // `holding_back` guards on `cfg!(test)` inside the trees crate, which is not under
        // test when this crate's tests run: on a throttled machine the live reading
        // held every planned list. The guard is asked here, in the crate under test.
        Machine { free: free_places(store), cap: store.seat_cap(), throttled: throttled(store) }
    }

    /// [`Machine::now`], with the working agents counted by `working`, which
    /// is asked only where there is a cap: [`read_on`] counts them once for
    /// this and for [`idle_on_ed`] (wsp-767), where each had read every
    /// record and asked every seat's backend on its own.
    fn counted(store: &Store, working: &dyn Fn() -> Vec<crate::room::Counted>) -> Machine {
        let cap = store.seat_cap();
        let free = cap.and_then(|_| crate::room::Gate::free_of(cap, &working()));
        Machine { free, cap, throttled: throttled(store) }
    }
}

fn throttled(store: &Store) -> Option<crate::throttle::Pause> {
    (!cfg!(test)).then(|| crate::throttle::holding_back(store)).flatten()
}

/// One planned list in queue order: whether it may start, and what its first
/// group would take, which is what the room was judged against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub wait: Option<Wait>,
    pub need: usize,
    /// Members of the first group that run at once as places free.
    pub width: usize,
    /// The first group is marked `together`, so `need` is all of it at once.
    pub together: bool,
}

/// The planned lists among `listed`, in queue order, each with what it waits
/// on. Reads the seats only when there is a planned list to answer for.
pub fn read(store: &Store, listed: &[Listed]) -> Vec<(String, Option<Wait>)> {
    read_on(store, listed, None).into_iter().map(|r| (r.id, r.wait)).collect()
}

/// [`read`] with each list's need beside its answer, and the machine given
/// where the caller has it already (`None` reads it). The daemon's pickup is
/// the other caller, and it needs the need to say how much room there was.
pub fn read_on(store: &Store, listed: &[Listed], machine: Option<Machine>) -> Vec<Row> {
    let planned: Vec<&Listed> = listed.iter().filter(|l| l.phase(store).status == ListStatus::Planned).collect();
    if planned.is_empty() {
        return Vec::new();
    }
    let seat_cap = store.seat_cap();
    let entries: Vec<Entry> = planned.iter().map(|l| entry_of(store, l, seat_cap)).collect();
    // One count of the working agents, taken when first asked, for the
    // room and for the lists waiting on a person.
    let counted: std::cell::OnceCell<Vec<crate::room::Counted>> = std::cell::OnceCell::new();
    let working = || counted.get_or_init(|| crate::room::working_now(store, &crate::cycle::Fleet)).clone();
    let machine = machine.unwrap_or_else(|| Machine::counted(store, &working));
    let mut world = World { free: machine.free, throttled: machine.throttled, ..World::default() };
    for l in listed {
        match l.phase(store).status {
            ListStatus::Complete | ListStatus::Closed => {
                world.complete.insert(l.list.id.clone());
            }
            ListStatus::Executing => world.executing.push((l.list.id.clone(), scope_of(store, &l.list))),
            _ => {}
        }
    }
    world.waiting_on_ed = idle_on_ed(store, listed, &working);
    let need: std::collections::BTreeMap<String, (usize, usize, bool)> =
        entries.iter().map(|e| (e.id.clone(), (e.need, e.width, e.together))).collect();
    queue(entries, &world)
        .into_iter()
        .map(|(id, wait)| {
            let (need, width, together) = need.get(&id).copied().unwrap_or((0, 0, false));
            Row { need, width, together, id, wait }
        })
        .collect()
}

/// The executing lists that wait on a person and have no seat of their own
/// working (`wsp-435`). Reads the seats only when there is such a list.
fn idle_on_ed(store: &Store, listed: &[Listed], working: &dyn Fn() -> Vec<crate::room::Counted>) -> BTreeSet<String> {
    let waiting: Vec<&Listed> = listed
        .iter()
        .filter(|l| {
            let p = l.phase(store);
            p.status == ListStatus::Executing && p.flags.waiting_on_ed && !p.flags.parked
        })
        .collect();
    if waiting.is_empty() {
        return BTreeSet::new();
    }
    let working = working();
    waiting.into_iter().filter(|l| !has_a_seat_working(&l.list, &working)).map(|l| l.list.id.clone()).collect()
}

/// Whether one of the working agents is this list's: a member's, by the task it
/// holds or its verifier's or leg's label, or a barrier check of the list.
fn has_a_seat_working(w: &Worklist, working: &[crate::room::Counted]) -> bool {
    let groups = w.groups();
    working.iter().any(|c| {
        let head = c.what.split(" · ").next().unwrap_or(&c.what);
        head == w.id || groups.iter().any(|g| g.members.iter().any(|m| m == head))
    })
}

impl Wait {
    /// What the wait is, without its numbers: `room`, `after pre-prep`. The
    /// pickup records this, so the line it tells when the list starts can say
    /// what it had been waiting on without a different count in it every tick.
    pub fn kind(&self) -> String {
        match self {
            Wait::Room { .. } => "room".into(),
            Wait::Throttle(_) => "throttle".into(),
            Wait::After(other) => format!("after {other}"),
            Wait::Alone(other) => format!("alone with {other}"),
            Wait::Manual => "manual group".into(),
            Wait::Empty => "no groups".into(),
        }
    }
}

/// The line `ls` and `next` print after a planned list: `may start`, or what
/// it waits on.
pub fn line(wait: &Option<Wait>) -> String {
    match wait {
        None => "may start".into(),
        Some(w) => w.text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> Entry {
        Entry {
            id: id.into(),
            prio: Prio::Normal,
            planned: Some("2026-11-05T10:00:00Z".into()),
            after: Vec::new(),
            alone: false,
            scope: Some("proj".into()),
            need: 3,
            width: 2,
            together: true,
            empty: false,
            manual: false,
        }
    }

    fn world() -> World {
        World { free: Some(8), ..World::default() }
    }

    fn only(e: Entry, w: &World) -> Option<Wait> {
        queue(vec![e], w).remove(0).1
    }

    #[test]
    fn a_list_with_nothing_in_its_way_may_start() {
        assert_eq!(only(entry("a"), &world()), None);
        assert_eq!(only(entry("a"), &World::default()), None, "no cap is room enough");
    }

    #[test]
    fn room_is_counted_against_the_first_group_and_a_verifier() {
        let mut w = world();
        w.free = Some(2);
        assert_eq!(only(entry("a"), &w), Some(Wait::Room { short: 1 }));
        w.free = Some(0);
        assert_eq!(only(entry("a"), &w).unwrap().text(), "room: 3 slots short");
        w.free = Some(3);
        assert_eq!(only(entry("a"), &w), None);
        assert_eq!(Wait::Room { short: 1 }.text(), "room: 1 slot short");
    }

    /// A first group not marked together needs one place to start (`wsp-584`);
    /// a together group still needs its whole width and a verifier.
    #[test]
    fn a_group_not_marked_together_needs_one_place_and_a_together_one_all_of_them() {
        let mut e = entry("a");
        e.need = 1;
        e.together = false;
        let mut w = world();
        w.free = Some(1);
        assert_eq!(only(e.clone(), &w), None, "one place of four is enough for an x2 group");
        assert_eq!(line(&only(e.clone(), &w)), "may start");
        w.free = Some(0);
        assert_eq!(only(e, &w), Some(Wait::Room { short: 1 }));
        w.free = Some(1);
        assert_eq!(only(entry("t"), &w).unwrap().text(), "room: 2 slots short", "together keeps its full need");
    }

    #[test]
    fn the_throttle_holds_a_list_back() {
        let mut w = world();
        w.throttled = Some(crate::throttle::sample_pause());
        assert_eq!(only(entry("a"), &w), Some(Wait::Throttle(crate::throttle::sample_pause())));
        assert!(only(entry("a"), &w).unwrap().text().contains("1-minute load is 14.2"));
    }

    #[test]
    fn a_list_waits_for_the_one_it_is_after_until_that_is_complete() {
        let mut e = entry("b");
        e.after = vec!["a".into()];
        let mut w = world();
        assert_eq!(only(e.clone(), &w).unwrap().text(), "after a");
        w.complete.insert("a".into());
        assert_eq!(only(e, &w), None);
    }

    #[test]
    fn a_list_after_several_waits_until_every_one_is_complete() {
        let mut e = entry("c");
        e.after = vec!["a".into(), "b".into()];
        let mut w = world();
        assert_eq!(only(e.clone(), &w).unwrap().text(), "after a, b");
        w.complete.insert("a".into());
        assert_eq!(only(e.clone(), &w), Some(Wait::After("b".into())), "one done is not enough");
        w.complete.insert("b".into());
        assert_eq!(only(e, &w), None);
    }

    #[test]
    fn alone_waits_for_an_executing_list_on_the_same_project_only() {
        let mut e = entry("b");
        e.alone = true;
        let mut w = world();
        w.executing.push(("a".into(), Some("other".into())));
        assert_eq!(only(e.clone(), &w), None, "another project is no obstacle");
        w.executing.push(("c".into(), Some("proj".into())));
        assert_eq!(only(e.clone(), &w).unwrap().text(), "alone: c executing");
        e.alone = false;
        assert_eq!(only(e.clone(), &w), None, "--no-alone shares the project");
        e.alone = true;
        e.scope = None;
        assert_eq!(only(e, &w), None, "a list with no home conflicts with nothing");
    }

    #[test]
    fn a_list_waiting_on_ed_does_not_hold_alone() {
        let mut e = entry("b");
        e.alone = true;
        let mut w = world();
        w.executing.push(("c".into(), Some("proj".into())));
        assert_eq!(only(e.clone(), &w).unwrap().text(), "alone: c executing");
        w.waiting_on_ed.insert("c".into());
        assert_eq!(only(e.clone(), &w), None, "c waits on a person with no seat working");
        w.executing.push(("d".into(), Some("proj".into())));
        assert_eq!(only(e, &w).unwrap().text(), "alone: d executing", "another still holds it");
    }

    #[test]
    fn a_list_has_a_seat_working_when_a_member_barrier_or_verifier_of_it_is() {
        let mut w = Worklist::new("l", "l");
        let mut g = crate::model::Group::default();
        g.members = vec!["m-1".into()];
        w.set_groups(&[g]);
        let at = |what: &str| vec![crate::room::Counted::whole(what)];
        assert!(has_a_seat_working(&w, &at("m-1")));
        assert!(has_a_seat_working(&w, &at("m-1 · verifying")));
        assert!(has_a_seat_working(&w, &at("l · barrier 1")));
        assert!(!has_a_seat_working(&w, &at("m-2")));
        assert!(!has_a_seat_working(&w, &[]));
    }

    #[test]
    fn a_manual_next_group_and_an_empty_list_are_waits() {
        let mut e = entry("a");
        e.manual = true;
        assert_eq!(only(e, &world()), Some(Wait::Manual));
        let mut e = entry("a");
        e.empty = true;
        assert_eq!(only(e, &world()).unwrap().text(), "no groups");
    }

    #[test]
    fn the_first_reason_is_told_in_the_order_room_throttle_after_alone_manual() {
        let mut e = entry("b");
        e.after = vec!["a".into()];
        e.alone = true;
        e.manual = true;
        let mut w = world();
        w.executing.push(("c".into(), Some("proj".into())));
        assert_eq!(only(e.clone(), &w), Some(Wait::After("a".into())));
        w.complete.insert("a".into());
        assert_eq!(only(e.clone(), &w), Some(Wait::Alone("c".into())));
        w.executing.clear();
        assert_eq!(only(e.clone(), &w), Some(Wait::Manual));
        w.throttled = Some(crate::throttle::sample_pause());
        assert_eq!(only(e.clone(), &w), Some(Wait::Throttle(crate::throttle::sample_pause())));
        w.free = Some(0);
        assert_eq!(only(e, &w), Some(Wait::Room { short: 3 }));
    }

    #[test]
    fn priority_beats_when_planned_and_then_the_earlier_plan_goes_first() {
        let mut early = entry("early");
        early.planned = Some("2026-11-05T09:00:00Z".into());
        let mut late = entry("late");
        late.planned = Some("2026-11-05T11:00:00Z".into());
        let mut urgent = entry("urgent");
        urgent.prio = Prio::High;
        urgent.planned = Some("2026-11-05T12:00:00Z".into());
        let mut idle = entry("idle");
        idle.prio = Prio::Low;
        idle.planned = Some("2026-11-04T00:00:00Z".into());
        let ids: Vec<String> =
            queue(vec![idle, late, urgent, early], &world()).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["urgent", "early", "late", "idle"]);
    }

    #[test]
    fn an_earlier_list_blocked_on_after_does_not_hold_back_a_later_one() {
        let mut first = entry("first");
        first.planned = Some("2026-11-05T09:00:00Z".into());
        first.after = vec!["never".into()];
        let mut second = entry("second");
        second.planned = Some("2026-11-05T10:00:00Z".into());
        let got = queue(vec![second, first], &world());
        assert_eq!(got[0], ("first".into(), Some(Wait::After("never".into()))));
        assert_eq!(got[1], ("second".into(), None));
    }

    #[test]
    fn a_list_that_may_start_takes_the_room_the_next_is_told_of() {
        let mut first = entry("first");
        first.planned = Some("2026-11-05T09:00:00Z".into());
        let mut second = entry("second");
        second.planned = Some("2026-11-05T10:00:00Z".into());
        let mut w = world();
        w.free = Some(4);
        let got = queue(vec![first, second], &w);
        assert_eq!(got[0].1, None);
        assert_eq!(got[1].1, Some(Wait::Room { short: 2 }));
    }

    #[test]
    fn priorities_are_read_by_word_and_default_to_normal() {
        assert_eq!(Prio::parse("HIGH"), Some(Prio::High));
        assert_eq!(Prio::parse("urgent"), None);
        assert_eq!(Prio::of(&Worklist::new("a", "A")), Prio::Normal);
    }
}
