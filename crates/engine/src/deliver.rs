//! The delivery worker (docs/state-core.md section 7's step 2, wsp-122): a
//! daemon thread, under `WSP_ORDERS=store` alone, that types each queued order
//! into its seat once, through the orders machine.
//!
//! **Woken, not paced.** The tick does not drive it and waits on none of it.
//! It sleeps in `wait_for` on the store's head and makes a pass each time the
//! head moves (an `order.queued`, a seat the fold mirrored `idle`). The wait's
//! bound is only for what no event announces: a stop, a lease run out, and a
//! typed order whose ask went unanswered coming due again.
//!
//! **A pass**, per seat the state holds:
//! - A seat with an order `typed` (a delivery asked and never answered: the
//!   host's wait ran out, or the worker that asked died) has it asked again
//!   under the same id, once `deliver:<seat>` can be taken: at once for an
//!   order this worker never asked (the last holder's lease must run out
//!   first), else [`AGAIN`] after this worker last asked it. The manager
//!   answers an id at most once, so the seat gets it once.
//! - Otherwise its oldest `queued` order, when the mirror reads the seat
//!   `idle` (or `started`, for a `first` order): `take("deliver:<seat>",
//!   wsp:<instance>, 60 s)`, `order.deliver` fenced with the token, the ask
//!   ([`Ask`]: `Place::deliver_order` under the order's id), and the answer.
//!
//! **The answers** ([`Answer::of`]):
//!
//! | the place said | applied |
//! |---|---|
//! | `Acked` (or `Started`) | `order.delivered`, `order.acked` |
//! | `Unconfirmed` (or `NotTaken`: it arrived) | `order.delivered` |
//! | `NoSeat`, `Failed`, `Unsupported`, `Unsubmitted` | `order.refused` (counts) |
//! | `NotReady` (busy) | `order.refused { counts: false }` |
//! | `Unreachable`, `Backend` (the host's "still pending") | nothing: the order stays `typed` |
//!
//! Then `release`. **Keys**: `order:<id>:delivered` and `order:<id>:acked`,
//! the keys group 5's fold applies the manager's same answers under, so an
//! answer both hear applies once (the machine also takes a repeat answer as an
//! empty decision). A refusal is `order:<id>:refused:<token>`: an order can be
//! refused more than once (busy, then a counted refusal per attempt), and each
//! is under its own lease's token. `order.deliver` is
//! `order:<id>:deliver:<token>`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wsp_server::SharedStore;
use wsp_store_contract::{
    At, Command, Fence, IdempotencyKey, LeaseName, Order, OrderId, OrderState, Principal, SeatId, SeatState, State, Token,
};

use crate::place::{Delivery, Refusal as Said};
use crate::store::Store;

/// How long a `deliver:<seat>` lease is taken for.
pub const LEASE: Duration = Duration::from_secs(60);

/// How long after this worker asked a typed order and heard nothing it asks
/// again (the host's own wait is 30 s at most).
pub const AGAIN: Duration = Duration::from_secs(30);

/// The longest wait on the head: the stop is heard, and a lease left by a
/// dead holder is taken, within it.
const WAIT: Duration = Duration::from_secs(5);

/// The delivery itself: `Place::deliver_order(seat, order, text)`.
pub type Ask = dyn Fn(&SeatId, &OrderId, &str) -> crate::place::Result<Delivery> + Send + Sync;

/// What the place's answer comes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Acked,
    Unconfirmed,
    Refused { why: String, counts: bool },
    /// No answer yet: asked again under the same id.
    Pending(String),
}

