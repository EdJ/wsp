//! Case 1, atomicity: a command that touches a seat, a claim and a row lands
//! entirely or leaves nothing and no event.
//!
//! `row.claim` under the row's claim writes `row.claimed` and `seat.working`.
//! A writer applies it to [`PAIRS`] (row, seat) pairs while a second writer
//! applies unrelated `row.add`s on another handle and a reader on a third
//! snapshots in a loop. Every snapshot must show each pair whole or untouched:
//! the row `doing` under its seat exactly when the seat is `working`, and the
//! log up to the snapshot's id holding both events or neither. Each command's
//! two events are adjacent in the log (nothing commits between them) and are
//! the ids `apply` returned. A refused command leaves the snapshot and the head
//! as they were.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use wsp_store_contract::*;

use super::{key, person, Case, CaseResult};
use crate::Harness;

const PAIRS: usize = 24;
/// At most this many unrelated commits race the writer.
const NOISE: usize = 200;

/// Case 1 of 2.3.
pub fn case_atomicity<H: Harness>(h: &H) -> CaseResult {
    let c = Case { case: 1, name: "atomicity" };
    let w = h.fresh();
    let other = h.open(&w);
    let r = h.open(&w);

    let mut pairs = vec![];
    for i in 0..PAIRS {
        let row = c.add_row(&w, format!("atom-row-{i}"))?;
        let seat = c.spawn_seat(&w, format!("atom-seat-{i}"))?;
        let fence = c.take_claim(&w, &row, &seat)?;
        pairs.push((row, seat, fence));
    }
    let done = AtomicBool::new(false);

    thread::scope(|sc| {
        let writer = sc.spawn(|| {
            let out = (|| {
                for (i, (row, seat, fence)) in pairs.iter().enumerate() {
                    let cmd = Command::RowClaim { row: row.clone(), seat: seat.clone(), fence: fence.clone() };
                    let applied =
                        c.store(w.apply(cmd, key(format!("atom-claim-{i}")), person()), "applying row.claim")?;
                    check_whole(&c, &w, &applied, row, seat)?;
                }
                Ok(())
            })();
            done.store(true, Ordering::SeqCst);
            out
        });
        let noise = sc.spawn(|| {
            let mut i = 0;
            while !done.load(Ordering::SeqCst) && i < NOISE {
                c.add_row(&other, format!("atom-noise-{i}"))?;
                i += 1;
            }
            Ok(())
        });
        let reader = sc.spawn(|| {
            loop {
                // Read the flag first: a pass that starts after the writer finished sees everything.
                let last = done.load(Ordering::SeqCst);
                check_snapshot(&c, &r, &pairs)?;
                if last {
                    return Ok(());
                }
            }
        });
        c.joined(writer.join())?;
        c.joined(noise.join())?;
        c.joined(reader.join())
    })?;

    // Every pair landed; and a refused command (row.claim of a row already `doing`) leaves nothing.
    let (row, seat, fence) = &pairs[0];
    let (before, head) = c.store(w.snapshot(), "taking a snapshot")?;
    let again = Command::RowClaim { row: row.clone(), seat: seat.clone(), fence: fence.clone() };
    let refused = w.apply(again, key("atom-refused"), person());
    c.ensure(matches!(refused, Err(Error::Refused(_))), || {
        format!("row.claim of {}, already doing, answered {refused:?}, not a refusal", row.0)
    })?;
    let (after, head_after) = c.store(w.snapshot(), "taking a snapshot")?;
    c.ensure(head_after == head && after == before, || {
        format!("a refused row.claim moved the head from {} to {} or changed the state", head.0, head_after.0)
    })?;
    c.ensure(after.rows.get(row).is_some_and(|x| x.state == RowState::Doing), || {
        format!("{} is not doing after its row.claim", row.0)
    })
}

/// `applied` is exactly `row.claimed` for `row` then `seat.working` for `seat`, adjacent in the log.
fn check_whole<S: Store>(c: &Case, s: &S, applied: &Applied, row: &RowId, seat: &SeatId) -> CaseResult {
    c.ensure(applied.events.len() == 2, || {
        format!("row.claim of {} returned {} event ids, not 2: {:?}", row.0, applied.events.len(), applied.events)
    })?;
    let first = applied.events[0];
    let got = c.store(s.events_after(EventId(first.0.saturating_sub(1)), 2), "reading row.claim's events")?;
    let seen: Vec<_> = got.iter().map(|e| (e.id, e.kind, e.subject.clone())).collect();
    let want = vec![
        (applied.events[0], EventKind::RowClaimed, Subject::Row(row.clone())),
        (applied.events[1], EventKind::SeatWorking, Subject::Seat(seat.clone())),
    ];
    c.ensure(seen == want, || {
        format!("row.claim of {} should read back as {want:?}, adjacent in the log; the log has {seen:?}", row.0)
    })
}

/// One snapshot shows each pair whole or untouched, in its state and in the log up to its id.
fn check_snapshot<S: Store>(c: &Case, s: &S, pairs: &[(RowId, SeatId, Fence)]) -> CaseResult {
    let (state, at) = c.store(s.snapshot(), "taking a snapshot")?;
    let log = c.store(s.events_after(EventId(0), usize::MAX), "reading the log")?;
    let upto = log.iter().filter(|e| e.id <= at);
    let claimed: BTreeSet<_> =
        upto.clone().filter(|e| e.kind == EventKind::RowClaimed).map(|e| e.subject.clone()).collect();
    let working: BTreeSet<_> = upto.filter(|e| e.kind == EventKind::SeatWorking).map(|e| e.subject.clone()).collect();
    for (row, seat, _) in pairs {
        let r = state.rows.get(row);
        let row_done = r.is_some_and(|r| r.state == RowState::Doing && r.seat.as_ref() == Some(seat));
        let seat_done = state.seats.get(seat).is_some_and(|s| s.state == SeatState::Working);
        c.ensure(row_done == seat_done, || {
            format!(
                "the snapshot at {} shows half a row.claim: {} doing under {}: {row_done}, {} working: {seat_done}",
                at.0, row.0, seat.0, seat.0
            )
        })?;
        let row_logged = claimed.contains(&Subject::Row(row.clone()));
        let seat_logged = working.contains(&Subject::Seat(seat.clone()));
        c.ensure(row_logged == seat_logged && row_logged == row_done, || {
            format!(
                "the log up to {} holds half a row.claim, or disagrees with the snapshot: row.claimed {}: {row_logged}, \
                 seat.working {}: {seat_logged}, the state shows it: {row_done}",
                at.0, row.0, seat.0
            )
        })?;
    }
    Ok(())
}
