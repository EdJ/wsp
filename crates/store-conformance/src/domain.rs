//! A small test domain: rows, seats, claims and orders, enough of section 3's
//! machines for the suite to drive a store with, and a few illegal transitions
//! so case 9 has something to refuse. `wsp-machines` brings the real one.
//!
//! Ids are named from the state: the n-th row is `r-n`, the n-th seat `s-n`,
//! the n-th order `o-n` (nothing is ever removed, so the count only grows).
//! A record's first event carries the whole record as its payload (`row.added`
//! a [`Row`], `seat.requested` a [`Seat`], `order.queued` an [`Order`]); a
//! transition's event carries what it changes.
//!
//! The events a store writes itself, outside [`Domain::decide`], are folded
//! here too (see [`crate::mem`]'s module docs): `lease.taken` carries a [`Lease`],
//! `claim.taken` a [`Claim`], and `lease.released` / `claim.released` name theirs
//! by subject. `text.set` changes no state.
//!
//! What is legal (the guards case 9 drives):
//!
//! | verb | from | refused otherwise with |
//! |---|---|---|
//! | `row.edit` | any but `done` | `illegal` |
//! | `row.claim` | `todo`, `sent_back`; a live seat; the fence is `claim:<row>`, held by that seat | `illegal`, `not_found`, `invalid`, `held` |
//! | `row.review` | `doing`; the fence is `claim:<row>` | `illegal`, `invalid` |
//! | `row.block` (on) | `todo`, `doing`, `review`, `sent_back` | `illegal` |
//! | `row.block` (off) | `blocked` | `illegal` |
//! | `row.park` | `todo`, `sent_back`, `blocked` | `illegal` |
//! | `row.land` | `review`; the fence is `verify:<row>` | `illegal`, `invalid` |
//! | `seat.end` | `requested`, `spawning`, `started`, `working`, `idle`, `lost` | `illegal` |
//! | `seat.despawn` | `ending`; `lost` with `lost: true` | `illegal` |
//! | `order.send` | to a seat not `ending` or `gone` | `illegal`, `not_found` |
//! | `order.cancel` | `queued`, `typed` | `illegal` |
//! | `order.deliver`, `order.delivered`, `order.acked`, `order.refused`, `order.done` | never: this domain stops an order at `queued` (wsp-117) | `illegal` |
//! | `hub.user.add`, `hub.user.revoke`, `hub.token.issue`, `hub.token.revoke` | never: the hub's identity is wsp-159's domain (wsp-158) | `illegal` |
//! | `mirror` | never: it is step 1's, and only the server's mirror domain decides it | `illegal` |
//!
//! `row.add` and `seat.spawn` are always legal (a second `seat.spawn` with the
//! same `(row, attempt)` as a seat not `gone` changes nothing: 3.2's spawn key).
//! The verbs of lists, asks and machines are refused with `invalid`: the test
//! domain does not have them.
//!
//! `mirror` is refused as a real domain refuses it (the contract's MAP.md):
//! `illegal`, with the state `owned` (this domain owns its entities, so nothing
//! mirrors them), naming the first subject it would `put` or remove, or the
//! project `""` when it names none.
//!
//! The orders machine's verbs past `order.send` (wsp-117) are refused the same
//! way: `illegal`, naming the order, with its state's wire name, or `""` when
//! the state has no such order. The hub's four verbs are refused `illegal` the
//! same way, naming the user or the token record, with the state `""` (this
//! domain holds none). `order.send`'s `first` is kept on the [`Order`].

use serde_json::{json, Value};
use wsp_store_contract::*;
use std::result::Result;

/// The lease a row's claim is (2.1): `claim:<row>`.
pub fn claim_lease(row: &RowId) -> LeaseName {
    LeaseName(format!("claim:{}", row.0))
}

/// The lease a verifier lands a row under (2.1): `verify:<row>`.
pub fn verify_lease(row: &RowId) -> LeaseName {
    LeaseName(format!("verify:{}", row.0))
}

/// The test domain. Stateless: everything it knows is in the [`State`] it is given.
#[derive(Clone, Copy, Debug, Default)]
pub struct TestDomain;

/// A serializable value as JSON (`null` if it will not serialize, which no contract type does).
macro_rules! to_value {
    ($e:expr) => {
        serde_json::to_value($e).unwrap_or(Value::Null)
    };
}

/// A JSON value as a contract type, if it is one.
macro_rules! from {
    ($t:ty, $v:expr) => {
        serde_json::from_value::<$t>($v.clone()).ok()
    };
}

