//! A work order the run typed at a seat, and no turn came of it — `wsp-252`.
//!
//! [`crate::place::submit`] presses submit on every send whose text it reads
//! back still sitting in the prompt, and a compound seat is no longer typed at
//! before its own `SessionStart`. This is what is left over: an order that went
//! in anyway and started nothing, found by the run rather than by a person.
//! On 2026-10-08 that was ten seats in gpui-migration and three in ux-kit, each
//! one a run stopped at a verifier, a check or a member until a governor ran a
//! `wsp tell` it was not supposed to need, or Ed pressed Enter an hour later.
//!
//! # Three steps, each taken once
//!
//! A seat the run is waiting on — a member it started, a verifier on a pass, a
//! barrier check — that reads idle [`AFTER`] seconds after its record opened,
//! with no turn begun since, is looked at:
//!
//! | its prompt reads | the step | and if no turn comes of it |
//! |---|---|---|
//! | the order, typed | press submit | the seat is replaced |
//! | empty | send the order again | the seat is replaced |
//! | nothing readable | nothing | — |
//!
//! **Replaced** is the seat ended, which hands it to the reading that already
//! exists for a seat that went: a verifier's pass is superseded and a fresh one
//! opens ([`crate::cycle::open_verifier`]), and a check's entry is abandoned
//! and a fresh one opens ([`crate::barrier::open`]). A member is never replaced
//! here: a member is somebody's tree and claim, and [`crate::repair`]'s stall
//! reading is what ends one. A member's order is not resent either, because the
//! handover it was given is the spawn's to compose; a press is the whole of it.
//!
//! # `cycle.log` is the record
//!
//! Each step is a line naming the seat, and the next step reads the line back:
//! the step is not taken again while it is there, and the step after waits
//! [`AFTER`] from its date. Not the member's `## Log`, which every future spawn
//! on the row is handed, and which a check has none of. The lines are written
//! with [`crate::cycle::log_line`], which writes the file whatever stdout is, because from the
//! daemon's tick stdout is `daemon.log` and a record written there is a record
//! this would never find — every tick would press again.
//!
//! # What it will not do
//!
//! Press at a prompt it cannot read. A return into a prompt nobody has read may
//! submit a person's draft, and an unreadable prompt is a reason to leave the
//! seat alone, not a licence. Nor act on a seat that is mid-turn, waiting on a
//! person, or has taken any turn since its record opened: that seat took its
//! order, and what it does next is not this module's question.

use crate::cycle::{self, Seats};
use crate::model::{Group, Status, Worklist};
use crate::place::State;
use crate::store::Store;
use crate::util;
use crate::worklist::Position;

/// How long a seat may sit idle with no turn since its record opened before
/// its prompt is looked at, and how long after each step before the next.
///
/// Two minutes: a healthy seat's first turn starts seconds after its order,
/// and a slow one's `SessionStart` was measured at thirty-one seconds — so a
/// seat still without a turn at two minutes is not one about to take it.
const AFTER: i64 = 120;

/// One seat the run is waiting on a first turn from.
struct Owed {
    /// Who, as the log line says it.
    who: String,
    seat: String,
    /// When its record opened, in epoch seconds.
    since: i64,
    /// The order to send again, where the run composes it.
    order: Option<String>,
    replace: Replace,
}

/// How a seat that never took its order is replaced.
enum Replace {
    /// The pass on this member is superseded and its seat ended.
    Pass(String),
    /// The check's seat is ended, and its entry is abandoned at the next open.
    Check,
    /// A member: not here.
    Not,
}

