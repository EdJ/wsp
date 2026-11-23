//! The count of a seat's refused deliveries (`wsp-634`), kept apart from
//! `wake` so the reconciler (`repair`) and `wsp wip` can read it without
//! reaching up into the seats layer — and, under `orders::mode() == store`,
//! the door the daemon's own tells go through instead ([`Door`], wsp-125),
//! where a deaf seat is one whose orders fail at the cap and this file is not
//! written.

use crate::orders::{self, Error};
use crate::store::Store;
use crate::util;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use wsp_store_contract::{Order, OrderId, OrderState, Principal, SeatId};

/// Where the scopes whose seat's supervisor keeps answering `failed` are
/// counted: scope → `{ n, since, why }` (wsp-634). A seat that refuses every
/// tick is a governor that is deaf, and nothing else says so.
const REFUSED_FILE: &str = "refused-deliveries.json";

/// How many consecutive ticks a seat may refuse a tell before the daemon ends it
/// so the reconciler seats a successor (wsp-634). Five: a refusal is the
/// supervisor's answer after its own four tries, so five of those is minutes.
pub(crate) const REFUSED_TICKS: u64 = 5;

fn refused_all(store: &Store) -> BTreeMap<String, Value> {
    std::fs::read_to_string(store.state.join(REFUSED_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn refused_write(store: &Store, all: &BTreeMap<String, Value>) {
    let _ = std::fs::create_dir_all(&store.state);
    let _ = std::fs::write(store.state.join(REFUSED_FILE), json!(all).to_string());
}

/// A refusal on this scope's seat, counted. Returns how many in a row, and the
/// reason the one before it gave (so a retry can say what it is retrying after).
pub(crate) fn refuse(store: &Store, scope: &str, why: &str) -> (u64, Option<String>) {
    let mut all = refused_all(store);
    let was = all.get(scope).cloned().unwrap_or(Value::Null);
    let n = was.get("n").and_then(Value::as_u64).unwrap_or(0) + 1;
    let since = was.get("since").and_then(Value::as_i64).unwrap_or_else(util::epoch_secs);
    let before = was.get("why").and_then(Value::as_str).map(str::to_string);
    all.insert(scope.to_string(), json!({ "n": n, "since": since, "why": why }));
    refused_write(store, &all);
    (n, before)
}

/// The seat took a tell, or nothing is owed it: the count ends.
pub(crate) fn accepted(store: &Store, scope: &str) {
    let mut all = refused_all(store);
    if all.remove(scope).is_some() {
        refused_write(store, &all);
    }
}

/// The scopes whose seat has refused delivery: scope → (ticks in a row, the last
/// reason). Read by `wsp wip` and by the reconciler.
pub(crate) fn refusing(store: &Store) -> BTreeMap<String, (u64, String)> {
    refused_all(store)
        .into_iter()
        .map(|(scope, v)| {
            let n = v.get("n").and_then(Value::as_u64).unwrap_or(0);
            (scope, (n, v.get("why").and_then(Value::as_str).unwrap_or_default().to_string()))
        })
        .filter(|(_, (n, _))| *n > 0)
        .collect()
}

// ---- the daemon's own tells, under the orders machine (wsp-125) -------------

/// Who the daemon's own tells are from in the orders machine.
fn by() -> Principal {
    Principal::Wsp("daemon".into())
}

/// The door the daemon's own tells go through once `orders::mode()` is
/// `store` (wsp-125): a governor's spool (`wake`), a barrier check told its
/// group moved (`cycle`), a stall (`repair::stuck_tick`). Each is
/// [`orders::send_keyed`] under a key that makes the next tick's repeat the
/// same order; nothing here waits, and the next tick reads the order's state.
/// `None` under `files`, where each caller keeps today's path.
pub(crate) struct Door<'a> {
    store: &'a Store,
    /// The store a test named; `None` is the daemon's own, through the door.
    db: Option<wsp_server::SharedStore>,
}

/// What a keyed send gave back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sent {
    pub id: OrderId,
    /// The key was new: this tick queued the order. `false` is a repeat, the
    /// same order an earlier tick sent.
    pub fresh: bool,
}

#[cfg(test)]
thread_local! {
    /// A test's store: [`door`] answers through it as though under `store`.
    pub(crate) static TEST_DOOR: std::cell::RefCell<Option<wsp_server::SharedStore>> =
        const { std::cell::RefCell::new(None) };
}

/// The door, when the orders machine owns orders; `None` under `files`.
pub(crate) fn door(store: &Store) -> Option<Door<'_>> {
    #[cfg(test)]
    if let Some(db) = TEST_DOOR.with(|d| d.borrow().clone()) {
        return Some(Door { store, db: Some(db) });
    }
    (orders::mode() == orders::Mode::Store).then_some(Door { store, db: None })
}