impl Answer {
    pub fn of(seat: &SeatId, said: crate::place::Result<Delivery>) -> Answer {
        match said {
            Ok(Delivery::Acked | Delivery::Started) => Answer::Acked,
            Ok(Delivery::Unconfirmed) | Err(Said::NotTaken) => Answer::Unconfirmed,
            Err(Said::NotReady(s)) => Answer::Refused { why: format!("{} is {}", seat.0, s.as_str()), counts: false },
            Err(e @ (Said::NoSeat(_) | Said::Failed(_) | Said::Unsupported(_) | Said::Unsubmitted(_))) => {
                Answer::Refused { why: e.to_string(), counts: true }
            }
            Err(e @ (Said::Unreachable(_) | Said::Backend(_))) => Answer::Pending(e.to_string()),
        }
    }
}

/// The worker's state between passes: who it is, how it asks, and which typed
/// orders it asked when.
pub struct Worker {
    db: SharedStore,
    by: Principal,
    ask: Box<Ask>,
    lease: Duration,
    again: Duration,
    asked: HashMap<OrderId, Instant>,
    /// Seats whose place said busy (`NotReady`), and when: not asked again
    /// until the mirror has read them other than idle since, or [`AGAIN`]
    /// has passed. Without it a busy refusal, an event, wakes the worker to
    /// ask the same busy seat again at once.
    busy: HashMap<SeatId, Instant>,
    log: Box<dyn Fn(&str) + Send>,
}

/// The `n` of an order id `<seat>-o<n>` (the machine's), for oldest-first;
/// any other id sorts after them by its words.
fn age(id: &OrderId) -> (u64, String) {
    let n = id.0.rsplit_once("-o").and_then(|(_, n)| n.parse().ok()).unwrap_or(u64::MAX);
    (n, id.0.clone())
}

