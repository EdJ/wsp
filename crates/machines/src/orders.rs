//! The orders machine (docs/state-core.md 3.1; wsp-118): an order's life from
//! `order.send` to `done`, `cancelled` or `failed`, as a pure [`Domain`].
//! [`Orders::owns`] says which commands are its, so a server routes them here.
//!
//! | verb | from | to | refused otherwise with |
//! |---|---|---|---|
//! | `order.send` | (a seat the state holds, not `gone`) | `queued` | `not_found` (seat), `illegal` (seat, `gone`) |
//! | `order.deliver` | `queued`; the fence is the current `deliver:<seat>`, held by the issuer; the seat `idle` or `working` (or `started`, for a `first` order) with no other order `typed` | `typed` | `illegal` (order), `invalid`, `not_found` (lease), `held`, `stale`, `not_found` (seat), `illegal` (seat, its state) |
//! | `order.delivered` | `typed` | `delivered` | `illegal` (order) |
//! | `order.acked` | `typed`, `delivered` | `acknowledged` | `illegal` (order) |
//! | `order.refused` | `typed` | `queued`, `attempts + 1`; `failed` at [`ATTEMPTS`]; with `counts: false`, `queued`, attempts unchanged | `illegal` (order) |
//! | `order.done` | `acknowledged` | `done` | `illegal` (order) |
//! | `order.cancel` | any but `done` and `cancelled` | `cancelled` | `illegal` (order) |
//!
//! Every command naming an order the state lacks is refused `not_found`
//! (order). Every refusal is a [`Refusal`] the contract has (wsp-117's codes,
//! as group 1's barrier check mapped 3.1's words onto them): "no seat" is
//! `not_found { seat }` or `illegal { seat, "gone", verb }`, and "busy" and
//! "not ready" are `illegal { seat, <the seat's state>, "order.deliver" }`.
//! A refusal drafts nothing.
//!
//! **A repeat answer decides nothing.** The delivery worker and the manager
//! fold may both report the same answer, so an answer the order has already
//! passed is an empty decision, not a refusal: `order.delivered` on a
//! `delivered`, `acknowledged` or `done` order; `order.acked` on an
//! `acknowledged` or `done` one; `order.done` on a `done` one; and
//! `order.refused` on a `queued` or `failed` order whose last refusal (`why`)
//! has the same words, which is the state that refusal left it in.
//!
//! **Choices beyond 3.1.**
//! - `order.cancel` on a `cancelled` order is refused `illegal`: it is no
//!   transition, and the conformance suite's case 9 refuses it so too. A
//!   `failed` order may be cancelled.
//! - `order.deliver` checks the fence itself as well as `apply` does: the
//!   lease it names must be the order's seat's `deliver:<seat>` (`invalid`),
//!   held in the state (`not_found`, subject the lease) by the command's
//!   principal (`held`) at the fence's token (`stale`). The order's own state
//!   is checked first, so a delivery that lost its race reads `illegal`.
//! - An order is named `<seat>-o<n>`, `n` one past the seat's orders in the
//!   state and stepped past any id already taken, so `decide` stays pure and a
//!   mirrored order's id (`<seat>-first-…`) is never reused.
//! - `mirror` is refused `illegal`, state `owned` (the contract's MAP.md: a
//!   domain that owns its entities is mirrored by nothing); every other verb
//!   not [`Orders::owns`]'s is `invalid`.
//!
//! **The events.** `order.queued` carries the whole [`Order`]; `order.typed`,
//! `order.delivered`, `order.done` and `order.cancelled` carry `null`;
//! `order.acknowledged` carries `{"turn_began"}`; `order.refused` carries
//! `{"why", "counts", "attempts"}` (the attempts after it) and `order.failed`
//! `{"why", "attempts"}`. [`Orders::evolve`](Domain::evolve) folds them, and
//! `lease.taken` / `lease.released`, which `order.deliver`'s guard reads; it
//! leaves the state as it was for every other event.

use serde_json::{json, Value};
use std::result::Result;
use wsp_store_contract::*;

/// The counted refusals an order is delivered through before it is `failed`
/// (part 3 of the person's answer on wsp-115). A refusal with `counts: false`
/// (the seat was busy) is no attempt.
pub const ATTEMPTS: u32 = 3;

/// The orders machine. Stateless: everything it knows is in the [`State`] it is given.
#[derive(Clone, Copy, Debug, Default)]
pub struct Orders;

