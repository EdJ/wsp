//! Case 7, wake-ups: `wait_for(after)` returns within a bound after a commit
//! from another handle.
//!
//! Each of [`ROUNDS`] rounds starts [`WAITERS`] threads, each on its own
//! handle, waiting with `wait_for(head, TIMEOUT)`; they and the committer are
//! released together from a barrier, and the committer applies one `row.add`
//! on its handle. Every waiter must return a head at or past the commit's id,
//! no later than [`BOUND`] after `apply` returned: far inside `TIMEOUT`, so a
//! store that only notices a commit when the wait times out fails. Whether a
//! waiter was already asleep when the commit landed or not, the bound holds;
//! the rounds make both happen.
//!
//! And a wait with no commit is no wake-up: `wait_for(head, QUIET)` returns the
//! head unchanged, and not before `QUIET` has (nearly) passed, so a store whose
//! `wait_for` returns at once cannot pass the rounds by polling.

use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

use std::result::Result;
use wsp_store_contract::*;

use super::{key, person, Case, CaseResult};
use crate::Harness;

const ROUNDS: usize = 12;
const WAITERS: usize = 3;
/// A waiter returns at most this long after the commit's `apply` returned.
const BOUND: Duration = Duration::from_secs(2);
/// What each waiter asks for: well past the bound.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The quiet wait, with no commit.
const QUIET: Duration = Duration::from_millis(300);

/// Case 7 of 2.3.
pub fn case_wakeups<H: Harness>(h: &H) -> CaseResult {
    let c = Case {
        case: 7,
        name: "wake-ups",
    };
    let hs = h.handles(WAITERS + 1);
    let (committer, waiters) = hs.split_first().expect("handles(n) gives n");

    for round in 0..ROUNDS {
        let after = c.store(committer.head(), "reading the head")?;
        let barrier = Barrier::new(WAITERS + 1);
        let (committed, woke) = thread::scope(|sc| {
            let threads: Vec<_> = waiters
                .iter()
                .map(|s| {
                    let (c, barrier) = (&c, &barrier);
                    sc.spawn(move || {
                        barrier.wait();
                        let head = c.store(s.wait_for(after, TIMEOUT), format!("waiting past {}", after.0))?;
                        Ok((head, Instant::now()))
                    })
                })
                .collect();
            barrier.wait();
            let cmd = Command::RowAdd {
                project: ProjectId("conformance".into()),
                title: format!("wake-{round}"),
                fields: Default::default(),
            };
            let applied = c.store(
                committer.apply(cmd, key(format!("wake-{round}")), person()),
                "committing",
            );
            let done = Instant::now();
            let woke = threads
                .into_iter()
                .map(|t| c.joined(t.join()))
                .collect::<Result<Vec<_>, _>>();
            Ok::<_, super::CaseFailure>(((applied?, done), woke?))
        })?;
        let (applied, done) = committed;
        let id = applied
            .events
            .last()
            .copied()
            .ok_or_else(|| c.fail("row.add wrote no event"))?;
        for (n, (head, at)) in woke.into_iter().enumerate() {
            let late = at.saturating_duration_since(done);
            c.ensure(head >= id && late <= BOUND, || {
                format!(
                    "round {round}: waiter {n}, waiting past {}, returned head {} {}ms after another handle \
                     committed {} ({applied:?}); the bound is {}ms",
                    after.0,
                    head.0,
                    late.as_millis(),
                    id.0,
                    BOUND.as_millis()
                )
            })?;
        }
    }

    let head = c.store(committer.head(), "reading the head")?;
    let started = Instant::now();
    let got = c.store(waiters[0].wait_for(head, QUIET), "waiting with no commit")?;
    let took = started.elapsed();
    c.ensure(got == head && took >= QUIET * 9 / 10, || {
        format!(
            "wait_for({}, {}ms) with no commit returned head {} after {}ms: a wait returns early only on a commit",
            head.0,
            QUIET.as_millis(),
            got.0,
            took.as_millis()
        )
    })
}
