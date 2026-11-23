//! A row's stage over [`State`] (wsp-308): the half the files decide, which
//! the mirror writes on the row, and the half its seat decides, overlaid here.
//!
//! **Which half is which.** Everything `src/stage.rs`'s `of_member` reads off
//! the task, its passes, its list's landing record, its claim and what its
//! agent last said is the mirror's, written on the row as [`key::STAGE`] (the
//! caption as if nobody had looked at the agent) and [`key::STAGE_STEPS`] (the
//! timeline `history` replays, before the live word settles its last step).
//! What a `doing` row's word is once every record has been passed — `working`,
//! `wants you` or `started` — is the seat's: a row whose word that is carries
//! [`key::STAGE_LIVE`], with what its agent said, and [`stage_of`] reads the
//! word off the seats bound to it in the same [`State`]. So a seat that stops
//! moves the row's stage with no row put.
//!
//! **One copy of each rule.** [`live`] (the word and its tail), [`settle`]
//! (the timeline's last step under the live word), [`needs_a_person`] and
//! [`census_word`] are the code `src/stage.rs`, `cmd_govern` and the mirror's
//! `seated_of` call, not copies of it.

use serde_json::Value;
use wsp_store_contract::{Row, Seat, SeatState, State};

use crate::common::key;
use crate::task::{Stage, Step, Told};

/// What the seat holding a `doing` row is doing, as far as anyone looked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Agent {
    /// Nobody asked, or no backend answered. Not "idle": not knowing whether a
    /// seat stopped is not knowing it stopped (`wip`'s rule).
    #[default]
    Unknown,
    /// A turn is in flight, or the agent is running and not stopped.
    Working,
    /// Stopped while holding `doing` work, in a seat that governs nothing —
    /// [`needs_a_person`], the one definition `wip` publishes.
    WantsYou,
}

/// Whether a stopped seat is waiting on a person: it is stopped, it holds
/// `doing` work, and it is not a seat that governs a project (a governor at
/// rest is between tasks, not stuck on one).
pub fn needs_a_person(stopped: bool, doing: bool, seat: bool) -> bool {
    stopped && doing && !seat
}

/// A `doing` row's caption once every record is passed: the word its agent
/// decides, and what the agent said as the tail. `at` is the caller's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    pub word: &'static str,
    pub from: &'static str,
    pub tail: Option<String>,
    pub text: String,
}

/// The word and tail of a `doing` row past every record: `wants you` or
/// `working` with what was said; `working` too when nobody looked and
/// something was said; else `started`, off the status alone.
pub fn live(said: Option<&str>, agent: Agent) -> Live {
    let (word, cap, says) = match agent {
        Agent::WantsYou => ("wants you", "Wants you", true),
        Agent::Working => ("working", "Working", true),
        Agent::Unknown if said.is_some() => ("working", "Working", true),
        Agent::Unknown => ("started", "Started", false),
    };
    let tail = said.filter(|_| says).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    match tail {
        Some(t) => Live { word, from: "say", text: format!("{cap} — {t}"), tail: Some(t) },
        None => Live { word, from: if says { "stage" } else { "status" }, tail: None, text: cap.to_string() },
    }
}

/// What the timeline's last step becomes under the live word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settle {
    Keep,
    /// The last step takes the live word: `started` and `working` are one
    /// stretch of being on it, and the timeline ends in the word shown.
    Rename,
    /// A `wants you` step is appended, at this instant: when the agent stopped
    /// is in no record, so the later of the stage's and the last step's.
    Append(String),
}

/// `history`'s last rule: a timeline never ends in a word the stage does not
/// show while an agent is on the row.
pub fn settle(last_word: &str, last_at: &str, now_word: &str, now_at: &str) -> Settle {
    let on_it = |w: &str| matches!(w, "started" | "working");
    if on_it(last_word) && on_it(now_word) {
        Settle::Rename
    } else if on_it(last_word) && now_word == "wants you" {
        Settle::Append(now_at.max(last_at).to_string())
    } else {
        Settle::Keep
    }
}