impl Orders {
    /// Whether `cmd` is one of this machine's: `order.send`, `order.cancel`,
    /// `order.deliver`, and the executor's answers `order.delivered`,
    /// `order.acked`, `order.refused` and `order.done`.
    pub fn owns(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::OrderSend { .. }
                | Command::OrderCancel { .. }
                | Command::OrderDeliver { .. }
                | Command::OrderDelivered { .. }
                | Command::OrderAcked { .. }
                | Command::OrderRefused { .. }
                | Command::OrderDone { .. }
        )
    }
}

/// A command's verb (`order.deliver`), as [`Refusal::Illegal`] carries it.
fn verb(cmd: &Command) -> String {
    serde_json::to_value(cmd).ok().and_then(|v| v.get("verb")?.as_str().map(str::to_string)).unwrap_or_default()
}

/// A state's wire name (`queued`), as [`Refusal::Illegal`] carries it.
macro_rules! wire {
    ($state:expr) => {
        serde_json::to_value($state).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    };
}

fn draft(kind: EventKind, order: &OrderId, payload: Value) -> Draft {
    Draft { kind, subject: Subject::Order(order.clone()), payload }
}

fn order<'s>(state: &'s State, id: &OrderId) -> Result<&'s Order, Refusal> {
    state.orders.get(id).ok_or_else(|| Refusal::NotFound { subject: Subject::Order(id.clone()) })
}

fn seat<'s>(state: &'s State, id: &SeatId) -> Result<&'s Seat, Refusal> {
    state.seats.get(id).ok_or_else(|| Refusal::NotFound { subject: Subject::Seat(id.clone()) })
}

fn illegal_order(o: &Order, cmd: &Command) -> Refusal {
    Refusal::Illegal { subject: Subject::Order(o.id.clone()), state: wire!(&o.state), verb: verb(cmd) }
}

fn illegal_seat(s: &Seat, cmd: &Command) -> Refusal {
    Refusal::Illegal { subject: Subject::Seat(s.id.clone()), state: wire!(&s.state), verb: verb(cmd) }
}

