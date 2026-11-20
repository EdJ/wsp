//! Case 10, text: `set_text` with a stale `Rev` is refused, so no edit of Ed's
//! prose is lost.
//!
//! Sequentially: a write under the current revision succeeds and moves the
//! revision; a second write under the old one is refused with
//! `conflict { current }` and leaves the text, the revision and the head as
//! they were. Then [`ROUNDS`] races: two handles read the same revision and,
//! released from a barrier, both write under it. Exactly one succeeds, the
//! other is refused naming the winner's revision, and the text reads the
//! winner's. No revision is ever issued twice.

use std::collections::BTreeSet;
use std::sync::Barrier;
use std::thread;

use std::result::Result;
use wsp_store_contract::*;

use super::{Case, CaseResult};
use crate::Harness;

const ROUNDS: usize = 16;
const SECTION: &str = "notes";

/// Case 10 of 2.3.
pub fn case_text<H: Harness>(h: &H) -> CaseResult {
    let c = Case { case: 10, name: "text" };
    let hs = h.handles(2);
    let row = c.add_row(&hs[0], "text-row")?;

    let (_, r0) = c.store(hs[0].text(&row, SECTION), "reading the text")?;
    let r1 =
        c.store(hs[0].set_text(&row, SECTION, "first".into(), r0.clone()), "writing under the current revision")?;
    c.ensure(r1 != r0, || format!("a write must move the revision; it stayed {r0:?}"))?;
    let head = c.store(hs[0].head(), "reading the head")?;
    let got = hs[1].set_text(&row, SECTION, "lost".into(), r0.clone());
    let want = Err(Error::Refused(Refusal::Conflict { current: r1.clone() }));
    c.ensure(got == want, || format!("a write under stale {r0:?} should be refused {want:?}; it answered {got:?}"))?;
    let now = c.store(hs[1].text(&row, SECTION), "reading the text")?;
    let head_after = c.store(hs[1].head(), "reading the head")?;
    c.ensure(now == ("first".to_string(), r1.clone()) && head_after == head, || {
        format!(
            "a refused write changed something: the text reads {now:?} (want \"first\" at {r1:?}), the head {} -> {}",
            head.0, head_after.0
        )
    })?;

    let mut revs: BTreeSet<Rev> = [r0, r1].into();
    for i in 0..ROUNDS {
        let (_, base) = c.store(hs[0].text(&row, SECTION), "reading the text")?;
        let barrier = Barrier::new(hs.len());
        let answers = thread::scope(|sc| {
            let racers: Vec<_> = hs
                .iter()
                .enumerate()
                .map(|(n, s)| {
                    let (barrier, base, row) = (&barrier, base.clone(), &row);
                    sc.spawn(move || {
                        barrier.wait();
                        s.set_text(row, SECTION, format!("round {i} by handle {n}"), base)
                    })
                })
                .collect();
            racers.into_iter().map(|r| c.joined(r.join().map(Ok))).collect::<Result<Vec<_>, _>>()
        })?;

        let won: Vec<(usize, Rev)> =
            answers.iter().enumerate().filter_map(|(n, a)| a.as_ref().ok().map(|r| (n, r.clone()))).collect();
        let &[(w, ref rev)] = won.as_slice() else {
            return Err(c.fail(format!(
                "round {i}: two writes under {base:?}; exactly one must succeed, they answered {answers:?}"
            )));
        };
        let loser = &answers[1 - w];
        let want = Err(Error::Refused(Refusal::Conflict { current: rev.clone() }));
        c.ensure(*loser == want, || {
            format!("round {i}: the losing write should be refused {want:?}; it answered {loser:?}")
        })?;
        c.ensure(revs.insert(rev.clone()), || format!("round {i}: revision {rev:?} was issued before"))?;
        let now = c.store(hs[1 - w].text(&row, SECTION), "reading the text")?;
        let text = format!("round {i} by handle {w}");
        c.ensure(now == (text.clone(), rev.clone()), || {
            format!("round {i}: the text should read {text:?} at {rev:?}; it reads {now:?}")
        })?;
    }
    Ok(())
}
