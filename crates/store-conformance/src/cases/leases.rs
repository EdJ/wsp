//! Case 3, lease exclusivity: N handles race `take`, exactly one wins per
//! expiry window, and tokens strictly increase across take-overs.
//!
//! Three take-overs of one lease, each a race of [`RACERS`] handles released
//! from a barrier with distinct holders:
//! 1. the first take of a free lease;
//! 2. after the holder releases it; the old token can then neither renew nor
//!    release the new holder's lease;
//! 3. after a short lease ([`SHORT`]) expires: while it is live every other
//!    handle's `take` answers `None`; past its expiry the racers poll `take`
//!    until one wins or each sees the lease taken (bounded at [`SHORT`] plus
//!    [`BOUND`]; the poll waits for a clock, not for an interleaving).
//!
//! Each race has exactly one winner, `lease()` names it at its token, and
//! every token is greater than the one before.

use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

use std::result::Result;
use wsp_store_contract::*;

use super::{Case, CaseFailure, CaseResult, LONG};
use crate::Harness;

const RACERS: usize = 8;
/// The lease that expires in round 3.
const SHORT: Duration = Duration::from_millis(800);
/// How long past [`SHORT`] the racers may poll before the case fails.
const BOUND: Duration = Duration::from_secs(10);

fn holder(i: usize) -> Principal {
    Principal::Wsp(format!("racer-{i}"))
}

/// Case 3 of 2.3.
pub fn case_lease_exclusivity<H: Harness>(h: &H) -> CaseResult {
    let c = Case { case: 3, name: "lease exclusivity" };
    let hs = h.handles(RACERS);
    let name = LeaseName("conformance:exclusive".into());

    // 1. A free lease.
    let (w1, t1) = race(&c, &hs, &name, None, "the first take")?;
    for (i, s) in hs.iter().enumerate().filter(|(i, _)| *i != w1) {
        let again = c.store(s.take(&name, &holder(i), LONG), "taking a held lease")?;
        c.ensure(again.is_none(), || {
            format!("racer {i} took {} at {again:?} while racer {w1} held it at {}", name.0, t1.0)
        })?;
    }

    // 2. Released, and raced again.
    c.store(Leases::release(&hs[w1], &name, t1), "releasing the lease")?;
    let (w2, t2) = race(&c, &hs, &name, Some(t1), "a take after release")?;
    let renewed = c.store(hs[w1].renew(&name, t1, LONG), "renewing under an old token")?;
    c.ensure(!renewed, || format!("token {} renewed {} after racer {w2} took it over at {}", t1.0, name.0, t2.0))?;
    c.store(Leases::release(&hs[w1], &name, t1), "releasing under an old token")?;
    let still = c.store(hs[w2].lease(&name), "reading the lease")?;
    c.ensure(still.as_ref().is_some_and(|l| l.token == t2 && l.holder == holder(w2)), || {
        format!("releasing under old token {} ended racer {w2}'s lease at {}: the lease reads {still:?}", t1.0, t2.0)
    })?;

    // 3. A short lease, live then expired.
    c.store(Leases::release(&hs[w2], &name, t2), "releasing the lease")?;
    let t3 = c.store(hs[0].take(&name, &holder(0), SHORT), "taking a short lease")?;
    let t3 = t3.ok_or_else(|| c.fail(format!("{} was free, and take answered None", name.0)))?;
    c.ensure(t3 > t2, || format!("tokens must strictly increase: {} after {}", t3.0, t2.0))?;
    let taken = Instant::now();
    for (i, s) in hs.iter().enumerate().skip(1) {
        let again = c.store(s.take(&name, &holder(i), LONG), "taking a live lease")?;
        if taken.elapsed() < SHORT {
            c.ensure(again.is_none(), || {
                format!("racer {i} took {} at {again:?} inside racer 0's expiry window (token {})", name.0, t3.0)
            })?;
        } else if again.is_some() {
            // The check ran too slowly to say anything; the window is gone.
            return Err(c.fail(format!("the store took over {SHORT:?} to answer {RACERS} takes; raise SHORT")));
        }
    }
    let (_, t4) = race(&c, &hs, &name, Some(t3), "a take after expiry")?;
    c.ensure(t4 > t3, || format!("tokens must strictly increase: {} after {}", t4.0, t3.0))
}

/// Races every handle at `take(name)` and returns the one winner and its
/// token, greater than `after`. With `after` set the lease may still be held
/// when the racers start (it expires), so each polls until it wins or sees
/// another's token past `after`.
fn race<S: Store + Sync>(
    c: &Case,
    hs: &[S],
    name: &LeaseName,
    after: Option<Token>,
    what: &str,
) -> Result<(usize, Token), CaseFailure> {
    let barrier = Barrier::new(hs.len());
    let deadline = Instant::now() + SHORT + BOUND;
    let won = thread::scope(|sc| {
        let racers: Vec<_> = hs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let barrier = &barrier;
                sc.spawn(move || -> Result<Option<Token>, CaseFailure> {
                    barrier.wait();
                    loop {
                        if let Some(t) = c.store(s.take(name, &holder(i), LONG), format!("{what}, racer {i}"))? {
                            return Ok(Some(t));
                        }
                        let now = c.store(s.lease(name), "reading the lease")?;
                        let past = |l: &Lease| after.is_none_or(|a| l.token > a);
                        if now.as_ref().is_some_and(past) || after.is_none() {
                            return Ok(None);
                        }
                        if Instant::now() > deadline {
                            return Err(c.fail(format!(
                                "{what}: {} was never free within {:?}; it reads {now:?}",
                                name.0,
                                SHORT + BOUND
                            )));
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                })
            })
            .collect();
        racers.into_iter().map(|r| c.joined(r.join())).collect::<Result<Vec<_>, _>>()
    })?;

    let winners: Vec<(usize, Token)> = won.iter().enumerate().filter_map(|(i, t)| t.map(|t| (i, t))).collect();
    let &[(w, t)] = winners.as_slice() else {
        return Err(c.fail(format!(
            "{what}: {} handles raced take of {}; exactly one must win, these did: {winners:?}",
            hs.len(),
            name.0
        )));
    };
    if let Some(a) = after {
        c.ensure(t > a, || format!("{what}: tokens must strictly increase across take-overs: {} after {}", t.0, a.0))?;
    }
    let lease = c.store(hs[w].lease(name), "reading the lease")?;
    c.ensure(lease.as_ref().is_some_and(|l| l.token == t && l.holder == holder(w)), || {
        format!("{what}: racer {w} won {} at {}, but the lease reads {lease:?}", name.0, t.0)
    })?;
    Ok((w, t))
}
