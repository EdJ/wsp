//! Case 5, event order: `events_after(n)` returns events in commit order with
//! no gap a reader can skip. A reader that saw id *m* never later receives an
//! id less than *m* that it did not see (the hard case, 8.4).
//!
//! [`WRITERS`] handles apply `row.add`s at once, and take and release a lease
//! of their own between them (events the store writes itself), released
//! together from a barrier, while a reader on another handle tails
//! the log the way a subscriber does: `events_after(cursor, PAGE)`, its cursor
//! moved to the last id of each page, `wait_for` (bounded) when it is caught
//! up. Every page must be strictly increasing and above its cursor. Once the
//! writers are done the reader drains, and then:
//! - every id the writers' `apply` returned is in the log;
//! - the whole log, read from 0, is strictly increasing;
//! - the reader saw every id in it: an id it never saw is one it skipped.
//!
//! The ordering checks come before "every applied id is in the log", and the
//! setup reads nothing back, so a store that publishes a lower id after a
//! higher one fails here on the order, not on a missing event.
//!
//! Ids need not be consecutive (a backend may lose a rolled-back id); only an
//! id that becomes visible below a cursor is a skip.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

use std::result::Result;
use wsp_store_contract::*;

use super::{key, person, Case, CaseResult, LONG};
use crate::Harness;

const WRITERS: usize = 4;
/// Commands each writer applies.
const EACH: usize = 40;
/// The reader's page: small, so it pages often.
const PAGE: usize = 7;
/// The whole case finishes inside this, or fails.
const BOUND: Duration = Duration::from_secs(20);

/// Case 5 of 2.3.
pub fn case_event_order<H: Harness>(h: &H) -> CaseResult {
    let c = Case {
        case: 5,
        name: "event order",
    };
    let hs = h.handles(WRITERS + 1);
    let (reader, writers) = hs.split_first().expect("handles(n) gives n");
    let start = c.store(reader.head(), "reading the head")?;
    let running = AtomicUsize::new(WRITERS);
    let barrier = Barrier::new(WRITERS + 1);
    let deadline = Instant::now() + BOUND;

    let (applied, seen) = thread::scope(|sc| {
        let racers: Vec<_> = writers
            .iter()
            .enumerate()
            .map(|(w, s)| {
                let (c, barrier, running) = (&c, &barrier, &running);
                sc.spawn(move || {
                    barrier.wait();
                    let out = write(c, s, w);
                    running.fetch_sub(1, Ordering::SeqCst);
                    out
                })
            })
            .collect();
        let tail = sc.spawn(|| {
            barrier.wait();
            tail(&c, reader, start, &running, deadline)
        });
        let mut applied = vec![];
        for r in racers {
            applied.extend(c.joined(r.join())?);
        }
        Ok::<_, super::CaseFailure>((applied, c.joined(tail.join())?))
    })?;

    let log: Vec<EventId> = c
        .store(reader.events_after(start, usize::MAX), "reading the log")?
        .iter()
        .map(|e| e.id)
        .collect();
    let backwards: Vec<_> = log
        .windows(2)
        .filter(|p| p[0] >= p[1])
        .map(|p| (p[0].0, p[1].0))
        .collect();
    c.ensure(backwards.is_empty(), || {
        format!(
            "events_after({}) is not strictly increasing; out of order at {backwards:?}",
            start.0
        )
    })?;
    let skipped: Vec<_> = log.iter().filter(|id| !seen.contains(id)).map(|id| id.0).collect();
    c.ensure(skipped.is_empty(), || {
        format!(
            "a reader tailing the log from {} skipped ids {skipped:?}: they became visible below its cursor",
            start.0
        )
    })?;
    let in_log: BTreeSet<_> = log.iter().copied().collect();
    let missing: Vec<_> = applied
        .iter()
        .filter(|id| !in_log.contains(id))
        .map(|id| id.0)
        .collect();
    c.ensure(missing.is_empty(), || {
        format!("apply returned ids that events_after never returns: {missing:?}")
    })
}

/// Writer `w`'s commands on `s`, and its lease's takes and releases; returns the ids `apply` returned.
fn write<S: Store>(c: &Case, s: &S, w: usize) -> Result<Vec<EventId>, super::CaseFailure> {
    let lease = LeaseName(format!("order-lease-{w}"));
    let holder = Principal::Wsp(format!("order-{w}"));
    let mut ids = vec![];
    for i in 0..EACH {
        let cmd = Command::RowAdd {
            project: ProjectId("conformance".into()),
            title: format!("order-{w}-{i}"),
            fields: Default::default(),
        };
        ids.extend(
            c.store(
                s.apply(cmd, key(format!("order-{w}-{i}")), person()),
                "applying row.add",
            )?
            .events,
        );
        if i % 4 == 0 {
            let taken = c.store(s.take(&lease, &holder, LONG), "taking a lease")?;
            let token = taken.ok_or_else(|| c.fail(format!("{} was free, and take answered None", lease.0)))?;
            c.store(Leases::release(s, &lease, token), "releasing a lease")?;
        }
    }
    Ok(ids)
}

/// Tails the log from `from` until the writers are done and it is drained;
/// returns every id it saw.
fn tail<S: Store>(
    c: &Case,
    s: &S,
    from: EventId,
    running: &AtomicUsize,
    deadline: Instant,
) -> Result<BTreeSet<EventId>, super::CaseFailure> {
    let mut cursor = from;
    let mut seen = BTreeSet::new();
    loop {
        // Read the count first: a pass that starts after the last writer finished sees everything.
        let last = running.load(Ordering::SeqCst) == 0;
        let page = c.store(
            s.events_after(cursor, PAGE),
            format!("reading the log after {}", cursor.0),
        )?;
        let mut prev = cursor;
        for e in &page {
            c.ensure(e.id > prev, || {
                let ids: Vec<_> = page.iter().map(|e| e.id.0).collect();
                format!(
                    "events_after({}) returned {ids:?}: not strictly increasing above its cursor",
                    cursor.0
                )
            })?;
            prev = e.id;
            seen.insert(e.id);
        }
        if let Some(e) = page.last() {
            cursor = e.id;
            continue;
        }
        if last {
            return Ok(seen);
        }
        c.ensure(Instant::now() < deadline, || {
            format!("the writers did not finish within {}s", BOUND.as_secs())
        })?;
        c.store(s.wait_for(cursor, Duration::from_millis(50)), "waiting for a commit")?;
    }
}
