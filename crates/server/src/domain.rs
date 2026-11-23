//! The composed domain (docs/state-core.md section 7's step 2, wsp-121): the
//! one [`Domain`] a store runs once the orders machine is in, routing each
//! command to the part that decides it. Pure, as its parts are.
//!
//! **Routing.** A command [`Orders::owns`] (the seven `order.*` verbs) goes to
//! the orders machine or the mirror by [`Mode`]; `mirror` goes to [`Mirror`];
//! every other verb is [`Mirror`]'s, which refuses it `illegal`, state
//! `mirrored` (steps 3 and 4 bring their machines).
//!
//! - **[`Mode::Mirrored`]** (step 1's behaviour, and the live daemon's until
//!   group 4's flag): the mirror puts orders, and the machine's verbs are
//!   refused `illegal` with state [`MIRRORED`], as [`Mirror`] refuses them.
//! - **[`Mode::Owned`]**: [`Orders`] decides its verbs, and a `mirror` that
//!   puts or removes an order is refused `illegal` with state [`OWNED`],
//!   naming the first order it puts (else the first it removes), and writes
//!   nothing of the rest of it. A `mirror` that names no order is the
//!   mirror's as before: the seats the orders machine reads are still
//!   mirrored.
//!
//! **`evolve`** folds every event through both parts, [`Mirror`] first: the
//! mirror's kinds and the store's own, then the `order.*` kinds. The two
//! overlap only on `lease.taken` and `lease.released`, which both fold the
//! same way, so the fold does not depend on the mode and a log written under
//! one mode folds the same under the other.

use wsp_machines::Orders;
use wsp_store_contract::{Command, Domain, Draft, Entity, Event, Principal, Refusal, State, Subject};

#[cfg(doc)]
use crate::mirror::MIRRORED;
use crate::mirror::Mirror;

/// The state a `mirror` naming an order is refused with under [`Mode::Owned`]:
/// the orders machine owns orders, and nothing mirrors them.
pub const OWNED: &str = "owned";

/// Who decides orders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The mirror puts orders; the orders machine's verbs are refused
    /// `illegal`, state [`MIRRORED`].
    #[default]
    Mirrored,
    /// The orders machine decides them; a `mirror` naming an order is refused
    /// `illegal`, state [`OWNED`].
    Owned,
}

/// The mirror and the orders machine, as one domain, under a [`Mode`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Composed {
    pub mode: Mode,
}

impl Composed {
    pub fn new(mode: Mode) -> Composed {
        Composed { mode }
    }
}

/// The first order a `mirror` puts, else the first it removes.
fn order_named(put: &[Entity], gone: &[Subject]) -> Option<Subject> {
    put.iter()
        .find_map(|e| match e {
            Entity::Order(o) => Some(Subject::Order(o.id.clone())),
            _ => None,
        })
        .or_else(|| gone.iter().find(|s| matches!(s, Subject::Order(_))).cloned())
}

impl Domain for Composed {
    fn decide(&self, state: &State, cmd: &Command, by: &Principal) -> Result<Vec<Draft>, Refusal> {
        match (self.mode, cmd) {
            (Mode::Owned, c) if Orders::owns(c) => Orders.decide(state, cmd, by),
            (Mode::Owned, Command::Mirror { put, gone }) => match order_named(put, gone) {
                Some(subject) => Err(Refusal::Illegal { subject, state: OWNED.into(), verb: "mirror".into() }),
                None => Mirror.decide(state, cmd, by),
            },
            _ => Mirror.decide(state, cmd, by),
        }
    }

