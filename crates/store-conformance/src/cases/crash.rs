//! Case 8, crash: kill a handle mid-`apply` (the harness's test hook) and
//! confirm the result is all-or-nothing on reopen.
//!
//! For each of [`ROUNDS`] (row, seat) pairs, a handle is armed with
//! `Harness::crash_next_apply` and applies `row.claim` (one command, two
//! events: `row.claimed` and `seat.working`); that `apply` must not answer
//! `Ok`. The store is then reopened with `Harness::reopen`, and on the new
//! handle:
//! - the log past the head before the crash holds both events of the command,
//!   adjacent, or nothing;
//! - the state agrees with the log: the row `doing` under its seat exactly when
//!   the seat is `working`, exactly when the events are there;
//! - `snapshot()` is the fold of the reopened log (the state was not kept from
//!   before the crash);
//! - if nothing landed, the same command under the same key now lands whole,
//!   so the crashed attempt left no key, claim or id behind that blocks it.
//!
//! Each round crashes a fresh handle opened on the reopened store, so the
//! store survives several crashes in a row.

use wsp_store_contract::*;

use super::{key, person, Case, CaseResult};
use crate::{Harness, TestDomain};

const ROUNDS: usize = 6;

/// Case 8 of 2.3.
pub fn case_crash<H: Harness>(h: &H) -> CaseResult {
    let c = Case { case: 8, name: "crash" };
    let mut s = h.fresh();
    for i in 0..ROUNDS {
        let row = c.add_row(&s, format!("crash-row-{i}"))?;
        let seat = c.spawn_seat(&s, format!("crash-seat-{i}"))?;
        let fence = c.take_claim(&s, &row, &seat)?;
        let head = c.store(s.head(), "reading the head")?;

        let victim = h.open(&s);
        h.crash_next_apply(&victim);
        let cmd = Command::RowClaim {
            row: row.clone(),
            seat: seat.clone(),
            fence: fence.clone(),
        };
        let k = format!("crash-claim-{i}");
        let got = victim.apply(cmd.clone(), key(&k), person());
        c.ensure(got.is_err(), || {
            format!(
                "row.claim of {} through a handle armed to crash answered {got:?}, not an error",
                row.0
            )
        })?;
        s = h.reopen(&victim);

        let written = c.store(s.events_after(head, usize::MAX), "reading the log after reopen")?;
        let seen: Vec<_> = written.iter().map(|e| (e.kind, e.subject.clone())).collect();
        let whole = vec![
            (EventKind::RowClaimed, Subject::Row(row.clone())),
            (EventKind::SeatWorking, Subject::Seat(seat.clone())),
        ];
        c.ensure(seen.is_empty() || seen == whole, || {
            format!(
                "after a crash mid row.claim of {} and a reopen, the log past {} holds {seen:?}: \
                 neither nothing nor the whole command {whole:?}",
                row.0, head.0
            )
        })?;
        let landed = !seen.is_empty();

        let (state, at) = c.store(s.snapshot(), "taking a snapshot after reopen")?;
        let log = c.store(s.events_after(EventId(0), usize::MAX), "reading the log after reopen")?;
        let folded = log
            .iter()
            .filter(|e| e.id <= at)
            .fold(State::default(), |st, e| TestDomain.evolve(st, e));
        c.ensure(folded == state, || {
            format!(
                "after a crash and a reopen, the snapshot at {} is not the fold of the log",
                at.0
            )
        })?;
        let row_done = state
            .rows
            .get(&row)
            .is_some_and(|r| r.state == RowState::Doing && r.seat.as_ref() == Some(&seat));
        let seat_done = state.seats.get(&seat).is_some_and(|x| x.state == SeatState::Working);
        c.ensure(row_done == landed && seat_done == landed, || {
            format!(
                "after a crash and a reopen, the state shows half a row.claim or disagrees with the log: \
                 {} doing under {}: {row_done}, {} working: {seat_done}, its events in the log: {landed}",
                row.0, seat.0, seat.0
            )
        })?;

        if !landed {
            let again = c.store(
                s.apply(cmd, key(&k), person()),
                format!("retrying row.claim of {} after the crash", row.0),
            )?;
            c.ensure(again.events.len() == 2, || {
                format!(
                    "row.claim of {} retried after a crash that wrote nothing returned {again:?}, not 2 events",
                    row.0
                )
            })?;
            let now = c.store(s.row(&row), "reading the row")?;
            c.ensure(now.is_some_and(|r| r.state == RowState::Doing), || {
                format!(
                    "row.claim of {} retried after the crash did not move it to doing",
                    row.0
                )
            })?;
        }
    }
    Ok(())
}