impl Door<'_> {
    /// `text` to `seat` under `key`: a key an earlier tick used is that tick's
    /// order, said back with `fresh: false`.
    pub(crate) fn send(&self, seat: &str, text: &str, key: &str) -> Result<Sent, Error> {
        let to = SeatId(seat.to_string());
        let known: Vec<OrderId> = self.orders_of(seat, &[])?.into_iter().map(|o| o.id).collect();
        let id = match &self.db {
            Some(db) => orders::send_keyed_in(db, &to, text, &by(), false, key)?,
            None => orders::send_keyed(self.store, &to, text, &by(), false, key)?,
        };
        let fresh = !known.contains(&id);
        Ok(Sent { id, fresh })
    }

    /// Order `id` as it stands, without waiting.
    pub(crate) fn order(&self, id: &OrderId) -> Result<Option<Order>, Error> {
        match &self.db {
            Some(db) => orders::order_in(db, id),
            None => orders::order(self.store, id),
        }
    }

    /// `seat`'s orders in `states` (every state when empty).
    pub(crate) fn orders_of(&self, seat: &str, states: &[OrderState]) -> Result<Vec<Order>, Error> {
        let to = SeatId(seat.to_string());
        match &self.db {
            Some(db) => orders::orders_of_in(db, &to, states),
            None => orders::orders_of(self.store, &to, states),
        }
    }

    /// **Deaf** (wsp-634 under `store`, part 3 of the person's answer on
    /// wsp-115): the seat's last order to settle reached `failed` — the
    /// delivery worker's attempts ran out at the machine's cap — with none
    /// acknowledged after it. The failed order's last refusal is the reason.
    pub(crate) fn deaf(&self, seat: &str) -> Option<String> {
        let settled = [OrderState::Acknowledged, OrderState::Done, OrderState::Failed];
        let last = self.orders_of(seat, &settled).ok()?.into_iter().max_by_key(|o| order_number(&o.id))?;
        (last.state == OrderState::Failed)
            .then(|| last.why.clone().unwrap_or_else(|| format!("order {} failed", last.id.0)))
    }
}

/// Where an order falls in its seat's sequence: `cpd-1-o12` is 12.
fn order_number(id: &OrderId) -> u64 {
    id.0.rsplit_once("-o").and_then(|(_, n)| n.parse().ok()).unwrap_or(0)
}