fn now_at() -> At {
    At(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

impl Worker {
    /// A worker applying as `wsp:<instance>`, asking through `ask`.
    pub fn new(db: SharedStore, instance: &str, ask: Box<Ask>) -> Worker {
        Worker {
            db,
            by: Principal::Wsp(instance.to_string()),
            ask,
            lease: LEASE,
            again: AGAIN,
            asked: HashMap::new(),
            busy: HashMap::new(),
            log: Box::new(|_| {}),
        }
    }

    /// Where a refused write or a failed store is said.
    pub fn logging(mut self, log: impl Fn(&str) + Send + 'static) -> Worker {
        self.log = Box::new(log);
        self
    }

    /// The lease's ttl and the re-ask interval (tests shorten both).
    pub fn timed(mut self, lease: Duration, again: Duration) -> Worker {
        self.lease = lease;
        self.again = again;
        self
    }

    /// The orders a pass would take now, one per seat: the typed order to ask
    /// again, else the oldest queued order of a seat that is ready for it.
    pub fn due(&self, state: &State) -> Vec<Order> {
        let mut by_seat: HashMap<&SeatId, Vec<&Order>> = HashMap::new();
        for o in state.orders.values() {
            by_seat.entry(&o.to).or_default().push(o);
        }
        let mut due = Vec::new();
        for (seat, mut orders) in by_seat {
            orders.sort_by_key(|o| age(&o.id));
            if let Some(typed) = orders.iter().find(|o| o.state == OrderState::Typed) {
                let recent = self.asked.get(&typed.id).is_some_and(|at| at.elapsed() < self.again);
                if !recent {
                    due.push((*typed).clone());
                }
                continue;
            }
            let Some(s) = state.seats.get(seat) else { continue };
            if self.busy.get(seat).is_some_and(|at| at.elapsed() < self.again) {
                continue;
            }
            let ready = |o: &Order| s.state == SeatState::Idle || (o.first && s.state == SeatState::Started);
            if let Some(q) = orders.iter().find(|o| o.state == OrderState::Queued && ready(o)) {
                due.push((*q).clone());
            }
        }
        due.sort_by_key(|o| age(&o.id));
        due
    }

    /// One pass: every due order delivered or asked again. The orders asked.
    pub fn pass(&mut self) -> Vec<OrderId> {
        let state = match self.db.snapshot() {
            Ok((s, _)) => s,
            Err(e) => {
                (self.log)(&format!("deliver: the store did not answer a snapshot: {e:?}"));
                return Vec::new();
            }
        };
        // Forget what is no longer typed, against this pass's own read, and a
        // busy seat the mirror has since read as anything but idle: its next
        // idle is a new one.
        self.asked.retain(|id, _| state.orders.get(id).is_some_and(|o| o.state == OrderState::Typed));
        self.busy.retain(|seat, _| state.seats.get(seat).is_some_and(|s| s.state == SeatState::Idle));
        let mut asked = Vec::new();
        for o in self.due(&state) {
            if self.deliver(&o) {
                asked.push(o.id);
            }
        }
        asked
    }

    fn apply(&self, cmd: Command, key: String) -> bool {
        match self.db.apply(cmd, IdempotencyKey(key.clone()), self.by.clone()) {
            Ok(_) => true,
            Err(e) => {
                (self.log)(&format!("deliver: {key} not applied: {e:?}"));
                false
            }
        }
    }

    /// `o` under `deliver:<seat>`: typed (if it was queued), asked, answered,
    /// released. Whether it was asked.
    fn deliver(&mut self, o: &Order) -> bool {
        let lease = LeaseName::deliver(&o.to);
        let token = match self.db.take(&lease, &self.by, self.lease) {
            Ok(Some(t)) => t,
            Ok(None) => return false,
            Err(e) => {
                (self.log)(&format!("deliver: {} not taken: {e:?}", lease.0));
                return false;
            }
        };
        let asked = self.deliver_under(o, &lease, token);
        if let Err(e) = wsp_store_contract::Leases::release(&*self.db, &lease, token) {
            (self.log)(&format!("deliver: {} not released: {e:?}", lease.0));
        }
        asked
    }

    fn deliver_under(&mut self, o: &Order, lease: &LeaseName, token: Token) -> bool {
        let id = &o.id;
        if o.state == OrderState::Queued {
            let fence = Fence { lease: lease.clone(), token };
            if !self.apply(Command::OrderDeliver { order: id.clone(), fence }, format!("order:{}:deliver:{}", id.0, token.0)) {
                return false;
            }
        }
        let said = (self.ask)(&o.to, id, &o.text);
        match Answer::of(&o.to, said) {
            Answer::Acked => {
                self.apply(Command::OrderDelivered { order: id.clone() }, format!("order:{}:delivered", id.0));
                self.apply(Command::OrderAcked { order: id.clone(), turn_began: now_at() }, format!("order:{}:acked", id.0));
            }
            Answer::Unconfirmed => {
                self.apply(Command::OrderDelivered { order: id.clone() }, format!("order:{}:delivered", id.0));
            }
            Answer::Refused { why, counts } => {
                if !counts {
                    self.busy.insert(o.to.clone(), Instant::now());
                }
                self.apply(Command::OrderRefused { order: id.clone(), why, counts }, format!("order:{}:refused:{}", id.0, token.0));
            }
            Answer::Pending(why) => {
                (self.log)(&format!("deliver: {} to {} not answered yet ({why}); asked again under the same id", id.0, o.to.0));
                self.asked.insert(id.clone(), Instant::now());
            }
        }
        true
    }

    /// How long the next wait on the head may be: [`WAIT`], or less when a
    /// typed order this worker asked comes due sooner.
    pub fn next_wait(&self) -> Duration {
        let soonest = self.asked.values().chain(self.busy.values()).map(|at| self.again.saturating_sub(at.elapsed())).min();
        soonest.map_or(WAIT, |d| d.min(WAIT)).max(Duration::from_millis(10))
    }

    /// Passes until `stop`: one at once, then one each time the head moves.
    pub fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            // The head before the pass: what the pass writes, and anything
            // written meanwhile, wakes the next wait at once.
            let head = match self.db.head() {
                Ok(h) => h,
                Err(e) => {
                    (self.log)(&format!("deliver: the store did not answer its head: {e:?}"));
                    std::thread::sleep(WAIT);
                    continue;
                }
            };
            self.pass();
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = self.db.wait_for(head, self.next_wait()) {
                (self.log)(&format!("deliver: wait_for failed: {e:?}"));
                std::thread::sleep(WAIT);
            }
        }
    }
}

