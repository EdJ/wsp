//! The smoke tests: a command applied through MemStore, and its event read back; and 2.3's cases against MemStore.

use std::collections::BTreeMap;
use std::time::Duration;

use wsp_store_conformance::{Harness, Knobs, MemHarness, TestDomain};
use wsp_store_contract::*;

#[test]
fn a_command_applied_through_memstore_reads_back_as_its_event() {
    let s = MemHarness::default().fresh();
    let by = Principal::Person("ed".into());
    let add = Command::RowAdd { project: ProjectId("wsp".into()), title: "one".into(), fields: BTreeMap::new() };
    let applied = s.apply(add.clone(), IdempotencyKey("k1".into()), by.clone()).unwrap();
    assert_eq!(applied.events, [EventId(1)]);
    assert_eq!(s.head().unwrap(), EventId(1));

    let events = s.events_after(EventId(0), 10).unwrap();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!((e.id, e.kind, &e.principal), (EventId(1), EventKind::RowAdded, &by));
    assert_eq!(e.subject, Subject::Row(RowId("r-1".into())));
    assert_eq!(s.row(&RowId("r-1".into())).unwrap().map(|r| r.state), Some(RowState::Todo));

    // The same key again returns the first result and writes nothing.
    assert_eq!(s.apply(add, IdempotencyKey("k1".into()), by).unwrap(), applied);
    assert_eq!(s.head().unwrap(), EventId(1));

    // Another handle on the same store sees it, and wakes on a commit from this one.
    let other = MemHarness::default().open(&s);
    assert_eq!(other.snapshot().unwrap().1, EventId(1));
    assert_eq!(other.wait_for(EventId(1), Duration::ZERO).unwrap(), EventId(1));
}

/// The test domain's claim path, end to end: a seat, a claim, a fenced row.claim
/// (one command, two events), a take-over that makes the old fence stale.
#[test]
fn a_fenced_claim_lands_and_a_stale_fence_is_refused() {
    let h = MemHarness::default();
    let s = h.fresh();
    let by = Principal::Person("ed".into());
    let key = |k: &str| IdempotencyKey(k.into());
    s.apply(Command::RowAdd { project: ProjectId("wsp".into()), title: "t".into(), fields: BTreeMap::new() }, key("a"), by.clone()).unwrap();
    s.apply(Command::SeatSpawn { row: None, attempt: 0, machine: None, kind: "claude".into(), model: None }, key("b"), by.clone()).unwrap();
    let (row, seat) = (RowId("r-1".into()), SeatId("s-1".into()));
    let ttl = Duration::from_secs(60);
    let t1 = Claims::claim(&s, &row, &seat, ttl, None).unwrap();
    let fence = Fence { lease: wsp_store_conformance::domain::claim_lease(&row), token: t1 };
    let applied = s
        .apply(Command::RowClaim { row: row.clone(), seat: seat.clone(), fence: fence.clone() }, key("c"), by.clone())
        .unwrap();
    assert_eq!(applied.events.len(), 2);
    assert_eq!(s.row(&row).unwrap().unwrap().state, RowState::Doing);
    assert_eq!(s.seat(&seat).unwrap().unwrap().state, SeatState::Working);

    // Released and claimed again: the token moves on, and the old fence is stale.
    Claims::release(&s, &row, t1).unwrap();
    let t2 = Claims::claim(&s, &row, &seat, ttl, None).unwrap();
    assert!(t2 > t1);
    let review = Command::RowReview { row: row.clone(), commit: "abc".into(), fence };
    assert_eq!(
        s.apply(review, key("d"), by),
        Err(Error::Refused(Refusal::Stale { lease: wsp_store_conformance::domain::claim_lease(&row), current: t2 }))
    );

    // The snapshot is the fold of the log.
    let (state, at) = s.snapshot().unwrap();
    let events = s.events_after(EventId(0), usize::MAX).unwrap();
    assert_eq!(events.last().map(|e| e.id), Some(at));
    let folded = events.iter().fold(State::default(), |st, e| TestDomain.evolve(st, e));
    assert_eq!(folded, state);
}

/// The crash hook: armed, the next apply stops midway and the handle is dead;
/// reopened, the store shows nothing of it, or half of it under `write_half`.
#[test]
fn the_crash_hook_stops_an_apply_and_reopen_shows_what_was_written() {
    for (knobs, visible) in [(Knobs::default(), 2), (Knobs { write_half: true, ..Knobs::default() }, 3)] {
        let h = MemHarness::with(knobs);
        let s = h.fresh();
        let by = Principal::Person("ed".into());
        let key = |k: &str| IdempotencyKey(k.into());
        s.apply(Command::RowAdd { project: ProjectId("wsp".into()), title: "t".into(), fields: BTreeMap::new() }, key("a"), by.clone()).unwrap();
        s.apply(Command::SeatSpawn { row: None, attempt: 0, machine: None, kind: "claude".into(), model: None }, key("b"), by.clone()).unwrap();
        let (row, seat) = (RowId("r-1".into()), SeatId("s-1".into()));
        let t = Claims::claim(&s, &row, &seat, Duration::from_secs(60), None).unwrap();
        let fence = Fence { lease: wsp_store_conformance::domain::claim_lease(&row), token: t };
        let w = h.open(&s);
        h.crash_next_apply(&w);
        let claim = Command::RowClaim { row: row.clone(), seat, fence };
        assert!(matches!(w.apply(claim, key("c"), by), Err(Error::Backend { .. })));
        assert!(matches!(w.head(), Err(Error::Backend { .. })), "the crashed handle is dead");
        let r = h.reopen(&w);
        assert_eq!(r.events_after(EventId(0), 100).unwrap().len(), visible + 1, "{knobs:?}");
    }
}

// 2.3's cases against MemStore, one test each (wsp-047 leg `state`).
mod cases {
    use wsp_store_conformance::cases::*;
    use wsp_store_conformance::MemHarness;

    #[test]
    fn case_1_atomicity() {
        case_atomicity(&MemHarness::default()).unwrap();
    }

    #[test]
    fn case_2_idempotency() {
        case_idempotency(&MemHarness::default()).unwrap();
    }

    #[test]
    fn case_3_lease_exclusivity() {
        case_lease_exclusivity(&MemHarness::default()).unwrap();
    }

    #[test]
    fn case_4_fencing() {
        case_fencing(&MemHarness::default()).unwrap();
    }

    #[test]
    fn case_9_machine_guards() {
        case_machine_guards(&MemHarness::default()).unwrap();
    }

    #[test]
    fn case_10_text() {
        case_text(&MemHarness::default()).unwrap();
    }
}