/// A state's wire name (`sent_back`), as [`Refusal::Illegal`] carries it.
macro_rules! wire {
    ($state:expr) => {
        to_value!($state).as_str().unwrap_or_default().to_string()
    };
}

/// A command's verb (`row.claim`).
pub fn verb(cmd: &Command) -> String {
    to_value!(cmd).get("verb").and_then(Value::as_str).unwrap_or_default().to_string()
}

fn invalid(reason: impl Into<String>) -> Refusal {
    Refusal::Invalid { reason: reason.into() }
}

fn draft(kind: EventKind, subject: Subject, payload: Value) -> Draft {
    Draft { kind, subject, payload }
}

/// The refusal of a verb the subject's state does not allow.
macro_rules! illegal {
    ($subject:expr, $state:expr, $cmd:expr) => {
        Refusal::Illegal { subject: $subject, state: wire!($state), verb: verb($cmd) }
    };
}

const LIVE: &[SeatState] =
    &[SeatState::Requested, SeatState::Spawning, SeatState::Started, SeatState::Working, SeatState::Idle];

impl TestDomain {
    fn row<'s>(state: &'s State, id: &RowId) -> Result<&'s Row, Refusal> {
        state.rows.get(id).ok_or_else(|| Refusal::NotFound { subject: Subject::Row(id.clone()) })
    }

    fn seat<'s>(state: &'s State, id: &SeatId) -> Result<&'s Seat, Refusal> {
        state.seats.get(id).ok_or_else(|| Refusal::NotFound { subject: Subject::Seat(id.clone()) })
    }

    fn row_in(row: &Row, ok: &[RowState], cmd: &Command) -> Result<(), Refusal> {
        if ok.contains(&row.state) {
            Ok(())
        } else {
            Err(illegal!(Subject::Row(row.id.clone()), &row.state, cmd))
        }
    }

    /// The subject a mirrored entity is put under.
    fn subject_of(entity: &Entity) -> Subject {
        match entity {
            Entity::Row(r) => Subject::Row(r.id.clone()),
            Entity::List(l) => Subject::List(l.id.clone()),
            Entity::Seat(s) => Subject::Seat(s.id.clone()),
            Entity::Claim(c) => Subject::Claim(c.row.clone()),
            Entity::Order(o) => Subject::Order(o.id.clone()),
            Entity::Ask(a) => Subject::Ask(a.id.clone()),
            Entity::Machine(m) => Subject::Machine(m.name.clone()),
        }
    }

    fn fence_is(fence: &Fence, lease: LeaseName) -> Result<(), Refusal> {
        if fence.lease == lease {
            Ok(())
        } else {
            Err(invalid(format!("the fence names {}, not {}", fence.lease.0, lease.0)))
        }
    }
}