/// The daemon's ask: compound under `WSP_HOST`'s host (the manager's, under
/// the store mode), `deliver_order` under the order's id. Not insisting: the
/// place's own reading of a busy seat is `NotReady`, which is no attempt.
fn place_ask() -> Box<Ask> {
    Box::new(|seat: &SeatId, order: &OrderId, text: &str| {
        let place = crate::place_compound::Compound::chosen(env!("CARGO_PKG_VERSION"))
            .map_err(|e| Said::Unreachable(e.to_string()))?;
        crate::place::Place::deliver_order(&place, &crate::place::Seat::new(&seat.0), &order.0, text)
    })
}

/// Start the worker beside the daemon's server, under [`crate::orders::Mode::Store`]
/// alone; `None`, and nothing started, otherwise.
pub fn start(store: &Store, db: SharedStore) -> Option<(JoinHandle<()>, Arc<AtomicBool>)> {
    if crate::orders::mode() != crate::orders::Mode::Store {
        return None;
    }
    start_with(store, db, place_ask())
}

/// [`start`] with the ask given, whatever the mode.
pub fn start_with(store: &Store, db: SharedStore, ask: Box<Ask>) -> Option<(JoinHandle<()>, Arc<AtomicBool>)> {
    let (root, state) = (store.root.clone(), store.state.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("wsp-deliver".into())
        .spawn(move || {
            // The store is opened on the thread: its caches are this thread's.
            let (root, state) = (root, state);
            let log = move |line: &str| wsp_store::store::log_line(&Store::at(root.clone(), state.clone()), line);
            Worker::new(db, "wsp", ask).logging(log).run(&flag)
        })
        .ok()?;
    Some((thread, stop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::tests::{db, ed, seat};
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use wsp_store_contract::{EventId, EventKind, Refusal, Subject};

    /// A seat's supervisor as far as delivery goes: answers an id at most once
    /// (an id asked again gets the first answer back, and is typed once), and
    /// answers what the test scripted for the next new id, `Acked` when nothing is.
    #[derive(Clone, Default)]
    struct Sup {
        typed: Arc<Mutex<Vec<(String, String)>>>,
        answered: Arc<Mutex<HashMap<String, crate::place::Result<Delivery>>>>,
        next: Arc<Mutex<Vec<crate::place::Result<Delivery>>>>,
        asks: Arc<Mutex<Vec<String>>>,
    }

    impl Sup {
        fn ask(&self) -> Box<Ask> {
            let sup = self.clone();
            Box::new(move |_seat, order, text| {
                sup.asks.lock().unwrap().push(order.0.clone());
                let next = {
                    let mut n = sup.next.lock().unwrap();
                    if n.is_empty() { Ok(Delivery::Acked) } else { n.remove(0) }
                };
                // An unanswered ask, or a refusal before typing, keeps no answer.
                let types = matches!(next, Ok(_)) || matches!(next, Err(Said::Failed(_)));
                if let Some(first) = sup.answered.lock().unwrap().get(&order.0) {
                    return first.clone();
                }
                if types {
                    sup.typed.lock().unwrap().push((order.0.clone(), text.to_string()));
                    sup.answered.lock().unwrap().insert(order.0.clone(), next.clone());
                }
                next
            })
        }
        fn script(&self, answers: Vec<crate::place::Result<Delivery>>) {
            *self.next.lock().unwrap() = answers;
        }
        fn typed(&self) -> Vec<(String, String)> {
            self.typed.lock().unwrap().clone()
        }
    }

    fn send(db: &SharedStore, to: &str, text: &str, first: bool) -> OrderId {
        let cmd = Command::OrderSend { to: SeatId(to.into()), text: text.into(), first };
        crate::orders::send_in(db, cmd, &ed()).unwrap()
    }

    fn order(db: &SharedStore, id: &OrderId) -> Order {
        db.snapshot().unwrap().0.orders[id].clone()
    }

    /// The `order.*` kinds written for `id`, in order.
    fn kinds(db: &SharedStore, id: &OrderId) -> Vec<EventKind> {
        db.events_after(EventId(0), 10_000)
            .unwrap()
            .into_iter()
            .filter(|e| e.subject == Subject::Order(id.clone()))
            .map(|e| e.kind)
            .collect()
    }

    /// An acked delivery: queued → typed → delivered → acknowledged, asked
    /// under the order's own id, typed once; the lease is given back.
    #[test]
    fn an_idle_seats_order_is_typed_once_under_its_id_and_acknowledged() {
        let env = crate::util::isolated("deliver-acked");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let sup = Sup::default();
        let mut w = Worker::new(db.clone(), "wsp", sup.ask());
        let id = send(&db, "cpd-1", "do the thing", false);
        assert_eq!(w.pass(), vec![id.clone()]);
        assert_eq!(
            kinds(&db, &id),
            [EventKind::OrderQueued, EventKind::OrderTyped, EventKind::OrderDelivered, EventKind::OrderAcknowledged]
        );
        assert_eq!(sup.typed(), [(id.0.clone(), "do the thing".to_string())]);
        assert!(db.lease(&LeaseName::deliver(&SeatId("cpd-1".into()))).unwrap().is_none(), "released");
        assert!(w.pass().is_empty(), "nothing is asked twice");
        assert_eq!(sup.typed().len(), 1);
    }

    /// A working seat's order waits, queued; an idle one's answers map to the
    /// machine's commands: unconfirmed is delivered, and a first order goes
    /// to a `started` seat too.
    #[test]
    fn a_working_seat_waits_and_a_first_order_goes_to_a_started_seat() {
        let env = crate::util::isolated("deliver-ready");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-w", SeatState::Working);
        seat(&db, "cpd-s", SeatState::Started);
        let sup = Sup::default();
        let mut w = Worker::new(db.clone(), "wsp", sup.ask());
        let busy = send(&db, "cpd-w", "later", false);
        let not_first = send(&db, "cpd-s", "not yet", false);
        let first = send(&db, "cpd-s", "your work", true);
        sup.script(vec![Ok(Delivery::Unconfirmed)]);
        assert_eq!(w.pass(), vec![first.clone()]);
        assert_eq!(order(&db, &first).state, OrderState::Delivered);
        assert_eq!(order(&db, &busy).state, OrderState::Queued);
        assert_eq!(order(&db, &not_first).state, OrderState::Queued);
        assert!(sup.asks.lock().unwrap().iter().all(|a| *a == first.0));
    }

    /// Busy (`NotReady`) is no attempt: queued again, attempts unchanged, and
    /// delivered once the seat reads idle. Three counted refusals are `failed`.
    #[test]
    fn a_busy_refusal_counts_nothing_and_three_counted_refusals_fail_the_order() {
        let env = crate::util::isolated("deliver-refused");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let sup = Sup::default();
        let mut w = Worker::new(db.clone(), "wsp", sup.ask());
        let id = send(&db, "cpd-1", "go", false);
        sup.script(vec![Err(Said::NotReady(crate::place::State::Working)), Err(Said::NotReady(crate::place::State::Working))]);
        w.pass();
        let o = order(&db, &id);
        assert_eq!((o.state, o.attempts), (OrderState::Queued, 0));
        assert!(w.pass().is_empty(), "a busy seat is not asked again until it moves");
        seat(&db, "cpd-1", SeatState::Working);
        assert!(w.pass().is_empty(), "a seat the mirror reads working is not asked");
        seat(&db, "cpd-1", SeatState::Idle);
        w.pass();
        let o = order(&db, &id);
        assert_eq!((o.state, o.attempts), (OrderState::Queued, 0), "a second busy refusal is applied too, and counts nothing");
        seat(&db, "cpd-1", SeatState::Working);
        w.pass();
        seat(&db, "cpd-1", SeatState::Idle);
        w.pass();
        assert_eq!(order(&db, &id).state, OrderState::Acknowledged);
        assert_eq!(sup.typed().len(), 1);

        // A seat whose supervisor refuses: each refusal is an attempt, under its own key.
        seat(&db, "cpd-2", SeatState::Idle);
        let doomed = send(&db, "cpd-2", "never", false);
        for n in 1..=3u32 {
            sup.script(vec![Err(Said::Unsupported("deliver under an order id"))]);
            w.pass();
            let o = order(&db, &doomed);
            let want = if n < 3 { OrderState::Queued } else { OrderState::Failed };
            assert_eq!((o.state, o.attempts), (want, n), "after refusal {n}");
        }
        assert!(w.pass().is_empty(), "a failed order is not asked again");
    }

    /// The worker dies between `order.deliver` and the answer: its lease runs
    /// out, the next worker takes it with a higher token, asks again under the
    /// same id, and the seat gets the order once. A write under the stale
    /// token is refused `stale`.
    #[test]
    fn a_worker_killed_mid_delivery_is_followed_by_one_that_asks_again_under_the_same_id() {
        let env = crate::util::isolated("deliver-killed");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let sup = Sup::default();
        let lease = LeaseName::deliver(&SeatId("cpd-1".into()));
        let id = send(&db, "cpd-1", "go", false);
        let by = Principal::Wsp("wsp".into());

        // The first worker: taken, typed, asked (the sup types it), and killed
        // before the answer is applied or the lease released.
        let ttl = Duration::from_millis(300);
        let old = db.take(&lease, &by, ttl).unwrap().expect("free");
        let fence = Fence { lease: lease.clone(), token: old };
        db.apply(Command::OrderDeliver { order: id.clone(), fence: fence.clone() }, IdempotencyKey("k1".into()), by.clone())
            .unwrap();
        let _lost = (sup.ask())(&SeatId("cpd-1".into()), &id, "go");
        assert_eq!(order(&db, &id).state, OrderState::Typed);

        // The next worker: nothing while the dead one's lease stands.
        let mut w = Worker::new(db.clone(), "wsp", sup.ask()).timed(ttl, Duration::from_secs(60));
        assert!(w.pass().is_empty(), "the lease is still held");
        std::thread::sleep(ttl + Duration::from_millis(100));
        assert_eq!(w.pass(), vec![id.clone()]);
        assert_eq!(order(&db, &id).state, OrderState::Acknowledged);
        assert_eq!(sup.typed(), [(id.0.clone(), "go".to_string())], "typed once");
        assert_eq!(sup.asks.lock().unwrap().iter().filter(|a| **a == id.0).count(), 2, "asked again under the same id");
        let taken: BTreeSet<u64> = db
            .events_after(EventId(0), 10_000)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::LeaseTaken && e.subject == Subject::Lease(lease.clone()))
            .filter_map(|e| e.payload.get("token").and_then(|t| t.as_u64()))
            .collect();
        assert_eq!(taken.iter().copied().collect::<Vec<_>>(), [old.0, old.0 + 1], "taken again with a higher token");

        // A write under the dead worker's token, on a fresh order, is stale.
        let next = send(&db, "cpd-1", "again", false);
        let now = db.take(&lease, &by, LEASE).unwrap().expect("free once released");
        let stale = db.apply(Command::OrderDeliver { order: next.clone(), fence }, IdempotencyKey("k2".into()), by.clone());
        assert!(
            matches!(stale, Err(wsp_store_contract::Error::Refused(Refusal::Stale { current, .. })) if current == now),
            "{stale:?}"
        );
        assert_eq!(order(&db, &next).state, OrderState::Queued);
    }

    /// An ask with no answer leaves the order typed; it is asked again under
    /// the same id once due, not on every wake.
    #[test]
    fn an_unanswered_ask_stays_typed_and_is_asked_again_when_due() {
        let env = crate::util::isolated("deliver-pending");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let sup = Sup::default();
        let again = Duration::from_millis(200);
        let mut w = Worker::new(db.clone(), "wsp", sup.ask()).timed(LEASE, again);
        let id = send(&db, "cpd-1", "go", false);
        sup.script(vec![Err(Said::Backend("cpd-1: delivery still pending after 30s".into()))]);
        assert_eq!(w.pass(), vec![id.clone()]);
        assert_eq!(order(&db, &id).state, OrderState::Typed);
        assert!(w.pass().is_empty(), "not due yet");
        assert!(w.next_wait() <= again);
        std::thread::sleep(again);
        assert_eq!(w.pass(), vec![id.clone()]);
        assert_eq!(order(&db, &id).state, OrderState::Acknowledged);
        assert_eq!(kinds(&db, &id).iter().filter(|k| **k == EventKind::OrderTyped).count(), 1);
    }

    /// The thread: woken by the send, no tick; the answer is heard in
    /// process within 2 s, and a subscriber of the log reads the four states.
    #[test]
    fn the_thread_delivers_a_send_within_two_seconds_without_a_tick() {
        let env = crate::util::isolated("deliver-thread");
        let store = Store::open();
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let sup = Sup::default();
        let (thread, stop) = start_with(&store, db.clone(), sup.ask()).expect("started");
        // Let the first pass find nothing and the thread settle into its wait.
        std::thread::sleep(Duration::from_millis(100));
        let sent = Instant::now();
        let id = send(&db, "cpd-1", "go", false);
        let o = crate::orders::answer_in(&db, &id, Duration::from_secs(2)).unwrap().expect("acknowledged within 2s");
        assert_eq!(o.state, OrderState::Acknowledged);
        assert!(sent.elapsed() < Duration::from_secs(2));
        assert_eq!(
            kinds(&db, &id),
            [EventKind::OrderQueued, EventKind::OrderTyped, EventKind::OrderDelivered, EventKind::OrderAcknowledged]
        );
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
    }

    /// Without `WSP_ORDERS=store` under the manager, no worker starts.
    #[test]
    fn no_worker_starts_unless_the_mode_is_store() {
        let env = crate::util::isolated("deliver-files");
        let store = Store::open();
        let db = db(&env.state().join("db"), wsp_server::Mode::Mirrored);
        std::env::remove_var(crate::orders::ENV);
        assert!(start(&store, db.clone()).is_none());
        std::env::set_var(crate::orders::ENV, "store");
        assert!(start(&store, db.clone()).is_none(), "store needs WSP_HOST=manager");
        std::env::remove_var(crate::orders::ENV);
    }

    #[test]
    fn the_place_answers_map_to_the_machines_commands() {
        let s = SeatId("cpd-1".into());
        assert_eq!(Answer::of(&s, Ok(Delivery::Acked)), Answer::Acked);
        assert_eq!(Answer::of(&s, Ok(Delivery::Unconfirmed)), Answer::Unconfirmed);
        assert!(matches!(Answer::of(&s, Err(Said::NoSeat(crate::place::Seat::new("cpd-1")))), Answer::Refused { counts: true, .. }));
        assert!(matches!(Answer::of(&s, Err(Said::Failed("x".into()))), Answer::Refused { counts: true, .. }));
        assert!(matches!(
            Answer::of(&s, Err(Said::NotReady(crate::place::State::Working))),
            Answer::Refused { counts: false, .. }
        ));
        assert!(matches!(Answer::of(&s, Err(Said::Unreachable("x".into()))), Answer::Pending(_)));
    }
}
