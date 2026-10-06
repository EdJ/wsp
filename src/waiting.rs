//! Whether a seat is waiting on somebody, and on whom. `wsp-172`.
//!
//! **One reading, two sources, and every reader asks this rather than its own
//! copy.** Ed, 2026-10-05: *a seat that is waiting on someone says so,
//! everywhere, and nothing treats it as idle.* The cycle that wrote that down
//! had two readers of one seat disagree: the attention pass read `cpd-254`'s
//! opencode permission prompt as `needs-a-person`, and seven hours later the
//! reconciler read the same seat as `idle`, told it to finish, and ended it.
//! The one that acts was the one that was wrong, and eight hours of work
//! survived only because nothing had committed it away.
//!
//! The two sources:
//!
//! - **The screen.** [`State::Blocked`] — a permission or trust prompt holding
//!   the keyboard. That is the backend's reading and not this module's: herdr
//!   scrapes it, and compound asks `compound-render`'s detector, whose opencode
//!   rule matches opencode 1.18's `△ Permission required` dialog (checked live
//!   against a real one on 2026-10-06). A prompt is waiting on **a person**,
//!   whatever seat governs the work: only a keypress answers it.
//! - **The store.** An open `wsp ask` whose asker is this seat, or this seat's
//!   task. The agent ran the verb, was told the answer comes back here, and is
//!   sitting at its prompt until it does — which reads `idle` on every screen.
//!   It is waiting on **whoever the question was routed to**: the governor that
//!   answers for the task, or a person when no seat is above it.
//!
//! **No new state, and that is the brief's instruction rather than a taste.**
//! `place::State::Blocked` already means *stopped in front of a question only a
//! person can answer*; this adds the store half beside it as a reading over
//! [`State`], not a seventh variant of it, so a backend never has to know what
//! a question record is.
//!
//! # What a reader does with it
//!
//! Nothing nudges or ends a waiting seat: [`crate::repair`]'s stall repair
//! holds it with no ceiling and says so once, the barrier gate holds on it
//! ([`crate::cycle`]), and `wsp tell` refuses the prompt half — a sentence
//! typed at a dialog selects an answer nobody chose. The answer half is *not*
//! refused by `tell`, because an answer typed to an agent waiting for one is
//! the delivery, not an interruption.

use std::collections::BTreeMap;

use crate::place::State;
use crate::store::Store;

/// What the seat is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Why {
    /// A prompt on its screen that only a keypress answers.
    Prompt,
    /// Its own `wsp ask`, by the question's id.
    Answer(String),
}

/// Whom it is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum On {
    Person,
    /// The governor of this scope.
    Seat(String),
}

/// A seat that is waiting, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wait {
    pub(crate) why: Why,
    pub(crate) on: On,
}

impl Wait {
    /// The word a column draws: `waiting · person`, `waiting · wsp-process`.
    pub(crate) fn word(&self) -> String {
        match &self.on {
            On::Person => "waiting · person".into(),
            On::Seat(scope) => format!("waiting · {scope}"),
        }
    }

    /// The reason, as a clause a log line can carry.
    pub(crate) fn sentence(&self) -> String {
        let whom = match &self.on {
            On::Person => "a person".to_string(),
            On::Seat(scope) => format!("the {scope} governor"),
        };
        match &self.why {
            Why::Prompt => format!("stopped on a prompt only {whom} can answer"),
            Why::Answer(id) => format!("waiting on {whom} to answer its question {id}"),
        }
    }

    /// The published shape — `wsp wip --json`'s `waiting`, which compound's
    /// chrome reads.
    pub(crate) fn json(&self) -> serde_json::Value {
        let (why, question) = match &self.why {
            Why::Prompt => ("prompt", None),
            Why::Answer(id) => ("answer", Some(id.as_str())),
        };
        let on = match &self.on {
            On::Person => "person",
            On::Seat(scope) => scope.as_str(),
        };
        serde_json::json!({ "why": why, "on": on, "question": question, "word": self.word() })
    }
}

/// The open questions, read once, indexed by who is sitting still behind each.
///
/// **Read once per pass and asked per seat**, because every reader here walks
/// many seats and `messages.json` is one file: the reconciler, a `wip` and a
/// watch tick each build one of these and ask it for every row.
#[derive(Debug, Clone, Default)]
pub(crate) struct Asks {
    by_pane: BTreeMap<String, (String, On)>,
    by_task: BTreeMap<String, (String, On)>,
}