impl Domain for TestDomain {
    fn decide(&self, state: &State, cmd: &Command, by: &Principal) -> Result<Vec<Draft>, Refusal> {
        use RowState::*;
        Ok(match cmd {
            Command::RowAdd { project, title, fields } => {
                let id = RowId(format!("r-{}", state.rows.len() + 1));
                let row = Row {
                    id: id.clone(),
                    project: project.clone(),
                    title: title.clone(),
                    state: Todo,
                    before_block: None,
                    seat: None,
                    group: None,
                    commit: None,
                    fields: fields.clone(),
                };
                vec![draft(EventKind::RowAdded, Subject::Row(id), to_value!(&row))]
            }
            Command::RowEdit { row, title, fields } => {
                let r = Self::row(state, row)?;
                Self::row_in(r, &[Todo, Doing, Review, SentBack, Blocked, Parked], cmd)?;
                vec![draft(EventKind::RowEdited, Subject::Row(row.clone()), json!({ "title": title, "fields": fields }))]
            }
            Command::RowClaim { row, seat, fence } => {
                let r = Self::row(state, row)?;
                Self::row_in(r, &[Todo, SentBack], cmd)?;
                let s = Self::seat(state, seat)?;
                if !LIVE.contains(&s.state) {
                    return Err(illegal!(Subject::Seat(seat.clone()), &s.state, cmd));
                }
                Self::fence_is(fence, claim_lease(row))?;
                match state.claims.get(row) {
                    Some(c) if c.seat == *seat => {}
                    Some(c) => {
                        return Err(Refusal::Held { lease: claim_lease(row), holder: Principal::Seat(c.seat.clone()) })
                    }
                    None => return Err(invalid(format!("{} is not claimed", row.0))),
                }
                vec![
                    draft(EventKind::RowClaimed, Subject::Row(row.clone()), json!({ "seat": seat })),
                    draft(EventKind::SeatWorking, Subject::Seat(seat.clone()), json!({ "row": row })),
                ]
            }
            Command::RowReview { row, commit, fence } => {
                let r = Self::row(state, row)?;
                Self::row_in(r, &[Doing], cmd)?;
                Self::fence_is(fence, claim_lease(row))?;
                let mut out =
                    vec![draft(EventKind::RowReviewed, Subject::Row(row.clone()), json!({ "commit": commit }))];
                if let Some(seat) = &r.seat {
                    out.push(draft(EventKind::SeatIdle, Subject::Seat(seat.clone()), Value::Null));
                }
                out
            }
            Command::RowBlock { row, block } => {
                let r = Self::row(state, row)?;
                match block {
                    Block::On { reason } => {
                        Self::row_in(r, &[Todo, Doing, Review, SentBack], cmd)?;
                        vec![draft(
                            EventKind::RowBlocked,
                            Subject::Row(row.clone()),
                            json!({ "reason": reason, "before": r.state }),
                        )]
                    }
                    Block::Off => {
                        Self::row_in(r, &[Blocked], cmd)?;
                        vec![draft(EventKind::RowUnblocked, Subject::Row(row.clone()), Value::Null)]
                    }
                }
            }
            Command::RowPark { row } => {
                let r = Self::row(state, row)?;
                Self::row_in(r, &[Todo, SentBack, Blocked], cmd)?;
                vec![draft(EventKind::RowParked, Subject::Row(row.clone()), Value::Null)]
            }
            Command::RowLand { row, verdict, fence } => {
                let r = Self::row(state, row)?;
                Self::row_in(r, &[Review], cmd)?;
                Self::fence_is(fence, verify_lease(row))?;
                match verdict {
                    Verdict::Holds { commit } => {
                        vec![draft(EventKind::RowLanded, Subject::Row(row.clone()), json!({ "commit": commit }))]
                    }
                    Verdict::Blocks { reason } => {
                        vec![draft(EventKind::RowSentBack, Subject::Row(row.clone()), json!({ "reason": reason }))]
                    }
                }
            }
            Command::SeatSpawn { row, attempt, machine, kind, model } => {
                let again = row.is_some()
                    && state.seats.values().any(|s| {
                        s.row == *row && s.attempt == *attempt && s.state != SeatState::Gone
                    });
                if again {
                    return Ok(vec![]);
                }
                let id = SeatId(format!("s-{}", state.seats.len() + 1));
                let seat = Seat {
                    id: id.clone(),
                    machine: machine.clone().unwrap_or_else(|| MachineName("local".into())),
                    kind: kind.clone(),
                    model: model.clone(),
                    state: SeatState::Requested,
                    before_lost: None,
                    row: row.clone(),
                    attempt: *attempt,
                    pid: None,
                    fields: Default::default(),
                };
                vec![draft(EventKind::SeatRequested, Subject::Seat(id), to_value!(&seat))]
            }
            Command::SeatEnd { seat, .. } => {
                let s = Self::seat(state, seat)?;
                if !(LIVE.contains(&s.state) || s.state == SeatState::Lost) {
                    return Err(illegal!(Subject::Seat(seat.clone()), &s.state, cmd));
                }
                vec![draft(EventKind::SeatEnding, Subject::Seat(seat.clone()), Value::Null)]
            }
            Command::SeatDespawn { seat, lost } => {
                let s = Self::seat(state, seat)?;
                let ok = s.state == SeatState::Ending || (s.state == SeatState::Lost && *lost);
                if !ok {
                    return Err(illegal!(Subject::Seat(seat.clone()), &s.state, cmd));
                }
                vec![draft(EventKind::SeatGone, Subject::Seat(seat.clone()), Value::Null)]
            }
            Command::OrderSend { to, text, first } => {
                let s = Self::seat(state, to)?;
                if matches!(s.state, SeatState::Ending | SeatState::Gone) {
                    return Err(illegal!(Subject::Seat(to.clone()), &s.state, cmd));
                }
                let id = OrderId(format!("o-{}", state.orders.len() + 1));
                let order = Order {
                    id: id.clone(),
                    from: by.clone(),
                    to: to.clone(),
                    text: text.clone(),
                    state: OrderState::Queued,
                    attempts: 0,
                    first: *first,
                    why: None,
                };
                vec![draft(EventKind::OrderQueued, Subject::Order(id), to_value!(&order))]
            }
            Command::OrderCancel { order } => {
                let o = state
                    .orders
                    .get(order)
                    .ok_or_else(|| Refusal::NotFound { subject: Subject::Order(order.clone()) })?;
                if !matches!(o.state, OrderState::Queued | OrderState::Typed) {
                    return Err(illegal!(Subject::Order(order.clone()), &o.state, cmd));
                }
                vec![draft(EventKind::OrderCancelled, Subject::Order(order.clone()), Value::Null)]
            }
            Command::Mirror { put, gone } => {
                let subject = put
                    .first()
                    .map(Self::subject_of)
                    .or_else(|| gone.first().cloned())
                    .unwrap_or_else(|| Subject::Project(ProjectId(String::new())));
                return Err(Refusal::Illegal { subject, state: "owned".into(), verb: verb(cmd) });
            }
            Command::OrderDeliver { order, .. }
            | Command::OrderDelivered { order }
            | Command::OrderAcked { order, .. }
            | Command::OrderRefused { order, .. }
            | Command::OrderDone { order } => {
                let state = state.orders.get(order).map(|o| wire!(&o.state)).unwrap_or_default();
                return Err(Refusal::Illegal { subject: Subject::Order(order.clone()), state, verb: verb(cmd) });
            }
            Command::HubUserAdd { name, .. } | Command::HubUserRevoke { name } => {
                return Err(Refusal::Illegal {
                    subject: Subject::User(name.clone()),
                    state: String::new(),
                    verb: verb(cmd),
                });
            }
            Command::HubTokenIssue { id, .. } | Command::HubTokenRevoke { id } => {
                return Err(Refusal::Illegal {
                    subject: Subject::Credential(id.clone()),
                    state: String::new(),
                    verb: verb(cmd),
                });
            }
            Command::ListGo { .. }
            | Command::ListHold { .. }
            | Command::ListRelease { .. }
            | Command::AskRaise { .. }
            | Command::AskAnswer { .. }
            | Command::AskAck { .. }
            | Command::MachineSet { .. } => {
                return Err(invalid(format!("{} is not in the test domain", verb(cmd))));
            }
        })
    }

