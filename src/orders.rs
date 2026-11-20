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
            let first = applied.events.iter().min().copied().ok_or_else(no_queued)?;
            let reply = client
                .request("events", serde_json::json!({ "from": first.0.saturating_sub(1), "limit": applied.events.len() }))
                .map_err(from_client)?;
            match reply.outcome {
                wsp_api::Outcome::Ok(wsp_api::Answer::Events { events, .. }) => queued(&events).ok_or_else(no_queued),
                wsp_api::Outcome::Ok(other) => Err(Error::Failed(format!("events was answered {other:?}"))),
                wsp_api::Outcome::Err(f) => Err(Error::Failed(format!("{:?}: {}", f.code, f.text))),
            }
        }
    }
}

/// [`send`] through an open store.
pub fn send_in(db: &SharedStore, cmd: Command, from: &Principal) -> Result<OrderId, Error> {
    let applied = db.apply(cmd, mint_key(), from.clone()).map_err(from_store)?;
    let first = applied.events.iter().min().copied().ok_or_else(no_queued)?;
    let events = db.events_after(EventId(first.0.saturating_sub(1)), applied.events.len()).map_err(from_store)?;
    queued(&events).ok_or_else(no_queued)
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use wsp_store_contract::{Entity, MachineName, Seat, SeatState};

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
        server.stop();
        drop(env);
    }
}