impl Asks {
    /// Every question still owed an answer, and whom it waits on.
    ///
    /// [`crate::message::Message::wants_answering`], the population `wsp ask`
    /// lists and `wsp watch unanswered` reports, so a question this counts is
    /// one those surfaces show. It includes a record this build cannot read,
    /// and here that errs the right way: a seat wrongly held as waiting is
    /// left alone, and a seat wrongly read as idle is ended.
    pub(crate) fn read(store: &Store) -> Asks {
        let mut asks = Asks::default();
        for m in store.messages().into_values() {
            if !m.wants_answering() || m.is_reply() {
                continue;
            }
            let Some(w) = &m.waiting else { continue };
            let on = m
                .about
                .task()
                .and_then(|id| store.find_task(id))
                .and_then(|t| crate::cmd_govern::answering_seat(store, &t))
                .map(|s| On::Seat(s.scope))
                .unwrap_or(On::Person);
            // Oldest first and the later one wins, so the question a seat names
            // is its most recent — the one it is most likely sitting on.
            if !w.pane.is_empty() {
                asks.by_pane.insert(w.pane.clone(), (m.id.clone(), on.clone()));
            }
            if !w.task.is_empty() {
                asks.by_task.insert(w.task.clone(), (m.id.clone(), on));
            }
        }
        asks
    }

    /// The open question this seat, or the task it holds, is waiting on.
    ///
    /// **The task as well as the pane**, because the pane is the perishable
    /// half: a member ended and started again is a new seat on the same task,
    /// and the question its predecessor asked is still the one the work is
    /// waiting on.
    pub(crate) fn of(&self, seat: &str, task: &str) -> Option<Wait> {
        let hit = self.by_pane.get(seat).or_else(|| (!task.is_empty()).then(|| self.by_task.get(task)).flatten())?;
        Some(Wait { why: Why::Answer(hit.0.clone()), on: hit.1.clone() })
    }

    #[cfg(test)]
    pub(crate) fn one(pane: &str, task: &str, id: &str, on: On) -> Asks {
        let mut a = Asks::default();
        a.by_pane.insert(pane.to_string(), (id.to_string(), on.clone()));
        a.by_task.insert(task.to_string(), (id.to_string(), on));
        a
    }
}

/// **The one reading.** A seat in `state`, holding `task`, is waiting — or not.
///
/// The prompt wins over the question: a modal holding the keyboard is the
/// repair a person has to make first, and an agent can be in one while a
/// question of its own is standing.
///
/// **The question counts only on a seat that reads `Idle`** — at its prompt,
/// which is where an agent that asked and stopped is sitting. One mid-turn
/// asked and carried on, and is working; one that is `Gone` has nobody left to
/// wait, and holding it would keep a dead seat from ever being started again;
/// one nobody can read is not waiting on anybody as far as this can say.
pub(crate) fn reading(state: Option<State>, asks: &Asks, seat: &str, task: &str) -> Option<Wait> {
    match state? {
        State::Idle => asks.of(seat, task),
        other => on_screen(other),
    }
}

/// The screen half alone, for the one reader that refuses only it: `wsp tell`.
///
/// A sentence typed at a dialog selects an answer nobody chose, so `tell`
/// refuses a prompt. It does *not* refuse a seat waiting on its own question,
/// because that is how an answer arrives.
pub(crate) fn on_screen(state: State) -> Option<Wait> {
    (state == State::Blocked).then_some(Wait { why: Why::Prompt, on: On::Person })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_on_screen_is_waiting_on_a_person_whoever_governs_the_work() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Seat("wsp-process".into()));
        let w = reading(Some(State::Blocked), &asks, "cpd-1", "wsp-1").expect("a prompt is waiting");
        assert_eq!(w.why, Why::Prompt, "the keyboard is held, so that is the repair first");
        assert_eq!(w.word(), "waiting · person");
    }

    #[test]
    fn an_open_ask_reads_waiting_on_the_seat_it_was_routed_to_while_the_screen_says_idle() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Seat("wsp-process".into()));
        let w = reading(Some(State::Idle), &asks, "cpd-1", "wsp-1").expect("idle at its prompt, and waiting");
        assert_eq!(w.word(), "waiting · wsp-process");
        assert!(w.sentence().contains("q-1"), "{}", w.sentence());
    }

    #[test]
    fn a_restarted_member_inherits_the_question_its_task_is_waiting_on() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Person);
        assert!(reading(Some(State::Idle), &asks, "cpd-9", "wsp-1").is_some(), "same task, new seat");
        assert!(reading(Some(State::Idle), &asks, "cpd-9", "wsp-2").is_none(), "another task is not");
    }

    #[test]
    fn an_open_ask_holds_only_a_seat_sitting_at_its_prompt() {
        let asks = Asks::one("cpd-1", "wsp-1", "q-1", On::Person);
        for state in [State::Working, State::Gone, State::Starting, State::Unknown, State::Empty] {
            assert_eq!(reading(Some(state), &asks, "cpd-1", "wsp-1"), None, "{state:?}");
        }
        assert_eq!(reading(None, &asks, "cpd-1", "wsp-1"), None, "nobody can say");
    }

    #[test]
    fn idle_with_nothing_asked_is_not_waiting() {
        assert_eq!(reading(Some(State::Idle), &Asks::default(), "cpd-1", "wsp-1"), None);
        assert_eq!(reading(None, &Asks::default(), "cpd-1", "wsp-1"), None);
    }
}