    fn evolve(&self, state: State, event: &Event) -> State {
        Orders.evolve(Mirror.evolve(state, event), event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::MIRRORED;
    use wsp_store_contract::{
        At, EventId, EventKind, MachineName, Order, OrderId, OrderState, SeatId, SeatState, Seat,
    };

    fn ed() -> Principal {
        Principal::Person("ed".into())
    }

    fn seat(id: &str) -> Seat {
        Seat {
            id: SeatId(id.into()),
            machine: MachineName("mac".into()),
            kind: "claude".into(),
            model: None,
            state: SeatState::Idle,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: None,
            fields: Default::default(),
        }
    }

    fn order(id: &str) -> Order {
        Order {
            workspace: Default::default(),
            id: OrderId(id.into()),
            from: ed(),
            to: SeatId("cpd-1".into()),
            text: "go".into(),
            state: OrderState::Queued,
            attempts: 0,
            first: false,
            why: None,
        }
    }

    fn fold(d: &Composed, state: State, drafts: Vec<Draft>) -> State {
        drafts.into_iter().enumerate().fold(state, |s, (i, dr)| {
            let e = Event {
                workspace: Default::default(),
                id: EventId(i as u64 + 1),
                at: At(0),
                principal: ed(),
                kind: dr.kind,
                subject: dr.subject,
                payload: dr.payload,
            };
            d.evolve(s, &e)
        })
    }

    fn send() -> Command {
        Command::OrderSend { to: SeatId("cpd-1".into()), text: "go".into(), first: false }
    }

    #[test]
    fn mirrored_refuses_the_machines_verbs_and_mirrors_orders() {
        let d = Composed::new(Mode::Mirrored);
        let state = fold(&d, State::default(), Mirror.decide(&State::default(), &Command::Mirror {
            put: vec![Entity::Seat(seat("cpd-1"))],
            gone: vec![],
        }, &ed()).unwrap());
        assert_eq!(
            d.decide(&state, &send(), &ed()),
            Err(Refusal::Illegal {
                subject: Subject::Seat(SeatId("cpd-1".into())),
                state: MIRRORED.into(),
                verb: "order.send".into()
            })
        );
        let put = Command::Mirror { put: vec![Entity::Order(order("o-1"))], gone: vec![] };
        let drafts = d.decide(&state, &put, &ed()).unwrap();
        assert_eq!(drafts.iter().map(|x| x.kind).collect::<Vec<_>>(), [EventKind::MirrorPut]);
        assert!(fold(&d, state, drafts).orders.contains_key(&OrderId("o-1".into())));
    }

    #[test]
    fn owned_decides_the_machines_verbs_and_refuses_a_mirror_naming_an_order() {
        let d = Composed::new(Mode::Owned);
        let seats = Command::Mirror { put: vec![Entity::Seat(seat("cpd-1"))], gone: vec![] };
        let state = fold(&d, State::default(), d.decide(&State::default(), &seats, &ed()).unwrap());
        assert!(state.seats.contains_key(&SeatId("cpd-1".into())), "a mirror naming no order is the mirror's");

        let drafts = d.decide(&state, &send(), &ed()).unwrap();
        assert_eq!(drafts.iter().map(|x| x.kind).collect::<Vec<_>>(), [EventKind::OrderQueued]);
        let state = fold(&d, state, drafts);
        let id = OrderId("cpd-1-o1".into());
        assert_eq!(state.orders[&id].state, OrderState::Queued, "evolve folds the machine's events");

        for (put, gone) in [
            (vec![Entity::Seat(seat("cpd-2")), Entity::Order(order("o-9"))], vec![]),
            (vec![], vec![Subject::Order(OrderId("o-9".into()))]),
        ] {
            assert_eq!(
                d.decide(&state, &Command::Mirror { put, gone }, &ed()),
                Err(Refusal::Illegal {
                    subject: Subject::Order(OrderId("o-9".into())),
                    state: OWNED.into(),
                    verb: "mirror".into()
                })
            );
        }
        let cancel = Command::OrderCancel { order: id.clone() };
        let state = fold(&d, state.clone(), d.decide(&state, &cancel, &ed()).unwrap());
        assert_eq!(state.orders[&id].state, OrderState::Cancelled);
    }
}
