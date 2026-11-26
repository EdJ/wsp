//! A governor seat with nothing left to govern is stood down. `wsp-337`.
//!
//! The compounding project's governor, cpd-2308, stayed seated with nothing
//! running under it: one more live `claude` holding its whole context while
//! the machine paged (`wsp-332`). Two seats, two rules.
//!
//! # A list's own seat: when its list is done
//!
//! [`crate::cycle::stand_down_own_seat`] already runs at the close, so a list
//! wsp runs leaves no seat behind it. This is the sweep behind that close, for
//! the list whose seat is filled again afterwards or was never reached: a done
//! list with somebody in its own post is stood down and ended once it is not
//! mid-turn. **Not while a rotation is in flight** — the successor is a live
//! agent on a slot that has not moved yet, and ending the post under it is the
//! reseat nobody asked for.
//!
//! # A project's seat: after a set idle time
//!
//! A project's seat outlives every run, so it is not ended with one. It is
//! ended when nothing has asked it anything for a while: no running or held
//! list with a member in the project (or under it), nothing in its
//! `wsp flag --seat` inbox, nothing owed it in its wake spool, and the agent
//! itself at a prompt. Any of those puts the clock back to zero; the clock is
//! `idle_since` on the governor record, so a daemon restart does not restart
//! it.
//!
//! **Vacated, and not marked stood down.** `stood_down` is a person's decision
//! that a post stays empty, and the reconciler honours it for ever. This is
//! the opposite: a new ask owes the scope an answer and the reconciler seats
//! it as it seats any vacant post with a backlog, and a list started by hand
//! under the project seats it as a running list's post. Nothing is lost by
//! ending it that a reseat does not bring back.
//!
//! **Never the primary interface**, which is the person's.
//!
//! The idle time is `idle_minutes` in `governor.json` under the state
//! directory, [`IDLE_MINUTES`] when absent. `0` turns the stand-down off.

use crate::cycle::Seats;
use crate::model::WorklistStatus;
use crate::place::State;
use crate::store::Store;
use crate::util;

/// How long a project's governor sits with nothing to do before it is ended.
pub const IDLE_MINUTES: u64 = 30;

/// The record's own key for when the seat was first found with nothing to do.
const IDLE_SINCE: &str = "idle_since";