/// Look at every seat group `at` of `w` is waiting on a first turn from, and
/// take the next step on each that is owed one. Before the verifiers and the
/// barrier in [`crate::cycle::step`], so a seat replaced here is reopened in
/// the same step.
pub(crate) fn look(store: &Store, seats: &dyn Seats, w: &Worklist, at: usize, g: &Group, pos: &Position) {
    let now = util::epoch_secs();
    let mut log: Option<String> = None;
    for o in owed(store, w, at, g, pos) {
        if now - o.since < AFTER || seats.turn_began_since(&o.seat, o.since) != Some(false) {
            continue;
        }
        if seats.state(&o.seat) != Some(State::Idle) {
            continue;
        }
        let log = log.get_or_insert_with(|| std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default());
        let pressed = newest(log, &pressed(&o.seat));
        let resent = newest(log, &resent(&o.seat));
        if newest(log, &replaced(&o.seat)).is_some() {
            continue;
        }
        let rested = |t: Option<i64>| t.is_none_or(|t| now - t >= AFTER);
        if !rested(pressed) || !rested(resent) {
            continue;
        }
        let head = format!("{} group {at}: {} in {} has started no turn in {} min", w.id, o.who, o.seat, (now - o.since) / 60);
        match seats.typed(&o.seat) {
            Some(true) if pressed.is_none() => {
                let took = if seats.press(&o.seat) { "" } else { " (no backend took the press)" };
                cycle::log_line(store, &format!("{head}, with its order typed in the prompt — {} once{took}", self::pressed(&o.seat)));
            }
            Some(false) if resent.is_none() && o.order.is_some() => {
                let outcome = match seats.resend(&o.seat, o.order.as_deref().unwrap_or_default()) {
                    Some(Ok(_)) => "delivered".to_string(),
                    Some(Err(e)) => format!("not delivered: {e}"),
                    None => "nothing answers for it".to_string(),
                };
                cycle::log_line(store, &format!("{head}, and its prompt is empty — {}: {outcome}", self::resent(&o.seat)));
            }
            Some(_) if pressed.is_some() || resent.is_some() => replace(store, seats, w, &o, &head),
            _ => {}
        }
    }
}

/// End a seat that took neither the press nor the resend, so the run opens a
/// fresh one. Said in `cycle.log` and to the governor, because a seat the run
/// ended and replaced is a decision somebody may want to see.
fn replace(store: &Store, seats: &dyn Seats, w: &Worklist, o: &Owed, head: &str) {
    let ended = match &o.replace {
        Replace::Not => return,
        Replace::Pass(member) => {
            crate::verification::supersede(store, member, &o.seat);
            !cycle::end_passes(store, seats, member, |p| p.pane.as_deref() == Some(o.seat.as_str())).is_empty()
        }
        Replace::Check => cycle::despawn_seat(&o.seat).is_ok(),
    };
    let how = if ended { "ended" } else { "could not be ended" };
    cycle::log_line(store, &format!("{head} after a press or a resend — {} {how}, and the run opens another", replaced(&o.seat)));
    cycle::tell(store, w, &format!(
        "{} in {} took no turn on its order, after wsp pressed submit or sent it again. wsp {how} {} and starts another.",
        o.who, o.seat, o.seat
    ));
}

/// The seats group `at` is waiting on a first turn from: its verifiers, its
/// check, and its members at `doing`.
fn owed(store: &Store, w: &Worklist, at: usize, g: &Group, pos: &Position) -> Vec<Owed> {
    let mut out = Vec::new();
    let claims = store.claims();
    for s in &pos.members {
        let Some(t) = store.find_task(&s.id) else { continue };
        if s.finished() {
            let Some(p) = crate::verification::passes(&t).into_iter().last() else { continue };
            if p.state != crate::verification::State::Running || !p.standing() {
                continue;
            }
            out.push(Owed {
                who: format!("{}'s verifier", t.id),
                seat: p.pane.clone().unwrap_or_default(),
                since: util::epoch_of(&p.at),
                order: Some(crate::cmd_task::fold(&cycle::verifier_order(store, &t))),
                replace: Replace::Pass(t.id.clone()),
            });
        } else if t.status() == Status::Doing && claims.contains_key(&t.id) {
            let (Some(seat), Some(since)) = (store.panes_for_task(&t.id).first().cloned(), crate::repair::started_at(&t))
            else {
                continue;
            };
            out.push(Owed { who: t.id.clone(), seat, since, order: None, replace: Replace::Not });
        }
    }
    if let Some(c) = crate::barrier::running(g) {
        if let Some(seat) = c.standing() {
            out.push(Owed {
                who: "the barrier check".into(),
                seat: seat.to_string(),
                since: util::epoch_of(&c.at),
                order: cycle::check_order(store, &w.id, at).map(|o| crate::cmd_task::fold(&o)),
                replace: Replace::Check,
            });
        }
    }
    out
}

// The markers each step writes and the next reads. Each ends on a delimiter, so
// `cpd-61`'s line is never read as `cpd-616`'s.
fn pressed(seat: &str) -> String {
    format!("pressed submit in {seat},")
}
fn resent(seat: &str) -> String {
    format!("sent the order to {seat} again")
}
fn replaced(seat: &str) -> String {
    format!("replacing {seat}:")
}

/// The date of the newest `cycle.log` line holding `marker`.
fn newest(log: &str, marker: &str) -> Option<i64> {
    log.lines()
        .rev()
        .find(|l| l.contains(marker))
        .and_then(|l| l.split_whitespace().next())
        .map(util::epoch_of)
        .filter(|t| *t > 0)
}
