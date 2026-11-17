//! Case 4, fencing: a write under token *t* after a take-over to *t+1* is
//! refused, and nothing it would have written is visible.
//!
//! Two fenced writes the test domain would otherwise allow, so only the fence
//! can stop them:
//! - `row.review` under a row's claim, after the claim is released and taken
//!   by another seat;
//! - `row.land` under `verify:<row>`, after the lease is released and taken by
//!   another verifier.
//!
//! Each stale write must be refused with `stale`, naming the lease and its
//! current token, and leave the head and the snapshot as they were; the same
//! write under the current token then lands, so the fence is not refusing
//! everything.

use wsp_store_contract::*;

use super::{key, person, Case, CaseResult, LONG};
use crate::domain::{claim_lease, verify_lease};
use crate::Harness;

/// Case 4 of 2.3.
pub fn case_fencing<H: Harness>(h: &H) -> CaseResult {
    let c = Case { case: 4, name: "fencing" };
    let s = h.fresh();
    let row = c.add_row(&s, "fence-row")?;
    let first = c.spawn_seat(&s, "fence-seat-1")?;
    let second = c.spawn_seat(&s, "fence-seat-2")?;

    // The claim, taken over by another seat.
    let old = c.claim_row(&s, &row, &first, "fence-claim")?;
    c.store(Claims::release(&s, &row, old.token), "releasing the claim")?;
    let current = c.store(Claims::claim(&s, &row, &second, LONG, None), "claiming for the second seat")?;
    c.ensure(current > old.token, || {
        format!("a take-over must issue a greater token: {} after {}", current.0, old.token.0)
    })?;
    let review = |fence| Command::RowReview { row: row.clone(), commit: "c0ffee".into(), fence };
    refused_stale(&c, &s, review(old.clone()), "fence-review-stale", &claim_lease(&row), current)?;
    c.apply(&s, review(Fence { lease: claim_lease(&row), token: current }), "fence-review")?;

    // The verifier's lease, taken over by another verifier.
    let lease = verify_lease(&row);
    let taken = c.store(s.take(&lease, &Principal::Wsp("verifier-1".into()), LONG), "taking the verify lease")?;
    let stale = taken.ok_or_else(|| c.fail(format!("{} was free, and take answered None", lease.0)))?;
    c.store(Leases::release(&s, &lease, stale), "releasing the verify lease")?;
    let taken = c.store(s.take(&lease, &Principal::Wsp("verifier-2".into()), LONG), "taking over the verify lease")?;
    let current = taken.ok_or_else(|| c.fail(format!("{} was released, and take answered None", lease.0)))?;
    c.ensure(current > stale, || format!("a take-over must issue a greater token: {} after {}", current.0, stale.0))?;
    let land = |token| Command::RowLand {
        row: row.clone(),
        verdict: Verdict::Holds { commit: "c0ffee".into() },
        fence: Fence { lease: lease.clone(), token },
    };
    refused_stale(&c, &s, land(stale), "fence-land-stale", &lease, current)?;
    c.apply(&s, land(current), "fence-land")?;
    let landed = c.store(s.row(&row), "reading the row")?;
    c.ensure(landed.as_ref().is_some_and(|r| r.state == RowState::Done), || {
        format!("row.land under the current token {} should land {}; it reads {landed:?}", current.0, row.0)
    })
}

/// `cmd` is refused with `stale { lease, current }`, and writes nothing.
fn refused_stale<S: Store>(c: &Case, s: &S, cmd: Command, k: &str, lease: &LeaseName, current: Token) -> CaseResult {
    let what = crate::domain::verb(&cmd);
    let (before, head) = c.store(s.snapshot(), "taking a snapshot")?;
    let got = s.apply(cmd, key(k), person());
    let want = Err(Error::Refused(Refusal::Stale { lease: lease.clone(), current }));
    c.ensure(got == want, || {
        format!(
            "{what} under a token older than {}'s current {} should be refused {want:?}; it answered {got:?}",
            lease.0, current.0
        )
    })?;
    let (after, head_after) = c.store(s.snapshot(), "taking a snapshot")?;
    let written = c.store(s.events_after(head, usize::MAX), "reading the log")?;
    c.ensure(head_after == head && written.is_empty() && after == before, || {
        format!(
            "a stale {what} was refused but wrote something: the head went {} -> {}, events {written:?}",
            head.0, head_after.0
        )
    })
}
