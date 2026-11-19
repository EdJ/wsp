//! Case 2, idempotency: the same key applied twice, concurrently from two
//! handles, yields one change, one set of events and the same `Applied`.
//!
//! Each of [`ROUNDS`] rounds releases two handles from a barrier to apply the
//! same `row.add` under the same key. Both must answer `Ok` with equal
//! `Applied`; the log must have grown by exactly those ids, and the state by
//! one row. A third, later apply of the key (from either handle) answers the
//! same and writes nothing. `row.add` is the command because applying it twice
//! is visible: the second would name another row.

use std::sync::Barrier;
use std::thread;

use std::result::Result;
use wsp_store_contract::*;

use super::{key, person, Case, CaseResult};
use crate::Harness;

const ROUNDS: usize = 24;

/// Case 2 of 2.3.
pub fn case_idempotency<H: Harness>(h: &H) -> CaseResult {
    let c = Case { case: 2, name: "idempotency" };
    let hs = h.handles(2);
    for i in 0..ROUNDS {
        let k = format!("idem-{i}");
        let cmd =
            Command::RowAdd { project: ProjectId("conformance".into()), title: k.clone(), fields: Default::default() };
        let (before, head) = c.store(hs[0].snapshot(), "taking a snapshot")?;

        let barrier = Barrier::new(hs.len());
        let answers = thread::scope(|sc| {
            let racers: Vec<_> = hs
                .iter()
                .map(|s| {
                    let (cmd, barrier) = (cmd.clone(), &barrier);
                    let k = key(&k);
                    sc.spawn(move || {
                        barrier.wait();
                        s.apply(cmd, k, person())
                    })
                })
                .collect();
            racers.into_iter().map(|r| c.joined(r.join().map(Ok))).collect::<Result<Vec<_>, _>>()
        })?;

        let mut applied = vec![];
        for (n, a) in answers.into_iter().enumerate() {
            applied.push(c.store(a, format!("handle {n} applying key {k}"))?);
        }
        let first = applied[0].clone();
        c.ensure(applied.iter().all(|a| *a == first), || {
            format!("two handles applied key {k} at once and were answered differently: {applied:?}")
        })?;

        let new = c.store(hs[1].events_after(head, usize::MAX), "reading the log")?;
        let ids: Vec<_> = new.iter().map(|e| e.id).collect();
        c.ensure(ids == first.events, || {
            format!("key {k} applied twice at once should write once, {:?}; the log grew by {ids:?}", first.events)
        })?;
        let (after, _) = c.store(hs[1].snapshot(), "taking a snapshot")?;
        c.ensure(after.rows.len() == before.rows.len() + 1, || {
            format!(
                "key {k} applied twice at once should add one row; rows went {} -> {}",
                before.rows.len(),
                after.rows.len()
            )
        })?;

        let later = c.store(hs[i % 2].apply(cmd, key(&k), person()), format!("applying key {k} again"))?;
        let head_after = c.store(hs[0].head(), "reading the head")?;
        let expected_head = first.events.last().copied().unwrap_or(head);
        c.ensure(later == first && head_after == expected_head, || {
            format!(
                "key {k} applied a third time answered {later:?} (first {first:?}) and moved the head to {}",
                head_after.0
            )
        })?;
    }
    Ok(())
}
