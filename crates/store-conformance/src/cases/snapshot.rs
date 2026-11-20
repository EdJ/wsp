//! Case 6, snapshot consistency: `snapshot()`'s state equals the fold of all
//! events up to its id.
//!
//! A writer drives every kind of write the store has, so the fold covers the
//! events `decide` writes and those the store writes itself: `row.add`,
//! `seat.spawn`, a claim and `row.claim`, `row.review`, a lease taken and
//! released, a claim released, a text set. A second writer adds rows on
//! another handle, and a reader on a third snapshots in a loop. For every
//! snapshot `(state, at)` the reader reads the log and folds the events with
//! ids up to `at` through the test domain's `evolve` from the empty state: the
//! fold must equal `state`. At the end, with the writers quiet, the snapshot's
//! id is the head and the last id in the log.
//!
//! The case creates its store with [`TestDomain`] (`Harness::fresh`), so it
//! folds with the same domain the store's `apply` decides with.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use std::result::Result;
use wsp_store_contract::*;

use super::{Case, CaseResult, LONG};
use crate::{Harness, TestDomain};

/// Rounds of the main writer's commands.
const ROUNDS: usize = 12;
/// Rows the second writer adds.
const NOISE: usize = 60;

/// Case 6 of 2.3.
pub fn case_snapshot<H: Harness>(h: &H) -> CaseResult {
    let c = Case {
        case: 6,
        name: "snapshot consistency",
    };
    let w = h.fresh();
    let other = h.open(&w);
    let r = h.open(&w);
    let running = AtomicUsize::new(2);

    thread::scope(|sc| {
        let writer = sc.spawn(|| {
            let out = (|| {
                for i in 0..ROUNDS {
                    let row = c.add_row(&w, format!("snap-row-{i}"))?;
                    let seat = c.spawn_seat(&w, format!("snap-seat-{i}"))?;
                    let fence = c.claim_row(&w, &row, &seat, format!("snap-claim-{i}"))?;
                    let review = Command::RowReview {
                        row: row.clone(),
                        commit: "c0ffee".into(),
                        fence: fence.clone(),
                    };
                    c.apply(&w, review, format!("snap-review-{i}"))?;
                    c.store(Claims::release(&w, &row, fence.token), "releasing a claim")?;
                    let lease = LeaseName(format!("snap-lease-{i}"));
                    let token = c.store(w.take(&lease, &Principal::Wsp("snap".into()), LONG), "taking a lease")?;
                    let token = token.ok_or_else(|| c.fail(format!("{} was free, and take answered None", lease.0)))?;
                    c.store(Leases::release(&w, &lease, token), "releasing a lease")?;
                    let (_, rev) = c.store(w.text(&row, "notes"), "reading a text")?;
                    c.store(w.set_text(&row, "notes", format!("note {i}"), rev), "setting a text")?;
                }
                Ok(())
            })();
            running.fetch_sub(1, Ordering::SeqCst);
            out
        });
        let noise = sc.spawn(|| {
            let out = (|| {
                for i in 0..NOISE {
                    c.add_row(&other, format!("snap-noise-{i}"))?;
                }
                Ok(())
            })();
            running.fetch_sub(1, Ordering::SeqCst);
            out
        });
        let reader = sc.spawn(|| loop {
            let last = running.load(Ordering::SeqCst) == 0;
            check(&c, &r)?;
            if last {
                return Ok(());
            }
        });
        c.joined(writer.join())?;
        c.joined(noise.join())?;
        c.joined(reader.join())
    })?;

    let at = check(&c, &w)?;
    let head = c.store(w.head(), "reading the head")?;
    let log = c.store(w.events_after(EventId(0), usize::MAX), "reading the log")?;
    let last = log.last().map(|e| e.id).unwrap_or_default();
    c.ensure(at == head && at == last, || {
        format!(
            "with no writer running, the snapshot is at {}, the head {} and the log ends at {}",
            at.0, head.0, last.0
        )
    })
}

/// One snapshot equals the fold of the log up to its id; returns the id.
fn check<S: Store>(c: &Case, s: &S) -> Result<EventId, super::CaseFailure> {
    let (state, at) = c.store(s.snapshot(), "taking a snapshot")?;
    let log = c.store(s.events_after(EventId(0), usize::MAX), "reading the log")?;
    let upto: Vec<_> = log.iter().filter(|e| e.id <= at).collect();
    let folded = upto.iter().fold(State::default(), |st, e| TestDomain.evolve(st, e));
    c.ensure(folded == state, || {
        format!(
            "the snapshot at {} is not the fold of the {} events up to it:\n  snapshot: {state:?}\n  fold:     {folded:?}",
            at.0,
            upto.len()
        )
    })?;
    Ok(at)
}