/// [`settle`] applied to a timeline.
pub fn settled(mut steps: Vec<Step>, now: &Told) -> Vec<Step> {
    let Some(last) = steps.last_mut() else { return steps };
    match settle(&last.word, &last.at, &now.word, &now.at) {
        Settle::Keep => {}
        Settle::Rename => last.word = now.word.clone(),
        Settle::Append(at) => steps.push(Step { word: "wants you".into(), at, tail: None }),
    }
    steps
}

/// The census word a seat carries (`place::State::as_str`), as the mirror's
/// `seated_of` reads it: an ended seat is `gone` whatever word it carried
/// before; a seat with no word reads its own state; a word this build does not
/// know is `unknown`.
pub fn census_word(seat: &Seat) -> &str {
    let word = seat.fields.get(key::CENSUS).map(String::as_str).unwrap_or("");
    match (seat.state, word) {
        (SeatState::Gone | SeatState::Ending | SeatState::Lost, _) => "gone",
        (_, w @ ("empty" | "starting" | "idle" | "working" | "blocked" | "gone" | "unreachable")) => w,
        (SeatState::Idle, "") => "idle",
        (SeatState::Working, "") => "working",
        (SeatState::Requested | SeatState::Spawning | SeatState::Started, "") => "starting",
        _ => "unknown",
    }
}

/// The row a seat's binding holds (`bindings.json`'s `task_id`).
pub fn bound_row(seat: &Seat) -> Option<String> {
    let b: Value = serde_json::from_str(seat.fields.get(key::BINDING)?).ok()?;
    b.get("task_id").and_then(Value::as_str).map(str::to_string)
}

/// What the seats bound to `row` say of its agent: `wants you` if any running
/// seat is stopped on a person, `working` if any other is running, else
/// unknown. `place::State::is_running` and `stopped`, read off the census word.
pub fn agent_of(state: &State, row: &str) -> Agent {
    let mut out = Agent::Unknown;
    for seat in state.seats.values().filter(|s| bound_row(s).as_deref() == Some(row)) {
        let word = census_word(seat);
        if !matches!(word, "starting" | "idle" | "working" | "blocked") {
            continue;
        }
        let governs = seat.fields.get(key::GOVERNS).is_some_and(|g| !g.is_empty());
        match needs_a_person(word != "working", true, governs) {
            true => out = Agent::WantsYou,
            false if out == Agent::Unknown => out = Agent::Working,
            false => {}
        }
    }
    out
}

