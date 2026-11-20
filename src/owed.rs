//! What a barrier is waiting on, and whether that is the person (`wsp-095`).
//!
//! Jesper's rule, 2026-11-18: *if something is in "Needs you", the person has to
//! do something now; anything else is noise.* web-chrome's "Needs you" read a
//! group's `held` and `barrier` states as needing him, so a hold whose person's
//! step was done and that waited only on a day, a hold waiting on other rows,
//! and a barrier whose own check was running all showed there.
//!
//! [`of`] answers the question once, from the list file alone, and the mirror
//! puts the answer on the group (`owed`, `owed_text`, and `until` or `on`), so
//! a reader lists a barrier only where `owed` is `person`, and the sentence
//! beside it says what the act is. Its inputs are the hold's own fields
//! (`held_until`, `held_on`, `held_for`, written by `wsp worklist hold`), the
//! group's checks and `agent:` line, and whether each row is settled.

use wsp_store::checks::State as CheckState;
use wsp_store::model::{Group, Worklist, WorklistStatus, MANUAL};
use wsp_store_contract::ListState;

use crate::util;

/// What the barrier at a group is waiting on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owed {
    /// A time: nothing is owed by anybody before it.
    Until(String),
    /// Rows that have not landed.
    Rows(Vec<String>),
    /// Its own check, running or about to be opened by the run.
    Check,
    /// The person, and the act that is owed.
    Person(String),
}

impl Owed {
    /// The word a reader filters on: `person` is the only one that needs him.
    pub fn word(&self) -> &'static str {
        match self {
            Owed::Until(_) => "time",
            Owed::Rows(_) => "rows",
            Owed::Check => "check",
            Owed::Person(_) => "person",
        }
    }

    /// The line a reader shows.
    pub fn text(&self) -> String {
        match self {
            Owed::Until(when) => format!("waiting until {when}"),
            Owed::Rows(rows) => format!("waiting on {}", rows.join(" ")),
            Owed::Check => "its check is running".to_string(),
            Owed::Person(act) => act.clone(),
        }
    }
}

/// Whether the run opens a check on this group's barrier itself: a group run by
/// an agent line, or a `manual` one whose written stop names no person
/// (`wsp-629`, `cycle::manual_barrier_policy`).
fn checked_by_the_run(g: &Group) -> bool {
    if g.policy().is_some() {
        return true;
    }
    let stop = g.stop.trim();
    g.agent.trim() == MANUAL && !stop.is_empty() && !crate::cycle::names_a_person(stop)
}