    fn evolve(&self, mut state: State, event: &Event) -> State {
        let p = &event.payload;
        // The row or seat the event names, changed in place; a subject the state lacks changes nothing.
        let with_row = |state: &mut State, f: &mut dyn FnMut(&mut Row)| {
            if let Subject::Row(id) = &event.subject {
                if let Some(r) = state.rows.get_mut(id) {
                    f(r);
                }
            }
        };
        let with_seat = |state: &mut State, f: &mut dyn FnMut(&mut Seat)| {
            if let Subject::Seat(id) = &event.subject {
                if let Some(s) = state.seats.get_mut(id) {
                    f(s);
                }
            }
        };
        match event.kind {
            EventKind::RowAdded => {
                if let Some(r) = from!(Row, p) {
                    state.rows.insert(r.id.clone(), r);
                }
            }
            EventKind::RowEdited => with_row(&mut state, &mut |r| {
                if let Some(t) = p.get("title").and_then(Value::as_str) {
                    r.title = t.to_string();
                }
                if let Some(fields) = p.get("fields").and_then(Value::as_object) {
                    for (k, v) in fields {
                        match v.as_str() {
                            Some(v) => r.fields.insert(k.clone(), v.to_string()),
                            None => r.fields.remove(k),
                        };
                    }
                }
            }),
            EventKind::RowClaimed => with_row(&mut state, &mut |r| {
                r.state = RowState::Doing;
                r.seat = p.get("seat").and_then(|v| from!(SeatId, v));
            }),
            EventKind::RowReviewed => with_row(&mut state, &mut |r| {
                r.state = RowState::Review;
                r.commit = p.get("commit").and_then(Value::as_str).map(str::to_string);
            }),
            EventKind::RowBlocked => with_row(&mut state, &mut |r| {
                r.before_block = p.get("before").and_then(|v| from!(RowState, v));
                r.state = RowState::Blocked;
            }),
            EventKind::RowUnblocked => with_row(&mut state, &mut |r| {
                r.state = r.before_block.take().unwrap_or(RowState::Todo);
            }),
            EventKind::RowParked => with_row(&mut state, &mut |r| r.state = RowState::Parked),
            EventKind::RowLanded => with_row(&mut state, &mut |r| {
                r.state = RowState::Done;
                r.seat = None;
            }),
            EventKind::RowSentBack => with_row(&mut state, &mut |r| {
                r.state = RowState::SentBack;
                r.seat = None;
            }),
            EventKind::SeatRequested => {
                if let Some(s) = from!(Seat, p) {
                    state.seats.insert(s.id.clone(), s);
                }
            }
            EventKind::SeatWorking => with_seat(&mut state, &mut |s| s.state = SeatState::Working),
            EventKind::SeatIdle => with_seat(&mut state, &mut |s| s.state = SeatState::Idle),
            EventKind::SeatEnding => with_seat(&mut state, &mut |s| s.state = SeatState::Ending),
            EventKind::SeatGone => with_seat(&mut state, &mut |s| s.state = SeatState::Gone),
            EventKind::OrderQueued => {
                if let Some(o) = from!(Order, p) {
                    state.orders.insert(o.id.clone(), o);
                }
            }
            EventKind::OrderCancelled => {
                if let Subject::Order(id) = &event.subject {
                    if let Some(o) = state.orders.get_mut(id) {
                        o.state = OrderState::Cancelled;
                    }
                }
            }
            EventKind::LeaseTaken => {
                if let Some(l) = from!(Lease, p) {
                    state.leases.insert(l.name.clone(), l);
                }
            }
            EventKind::LeaseReleased => {
                if let Subject::Lease(name) = &event.subject {
                    state.leases.remove(name);
                }
            }
            EventKind::ClaimTaken => {
                if let Some(c) = from!(Claim, p) {
                    state.claims.insert(c.row.clone(), c);
                }
            }
            EventKind::ClaimReleased => {
                if let Subject::Claim(row) = &event.subject {
                    state.claims.remove(row);
                }
            }
            _ => {}
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real domain refuses step 1's `mirror` as `illegal`, naming the first
    /// subject it would put or remove (wsp-070).
    #[test]
    fn mirror_is_refused_as_illegal() {
        let ed = Principal::Person("ed".into());
        let seat = SeatId("s-1".into());
        let gone = Command::Mirror { put: vec![], gone: vec![Subject::Seat(seat.clone())] };
        assert_eq!(
            TestDomain.decide(&State::default(), &gone, &ed),
            Err(Refusal::Illegal { subject: Subject::Seat(seat), state: "owned".into(), verb: "mirror".into() })
        );
        let none = Command::Mirror { put: vec![], gone: vec![] };
        assert!(matches!(TestDomain.decide(&State::default(), &none, &ed), Err(Refusal::Illegal { .. })));
    }
    /// The orders machine's verbs past `order.send` (wsp-117) are refused
    /// `illegal`, naming the order, with its state (`""` when there is none).
    #[test]
    fn the_orders_machine_verbs_are_refused_as_illegal() {
        let ed = Principal::Person("ed".into());
        let o = OrderId("o-1".into());
        let fence = Fence { lease: LeaseName::deliver(&SeatId("s-1".into())), token: Token(1) };
        let verbs = [
            Command::OrderDeliver { order: o.clone(), fence },
            Command::OrderDelivered { order: o.clone() },
            Command::OrderAcked { order: o.clone(), turn_began: At(1) },
            Command::OrderRefused { order: o.clone(), why: "busy".into(), counts: false },
            Command::OrderDone { order: o.clone() },
        ];
        let mut queued = State::default();
        queued.orders.insert(
            o.clone(),
            Order {
                id: o.clone(),
                from: ed.clone(),
                to: SeatId("s-1".into()),
                text: "go".into(),
                state: OrderState::Queued,
                attempts: 0,
                first: false,
                why: None,
            },
        );
        for cmd in &verbs {
            for (state, word) in [(&State::default(), ""), (&queued, "queued")] {
                assert_eq!(
                    TestDomain.decide(state, cmd, &ed),
                    Err(Refusal::Illegal { subject: Subject::Order(o.clone()), state: word.into(), verb: verb(cmd) })
                );
            }
        }
    }

    /// The hub's four verbs (wsp-158) are refused `illegal`, naming the user or
    /// the token record: only the hub's domain decides them.
    #[test]
    fn the_hub_verbs_are_refused_as_illegal() {
        let ed = Principal::Person("ed".into());
        let user = UserName("ann".into());
        let token = TokenId("t-1".into());
        let users = [
            Command::HubUserAdd { name: user.clone(), admin: true },
            Command::HubUserRevoke { name: user.clone() },
        ];
        let tokens = [
            Command::HubTokenIssue { id: token.clone(), user: user.clone(), hash: "ab".repeat(32) },
            Command::HubTokenRevoke { id: token.clone() },
        ];
        for cmd in &users {
            assert_eq!(
                TestDomain.decide(&State::default(), cmd, &ed),
                Err(Refusal::Illegal { subject: Subject::User(user.clone()), state: "".into(), verb: verb(cmd) })
            );
        }
        for cmd in &tokens {
            assert_eq!(
                TestDomain.decide(&State::default(), cmd, &ed),
                Err(Refusal::Illegal {
                    subject: Subject::Credential(token.clone()),
                    state: "".into(),
                    verb: verb(cmd)
                })
            );
        }
    }
}
