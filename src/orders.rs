//! The orders door (docs/state-core.md section 7's step 2, wsp-122): the one
//! way the root sends an order to a seat and waits for its answer once the
//! orders machine owns orders, behind `WSP_ORDERS`.
//!
//! - [`mode`]: `WSP_ORDERS=store` under `WSP_HOST=manager` is [`Mode::Store`];
//!   anything else, unset included, is [`Mode::Files`], today's path (part 2
//!   of the person's answer on wsp-115: the in-process host keeps the files
//!   until M1's contraction). Under `Store` the daemon opens `state.db` with
//!   the orders machine deciding ([`server_mode`], `Owned`) and starts the
//!   delivery worker (`crate::deliver`).
//! - [`send`] applies `order.send` and returns the order's id. In the daemon
//!   (which named its open store with [`serve_in_process`]) it goes through
//!   that `SharedStore`; anywhere else through [`wsp_client::Client::apply`],
//!   under one idempotency key across its retries.
//! - [`answer`] waits for the order to settle: `acknowledged`, `failed` or
//!   `cancelled` (and `done`, which only follows `acknowledged`, so an order
//!   that went past it while nobody was looking is settled too). In process
//!   it follows the head with `wait_for`; outside it, `await_order`.
//! - [`send_keyed`] is [`send`] under the caller's key, so a repeat (the
//!   daemon's next tick) is the same order; [`order`] reads an order in any
//!   state and [`orders_of`] a seat's orders in some states, neither waiting.
//!
//! Nothing here delivers: that is the worker's, under `deliver:<seat>`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use wsp_server::SharedStore;
use wsp_store_contract::{
    Command, EventId, EventKind, IdempotencyKey, Order, OrderId, OrderState, Principal, Refusal, SeatId, Subject,
};

use crate::store::Store;

/// The flag: `store` or `files`.
pub const ENV: &str = "WSP_ORDERS";

/// How long a process outside the daemon waits for the API to answer a dial.
const DIAL_WAIT: Duration = Duration::from_secs(5);

/// The states [`answer`] waits for.
pub const SETTLED: &[OrderState] =
    &[OrderState::Acknowledged, OrderState::Done, OrderState::Failed, OrderState::Cancelled];

/// Who owns an order's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Today's path: the files and the phases that stand in for an order.
    Files,
    /// The orders machine in `state.db`, and the daemon's delivery worker.
    Store,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Files => "files",
            Mode::Store => "store",
        }
    }
}

/// The mode this process runs under, from `WSP_ORDERS` and `WSP_HOST`.
pub fn mode() -> Mode {
    let var = |k: &str| std::env::var(k).ok();
    mode_of(var(ENV).as_deref(), var(crate::host_tcp::ENV).as_deref())
}

/// [`mode`], pure: `store` holds only under the manager.
pub fn mode_of(orders: Option<&str>, host: Option<&str>) -> Mode {
    match (orders.map(str::trim), host.map(str::trim)) {
        (Some("store"), Some("manager")) => Mode::Store,
        _ => Mode::Files,
    }
}

/// What `doctor` says of the mode, one line.
pub fn describe(mode: Mode, orders: Option<&str>) -> String {
    match (mode, orders.map(str::trim).filter(|v| !v.is_empty())) {
        (Mode::Store, _) => "orders: store (the orders machine owns them; the daemon delivers under deliver:<seat>)".into(),
        (Mode::Files, None) => "orders: files (WSP_ORDERS unset)".into(),
        (Mode::Files, Some("store")) => "orders: files (WSP_ORDERS=store holds only under WSP_HOST=manager)".into(),
        (Mode::Files, Some(v)) => format!("orders: files (WSP_ORDERS={v})"),
    }
}

/// The domain mode the daemon opens `state.db` with.
pub fn server_mode(mode: Mode) -> wsp_server::Mode {
    match mode {
        Mode::Store => wsp_server::Mode::Owned,
        Mode::Files => wsp_server::Mode::Mirrored,
    }
}

static IN_PROCESS: OnceLock<SharedStore> = OnceLock::new();

/// The daemon's open store, which [`send`] and [`answer`] use in this process
/// from now on. Set once; a second call is ignored.
pub fn serve_in_process(db: SharedStore) {
    let _ = IN_PROCESS.set(db);
}

fn in_process() -> Option<&'static SharedStore> {
    IN_PROCESS.get()
}

