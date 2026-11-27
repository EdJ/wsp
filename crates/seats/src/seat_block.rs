//! A seat that reads Blocked at a question is raised to the person (`wsp-788`).
//!
//! On 2026-11-26 the primary seat sat ten hours at a Claude question dialog for
//! the person. wsp read it as idle, held two orders to it with no word to
//! anyone, and `wsp yours` said nothing waited. A prompt on a screen is read by
//! nobody, so [`sync`] puts it on the list: one open question for the person per
//! blocked seat, naming the seat, its row and the prompt's text, lapsed when the
//! seat reads anything else. `wsp tell` to such a seat names it
//! ([`blocked_on`]) rather than only "held".

use crate::message::{self, About, Kind, Message, Party, Waiting};
use crate::place::State;
use crate::store::Store;

/// What every item this module raises says, and what finds it again.
const MARK: &str = "is waiting on a question to the person";

/// One seat as read: what the census says and what its screen shows.
pub struct Reading {
    pub seat: String,
    pub state: State,
    pub screen: Option<String>,
}

/// The open item for `seat`, if one is raised.
fn open_for(store: &Store, seat: &str) -> Option<Message> {
    message::for_the_person(store)
        .into_iter()
        .find(|m| m.from.is_wsp() && m.text.contains(MARK) && m.waiting.as_ref().is_some_and(|w| w.pane == seat))
}

/// The sentence a tell to `seat` adds when it is blocked at a question the
/// person has been asked: `None` where nothing is raised for it.
pub fn blocked_on(store: &Store, seat: &str) -> Option<String> {
    open_for(store, seat).map(|m| m.title().to_string())
}

/// Raise each blocked seat once, and lapse the item of each that is not blocked
/// now. `seats` is every seat read this pass; a seat absent from it is left as
/// it is (the machine that could not be seen is not a seat that moved on).
/// Returns `(raised, lapsed)`.
pub fn sync(store: &Store, seats: &[Reading]) -> (usize, usize) {
    let (mut raised, mut lapsed) = (0, 0);
    for r in seats {
        let open = open_for(store, &r.seat);
        match (r.state == State::Blocked, open) {
            (true, None) => {
                let row = store
                    .bindings()
                    .get(&r.seat)
                    .and_then(|b| b.get("task_id").and_then(|v| v.as_str()).map(str::to_string))
                    .filter(|t| !t.is_empty());
                let whose = match &row {
                    Some(t) => format!("{} (row {t})", r.seat),
                    None => r.seat.clone(),
                };
                let said = match r.screen.as_deref().and_then(crate::waiting::prompt_text) {
                    Some(p) => format!("\"{p}\""),
                    None => "(its screen could not be read)".to_string(),
                };
                let text = format!(
                    "{whose} {MARK}: {said}\nNothing moves at that seat until it is answered, and a `wsp tell` to it is held. \
                     Only a keypress at the seat answers it: `wsp peek --pane {}` shows it.",
                    r.seat
                );
                let mut m = Message::question(Party::wsp(), Kind::Note, &text, Waiting::new(&r.seat, row.as_deref().unwrap_or("")))
                    .for_the_person();
                if let Some(t) = &row {
                    m = m.about(About::Task(t.clone()));
                }
                match message::raise(store, &m) {
                    Ok(()) => raised += 1,
                    Err(e) => crate::store::log_line(store, &format!("{}: its question could not be raised: {e}", r.seat)),
                }
            }
            (false, Some(m)) => {
                let why = format!("{} no longer reads blocked ({})", r.seat, r.state.as_str());
                if message::lapse(store, &m.id, &Party::lapsed(), &why).is_ok() {
                    lapsed += 1;
                }
            }
            _ => {}
        }
    }
    (raised, lapsed)
}

/// [`sync`] against the machine's own backends: the daemon's call.
pub fn sync_now(store: &Store) -> (usize, usize) {
    let Ok(backends) = crate::cmd_spawn::told_backends(false) else { return (0, 0) };
    let mut seen = Vec::new();
    for b in backends.iter() {
        let Ok(c) = b.census() else { continue };
        for row in c.seats() {
            let seat = row.seat.as_str().to_string();
            let screen = match row.state == State::Blocked {
                true => {
                    let compound = crate::place_compound::Compound::new();
                    let id = crate::place::Seat::new(&seat);
                    match compound.socket_of(&id).is_some() {
                        true => compound.read_screen(&id).ok(),
                        false => crate::cmd_agent::read_pane_text(&seat),
                    }
                }
                false => None,
            };
            seen.push(Reading { seat, state: row.state, screen });
        }
    }
    sync(store, &seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(state: State) -> Vec<Reading> {
        let screen = "Which database?\n 1. Postgres\n 2. SQLite\nenter to select".to_string();
        vec![Reading { seat: "cpd-9".into(), state, screen: Some(screen) }]
    }

    #[test]
    fn a_seat_blocked_at_a_question_raises_one_item_with_its_text_and_clears_when_it_moves_on() {
        for next in [State::Idle, State::Working] {
            // engine's `hold::tests` fixtures are test-only in engine; a plain store is all this needs.
            let store = crate::testkit::store(&format!("seat-block-{}", next.as_str()));
            assert_eq!(sync(&store, &read(State::Blocked)), (1, 0));
            assert_eq!(sync(&store, &read(State::Blocked)), (0, 0), "once per spell, not once per pass");
            let items = crate::cmd_message::yours_in(&store, None);
            assert_eq!(items.len(), 1);
            assert!(items[0].text.contains("cpd-9") && items[0].text.contains("Which database?"), "{}", items[0].text);
            assert!(blocked_on(&store, "cpd-9").unwrap().contains("Which database?"));
            assert_eq!(sync(&store, &read(next)), (0, 1));
            assert!(crate::cmd_message::yours_in(&store, None).is_empty(), "{next:?}");
            assert_eq!(blocked_on(&store, "cpd-9"), None);
        }
    }
}