/// What the barrier at group `g` of `w` is waiting on, where its state is
/// `held` or `barrier`; `None` for any other state. `settled` answers whether a
/// row has landed; `now` is seconds since the epoch.
pub fn of(w: &Worklist, g: &Group, state: ListState, settled: &dyn Fn(&str) -> bool, now: i64) -> Option<Owed> {
    let last = g.checks.last();
    let spent = last.is_some_and(|c| c.spent() || c.gave_up);
    match state {
        ListState::Held => {
            if let Some(until) = w.held_until.as_deref().filter(|u| util::epoch_of(u) > now) {
                return Some(Owed::Until(until.to_string()));
            }
            // The rows the hold names, then the group's own: a `followup
            // --blocking` row joins the group it holds.
            let mut rows: Vec<String> = Vec::new();
            for r in w.held_on.iter().chain(g.members.iter()) {
                if !settled(r) && !rows.contains(r) {
                    rows.push(r.clone());
                }
            }
            if !rows.is_empty() {
                return Some(Owed::Rows(rows));
            }
            if last.is_some_and(|c| c.open()) && !spent {
                return Some(Owed::Check);
            }
            let act = match w.held_for.as_deref() {
                Some(act) => format!("{act}, then `wsp worklist go {}`", w.id),
                None => format!("read why `{}` is held, then `wsp worklist go {}` or hold it again", w.id, w.id),
            };
            Some(Owed::Person(act))
        }
        ListState::Barrier => {
            if w.status() == WorklistStatus::Held {
                return None;
            }
            if spent {
                let tries = last.map(|c| c.tries).unwrap_or(0);
                return Some(Owed::Person(format!(
                    "its check never started ({tries} tries): read the stop and `wsp worklist go {}` or hold it",
                    w.id
                )));
            }
            if last.is_some_and(|c| c.open()) {
                return Some(Owed::Check);
            }
            // Blocking: the follow-ups land, and a fresh check reads the group.
            if last.is_some_and(|c| c.state == CheckState::Blocking) || checked_by_the_run(g) {
                return Some(Owed::Check);
            }
            Some(Owed::Person(if g.stop.trim().is_empty() {
                format!("pass the barrier: `wsp worklist go {}`", w.id)
            } else {
                format!("read the stop and pass it: `wsp worklist go {}` or hold it", w.id)
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store::checks::Check;

    const NOW: i64 = 1_800_000_000;

    fn list(status: WorklistStatus) -> Worklist {
        let mut w = Worklist::new("m1", "m1");
        w.set_status(status);
        w
    }

    fn group(agent: &str, stop: &str) -> Group {
        Group { members: vec!["a-1".into()], agent: agent.into(), stop: stop.into(), ..Group::default() }
    }

    fn check(state: CheckState) -> Check {
        let mut c = Check::opened(None, None);
        c.at = util::iso_at(NOW - 60);
        c.state = state;
        c.pane = Some("w1:p1".into());
        c
    }

    fn all_landed(_: &str) -> bool {
        true
    }

    /// m1-agent-manager barrier 7: the person's step is done, what is left is a day.
    #[test]
    fn a_hold_whose_remaining_condition_is_a_day_waits_until_then_and_does_not_need_the_person() {
        let mut w = list(WorklistStatus::Held);
        w.held_until = Some(util::iso_at(NOW + 86_400));
        w.held_for = Some("read the day's logs".into());
        let owed = of(&w, &group("claude", "a day on the trunk"), ListState::Held, &all_landed, NOW).unwrap();
        assert_eq!(owed, Owed::Until(util::iso_at(NOW + 86_400)));
        assert_eq!(owed.word(), "time");
        assert_eq!(owed.text(), format!("waiting until {}", util::iso_at(NOW + 86_400)));
        // And once the day has passed, the act is the person's, and says what it is.
        let later = of(&w, &group("claude", "a day on the trunk"), ListState::Held, &all_landed, NOW + 86_401).unwrap();
        assert_eq!(later.word(), "person");
        assert!(later.text().starts_with("read the day's logs, then `wsp worklist go m1`"), "{}", later.text());
    }

    /// m3-agent-manager barrier 4: waits on another list's group and on a row.
    #[test]
    fn a_hold_waiting_on_rows_names_the_rows_that_have_not_landed_and_does_not_need_the_person() {
        let mut w = list(WorklistStatus::Held);
        w.held_on = vec!["wsp-093".into(), "m1-g8".into(), "done-1".into()];
        let landed = |r: &str| r == "done-1" || r == "a-1";
        let owed = of(&w, &group("claude", "x"), ListState::Held, &landed, NOW).unwrap();
        assert_eq!(owed, Owed::Rows(vec!["wsp-093".into(), "m1-g8".into()]));
        assert_eq!(owed.text(), "waiting on wsp-093 m1-g8");
        assert_ne!(owed.word(), "person");
    }

    #[test]
    fn a_held_barrier_with_nothing_left_but_the_person_says_what_the_act_is() {
        let w = list(WorklistStatus::Held);
        let owed = of(&w, &group("claude", "x"), ListState::Held, &all_landed, NOW).unwrap();
        assert_eq!(owed.word(), "person");
        assert!(owed.text().contains("wsp worklist go m1"), "{}", owed.text());
    }

    /// restart-followups, m3-t2-plan: "wants a sentence" while the check seat ran.
    #[test]
    fn a_barrier_whose_check_is_running_or_about_to_run_does_not_need_the_person() {
        let w = list(WorklistStatus::Running);
        let mut g = group("claude", "the suite is green");
        assert_eq!(of(&w, &g, ListState::Barrier, &all_landed, NOW), Some(Owed::Check), "about to be opened");
        g.checks.push(check(CheckState::Running));
        assert_eq!(of(&w, &g, ListState::Barrier, &all_landed, NOW), Some(Owed::Check), "running");
        let mut manual = group(MANUAL, "the suite is green");
        assert_eq!(of(&w, &manual, ListState::Barrier, &all_landed, NOW), Some(Owed::Check), "a manual group's check");
        manual.checks.push(check(CheckState::Blocking));
        assert_eq!(of(&w, &manual, ListState::Barrier, &all_landed, NOW), Some(Owed::Check), "waiting on its follow-ups");
        // A held list whose check is still reading it is the check's too.
        let mut held = group("claude", "x");
        held.checks.push(check(CheckState::Running));
        assert_eq!(of(&list(WorklistStatus::Held), &held, ListState::Held, &all_landed, NOW), Some(Owed::Check));
    }

    #[test]
    fn a_barrier_is_the_persons_only_when_nothing_else_will_read_it_and_says_what_to_do() {
        let w = list(WorklistStatus::Running);
        let by_hand = of(&w, &group("", "Ed reads the results"), ListState::Barrier, &all_landed, NOW).unwrap();
        assert_eq!(by_hand.word(), "person");
        assert!(by_hand.text().contains("read the stop"), "{}", by_hand.text());
        let for_ed = of(&w, &group(MANUAL, "Ed reads the results"), ListState::Barrier, &all_landed, NOW).unwrap();
        assert_eq!(for_ed.word(), "person", "a manual stop that names a person is left to him");
        let mut gave_up = group("claude", "x");
        let mut c = check(CheckState::Running);
        c.gave_up = true;
        gave_up.checks.push(c);
        let owed = of(&w, &gave_up, ListState::Barrier, &all_landed, NOW).unwrap();
        assert_eq!(owed.word(), "person");
        assert!(owed.text().contains("never started"), "{}", owed.text());
        assert_eq!(of(&w, &group("claude", "x"), ListState::Running, &all_landed, NOW), None, "a running group owes nothing");
    }
}