/// The next order id for `seat`: `<seat>-o<n>`, past every id the state holds.
fn mint(state: &State, seat: &SeatId) -> OrderId {
    let mut n = state.orders.values().filter(|o| o.to == *seat).count() + 1;
    loop {
        let id = OrderId(format!("{}-o{n}", seat.0));
        if !state.orders.contains_key(&id) {
            return id;
        }
        n += 1;
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

/// `order.deliver`'s guards past the order's own state: the fence, then the seat.
fn may_deliver(state: &State, o: &Order, fence: &Fence, by: &Principal, cmd: &Command) -> Result<(), Refusal> {
    let lease = LeaseName::deliver(&o.to);
    if fence.lease != lease {
        return Err(Refusal::Invalid {
            reason: format!("{} is delivered under {}, and the fence names {}", o.id.0, lease.0, fence.lease.0),
        });
    }
    let held = state.leases.get(&lease).ok_or_else(|| Refusal::NotFound { subject: Subject::Lease(lease.clone()) })?;
    if held.holder != *by {
        return Err(Refusal::Held { lease, holder: held.holder.clone() });
    }
    if held.token != fence.token {
        return Err(Refusal::Stale { lease, current: held.token });
    }
    let s = seat(state, &o.to)?;
    let ready = matches!(s.state, SeatState::Idle | SeatState::Working) || (o.first && s.state == SeatState::Started);
    let busy = state.orders.values().any(|other| other.to == o.to && other.id != o.id && other.state == OrderState::Typed);
    if !ready || busy {
        return Err(illegal_seat(s, cmd));
    }
    Ok(())
}

impl Domain for Orders {
    fn decide(&self, state: &State, cmd: &Command, by: &Principal) -> Result<Vec<Draft>, Refusal> {
        use OrderState::*;
        Ok(match cmd {
            Command::OrderSend { to, text, first } => {
                let s = seat(state, to)?;
                if s.state == SeatState::Gone {
                    return Err(illegal_seat(s, cmd));
                }
                let id = mint(state, to);
                let o = Order {
                    id: id.clone(),
                    from: by.clone(),
                    to: to.clone(),
                    text: text.clone(),
                    state: Queued,
                    attempts: 0,
                    first: *first,
                    why: None,
                };
                vec![draft(EventKind::OrderQueued, &id, serde_json::to_value(&o).unwrap_or(Value::Null))]
            }
            Command::OrderDeliver { order: id, fence } => {
                let o = order(state, id)?;
                if o.state != Queued {
                    return Err(illegal_order(o, cmd));
                }
                may_deliver(state, o, fence, by, cmd)?;
                vec![draft(EventKind::OrderTyped, id, Value::Null)]
            }
            Command::OrderDelivered { order: id } => match order(state, id)? {
                o if o.state == Typed => vec![draft(EventKind::OrderDelivered, id, Value::Null)],
                o if matches!(o.state, Delivered | Acknowledged | Done) => vec![],
                o => return Err(illegal_order(o, cmd)),
            },
            Command::OrderAcked { order: id, turn_began } => match order(state, id)? {
                o if matches!(o.state, Typed | Delivered) => {
                    vec![draft(EventKind::OrderAcknowledged, id, json!({ "turn_began": turn_began }))]
                }
                o if matches!(o.state, Acknowledged | Done) => vec![],
                o => return Err(illegal_order(o, cmd)),
            },
            Command::OrderRefused { order: id, why, counts } => match order(state, id)? {
                o if o.state == Typed => {
                    let attempts = if *counts { o.attempts + 1 } else { o.attempts };
                    if attempts >= ATTEMPTS {
                        vec![draft(EventKind::OrderFailed, id, json!({ "why": why, "attempts": attempts }))]
                    } else {
                        vec![draft(
                            EventKind::OrderRefused,
                            id,
                            json!({ "why": why, "counts": counts, "attempts": attempts }),
                        )]
                    }
                }
                o if matches!(o.state, Queued | Failed) && o.why.as_deref() == Some(why.as_str()) => vec![],
                o => return Err(illegal_order(o, cmd)),
            },
            Command::OrderDone { order: id } => match order(state, id)? {
                o if o.state == Acknowledged => vec![draft(EventKind::OrderDone, id, Value::Null)],
                o if o.state == Done => vec![],
                o => return Err(illegal_order(o, cmd)),
            },
            Command::OrderCancel { order: id } => match order(state, id)? {
                o if matches!(o.state, Done | Cancelled) => return Err(illegal_order(o, cmd)),
                _ => vec![draft(EventKind::OrderCancelled, id, Value::Null)],
            },
            Command::Mirror { put, gone } => {
                let subject = put
                    .first()
                    .map(subject_of)
                    .or_else(|| gone.first().cloned())
                    .unwrap_or_else(|| Subject::Project(ProjectId(String::new())));
                return Err(Refusal::Illegal { subject, state: "owned".into(), verb: verb(cmd) });
            }
            _ => return Err(Refusal::Invalid { reason: format!("{} is not the orders machine's", verb(cmd)) }),
        })
    }

    fn evolve(&self, mut state: State, event: &Event) -> State {
        let p = &event.payload;
        let attempts = p.get("attempts").and_then(Value::as_u64).map(|n| n as u32);
        let why = p.get("why").and_then(Value::as_str).map(str::to_string);
        match (&event.kind, &event.subject) {
            (EventKind::OrderQueued, _) => {
                if let Ok(o) = serde_json::from_value::<Order>(p.clone()) {
                    state.orders.insert(o.id.clone(), o);
                }
            }
            (kind, Subject::Order(id)) => {
                if let Some(o) = state.orders.get_mut(id) {
                    match kind {
                        EventKind::OrderTyped => o.state = OrderState::Typed,
                        EventKind::OrderDelivered => o.state = OrderState::Delivered,
                        EventKind::OrderAcknowledged => o.state = OrderState::Acknowledged,
                        EventKind::OrderDone => o.state = OrderState::Done,
                        EventKind::OrderCancelled => o.state = OrderState::Cancelled,
                        EventKind::OrderRefused | EventKind::OrderFailed => {
                            o.state =
                                if *kind == EventKind::OrderFailed { OrderState::Failed } else { OrderState::Queued };
                            o.attempts = attempts.unwrap_or(o.attempts);
                            o.why = why.or(o.why.take());
                        }
                        _ => {}
                    }
                }
            }
            (EventKind::LeaseTaken, _) => {
                if let Ok(l) = serde_json::from_value::<Lease>(p.clone()) {
                    state.leases.insert(l.name.clone(), l);
                }
            }
            (EventKind::LeaseReleased, Subject::Lease(name)) => {
                state.leases.remove(name);
            }
            _ => {}
        }
        state
    }
}