pub fn idle_minutes(state: &std::path::Path) -> u64 {
    std::fs::read_to_string(state.join("governor.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("idle_minutes").and_then(serde_json::Value::as_u64))
        .unwrap_or(IDLE_MINUTES)
}

pub fn tick(store: &Store, seats: &dyn Seats) {
    tick_at(store, seats, util::epoch_secs(), idle_minutes(&store.state));
}

fn tick_at(store: &Store, seats: &dyn Seats, now: i64, idle_minutes: u64) {
    stand_down_done_lists(store);
    if idle_minutes > 0 {
        stand_down_idle_projects(store, seats, now, idle_minutes as i64 * 60);
    }
}

/// Whether a rotation into this scope's seat is in flight: a successor named,
/// and the handover not failed.
fn rotating(store: &Store, scope: &str) -> bool {
    store.handovers().get(scope).is_some_and(|rec| {
        let said = |k: &str| rec.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
        !said("to").is_empty() && said("failed").is_empty()
    })
}

fn stand_down_done_lists(store: &Store) {
    let governors = store.governors();
    for w in store.worklists().iter().filter(|w| w.status() == WorklistStatus::Done) {
        if crate::governors::seat_of_scope(&w.id, &governors).is_some() && !rotating(store, &w.id) {
            crate::cycle::stand_down_own_seat(store, &w.id);
        }
    }
}

/// Whether a running or held list has a member in this project or under it.
fn list_under(store: &Store, index: &crate::resolve::Index, project: &str) -> bool {
    store
        .worklists()
        .iter()
        .filter(|w| matches!(w.status(), WorklistStatus::Running | WorklistStatus::Held))
        .flat_map(|w| w.groups().into_iter().flat_map(|g| g.members))
        .filter_map(|m| store.find_task(&m).and_then(|t| t.project))
        .any(|p| p == project || index.ancestors(&p).iter().any(|a| a == project))
}

/// Whether a hand raised and still open is this seat's to answer.
fn flag_open(store: &Store, scope: &str) -> bool {
    crate::message::raised(store)
        .iter()
        .filter(|m| !m.is_reply())
        .filter_map(|m| m.about.task().and_then(|id| store.find_task(id)))
        .any(|t| Seats::answering_scope(crate::cycle::seats(), store, &t).is_some_and(|s| s == scope))
}

fn stand_down_idle_projects(store: &Store, seats: &dyn Seats, now: i64, idle_secs: i64) {
    let projects = store.projects();
    let index = crate::resolve::Index::new(projects.clone());
    let governors = store.governors();
    for (scope, rec) in governors.iter() {
        if crate::governors::is_primary(scope) || !projects.iter().any(|p| &p.id == scope) {
            continue;
        }
        let Some(seat) = crate::governors::seat_of_scope(scope, &governors) else { continue };
        // A seat that cannot be read is held, as everywhere: only an agent at
        // a prompt is idle.
        let at_a_prompt = seats.state(&seat.pane) == Some(State::Idle);
        let busy = !at_a_prompt
            || rotating(store, scope)
            || list_under(store, &index, scope)
            || Seats::owed_to(crate::cycle::seats(), store, scope) > 0
            || flag_open(store, scope);
        let since = util::epoch_of(rec.get(IDLE_SINCE).and_then(serde_json::Value::as_str).unwrap_or_default());
        if busy {
            if since > 0 {
                store.edit_governor(scope, |r| r.as_object_mut().is_some_and(|o| o.remove(IDLE_SINCE).is_some()));
            }
            continue;
        }
        if since <= 0 {
            store.edit_governor(scope, |r| {
                r.as_object_mut().map(|o| o.insert(IDLE_SINCE.into(), serde_json::json!(util::iso_at(now)))).is_some()
            });
            continue;
        }
        if now - since >= idle_secs {
            stand_down_project(store, scope, idle_secs / 60);
        }
    }
}

fn stand_down_project(store: &Store, scope: &str, minutes: i64) {
    let Some(seat) = crate::governors::seat_of_scope(scope, &store.governors()) else { return };
    // Read before the vacate, which empties the record the room is found by.
    let sitting = crate::cycle::occupied(store, &seat);
    crate::governors::vacate(store, scope);
    let said = |tail: &str| {
        crate::store::log_line(store, &format!("{scope}: nothing under its seat for {minutes} minutes — stood its seat down{tail}"))
    };
    let Some(pane) = sitting else { return said("; nobody was sitting in it") };
    match crate::cycle::end_seat_when_idle(store, &pane) {
        Ok(()) => said(&format!("; {pane} is ended once it is not mid-turn, and a new ask or list seats one again")),
        Err(why) => said(&format!(", but could not arrange {pane}'s ending: {why}. `wsp despawn --pane {pane} --keep-tree` ends it")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cycle::tests::ENDED;
    use crate::model::{Group, Status, Task, Worklist};
    use std::collections::BTreeMap;

    struct At(BTreeMap<String, State>);

    impl Seats for At {
        fn state(&self, seat: &str) -> Option<State> {
            self.0.get(seat).copied()
        }
    }

    fn idle() -> At {
        At(BTreeMap::from([("cpd-1".to_string(), State::Idle)]))
    }

    const MIN: i64 = 60;
    const T0: i64 = 1_800_000_000;

    fn world(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("stand-down-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        store.save_project(&crate::model::Project::new("p")).unwrap();
        store.set_governor(
            "p",
            serde_json::json!({ "workspace": "cpd-1", "pane": "cpd-1", "host": util::hostname(), "kind": "claude" }),
        );
        ENDED.with(|e| e.borrow_mut().clear());
        (env, store)
    }

    fn ended() -> Vec<String> {
        ENDED.with(|e| e.borrow_mut().drain(..).collect())
    }

    fn seated(store: &Store, scope: &str) -> bool {
        crate::governors::seat_of_scope(scope, &store.governors()).is_some()
    }

    fn a_list(store: &Store, id: &str, status: WorklistStatus, member: &str) {
        let mut m = Task::new(member, member);
        m.project = Some("p".into());
        m.set_status(Status::Doing);
        store.save_task(&m).unwrap();
        let mut w = Worklist::new(id, id);
        w.set_status(status);
        w.set_groups(&[Group { members: vec![member.into()], agent: "claude".into(), ..Group::default() }]);
        store.save_worklist(&w).unwrap();
    }

    #[test]
    fn a_project_governor_with_nothing_under_it_ends_after_the_idle_time_and_not_before() {
        let (_env, store) = world("idle");
        tick_at(&store, &idle(), T0, 30);
        assert!(store.governors()["p"].get(IDLE_SINCE).is_some(), "the clock starts at the first idle reading");
        tick_at(&store, &idle(), T0 + 29 * MIN, 30);
        assert!(seated(&store, "p") && ended().is_empty(), "not before");
        tick_at(&store, &idle(), T0 + 30 * MIN, 30);
        assert_eq!(ended(), vec!["cpd-1".to_string()]);
        assert!(!seated(&store, "p"), "vacated");
        assert!(crate::governors::stood_at(&store.governors(), "p").is_none(), "not a person's decision: a new ask seats it");
        tick_at(&store, &idle(), T0 + 31 * MIN, 30);
        assert!(ended().is_empty(), "once");
    }

    #[test]
    fn a_running_or_held_list_with_a_member_in_the_project_keeps_the_seat_and_resets_the_clock() {
        for status in [WorklistStatus::Running, WorklistStatus::Held] {
            let (_env, store) = world(status.as_str());
            tick_at(&store, &idle(), T0, 30);
            a_list(&store, "run", status, "m-1");
            tick_at(&store, &idle(), T0 + 40 * MIN, 30);
            assert!(seated(&store, "p") && ended().is_empty(), "{status:?}");
            assert!(store.governors()["p"].get(IDLE_SINCE).is_none(), "the clock is back to zero");
        }
    }

    #[test]
    fn a_done_list_does_not_keep_the_seat() {
        let (_env, store) = world("done-under");
        a_list(&store, "run", WorklistStatus::Done, "m-1");
        tick_at(&store, &idle(), T0, 30);
        tick_at(&store, &idle(), T0 + 30 * MIN, 30);
        assert_eq!(ended(), vec!["cpd-1".to_string()]);
    }

    #[test]
    fn a_seat_mid_turn_or_unreadable_is_not_idle() {
        let (_env, store) = world("turn");
        let working = At(BTreeMap::from([("cpd-1".to_string(), State::Working)]));
        tick_at(&store, &working, T0, 30);
        tick_at(&store, &working, T0 + 60 * MIN, 30);
        let blind = At(BTreeMap::new());
        tick_at(&store, &blind, T0 + 60 * MIN, 30);
        tick_at(&store, &blind, T0 + 120 * MIN, 30);
        assert!(seated(&store, "p") && ended().is_empty());
    }

    #[test]
    fn a_hand_raised_to_the_seat_keeps_it_until_it_is_lowered() {
        let (_env, store) = world("flag");
        let mut m = Task::new("m-1", "m-1");
        m.project = Some("p".into());
        m.set_status(Status::Doing);
        store.save_task(&m).unwrap();
        let q = crate::message::Message::question(
            crate::message::Party::pane("cpd-9", ""),
            crate::message::Kind::Note,
            "which of the two?",
            crate::message::Waiting::new("cpd-9", "m-1"),
        )
        .about(crate::message::About::Task("m-1".into()));
        store.save_message(&q).unwrap();
        assert!(flag_open(&store, "p"), "the fixture is an open hand the seat answers for");
        tick_at(&store, &idle(), T0, 30);
        tick_at(&store, &idle(), T0 + 90 * MIN, 30);
        assert!(seated(&store, "p") && ended().is_empty(), "it stays with a hand open");
    }

    #[test]
    fn something_owed_in_its_spool_keeps_the_seat() {
        let (_env, store) = world("owed");
        crate::wake::say(&store, "p", "a question nobody has answered", None);
        assert!(crate::wake::owed_to(&store, "p") > 0, "the fixture is owed an answer");
        tick_at(&store, &idle(), T0, 30);
        tick_at(&store, &idle(), T0 + 90 * MIN, 30);
        assert!(seated(&store, "p") && ended().is_empty());
    }

    #[test]
    fn the_primary_and_a_zero_idle_time_are_never_stood_down() {
        let (_env, store) = world("primary");
        store.set_governor(
            crate::governors::PRIMARY,
            serde_json::json!({ "workspace": "cpd-1", "pane": "cpd-1", "host": util::hostname() }),
        );
        store.save_project(&crate::model::Project::new(crate::governors::PRIMARY)).unwrap();
        tick_at(&store, &idle(), T0, 30);
        tick_at(&store, &idle(), T0 + 90 * MIN, 30);
        assert!(seated(&store, crate::governors::PRIMARY), "the person's seat stays");
        ended();
        tick_at(&store, &idle(), T0 + 200 * MIN, 0);
        assert!(ended().is_empty(), "0 is off");
    }

    #[test]
    fn a_done_lists_seat_is_stood_down_and_not_under_a_rotation() {
        let (_env, store) = world("done-list");
        a_list(&store, "run", WorklistStatus::Done, "m-1");
        store.set_governor(
            "run",
            serde_json::json!({ "workspace": "cpd-7", "pane": "cpd-7", "host": util::hostname(), "kind": "claude" }),
        );
        store.set_handover("run", serde_json::json!({ "from": "cpd-7", "to": "cpd-8" }));
        tick_at(&store, &idle(), T0, 0);
        assert!(seated(&store, "run") && ended().is_empty(), "a rotation in flight is left");

        store.clear_handover("run");
        tick_at(&store, &idle(), T0, 0);
        assert_eq!(ended(), vec!["cpd-7".to_string()]);
        assert!(!seated(&store, "run"));
        assert!(crate::governors::stood_at(&store.governors(), "run").is_some(), "stood down, so the reconciler does not refill it");
        tick_at(&store, &idle(), T0, 0);
        assert!(ended().is_empty(), "once");
    }

    #[test]
    fn a_running_lists_seat_is_left_alone() {
        let (_env, store) = world("running-list");
        a_list(&store, "run", WorklistStatus::Running, "m-1");
        store.set_governor(
            "run",
            serde_json::json!({ "workspace": "cpd-7", "pane": "cpd-7", "host": util::hostname(), "kind": "claude" }),
        );
        tick_at(&store, &idle(), T0, 0);
        assert!(seated(&store, "run") && ended().is_empty());
    }
}
