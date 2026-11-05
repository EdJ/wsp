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
    /// The load is high and builds are held back.
    Throttle,
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
            Wait::Throttle => "throttle: the load holds builds back".into(),
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
    pub after: Option<String>,
    pub alone: bool,
    /// What `alone` is judged against: the project, or [`OWN_SOURCE`] for
    /// anything under it. `None` for a list with no home, which conflicts with
    /// nothing.
    pub scope: Option<String>,
    /// Agents its first group would take at once: members (up to its cap) and
    /// a verifier.
    pub need: usize,
    pub empty: bool,
    pub manual: bool,
}

/// The machine, as the decision reads it.
#[derive(Debug, Clone, Default)]
pub struct World {
    /// Places free under the seat's cap; `None` where there is no cap.
    pub free: Option<usize>,
    /// The load warning is up.
    pub throttled: bool,
    /// Lists complete or closed.
    pub complete: BTreeSet<String>,
    /// (list, scope) of every executing list.
    pub executing: Vec<(String, Option<String>)>,
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
    if world.throttled {
        return Some(Wait::Throttle);
    }
    if let Some(other) = e.after.as_ref().filter(|o| !world.complete.contains(*o)) {
        return Some(Wait::After(other.clone()));
    }
    if e.alone {
        if let Some((other, _)) = world.executing.iter().find(|(id, scope)| *id != e.id && scope.is_some() && *scope == e.scope) {
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
    let need = groups.first().map_or(0, |g| {
        let members = g.members.len();
        let running = g.cap.map_or(members, |c| c.min(members));
        let want = running + 1;
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
        empty: groups.is_empty(),
        manual: next.is_some_and(|g| g.policy().is_none()),
    }
}

/// Places free under the seat's cap, counted as the run counts them.
fn free_places(store: &Store) -> Option<usize> {
    let gate = crate::room::Gate::new(store, &crate::cycle::Fleet);
    let cap = gate.cap?;
    Some(cap.saturating_sub(gate.working().len()))
}

/// The planned lists among `listed`, in queue order, each with what it waits
/// on. Reads the seats only when there is a planned list to answer for.
pub fn read(store: &Store, listed: &[Listed]) -> Vec<(String, Option<Wait>)> {
    let planned: Vec<&Listed> = listed.iter().filter(|l| l.phase(store).status == ListStatus::Planned).collect();
    if planned.is_empty() {
        return Vec::new();
    }
    let seat_cap = store.seat_cap();
    let entries = planned.iter().map(|l| entry_of(store, l, seat_cap)).collect();
    let mut world = World { free: free_places(store), throttled: crate::throttle::holding_back(store), ..World::default() };
    for l in listed {
        match l.phase(store).status {
            ListStatus::Complete | ListStatus::Closed => {
                world.complete.insert(l.list.id.clone());
            }
            ListStatus::Executing => world.executing.push((l.list.id.clone(), scope_of(store, &l.list))),
            _ => {}
        }
    }
    queue(entries, &world)
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
            after: None,
            alone: false,
            scope: Some("proj".into()),
            need: 3,
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

    #[test]
    fn the_throttle_holds_a_list_back() {
        let mut w = world();
        w.throttled = true;
        assert_eq!(only(entry("a"), &w), Some(Wait::Throttle));
    }

    #[test]
    fn a_list_waits_for_the_one_it_is_after_until_that_is_complete() {
        let mut e = entry("b");
        e.after = Some("a".into());
        let mut w = world();
        assert_eq!(only(e.clone(), &w).unwrap().text(), "after a");
        w.complete.insert("a".into());
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
        e.after = Some("a".into());
        e.alone = true;
        e.manual = true;
        let mut w = world();
        w.executing.push(("c".into(), Some("proj".into())));
        assert_eq!(only(e.clone(), &w), Some(Wait::After("a".into())));
        w.complete.insert("a".into());
        assert_eq!(only(e.clone(), &w), Some(Wait::Alone("c".into())));
        w.executing.clear();
        assert_eq!(only(e.clone(), &w), Some(Wait::Manual));
        w.throttled = true;
        assert_eq!(only(e.clone(), &w), Some(Wait::Throttle));
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
        first.after = Some("never".into());
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