/// Why an order was not sent, or its answer not read.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The orders machine said no (`not_found` seat, `illegal` gone seat, or
    /// `illegal`, state `mirrored`, under the files mode's domain).
    Refused(Refusal),
    /// No API answered.
    Silent(String),
    /// The store or the API failed otherwise.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused(r) => write!(f, "refused: {r:?}"),
            Error::Silent(why) => write!(f, "no API answered: {why}"),
            Error::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

fn from_store(e: wsp_store_contract::Error) -> Error {
    match e {
        wsp_store_contract::Error::Refused(r) => Error::Refused(r),
        other => Error::Failed(format!("{other:?}")),
    }
}

fn from_client(e: wsp_client::Error) -> Error {
    match e {
        wsp_client::Error::Refused(r) => Error::Refused(r),
        wsp_client::Error::Silent(s) => Error::Silent(s.to_string()),
        other => Error::Failed(other.to_string()),
    }
}

/// A key no other send in this process has used.
fn mint_key() -> IdempotencyKey {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    IdempotencyKey(format!("send:{}:{nanos:x}:{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// The order an `order.queued` event names.
fn queued(events: &[wsp_store_contract::Event]) -> Option<OrderId> {
    events.iter().find(|e| e.kind == EventKind::OrderQueued).and_then(|e| match &e.subject {
        Subject::Order(id) => Some(id.clone()),
        _ => None,
    })
}

fn no_queued() -> Error {
    Error::Failed("order.send was applied and wrote no order.queued".into())
}

/// Send `text` to `seat` as `from`: `order.send`, and the id the machine gave
/// the order. `first` marks a spawn's first order (delivered to a `started`
/// seat as well as an `idle` one).
pub fn send(store: &Store, seat: &SeatId, text: &str, from: &Principal, first: bool) -> Result<OrderId, Error> {
    let cmd = Command::OrderSend { to: seat.clone(), text: text.to_string(), first };
    match in_process() {
        Some(db) => send_in(db, cmd, from),
        None => {
            let mut client = dial(store, from)?;
            let applied = client.apply(&cmd).map_err(from_client)?;
            queued_through(&mut client, &applied.events)
        }
    }
}

/// The order an applied `order.send` queued, read back through the API from
/// the events the apply wrote.
fn queued_through(client: &mut wsp_client::Client, written: &[EventId]) -> Result<OrderId, Error> {
    let first = written.iter().min().copied().ok_or_else(no_queued)?;
    let reply = client
        .request("events", serde_json::json!({ "from": first.0.saturating_sub(1), "limit": written.len() }))
        .map_err(from_client)?;
    match reply.outcome {
        wsp_api::Outcome::Ok(wsp_api::Answer::Events { events, .. }) => queued(&events).ok_or_else(no_queued),
        wsp_api::Outcome::Ok(other) => Err(Error::Failed(format!("events was answered {other:?}"))),
        wsp_api::Outcome::Err(f) => Err(Error::Failed(format!("{:?}: {}", f.code, f.text))),
    }
}

/// [`send`] through an open store.
pub fn send_in(db: &SharedStore, cmd: Command, from: &Principal) -> Result<OrderId, Error> {
    send_in_keyed(db, cmd, mint_key(), from)
}

/// `cmd` applied under `key` through an open store, and the order it queued.
/// A key the store has seen gives the first apply's events, so the same order.
fn send_in_keyed(db: &SharedStore, cmd: Command, key: IdempotencyKey, from: &Principal) -> Result<OrderId, Error> {
    let applied = db.apply(cmd, key, from.clone()).map_err(from_store)?;
    let first = applied.events.iter().min().copied().ok_or_else(no_queued)?;
    let events = db.events_after(EventId(first.0.saturating_sub(1)), applied.events.len()).map_err(from_store)?;
    queued(&events).ok_or_else(no_queued)
}

/// How many times [`send_keyed`] sends outside the daemon, at most, after a
/// lost reply or connection. The key makes each one after the first safe.
const KEYED_ATTEMPTS: u32 = 3;

/// The idempotency key [`send_keyed`] applies under for the caller's `key`:
/// namespaced, so it meets no key [`mint_key`] or anyone else makes.
fn caller_key(key: &str) -> IdempotencyKey {
    IdempotencyKey(format!("send:key:{key}"))
}

/// [`send`] under the caller's `key` rather than a fresh one: the same key
/// sent again (the daemon's next tick, say) is the same order, and the same
/// [`OrderId`] comes back, because the store gives a repeated key the first
/// apply's result (store-contract's `Store::apply`, wsp-065). Two keys are two
/// orders. In the daemon through its open store; anywhere else through the API
/// under the same key on each retry.
pub fn send_keyed(
    store: &Store,
    seat: &SeatId,
    text: &str,
    from: &Principal,
    first: bool,
    key: &str,
) -> Result<OrderId, Error> {
    let cmd = Command::OrderSend { to: seat.clone(), text: text.to_string(), first };
    match in_process() {
        Some(db) => send_in_keyed(db, cmd, caller_key(key), from),
        None => {
            let mut client = dial(store, from)?;
            let args = match serde_json::to_value(&cmd) {
                Ok(mut v) => v.get_mut("args").map(serde_json::Value::take).unwrap_or_default(),
                Err(e) => return Err(Error::Failed(format!("order.send as json: {e}"))),
            };
            let mut attempt = 0;
            let reply = loop {
                attempt += 1;
                match client.request_keyed("order.send", args.clone(), caller_key(key)) {
                    Err(wsp_client::Error::Timeout | wsp_client::Error::Lost(_)) if attempt < KEYED_ATTEMPTS => {}
                    other => break other.map_err(from_client)?,
                }
            };
            match reply.outcome {
                wsp_api::Outcome::Ok(wsp_api::Answer::Applied) => queued_through(&mut client, &reply.events),
                wsp_api::Outcome::Ok(other) => Err(Error::Failed(format!("order.send was answered {other:?}"))),
                wsp_api::Outcome::Err(f) => match f.refusal {
                    Some(r) => Err(Error::Refused(r)),
                    None => Err(Error::Failed(format!("{:?}: {}", f.code, f.text))),
                },
            }
        }
    }
}

/// [`send_keyed`] through an open store named by the caller rather than the
/// daemon's (`crate::refusals::Door`'s tests, wsp-125).
pub fn send_keyed_in(
    db: &SharedStore,
    seat: &SeatId,
    text: &str,
    from: &Principal,
    first: bool,
    key: &str,
) -> Result<OrderId, Error> {
    let cmd = Command::OrderSend { to: seat.clone(), text: text.to_string(), first };
    send_in_keyed(db, cmd, caller_key(key), from)
}

/// Order `id` in whatever state it is in now, without waiting; `None` when no
/// store holds it. In the daemon a read of its open store; anywhere else the
/// API's `orders` read.
pub fn order(store: &Store, id: &OrderId) -> Result<Option<Order>, Error> {
    match in_process() {
        Some(db) => order_in(db, id),
        None => {
            let mut client = dial(store, &Principal::Wsp("orders".into()))?;
            Ok(read_orders(&mut client, serde_json::json!({}))?.into_iter().find(|o| &o.id == id))
        }
    }
}

/// [`order`] on an open store.
pub fn order_in(db: &SharedStore, id: &OrderId) -> Result<Option<Order>, Error> {
    let (state, _) = db.snapshot().map_err(from_store)?;
    Ok(state.orders.get(id).cloned())
}

/// `seat`'s orders in any of `states` (every state when `states` is empty, as
/// the store and the API read it), without waiting. In the daemon a read of
/// its open store; anywhere else the API's `orders` read.
pub fn orders_of(store: &Store, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>, Error> {
    match in_process() {
        Some(db) => orders_of_in(db, seat, states),
        None => {
            let mut client = dial(store, &Principal::Wsp("orders".into()))?;
            read_orders(&mut client, serde_json::json!({ "seat": seat, "states": states }))
        }
    }
}

/// [`orders_of`] on an open store.
pub fn orders_of_in(db: &SharedStore, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>, Error> {
    db.orders_for(seat, states).map_err(from_store)
}

/// The API's `orders` read under `args`.
fn read_orders(client: &mut wsp_client::Client, args: serde_json::Value) -> Result<Vec<Order>, Error> {
    let reply = client.request("orders", args).map_err(from_client)?;
    match reply.outcome {
        wsp_api::Outcome::Ok(wsp_api::Answer::Orders(orders)) => Ok(orders),
        wsp_api::Outcome::Ok(other) => Err(Error::Failed(format!("orders was answered {other:?}"))),
        wsp_api::Outcome::Err(f) => Err(Error::Failed(format!("{:?}: {}", f.code, f.text))),
    }
}

fn dial(store: &Store, by: &Principal) -> Result<wsp_client::Client, Error> {
    wsp_client::Client::dial(&store.state, by.clone(), DIAL_WAIT).map_err(|s| Error::Silent(s.to_string()))
}

/// Order `id` once it is settled ([`SETTLED`]); `None` when `wait` runs out
/// first, or when no store holds such an order by then.
pub fn answer(store: &Store, id: &OrderId, wait: Duration) -> Result<Option<Order>, Error> {
    match in_process() {
        Some(db) => answer_in(db, id, wait),
        None => {
            let mut client = dial(store, &Principal::Wsp("orders".into()))?;
            client.await_order(id, SETTLED, wait).map_err(from_client)
        }
    }
}

/// [`answer`] on an open store: the order read after each move of the head,
/// the head followed with `wait_for`. No poll: a quiet store is one wait.
pub fn answer_in(db: &SharedStore, id: &OrderId, wait: Duration) -> Result<Option<Order>, Error> {
    let deadline = Instant::now() + wait;
    loop {
        // The head before the read: a move between the two wakes the wait at once.
        let head = db.head().map_err(from_store)?;
        let (state, _) = db.snapshot().map_err(from_store)?;
        if let Some(o) = state.orders.get(id).filter(|o| SETTLED.contains(&o.state)) {
            return Ok(Some(o.clone()));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        db.wait_for(head, left).map_err(from_store)?;
    }
}

// ---- tells and answers as orders (wsp-124) -------------------------------

/// How long a tell waits for its order to settle: today's wait, the
/// supervisor's 30 s for one delivery.
pub const TELL_WAIT: Duration = Duration::from_secs(30);

/// What an order has come to, as the place's own answer to a tell says it, so
/// a tell under `store` prints what the files path prints for the same
/// outcome. `None` while it is `queued` or `typed`: the daemon's still.
pub fn said(o: &Order) -> Option<crate::place::Result<crate::place::Delivery>> {
    use crate::place::{Delivery, Refusal as Said};
    use OrderState as S;
    match o.state {
        S::Acknowledged | S::Done => Some(Ok(Delivery::Acked)),
        S::Delivered => Some(Ok(Delivery::Unconfirmed)),
        S::Failed => Some(Err(Said::Failed(o.why.clone().unwrap_or_else(|| "the order failed".into())))),
        S::Cancelled => Some(Err(Said::Failed("the order was cancelled".into()))),
        S::Queued | S::Typed => None,
    }
}

/// Who the orders machine is told sent an order: the seat this process runs
/// in, else the person, under the address the sentence opens with.
pub fn principal(from: &str) -> Principal {
    match crate::place::seat_from_env() {
        Some(s) => Principal::Seat(SeatId(s.to_string())),
        None => Principal::Person(from.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use wsp_store_contract::{Entity, Fence, LeaseName, Leases, MachineName, Seat, SeatState};

    /// A fresh `state.db` under `dir`, its domain the composed one under `mode`.
    pub(crate) fn db(dir: &std::path::Path, mode: wsp_server::Mode) -> SharedStore {
        std::fs::create_dir_all(dir).unwrap();
        let _ = std::fs::remove_file(dir.join("state.db"));
        Arc::new(
            wsp_store_sqlite::SqliteStore::open(dir.join("state.db"), Arc::new(wsp_server::Composed::new(mode))).unwrap(),
        )
    }

    /// `seat` mirrored into `db` in `state`, as the fold puts it.
    pub(crate) fn seat(db: &SharedStore, seat: &str, state: SeatState) {
        static N: AtomicU64 = AtomicU64::new(0);
        let s = Seat {
            id: SeatId(seat.into()),
            machine: MachineName("mac".into()),
            kind: "claude".into(),
            model: None,
            state,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: None,
            fields: Default::default(),
        };
        let key = IdempotencyKey(format!("test:seat:{seat}:{}", N.fetch_add(1, Ordering::Relaxed)));
        db.apply(Command::Mirror { put: vec![Entity::Seat(s)], gone: vec![] }, key, Principal::Manager(MachineName("mac".into())))
            .unwrap();
    }

    pub(crate) fn ed() -> Principal {
        Principal::Person("ed".into())
    }

    #[test]
    fn store_holds_only_under_the_manager_and_anything_else_is_files() {
        assert_eq!(mode_of(Some("store"), Some("manager")), Mode::Store);
        assert_eq!(mode_of(Some(" store "), Some("manager")), Mode::Store);
        for (orders, host) in [
            (None, None),
            (None, Some("manager")),
            (Some("files"), Some("manager")),
            (Some("store"), None),
            (Some("store"), Some("inproc")),
            (Some("yes"), Some("manager")),
        ] {
            assert_eq!(mode_of(orders, host), Mode::Files, "{orders:?} under {host:?}");
        }
        assert_eq!(server_mode(Mode::Store), wsp_server::Mode::Owned);
        assert_eq!(server_mode(Mode::Files), wsp_server::Mode::Mirrored);
        assert!(describe(Mode::Files, None).contains("unset"));
        assert!(describe(Mode::Files, Some("store")).contains("only under WSP_HOST=manager"));
        assert!(describe(Mode::Store, Some("store")).starts_with("orders: store"));
    }

    /// In process: `order.send` through the open store gives the machine's id,
    /// and `answer` follows the head to the order's settling.
    #[test]
    fn a_send_in_process_is_queued_and_its_answer_is_heard_when_it_settles() {
        let env = crate::util::isolated("orders-in-process");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-1", SeatState::Idle);
        let cmd = Command::OrderSend { to: SeatId("cpd-1".into()), text: "go".into(), first: false };
        let id = send_in(&db, cmd, &ed()).unwrap();
        assert_eq!(id, OrderId("cpd-1-o1".into()));
        assert_eq!(answer_in(&db, &id, Duration::from_millis(50)).unwrap(), None, "a queued order is not settled");

        let later = db.clone();
        let cancel = id.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            later.apply(Command::OrderCancel { order: cancel }, IdempotencyKey("c".into()), ed()).unwrap();
        });
        let started = Instant::now();
        let o = answer_in(&db, &id, Duration::from_secs(10)).unwrap().expect("settled");
        assert_eq!(o.state, OrderState::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5), "woken by the event, not a timeout");
        t.join().unwrap();

        let gone = Command::OrderSend { to: SeatId("cpd-none".into()), text: "go".into(), first: false };
        assert!(matches!(send_in(&db, gone, &ed()), Err(Error::Refused(Refusal::NotFound { .. }))));
    }

    /// In process: one caller's key is one order however often it is sent, and
    /// two keys are two.
    #[test]
    fn a_keyed_send_repeated_is_the_same_order_and_two_keys_are_two() {
        let env = crate::util::isolated("orders-keyed");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-3", SeatState::Idle);
        let cmd = || Command::OrderSend { to: SeatId("cpd-3".into()), text: "go".into(), first: false };
        let a = send_in_keyed(&db, cmd(), caller_key("tick:1"), &ed()).unwrap();
        let again = send_in_keyed(&db, cmd(), caller_key("tick:1"), &ed()).unwrap();
        assert_eq!(a, again, "the same key gives the same order");
        assert_eq!(orders_of_in(&db, &SeatId("cpd-3".into()), &[]).unwrap().len(), 1, "one order for one key");
        let b = send_in_keyed(&db, cmd(), caller_key("tick:2"), &ed()).unwrap();
        assert_ne!(a, b, "another key is another order");
        assert_eq!(orders_of_in(&db, &SeatId("cpd-3".into()), &[]).unwrap().len(), 2);
    }

    /// `order` reads a queued order that `answer` with no wait does not report,
    /// and `orders_of` lists only the named seat's orders in the named states.
    #[test]
    fn an_order_is_read_in_any_state_and_a_seats_failed_orders_are_listed() {
        let env = crate::util::isolated("orders-read");
        let db = db(&env.state().join("db"), wsp_server::Mode::Owned);
        seat(&db, "cpd-4", SeatState::Idle);
        seat(&db, "cpd-5", SeatState::Idle);
        let to = |s: &str| Command::OrderSend { to: SeatId(s.into()), text: "go".into(), first: false };
        let q = send_in(&db, to("cpd-4"), &ed()).unwrap();
        assert_eq!(answer_in(&db, &q, Duration::ZERO).unwrap(), None, "a queued order is not settled");
        let read = order_in(&db, &q).unwrap().expect("read without waiting");
        assert_eq!(read.state, OrderState::Queued);
        assert_eq!(order_in(&db, &OrderId("cpd-4-o99".into())).unwrap(), None);

        // Failed: one of cpd-4's and one of cpd-5's, typed under the seat's
        // lease and refused as the delivery worker would, until the machine's
        // attempts run out.
        let by = Principal::Wsp("wsp".into());
        let fail = |seat: &str| {
            let id = send_in(&db, to(seat), &ed()).unwrap();
            let lease = LeaseName::deliver(&SeatId(seat.into()));
            for n in 0.. {
                if order_in(&db, &id).unwrap().unwrap().state == OrderState::Failed {
                    break;
                }
                assert!(n < 50, "{id:?} never failed");
                let token = Leases::take(&*db, &lease, &by, Duration::from_secs(60)).unwrap().expect("free");
                let fence = Fence { lease: lease.clone(), token };
                let key = |k: &str| IdempotencyKey(format!("test:{k}:{}:{n}", id.0));
                db.apply(Command::OrderDeliver { order: id.clone(), fence }, key("deliver"), by.clone()).unwrap();
                let refused = Command::OrderRefused { order: id.clone(), why: format!("no {n}"), counts: true };
                db.apply(refused, key("refused"), by.clone()).unwrap();
                Leases::release(&*db, &lease, token).unwrap();
            }
            id
        };
        let f4 = fail("cpd-4");
        let f5 = fail("cpd-5");
        let failed = orders_of_in(&db, &SeatId("cpd-4".into()), &[OrderState::Failed]).unwrap();
        assert_eq!(failed.iter().map(|o| o.id.clone()).collect::<Vec<_>>(), vec![f4]);
        let failed5 = orders_of_in(&db, &SeatId("cpd-5".into()), &[OrderState::Failed]).unwrap();
        assert_eq!(failed5.iter().map(|o| o.id.clone()).collect::<Vec<_>>(), vec![f5]);
        let queued = orders_of_in(&db, &SeatId("cpd-4".into()), &[OrderState::Queued]).unwrap();
        assert_eq!(queued.iter().map(|o| o.id.clone()).collect::<Vec<_>>(), vec![q]);
    }

    /// Outside the daemon: the same send through the API with `wsp-client`,
    /// and the answer through `await_order`.
    #[test]
    fn a_send_through_the_api_is_queued_and_its_answer_is_awaited() {
        let env = crate::util::isolated("orders-client");
        let store = Store::open();
        let db = db(&store.state, wsp_server::Mode::Owned);
        seat(&db, "cpd-2", SeatState::Idle);
        let mut cfg = wsp_server::Config::new(&store.state);
        cfg.port = 0;
        let server = wsp_server::Server::start(db.clone(), cfg).unwrap();

        let id = send(&store, &SeatId("cpd-2".into()), "go", &ed(), false).unwrap();
        assert_eq!(id, OrderId("cpd-2-o1".into()));
        assert_eq!(db.snapshot().unwrap().0.orders[&id].state, OrderState::Queued);
        db.apply(Command::OrderCancel { order: id.clone() }, IdempotencyKey("c".into()), ed()).unwrap();
        let o = answer(&store, &id, Duration::from_secs(10)).unwrap().expect("settled");
        assert_eq!(o.state, OrderState::Cancelled);

        // The keyed send and the reads, through the API as well.
        let seat2 = SeatId("cpd-2".into());
        let k = send_keyed(&store, &seat2, "again", &ed(), false, "tick:1").unwrap();
        assert_eq!(send_keyed(&store, &seat2, "again", &ed(), false, "tick:1").unwrap(), k);
        assert_ne!(send_keyed(&store, &seat2, "again", &ed(), false, "tick:2").unwrap(), k);
        assert_eq!(order(&store, &k).unwrap().map(|o| o.state), Some(OrderState::Queued));
        assert_eq!(order(&store, &OrderId("cpd-2-o99".into())).unwrap(), None);
        assert_eq!(orders_of(&store, &seat2, &[OrderState::Queued]).unwrap().len(), 2);
        assert_eq!(orders_of(&store, &seat2, &[OrderState::Cancelled]).unwrap().len(), 1);
        assert_eq!(orders_of(&store, &seat2, &[]).unwrap().len(), 3);
        server.stop();
        drop(env);
    }
}
