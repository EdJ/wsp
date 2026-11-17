//! Case 9, machine guards: an illegal transition is refused by `apply` with
//! its named reason, and no event is written.
//!
//! It drives the test domain's table (`crate::domain`'s module docs) until
//! wsp-machines' tables replace it. A world is set up through legal commands:
//! rows `todo`, `doing`, `done`, `blocked` and `parked`; seats `working`,
//! `ending` and `gone`; an order `cancelled`. Then each illegal verb is applied
//! to it, the fenced ones under a fence the store accepts (so the guard, not
//! the fence, refuses). Each must be refused with exactly
//! `illegal { subject, state, verb }` (the subject the guard names, its state's
//! wire name, the verb's), and leave the head and the snapshot as they were.

use std::result::Result;
use wsp_store_contract::*;

use super::{key, person, Case, CaseResult, LONG};
use crate::domain::verify_lease;
use crate::Harness;

/// Case 9 of 2.3.
pub fn case_machine_guards<H: Harness>(h: &H) -> CaseResult {
    let c = Case { case: 9, name: "machine guards" };
    let s = h.fresh();

    // The world.
    let worker = c.spawn_seat(&s, "guard-seat-worker")?;
    let ending = c.spawn_seat(&s, "guard-seat-ending")?;
    c.apply(&s, Command::SeatEnd { seat: ending.clone(), fence: None }, "guard-end")?;
    let gone = c.spawn_seat(&s, "guard-seat-gone")?;
    c.apply(&s, Command::SeatEnd { seat: gone.clone(), fence: None }, "guard-end-gone")?;
    c.apply(&s, Command::SeatDespawn { seat: gone.clone(), lost: false }, "guard-despawn")?;

    let done = c.add_row(&s, "guard-row-done")?;
    let claim_done = c.claim_row(&s, &done, &worker, "guard-claim-done")?;
    c.apply(&s, Command::RowReview { row: done.clone(), commit: "c0ffee".into(), fence: claim_done }, "guard-review")?;
    let verify_done = verify_fence(&c, &s, &done)?;
    let holds = Verdict::Holds { commit: "c0ffee".into() };
    c.apply(
        &s,
        Command::RowLand { row: done.clone(), verdict: holds.clone(), fence: verify_done.clone() },
        "guard-land",
    )?;

    let doing = c.add_row(&s, "guard-row-doing")?;
    let claim_doing = c.claim_row(&s, &doing, &worker, "guard-claim-doing")?;
    let verify_doing = verify_fence(&c, &s, &doing)?;

    let todo = c.add_row(&s, "guard-row-todo")?;
    let claim_todo = c.take_claim(&s, &todo, &worker)?;

    let blocked = c.add_row(&s, "guard-row-blocked")?;
    let on = || Block::On { reason: "waiting".into() };
    c.apply(&s, Command::RowBlock { row: blocked.clone(), block: on() }, "guard-block")?;
    let parked = c.add_row(&s, "guard-row-parked")?;
    c.apply(&s, Command::RowPark { row: parked.clone() }, "guard-park")?;

    let sent = c.apply(&s, Command::OrderSend { to: worker.clone(), text: "hello".into() }, "guard-order")?;
    let order = match sent.events.first().map(|id| c.subject_of(&s, *id)).transpose()? {
        Some(Subject::Order(id)) => id,
        other => return Err(c.fail(format!("order.send's event names {other:?}, not an order"))),
    };
    c.apply(&s, Command::OrderCancel { order: order.clone() }, "guard-cancel")?;

    let row = |id: &RowId| Subject::Row(id.clone());
    let seat = |id: &SeatId| Subject::Seat(id.clone());
    let cases: Vec<(Command, Subject, &str)> = vec![
        (
            Command::RowEdit { row: done.clone(), title: Some("x".into()), fields: Default::default() },
            row(&done),
            "done",
        ),
        (
            Command::RowClaim { row: doing.clone(), seat: worker.clone(), fence: claim_doing.clone() },
            row(&doing),
            "doing",
        ),
        (
            Command::RowClaim { row: todo.clone(), seat: ending.clone(), fence: claim_todo.clone() },
            seat(&ending),
            "ending",
        ),
        (Command::RowReview { row: todo.clone(), commit: "c0ffee".into(), fence: claim_todo }, row(&todo), "todo"),
        (Command::RowBlock { row: blocked.clone(), block: on() }, row(&blocked), "blocked"),
        (Command::RowBlock { row: todo.clone(), block: Block::Off }, row(&todo), "todo"),
        (Command::RowPark { row: doing.clone() }, row(&doing), "doing"),
        (Command::RowPark { row: parked.clone() }, row(&parked), "parked"),
        (Command::RowLand { row: doing.clone(), verdict: holds.clone(), fence: verify_doing }, row(&doing), "doing"),
        (Command::RowLand { row: done.clone(), verdict: holds, fence: verify_done }, row(&done), "done"),
        (Command::SeatEnd { seat: ending.clone(), fence: None }, seat(&ending), "ending"),
        (Command::SeatEnd { seat: gone.clone(), fence: None }, seat(&gone), "gone"),
        (Command::SeatDespawn { seat: worker.clone(), lost: false }, seat(&worker), "working"),
        (Command::OrderSend { to: ending.clone(), text: "late".into() }, seat(&ending), "ending"),
        (Command::OrderCancel { order: order.clone() }, Subject::Order(order), "cancelled"),
    ];

    for (n, (cmd, subject, state)) in cases.into_iter().enumerate() {
        let verb = crate::domain::verb(&cmd);
        let (before, head) = c.store(s.snapshot(), "taking a snapshot")?;
        let got = s.apply(cmd, key(format!("guard-illegal-{n}")), person());
        let want =
            Err(Error::Refused(Refusal::Illegal { subject: subject.clone(), state: state.into(), verb: verb.clone() }));
        c.ensure(got == want, || {
            format!("{verb} on {subject:?} in state {state} should be refused {want:?}; it answered {got:?}")
        })?;
        let (after, head_after) = c.store(s.snapshot(), "taking a snapshot")?;
        let written = c.store(s.events_after(head, usize::MAX), "reading the log")?;
        c.ensure(head_after == head && written.is_empty() && after == before, || {
            format!(
                "the illegal {verb} on {subject:?} was refused but wrote something: the head went {} -> {}, events {written:?}",
                head.0, head_after.0
            )
        })?;
    }
    Ok(())
}

/// Takes `verify:<row>` and returns its fence.
fn verify_fence<S: Store>(c: &Case, s: &S, row: &RowId) -> Result<Fence, super::CaseFailure> {
    let lease = verify_lease(row);
    let token =
        c.store(s.take(&lease, &Principal::Wsp("verifier".into()), LONG), "setting up, taking a verify lease")?;
    let token = token.ok_or_else(|| c.fail(format!("setting up: {} was free, and take answered None", lease.0)))?;
    Ok(Fence { lease, token })
}