/// A row's stage: the mirror's caption and timeline, with a `doing` row's
/// live word read off its seats, and the timeline settled under the word
/// shown. `None` for a row no mirror of this build staged.
pub fn stage_of(row: &Row, state: &State) -> Option<Stage> {
    let told: Value = serde_json::from_str(row.fields.get(key::STAGE)?).ok()?;
    let mut now = Told::of_json(&told)?;
    if let Some(l) = row.fields.get(key::STAGE_LIVE) {
        let l: Value = serde_json::from_str(l).ok()?;
        let said = l.get("said").and_then(Value::as_str);
        let Live { word, from, tail, text } = live(said, agent_of(state, &row.id.0));
        now = Told { word: word.into(), tail, text, from: from.into(), at: now.at };
    }
    let steps: Value = row.fields.get(key::STAGE_STEPS).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
    let steps = steps.as_array().map(|a| a.iter().filter_map(Step::of_json).collect()).unwrap_or_default();
    Some(Stage { steps: settled(steps, &now), now })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use wsp_store_contract::{MachineName, ProjectId, RowId, RowState, SeatId};

    fn row(live: Option<&str>) -> Row {
        let mut fields = BTreeMap::new();
        fields.insert(key::STAGE.into(), r#"{"word":"started","text":"Started","from":"status","at":"2026-11-18T10:00:00Z"}"#.into());
        fields.insert(key::STAGE_STEPS.into(), r#"[{"word":"not started","at":"2026-11-18T09:00:00Z"},{"word":"working","at":"2026-11-18T09:30:00Z"}]"#.into());
        if let Some(l) = live {
            fields.insert(key::STAGE_LIVE.into(), l.into());
        }
        Row {
            workspace: Default::default(),
            id: RowId("beta-003".into()),
            project: ProjectId("beta".into()),
            title: "Doing row".into(),
            state: RowState::Doing,
            before_block: None,
            seat: None,
            group: None,
            commit: None,
            fields,
        }
    }

    fn seat(id: &str, state: SeatState, census: &str, governs: bool) -> Seat {
        let mut fields = BTreeMap::new();
        fields.insert(key::BINDING.into(), r#"{"task_id":"beta-003"}"#.into());
        if !census.is_empty() {
            fields.insert(key::CENSUS.into(), census.into());
        }
        if governs {
            fields.insert(key::GOVERNS.into(), "beta".into());
        }
        Seat {
            id: SeatId(id.into()),
            machine: MachineName("local".into()),
            kind: "claude".into(),
            model: None,
            state,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: None,
            fields,
        }
    }

    fn with(seats: &[Seat]) -> State {
        let mut s = State::default();
        for x in seats {
            s.seats.insert(x.id.clone(), x.clone());
        }
        s
    }

    #[test]
    fn the_live_word_is_the_seats_and_the_tail_what_was_said() {
        assert_eq!(live(Some(" wiring it "), Agent::WantsYou).text, "Wants you — wiring it");
        assert_eq!(live(None, Agent::Working), Live { word: "working", from: "stage", tail: None, text: "Working".into() });
        assert_eq!(live(Some("  "), Agent::Unknown).from, "stage", "said, but nothing: working, bare");
        assert_eq!(live(None, Agent::Unknown), Live { word: "started", from: "status", tail: None, text: "Started".into() });
    }

    #[test]
    fn a_stopped_seat_wants_you_unless_it_governs_and_a_gone_one_says_nothing() {
        let r = row(Some(r#"{"said":null}"#));
        assert_eq!(agent_of(&with(&[]), "beta-003"), Agent::Unknown);
        assert_eq!(agent_of(&with(&[seat("a", SeatState::Working, "", false)]), "beta-003"), Agent::Working);
        assert_eq!(agent_of(&with(&[seat("a", SeatState::Idle, "blocked", false)]), "beta-003"), Agent::WantsYou);
        assert_eq!(agent_of(&with(&[seat("a", SeatState::Idle, "idle", true)]), "beta-003"), Agent::Working, "a governor at rest");
        assert_eq!(agent_of(&with(&[seat("a", SeatState::Gone, "idle", false)]), "beta-003"), Agent::Unknown);
        let both = with(&[seat("a", SeatState::Working, "working", false), seat("b", SeatState::Idle, "idle", false)]);
        assert_eq!(agent_of(&both, "beta-003"), Agent::WantsYou, "either seat stopped on you");

        let s = stage_of(&r, &with(&[seat("a", SeatState::Idle, "idle", false)])).unwrap();
        assert_eq!(s.now.word, "wants you");
        assert_eq!(s.steps.last().unwrap(), &Step { word: "wants you".into(), at: "2026-11-18T10:00:00Z".into(), tail: None });
    }

    #[test]
    fn a_row_its_seat_does_not_decide_keeps_the_mirrors_word_and_its_timeline_settles() {
        let s = stage_of(&row(None), &with(&[seat("a", SeatState::Idle, "idle", false)])).unwrap();
        assert_eq!((s.now.word.as_str(), s.now.from.as_str()), ("started", "status"));
        assert_eq!(s.steps.last().unwrap().word, "started", "renamed to the word shown");
        assert_eq!(stage_of(&Row { fields: BTreeMap::new(), ..row(None) }, &State::default()), None);
    }
}