/// Whether this process has already arranged the end of `seat` for deafness:
/// a failed order stays failed, so the tick after the one that ended a seat
/// reads it deaf again until it is gone. Returns `true` the first time only.
/// Per store, so two isolated stores in one process do not share it.
pub(crate) fn first_ending(store: &Store, seat: &str) -> bool {
    static ENDED: std::sync::Mutex<std::collections::BTreeSet<String>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    let key = format!("{}:{seat}", store.state.display());
    ENDED.lock().map(|mut e| e.insert(key)).unwrap_or(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;
    use wsp_store_contract::{At, Command, Fence, IdempotencyKey, LeaseName, Leases, SeatState};

    /// A store under the orders machine for this thread's [`door`], `seats`
    /// mirrored into it idle; the door closes when the guard drops.
    pub(crate) struct Opened(pub wsp_server::SharedStore);

    impl Drop for Opened {
        fn drop(&mut self) {
            TEST_DOOR.with(|d| *d.borrow_mut() = None);
        }
    }

    pub(crate) fn open(store: &Store, seats: &[&str]) -> Opened {
        let db = crate::orders::tests::db(&store.state.join("orders-db"), wsp_server::Mode::Owned);
        for s in seats {
            crate::orders::tests::seat(&db, s, SeatState::Idle);
        }
        TEST_DOOR.with(|d| *d.borrow_mut() = Some(db.clone()));
        Opened(db)
    }

    /// One attempt as the delivery worker makes it, under `seat`'s lease.
    fn attempt(db: &wsp_server::SharedStore, id: &OrderId, then: &dyn Fn(u64) -> Vec<Command>, n: u64) {
        let by = Principal::Wsp("wsp".into());
        let seat = db.snapshot().unwrap().0.orders[id].to.clone();
        let lease = LeaseName::deliver(&seat);
        let token = Leases::take(&**db, &lease, &by, Duration::from_secs(60)).unwrap().expect("free");
        let key = |k: &str| IdempotencyKey(format!("test:{k}:{}:{n}", id.0));
        db.apply(Command::OrderDeliver { order: id.clone(), fence: Fence { lease: lease.clone(), token } }, key("deliver"), by.clone())
            .unwrap();
        for (i, c) in then(n).into_iter().enumerate() {
            db.apply(c, key(&format!("then{i}")), by.clone()).unwrap();
        }
        Leases::release(&**db, &lease, token).unwrap();
    }

    /// Order `id` delivered and acknowledged by its seat.
    pub(crate) fn ack(db: &wsp_server::SharedStore, id: &OrderId) {
        let then = |_| {
            vec![
                Command::OrderDelivered { order: id.clone() },
                Command::OrderAcked { order: id.clone(), turn_began: At(1) },
            ]
        };
        attempt(db, id, &then, 0);
    }

    /// Order `id` refused by its seat until the machine's attempts run out.
    pub(crate) fn fail(db: &wsp_server::SharedStore, id: &OrderId, why: &str) {
        for n in 0.. {
            if crate::orders::order_in(db, id).unwrap().unwrap().state == OrderState::Failed {
                return;
            }
            assert!(n < 50, "{id:?} never failed");
            let then = |_| vec![Command::OrderRefused { order: id.clone(), why: why.to_string(), counts: true }];
            attempt(db, id, &then, n);
        }
    }

    #[test]
    fn a_keyed_send_says_whether_it_queued_and_a_seat_is_deaf_once_its_last_order_failed() {
        let env = util::isolated("refusals-door");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let opened = open(&store, &["cpd-1"]);
        let door = door(&store).expect("the test's door");
        let a = door.send("cpd-1", "go", "k1").unwrap();
        assert!(a.fresh);
        let again = door.send("cpd-1", "go", "k1").unwrap();
        assert_eq!(again, Sent { id: a.id.clone(), fresh: false }, "a repeat is the same order");
        assert_eq!(door.deaf("cpd-1"), None, "a queued order is nothing yet");
        fail(&opened.0, &a.id, "refused");
        assert_eq!(door.deaf("cpd-1").as_deref(), Some("refused"));
        let b = door.send("cpd-1", "go", "k2").unwrap();
        ack(&opened.0, &b.id);
        assert_eq!(door.deaf("cpd-1"), None, "an acknowledged order after the failed one hears");
        assert!(first_ending(&store, "cpd-1") && !first_ending(&store, "cpd-1"));
        assert!(!store.state.join(REFUSED_FILE).exists());
    }
}
