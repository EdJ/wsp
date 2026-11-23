//! `wsp-server` on an ephemeral loopback port over `MemStore` (wsp-073's done
//! list): `hello` and its token, each read verb, `wip`'s cut (wsp-695), commands `unsupported`,
//! malformed lines, `subscribe {}` and `subscribe {from}` (across a restart,
//! and `too_old`), the silent client dropped, two subscribers in one order,
//! the start refused while another server answers, and the mirror domain:
//! silent for an unchanged entity, and its fold equal to the snapshot (2.3
//! case 6, run with `Mirror` as the domain). Step 2 (wsp-121): the composed
//! domain under `Owned` applies a client's `order.send` once per key and its
//! `order.queued` reaches a subscriber, refuses a seat's `order.acked`
//! `unauthorised` and a `mirror` putting an order `illegal`, state `owned`;
//! under `Mirrored`, `order.send` is `illegal`, state `mirrored`.
//! The view verbs (wsp-307): whatever the `Views` hand in, whole, and
//! `unsupported` without them.

use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use wsp_api::{
    Answer, ApiToken, Call, Code, ConnError, Connection, Feed, Frame, Hello, Listen, NoArgs, Outcome, Refused, Reply,
    ReqId, Request, Subscribe, Verb, LISTEN_FILE, PROTOCOL, TOKEN_FILE,
};
use wsp_server::tcp::Tcp;
use wsp_server::{Composed, Config, Mirror, Mode, Server, SharedStore, StartError, OWNED};
use wsp_store_conformance::MemStore;
use wsp_store_contract::*;

/// The upper bound on every wait for a frame, a reply or a dropped connection:
/// generous, so a machine under load is slow and not failed. Each wait ends as
/// soon as what it waits for comes.
const WAIT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------- fixtures

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wsp-server-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

fn mirror_store() -> MemStore {
    MemStore::with_domain(Arc::new(Mirror))
}

fn start(store: SharedStore, dir: &Path, beat: Duration) -> Server {
    let mut cfg = Config::new(dir);
    cfg.port = 0;
    cfg.beat = beat;
    Server::start(store, cfg).unwrap()
}

fn ed() -> Principal {
    Principal::Person("ed".into())
}

/// Where the server says it listens, as a client finds it.
fn listen(dir: &Path) -> Listen {
    serde_json::from_str(&fs::read_to_string(dir.join(LISTEN_FILE)).unwrap()).unwrap()
}

fn addr(dir: &Path) -> SocketAddr {
    listen(dir).addrs[0].parse().unwrap()
}

fn token(dir: &Path) -> ApiToken {
    ApiToken(fs::read_to_string(dir.join(TOKEN_FILE)).unwrap().trim().to_string())
}

fn hello(token: ApiToken) -> Frame {
    Frame::Hello(Hello { token, by: ed(), client: "wsp-server tests".into(), protocol: PROTOCOL })
}

/// A welcomed client, who it is, and the frames it has read past while waiting for a reply.
struct Client {
    conn: Tcp,
    by: Principal,
    next: u64,
    feed: Vec<Feed>,
}

fn connect(dir: &Path) -> Client {
    connect_as(dir, ed())
}

fn connect_as(dir: &Path, by: Principal) -> Client {
    let mut conn = Tcp::dial(addr(dir), WAIT).unwrap();
    let hello = Frame::Hello(Hello { token: token(dir), by: by.clone(), client: "wsp-server tests".into(), protocol: PROTOCOL });
    conn.send(&hello).unwrap();
    match conn.recv(WAIT).unwrap() {
        Some(Frame::Welcome(w)) => assert_eq!((w.server.as_str(), w.protocol), ("wsp", PROTOCOL)),
        other => panic!("expected welcome, got {other:?}"),
    }
    Client { conn, by, next: 1, feed: vec![] }
}

impl Client {
    fn send(&mut self, call: Call) -> ReqId {
        self.send_keyed(call, None)
    }

    fn send_keyed(&mut self, call: Call, key: Option<&str>) -> ReqId {
        let id = ReqId(self.next);
        self.next += 1;
        let key = key.map(|k| IdempotencyKey(k.into()));
        let req = Request { v: PROTOCOL, id, key, by: self.by.clone(), call };
        self.conn.send(&Frame::Request(req)).unwrap();
        id
    }

    /// A command under `key` (none when `None`), and its whole reply.
    fn command(&mut self, cmd: Command, key: Option<&str>) -> Reply {
        let id = self.send_keyed(Call::Command(cmd), key);
        self.reply(id)
    }

    /// The reply to `id`; feed frames read past are kept.
    fn reply(&mut self, id: ReqId) -> Reply {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "no reply to {id:?}");
            match self.conn.recv(WAIT).unwrap() {
                Some(Frame::Reply(r)) if r.id == id => return r,
                Some(Frame::Feed(f)) => self.feed.push(f),
                Some(other) => panic!("expected the reply to {id:?}, got {other:?}"),
                None => {}
            }
        }
    }

    fn call(&mut self, call: Call) -> Outcome {
        let id = self.send(call);
        self.reply(id).outcome
    }

    fn read(&mut self, verb: Verb) -> Answer {
        match self.call(Call::Verb(verb)) {
            Outcome::Ok(a) => a,
            Outcome::Err(f) => panic!("a read was refused: {f:?}"),
        }
    }

    fn subscribe(&mut self, from: Option<EventId>) {
        assert_eq!(self.read(Verb::Subscribe(Subscribe { from })), Answer::Subscribed);
    }

    /// The next feed frame that is not a beat.
    fn next_feed(&mut self) -> Feed {
        if let Some(i) = self.feed.iter().position(|f| !matches!(f, Feed::Beat { .. })) {
            let f = self.feed.remove(i);
            self.feed.drain(..i);
            return f;
        }
        self.feed.clear();
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "no feed frame");
            match self.conn.recv(WAIT).unwrap() {
                Some(Frame::Feed(Feed::Beat { .. })) | None => {}
                Some(Frame::Feed(f)) => return f,
                Some(other) => panic!("expected a feed frame, got {other:?}"),
            }
        }
    }

    /// Events from the feed until one with id `upto`.
    fn events_until(&mut self, upto: EventId) -> Vec<Event> {
        let mut out = vec![];
        while out.last().is_none_or(|e: &Event| e.id < upto) {
            match self.next_feed() {
                Feed::Event(e) => out.push(e),
                other => panic!("expected an event, got {other:?}"),
            }
        }
        out
    }
}

fn row(id: &str, title: &str) -> Row {
    Row {
        workspace: Default::default(),
        id: RowId(id.into()),
        project: ProjectId("wsp".into()),
        title: title.into(),
        state: RowState::Todo,
        before_block: None,
        seat: Some(SeatId("cpd-1".into())),
        group: None,
        commit: None,
        fields: Default::default(),
    }
}

fn seat(id: &str, machine: &str) -> Seat {
    Seat {
        id: SeatId(id.into()),
        machine: MachineName(machine.into()),
        kind: "claude".into(),
        model: None,
        state: SeatState::Working,
        before_lost: None,
        row: None,
        attempt: 0,
        pid: Some(42),
        fields: Default::default(),
    }
}

/// One of every entity, and a second row and seat to filter by.
fn everything() -> Vec<Entity> {
    let mut parked = row("wsp-2", "two");
    parked.state = RowState::Parked;
    vec![
        Entity::Row(row("wsp-1", "one")),
        Entity::Row(parked),
        Entity::List(List {
            workspace: Default::default(),
            id: ListId("s1".into()),
            state: ListState::Running,
            groups: vec![],
            fields: Default::default(),
        }),
        Entity::Seat(seat("cpd-1", "mac")),
        Entity::Seat(seat("cpd-2", "pi")),
        Entity::Claim(Claim { row: RowId("wsp-1".into()), seat: SeatId("cpd-1".into()), token: Token(1), expires: At(9) }),
        Entity::Order(Order {
            workspace: Default::default(),
            id: OrderId("o-1".into()),
            from: ed(),
            to: SeatId("cpd-1".into()),
            text: "go".into(),
            state: OrderState::Queued,
            attempts: 0,
            first: false,
            why: None,
        }),
        Entity::Ask(Ask {
            workspace: Default::default(),
            id: AskId("a-1".into()),
            kind: AskKind::Flag,
            from: ed(),
            to: None,
            about: None,
            text: "look".into(),
            state: AskState::Raised,
            answer: None,
            fields: Default::default(),
        }),
        Entity::Machine(Machine {
            name: MachineName("mac".into()),
            transport: Transport::Local,
            beat: None,
            facts: Default::default(),
        }),
    ]
}

fn apply(s: &MemStore, put: Vec<Entity>, gone: Vec<Subject>, key: &str) -> Applied {
    s.apply(Command::Mirror { put, gone }, IdempotencyKey(key.into()), ed()).unwrap()
}

/// `n` mirror commands, each putting one new row: one event each.
fn rows(s: &MemStore, from: usize, n: usize) {
    for i in from..from + n {
        apply(s, vec![Entity::Row(row(&format!("r-{i}"), "x"))], vec![], &format!("rows-{i}"));
    }
}

// ---------------------------------------------------------------- hello

#[test]
fn no_token_or_a_wrong_one_closes_the_connection_and_the_right_one_is_welcomed() {
    let d = dir("hello");
    let store = mirror_store();
    rows(&store, 0, 3);
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(10));

    let token_mode = fs::metadata(d.join(TOKEN_FILE)).unwrap().permissions().mode() & 0o777;
    assert_eq!(token_mode, 0o600);
    assert_eq!(token(&d).0.len(), 64);
    assert_eq!(listen(&d).addrs, vec![server.addrs()[0].to_string()]);
    assert!(listen(&d).addrs[0].starts_with("127.0.0.1:"));

    let refused_then_closed = |first: &str, code: Code| {
        let mut c = Tcp::dial(addr(&d), WAIT).unwrap();
        c.send_line(first).unwrap();
        match c.recv(WAIT).unwrap() {
            Some(Frame::Refused(Refused { code: got, .. })) => assert_eq!(got, code, "after {first}"),
            other => panic!("expected refused {code:?} after {first}, got {other:?}"),
        }
        assert_eq!(c.recv(WAIT), Err(ConnError::Closed), "the connection is closed after {first}");
    };
    let line = |f: Frame| serde_json::to_string(&f).unwrap() + "\n";

    // No hello at all: a request first.
    let req = Request { v: PROTOCOL, id: ReqId(1), key: None, by: ed(), call: Call::Verb(Verb::State(NoArgs {})) };
    refused_then_closed(&line(Frame::Request(req)), Code::Malformed);
    // A hello with no token.
    refused_then_closed(
        "{\"type\":\"hello\",\"by\":\"person:ed\",\"client\":\"x\",\"protocol\":1}\n",
        Code::Malformed,
    );
    // An empty token, and a wrong one.
    refused_then_closed(&line(hello(ApiToken(String::new()))), Code::Unauthorised);
    refused_then_closed(&line(hello(ApiToken("0".repeat(64)))), Code::Unauthorised);

    // The right one.
    let mut c = Tcp::dial(addr(&d), WAIT).unwrap();
    c.send(&hello(token(&d))).unwrap();
    match c.recv(WAIT).unwrap() {
        Some(Frame::Welcome(w)) => {
            assert_eq!(w.head, store.head().unwrap());
            assert_eq!(w.head, EventId(3));
        }
        other => panic!("expected welcome, got {other:?}"),
    }
    assert_eq!(server.token(), &token(&d));
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- reads

#[test]
fn each_read_verb_answers_what_the_store_holds_and_a_command_is_unsupported() {
    let d = dir("reads");
    let store = mirror_store();
    apply(&store, everything(), vec![], "all");
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(10));
    let mut c = connect(&d);
    let (state, at) = store.snapshot().unwrap();
    let wsp1 = RowId("wsp-1".into());

    assert_eq!(c.read(Verb::State(NoArgs {})), Answer::State { state: state.clone(), at });
    assert_eq!(c.read(Verb::Row { row: wsp1.clone() }), Answer::Row(store.row(&wsp1).unwrap()));
    assert!(matches!(c.read(Verb::Row { row: wsp1.clone() }), Answer::Row(Some(_))));
    assert_eq!(c.read(Verb::Row { row: RowId("nope".into()) }), Answer::Row(None));
    assert_eq!(c.read(Verb::Rows(RowFilter::default())), Answer::Rows(store.rows(&RowFilter::default()).unwrap()));
    let parked = RowFilter { states: vec![RowState::Parked], ..Default::default() };
    match c.read(Verb::Rows(parked.clone())) {
        Answer::Rows(r) => {
            assert_eq!(r, store.rows(&parked).unwrap());
            assert_eq!(r.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    let s1 = ListId("s1".into());
    assert_eq!(c.read(Verb::List { list: s1.clone() }), Answer::List(store.list(&s1).unwrap()));
    assert_eq!(c.read(Verb::Lists(ListFilter::default())), Answer::Lists(state.lists.values().cloned().collect()));
    let cpd1 = SeatId("cpd-1".into());
    assert_eq!(c.read(Verb::Seat { seat: cpd1.clone() }), Answer::Seat(store.seat(&cpd1).unwrap()));
    let on_pi = SeatFilter { machine: Some(MachineName("pi".into())), ..Default::default() };
    assert_eq!(c.read(Verb::Seats(on_pi.clone())), Answer::Seats(store.seats(&on_pi).unwrap()));
    assert_eq!(
        c.read(Verb::Orders { seat: Some(cpd1.clone()), states: vec![] }),
        Answer::Orders(store.orders_for(&cpd1, &[]).unwrap())
    );
    assert_eq!(
        c.read(Verb::Orders { seat: None, states: vec![OrderState::Queued] }),
        Answer::Orders(state.orders.values().cloned().collect())
    );
    assert_eq!(c.read(Verb::Orders { seat: None, states: vec![OrderState::Done] }), Answer::Orders(vec![]));
    assert_eq!(
        c.read(Verb::Asks { kinds: vec![AskKind::Flag], states: vec![] }),
        Answer::Asks(state.asks.values().cloned().collect())
    );
    assert_eq!(c.read(Verb::Asks { kinds: vec![AskKind::Ask], states: vec![] }), Answer::Asks(vec![]));
    assert_eq!(c.read(Verb::Machines(NoArgs {})), Answer::Machines(state.machines.values().cloned().collect()));
    assert_eq!(
        c.read(Verb::Events { from: EventId(2), limit: Some(3) }),
        Answer::Events { events: store.events_after(EventId(2), 3).unwrap(), head: at }
    );
    assert_eq!(
        c.read(Verb::Events { from: EventId(0), limit: None }),
        Answer::Events { events: store.events_after(EventId(0), 1000).unwrap(), head: at }
    );

    // Every command verb is unsupported, and writes nothing.
    for cmd in [
        Command::RowAdd { project: ProjectId("wsp".into()), title: "x".into(), fields: Default::default() },
        Command::Mirror { put: vec![Entity::Row(row("wsp-9", "nine"))], gone: vec![] },
    ] {
        match c.call(Call::Command(cmd)) {
            Outcome::Err(f) => assert_eq!(f.code, Code::Unsupported, "{f:?}"),
            other => panic!("a command was answered {other:?}"),
        }
    }
    assert_eq!(store.head().unwrap(), at, "an unsupported command writes nothing");
    // A verb nobody knows is unsupported; a known verb whose args do not fit, malformed.
    let unknown = Call::Unknown { verb: "row.teleport".into(), args: json!({}) };
    assert!(matches!(c.call(unknown), Outcome::Err(f) if f.code == Code::Unsupported));
    let bad_args = Call::Unknown { verb: "row".into(), args: json!({ "nope": 1 }) };
    assert!(matches!(c.call(bad_args), Outcome::Err(f) if f.code == Code::Malformed));

    // A malformed line is answered malformed and the connection stays up:
    // as refused when it has no id, as a reply when it has one.
    c.conn.send_line("this is not json\n").unwrap();
    assert!(matches!(c.conn.recv(WAIT).unwrap(), Some(Frame::Refused(Refused { code: Code::Malformed, .. }))));
    c.conn.send_line("{\"type\":\"request\",\"id\":77,\"v\":1,\"verb\":\"state\"}\n").unwrap();
    assert!(matches!(
        c.conn.recv(WAIT).unwrap(),
        Some(Frame::Reply(Reply { id: ReqId(77), outcome: Outcome::Err(f), .. })) if f.code == Code::Malformed
    ));
    assert_eq!(c.read(Verb::Machines(NoArgs {})), Answer::Machines(state.machines.values().cloned().collect()));

    // A request by another principal than the hello's is forbidden.
    let id = ReqId(500);
    let other = Request {
        v: PROTOCOL,
        id,
        key: None,
        by: Principal::Seat(SeatId("cpd-9".into())),
        call: Call::Verb(Verb::State(NoArgs {})),
    };
    c.conn.send(&Frame::Request(other)).unwrap();
    assert!(matches!(c.reply(id).outcome, Outcome::Err(f) if f.code == Code::Forbidden));
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

/// `wip` (wsp-695) over the wire: the store's `wip_snapshot` whole, bodies only
/// on rows not done and lists not finished, raised asks only, and `state` beside
/// it unchanged.
#[test]
fn wip_answers_bodies_only_on_open_rows_and_state_beside_it_is_unchanged() {
    let d = dir("wip");
    let store = mirror_store();
    let mut put = everything();
    let with_body = |mut r: Row| {
        r.fields.insert("body".into(), "the long prose".into());
        r.fields.insert("prose".into(), "more of it".into());
        r.fields.insert("head".into(), "a head".into());
        Entity::Row(r)
    };
    let mut parked = row("wsp-3", "parked");
    parked.state = RowState::Parked;
    let mut done = row("wsp-4", "done");
    done.state = RowState::Done;
    put.extend([with_body(parked), with_body(done)]);
    let mut list_fields = std::collections::BTreeMap::new();
    list_fields.insert("body".to_string(), "list prose".to_string());
    list_fields.insert("name".to_string(), "the list".to_string());
    put.push(Entity::List(List { workspace: Default::default(), id: ListId("s2".into()), state: ListState::Running, groups: vec![], fields: list_fields.clone() }));
    put.push(Entity::List(List { workspace: Default::default(), id: ListId("s3".into()), state: ListState::Finished, groups: vec![], fields: list_fields }));
    put.push(Entity::Ask(Ask {
        workspace: Default::default(),
        id: AskId("a-2".into()),
        kind: AskKind::Ask,
        from: ed(),
        to: None,
        about: None,
        text: "settled".into(),
        state: AskState::Answered,
        answer: Some("yes".into()),
        fields: Default::default(),
    }));
    apply(&store, put, vec![], "all");
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(10));
    let mut c = connect(&d);
    let (full, at) = store.snapshot().unwrap();

    let Answer::Wip { state, at: wip_at } = c.read(Verb::Wip(NoArgs {})) else { panic!("not a wip answer") };
    assert_eq!((state.clone(), wip_at), store.wip_snapshot().unwrap());
    assert_eq!(wip_at, at);
    assert_eq!(state.rows.len(), full.rows.len(), "every row is kept");
    let fields = |id: &str| state.rows[&RowId(id.into())].fields.clone();
    assert_eq!(fields("wsp-3").get("body").map(String::as_str), Some("the long prose"), "an open row is whole");
    assert_eq!(fields("wsp-3").get("prose").map(String::as_str), Some("more of it"));
    for body in WIP_BODIES {
        assert_eq!(fields("wsp-4").get(body), None, "a done row loses {body}");
        assert_eq!(state.lists[&ListId("s3".into())].fields.get(body), None, "a finished list loses {body}");
    }
    assert_eq!(state.lists[&ListId("s2".into())], full.lists[&ListId("s2".into())], "a running list is whole");
    assert_eq!(fields("wsp-4").get("head").map(String::as_str), Some("a head"), "a done row keeps its head");
    assert_eq!(state.lists[&ListId("s3".into())].fields.get("name").map(String::as_str), Some("the list"));
    assert_eq!(state.asks.keys().cloned().collect::<Vec<_>>(), vec![AskId("a-1".into())], "raised asks only");
    assert_eq!((state.seats.clone(), state.claims.clone()), (full.seats.clone(), full.claims.clone()));
    assert!(state.orders.is_empty() && state.machines.is_empty());

    // `state` is unchanged: the done row's body is there.
    assert_eq!(c.read(Verb::State(NoArgs {})), Answer::State { state: full.clone(), at });
    assert!(full.rows[&RowId("wsp-4".into())].fields.contains_key("body"));
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

/// What a daemon hands in for the view verbs, faked: fixed documents, a `show`
/// that knows one id, and a count of the calls (wsp-307).
struct FakeViews {
    calls: AtomicUsize,
}

impl wsp_server::Views for FakeViews {
    fn throttle(&self) -> std::result::Result<serde_json::Value, wsp_api::Failure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "mode": "auto", "load": [1.5, 1.25, 1.0], "builds": [] }))
    }
    fn spend(&self) -> std::result::Result<serde_json::Value, wsp_api::Failure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "currency": "USD", "rows": { "landed_30d": 2, "cost_per_landed_row": 1.75 } }))
    }
    fn show(&self, id: &str) -> std::result::Result<serde_json::Value, wsp_api::Failure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match id {
            "wsp-1" => Ok(json!({ "id": "wsp-1", "body": "the body\n" })),
            _ => Err(wsp_api::Failure { code: Code::NotFound, text: format!("no task {id}"), refusal: None }),
        }
    }
}

#[test]
fn the_view_verbs_answer_what_the_views_hand_in_whole_and_are_unsupported_without_them() {
    let d = dir("views");
    let views = Arc::new(FakeViews { calls: AtomicUsize::new(0) });
    let mut cfg = Config::new(&d);
    cfg.port = 0;
    cfg.views = Some(views.clone());
    let server = Server::start(Arc::new(mirror_store()), cfg).unwrap();
    let mut c = connect(&d);
    assert_eq!(
        c.read(Verb::Throttle(NoArgs {})),
        Answer::Throttle(json!({ "mode": "auto", "load": [1.5, 1.25, 1.0], "builds": [] }))
    );
    assert_eq!(
        c.read(Verb::Spend(NoArgs {})),
        Answer::Spend(json!({ "currency": "USD", "rows": { "landed_30d": 2, "cost_per_landed_row": 1.75 } }))
    );
    assert_eq!(
        c.read(Verb::Show { id: "wsp-1".into() }),
        Answer::Show(json!({ "id": "wsp-1", "body": "the body\n" }))
    );
    match c.call(Call::Verb(Verb::Show { id: "wsp-404".into() })) {
        Outcome::Err(f) => assert_eq!((f.code, f.text.as_str()), (Code::NotFound, "no task wsp-404")),
        other => panic!("a show of no row was answered {other:?}"),
    }
    // `show` needs its id: without one it is malformed, and no view is asked.
    let asked = views.calls.load(Ordering::SeqCst);
    assert!(matches!(c.call(Call::Unknown { verb: "show".into(), args: json!({}) }), Outcome::Err(f) if f.code == Code::Malformed));
    // A request by another principal than the hello's is forbidden, as any read's is.
    let id = ReqId(600);
    let other = Request { v: PROTOCOL, id, key: None, by: Principal::Seat(SeatId("cpd-9".into())), call: Call::Verb(Verb::Spend(NoArgs {})) };
    c.conn.send(&Frame::Request(other)).unwrap();
    assert!(matches!(c.reply(id).outcome, Outcome::Err(f) if f.code == Code::Forbidden));
    assert_eq!(views.calls.load(Ordering::SeqCst), asked, "neither a malformed nor a forbidden request reaches the views");
    server.stop();
    let _ = fs::remove_dir_all(&d);

    // A server started with no views answers each one unsupported.
    let d = dir("no-views");
    let server = start(Arc::new(mirror_store()), &d, Duration::from_secs(10));
    let mut c = connect(&d);
    for verb in [Verb::Throttle(NoArgs {}), Verb::Spend(NoArgs {}), Verb::Show { id: "wsp-1".into() }] {
        match c.call(Call::Verb(verb)) {
            Outcome::Err(f) => assert_eq!(f.code, Code::Unsupported, "{f:?}"),
            other => panic!("a view verb with no views was answered {other:?}"),
        }
    }
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- subscribe

#[test]
fn subscribe_sends_the_snapshot_then_every_event_after_it_and_two_subscribers_agree() {
    let d = dir("snapshot");
    let store = mirror_store();
    rows(&store, 0, 5);
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(2));
    let mut a = connect(&d);
    let mut b = connect(&d);
    a.subscribe(None);
    let (at, state) = match a.next_feed() {
        Feed::State { state, at } => (at, state),
        other => panic!("expected the snapshot first, got {other:?}"),
    };
    assert_eq!((state.clone(), at), store.snapshot().unwrap());
    assert_eq!(at, EventId(5));
    b.subscribe(None);
    assert!(matches!(b.next_feed(), Feed::State { at: EventId(5), .. }));

    // Writers on two handles, events of every kind the mirror store writes.
    let w1 = store.handle();
    let w2 = store.handle();
    thread::scope(|sc| {
        sc.spawn(|| rows(&w1, 100, 30));
        sc.spawn(|| {
            for i in 0..10 {
                let name = LeaseName(format!("lease-{i}"));
                let t = w2.take(&name, &ed(), Duration::from_secs(60)).unwrap().unwrap();
                Leases::release(&w2, &name, t).unwrap();
                apply(&w2, vec![], vec![Subject::Row(RowId(format!("r-{}", i % 5)))], &format!("gone-{i}"));
            }
        });
    });
    let head = store.head().unwrap();
    let want = store.events_after(at, usize::MAX).unwrap();
    assert_eq!(want.last().map(|e| e.id), Some(head));

    let got_a = a.events_until(head);
    let got_b = b.events_until(head);
    assert_eq!(got_a, want, "subscriber a got every event after the snapshot, in commit order");
    assert_eq!(got_b, got_a, "two subscribers see the same order");
    let ids: Vec<u64> = got_a.iter().map(|e| e.id.0).collect();
    assert_eq!(ids, (at.0 + 1..=head.0).collect::<Vec<_>>(), "no gap");
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn subscribe_from_n_gets_exactly_the_events_after_n_also_across_a_restart() {
    let d = dir("from");
    let store = mirror_store();
    rows(&store, 0, 10);
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(2));
    let token_before = token(&d);
    let mut c = connect(&d);
    c.subscribe(Some(EventId(4)));
    let got = c.events_until(EventId(10));
    assert_eq!(got, store.events_after(EventId(4), usize::MAX).unwrap());
    assert_eq!(got.first().map(|e| e.id), Some(EventId(5)));

    rows(&store, 10, 2);
    let got = c.events_until(EventId(12));
    assert_eq!(got.iter().map(|e| e.id.0).collect::<Vec<_>>(), [11, 12]);
    let last = got.last().unwrap().id;

    // The server stops; the client sees its connection close.
    server.stop();
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the connection outlived its server");
        match c.conn.recv(Duration::from_millis(200)) {
            Err(_) => break,
            Ok(Some(Frame::Feed(Feed::Beat { .. }))) | Ok(None) => {}
            Ok(other) => panic!("after the last event, got {other:?}"),
        }
    }
    rows(&store, 12, 3);

    // A new server on the same store and state dir; the client finds it by api.json.
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(2));
    assert_eq!(token(&d), token_before, "a restart keeps the token");
    let mut c = connect(&d);
    c.subscribe(Some(last));
    let got = c.events_until(EventId(15));
    assert_eq!(got, store.events_after(last, usize::MAX).unwrap());
    assert_eq!(got.iter().map(|e| e.id.0).collect::<Vec<_>>(), [13, 14, 15]);
    rows(&store, 15, 1);
    assert_eq!(c.events_until(EventId(16)).len(), 1);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn subscribe_from_before_retention_is_answered_too_old() {
    let d = dir("too-old");
    let store = mirror_store();
    rows(&store, 0, 6);
    let kept = Retained { inner: store.handle(), oldest: EventId(3) };
    let server = start(Arc::new(kept), &d, Duration::from_secs(2));
    let mut c = connect(&d);
    match c.call(Call::Verb(Verb::Subscribe(Subscribe { from: Some(EventId(1)) }))) {
        Outcome::Err(f) => {
            assert_eq!(f.code, Code::TooOld);
            assert_eq!(f.refusal, Some(Refusal::TooOld { oldest: EventId(3) }));
        }
        other => panic!("expected too_old, got {other:?}"),
    }
    // The connection stays, and a subscribe from what is kept is answered.
    c.subscribe(Some(EventId(3)));
    assert_eq!(c.events_until(EventId(6)).len(), 3);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_client_silent_for_three_beats_is_dropped_and_one_that_talks_is_kept() {
    let d = dir("silent");
    let store = mirror_store();
    // Wide enough that the talker, asking once a beat, stays inside three
    // beats with the test thread starved for a second by a loaded machine.
    let beat = Duration::from_millis(500);
    let server = start(Arc::new(store.handle()), &d, beat);
    let mut quiet = connect(&d);
    let mut talker = connect(&d);
    quiet.subscribe(None);
    talker.subscribe(None);
    let began = Instant::now();

    let mut beats = 0;
    let dropped = loop {
        assert!(began.elapsed() < WAIT, "a silent client was not dropped");
        match quiet.conn.recv(beat) {
            Ok(Some(Frame::Feed(Feed::Beat { beat: head, .. }))) => {
                assert_eq!(head, store.head().unwrap());
                beats += 1;
            }
            Ok(_) => {}
            Err(_) => break began.elapsed(),
        }
        // The talker asks something cheap every half beat, so it is never silent.
        let id = talker.send(Call::Verb(Verb::Events { from: EventId(0), limit: Some(0) }));
        talker.reply(id);
    };
    assert!(dropped >= beat * 3 - Duration::from_millis(50), "dropped after {dropped:?}, before three beats");
    assert!(beats >= 2, "the silent client had beats while it waited ({beats})");

    // The talker outlives two more silences' worth.
    let until = Instant::now() + beat * 4;
    while Instant::now() < until {
        let id = talker.send(Call::Verb(Verb::Events { from: EventId(0), limit: Some(0) }));
        talker.reply(id);
        thread::sleep(beat / 2);
    }
    assert!(talker.feed.iter().any(|f| matches!(f, Feed::Beat { .. })), "the talker got beats too");
    assert_eq!(server.clients(), 1, "only the talker is left");
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- start

#[test]
fn the_start_is_refused_while_another_server_answers_its_port_and_names_it() {
    let d = dir("answered");
    let store = mirror_store();
    let first = start(Arc::new(store.handle()), &d, Duration::from_secs(10));
    let port = first.addrs()[0].port();

    let mut cfg = Config::new(&d);
    cfg.port = port;
    match Server::start(Arc::new(store.handle()), cfg) {
        Err(StartError::Answered { who, addr }) => {
            assert_eq!(addr.port(), port);
            assert!(who.contains("a wsp server with this token"), "{who}");
        }
        Err(e) => panic!("expected answered, got {e}"),
        Ok(_) => panic!("a second server started on a port that answers"),
    }
    let other = dir("answered-other");
    let mut cfg = Config::new(&other);
    cfg.port = port;
    match Server::start(Arc::new(store.handle()), cfg) {
        Err(StartError::Answered { who, .. }) => assert!(who.contains("another token"), "{who}"),
        Err(e) => panic!("expected answered, got {e}"),
        Ok(_) => panic!("a second server started on a port that answers"),
    }
    assert!(fs::read_to_string(first.log_path()).unwrap().contains("refused to start"));
    first.stop();
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&other);
}

// ---------------------------------------------------------------- the mirror

#[test]
fn the_mirror_writes_nothing_for_an_unchanged_entity_a_put_for_a_changed_one_and_a_gone_for_a_removed_one() {
    let store = mirror_store();
    let first = apply(&store, everything(), vec![], "m-1");
    assert_eq!(first.events.len(), everything().len());
    assert_eq!(apply(&store, everything(), vec![], "m-2"), Applied { events: vec![] }, "a re-mirror is silent");

    let mut changed = everything();
    changed[0] = Entity::Row(row("wsp-1", "ONE"));
    let applied = apply(&store, changed, vec![], "m-3");
    assert_eq!(applied.events.len(), 1);
    let e = &store.events_after(EventId(applied.events[0].0 - 1), 1).unwrap()[0];
    assert_eq!((e.kind, &e.subject), (EventKind::MirrorPut, &Subject::Row(RowId("wsp-1".into()))));
    assert_eq!(store.row(&RowId("wsp-1".into())).unwrap().unwrap().title, "ONE");

    let applied = apply(&store, vec![], vec![Subject::Seat(SeatId("cpd-2".into()))], "m-4");
    assert_eq!(applied.events.len(), 1);
    let e = &store.events_after(EventId(applied.events[0].0 - 1), 1).unwrap()[0];
    assert_eq!((e.kind, &e.subject), (EventKind::MirrorGone, &Subject::Seat(SeatId("cpd-2".into()))));
    assert_eq!(store.seat(&SeatId("cpd-2".into())).unwrap(), None);
    let again = apply(&store, vec![], vec![Subject::Seat(SeatId("cpd-2".into()))], "m-5");
    assert!(again.events.is_empty(), "a subject already gone is not gone again");

    let refused = store.apply(Command::RowPark { row: RowId("wsp-1".into()) }, IdempotencyKey("m-6".into()), ed());
    assert!(matches!(refused, Err(Error::Refused(Refusal::Illegal { .. }))), "{refused:?}");
}

/// 2.3 case 6 with `Mirror` as the domain: the store's snapshot is, at every
/// moment, the fold of `Mirror::evolve` over the log up to its id, while two
/// writers mirror, take and release leases and claims, and set texts.
#[test]
fn the_mirrors_fold_equals_the_snapshot() {
    let w = mirror_store();
    let other = w.handle();
    let r = w.handle();
    let running = AtomicUsize::new(2);
    let check = |s: &MemStore| -> EventId {
        let (state, at) = s.snapshot().unwrap();
        let log = s.events_after(EventId(0), usize::MAX).unwrap();
        let folded = log.iter().filter(|e| e.id <= at).fold(State::default(), |st, e| Mirror.evolve(st, e));
        assert_eq!(folded, state, "the snapshot at {} is the fold of the log up to it", at.0);
        at
    };
    thread::scope(|sc| {
        sc.spawn(|| {
            for i in 0..12 {
                let id = format!("snap-{i}");
                apply(&w, vec![Entity::Row(row(&id, "a")), Entity::Seat(seat(&id, "mac"))], vec![], &format!("a-{i}"));
                apply(&w, vec![Entity::Row(row(&id, "a"))], vec![], &format!("same-{i}"));
                apply(&w, vec![Entity::Row(row(&id, "b"))], vec![], &format!("b-{i}"));
                let rid = RowId(id.clone());
                let t = Claims::claim(&w, &rid, &SeatId(id.clone()), Duration::from_secs(60), None).unwrap();
                Claims::release(&w, &rid, t).unwrap();
                let lease = LeaseName(format!("lease-{i}"));
                let t = w.take(&lease, &Principal::Wsp("snap".into()), Duration::from_secs(60)).unwrap().unwrap();
                assert!(w.renew(&lease, t, Duration::from_secs(60)).unwrap());
                Leases::release(&w, &lease, t).unwrap();
                let (_, rev) = w.text(&rid, "notes").unwrap();
                w.set_text(&rid, "notes", format!("note {i}"), rev).unwrap();
                apply(&w, vec![], vec![Subject::Seat(SeatId(id))], &format!("gone-{i}"));
            }
            running.fetch_sub(1, Ordering::SeqCst);
        });
        sc.spawn(|| {
            rows(&other, 1000, 60);
            running.fetch_sub(1, Ordering::SeqCst);
        });
        sc.spawn(|| loop {
            let last = running.load(Ordering::SeqCst) == 0;
            check(&r);
            if last {
                return;
            }
        });
    });
    let at = check(&w);
    assert_eq!(at, w.head().unwrap());
    let state = w.snapshot().unwrap().0;
    assert_eq!(state.rows.len(), 72);
    assert!(state.seats.is_empty() && state.claims.is_empty() && state.leases.is_empty());
}

// ---------------------------------------------------------------- a store with retention

/// `MemStore` with retention: `events_after` refuses `too_old` before `oldest`.
struct Retained {
    inner: MemStore,
    oldest: EventId,
}

impl Reads for Retained {
    fn row(&self, id: &RowId) -> Result<Option<Row>> {
        self.inner.row(id)
    }
    fn rows(&self, f: &RowFilter) -> Result<Vec<Row>> {
        self.inner.rows(f)
    }
    fn seat(&self, id: &SeatId) -> Result<Option<Seat>> {
        self.inner.seat(id)
    }
    fn seats(&self, f: &SeatFilter) -> Result<Vec<Seat>> {
        self.inner.seats(f)
    }
    fn list(&self, id: &ListId) -> Result<Option<List>> {
        self.inner.list(id)
    }
    fn orders_for(&self, seat: &SeatId, states: &[OrderState]) -> Result<Vec<Order>> {
        self.inner.orders_for(seat, states)
    }
    fn claim(&self, row: &RowId) -> Result<Option<Claim>> {
        Reads::claim(&self.inner, row)
    }
    fn lease(&self, name: &LeaseName) -> Result<Option<Lease>> {
        self.inner.lease(name)
    }
    fn machine(&self, name: &MachineName) -> Result<Option<Machine>> {
        self.inner.machine(name)
    }
    fn snapshot(&self) -> Result<(State, EventId)> {
        self.inner.snapshot()
    }
    fn wip_snapshot(&self) -> Result<(State, EventId)> {
        self.inner.wip_snapshot()
    }
}

impl Events for Retained {
    fn events_after(&self, after: EventId, limit: usize) -> Result<Vec<Event>> {
        if after < self.oldest {
            return Err(Error::Refused(Refusal::TooOld { oldest: self.oldest }));
        }
        self.inner.events_after(after, limit)
    }
    fn head(&self) -> Result<EventId> {
        self.inner.head()
    }
    fn wait_for(&self, after: EventId, timeout: Duration) -> Result<EventId> {
        self.inner.wait_for(after, timeout)
    }
}

impl Commands for Retained {
    fn apply(&self, cmd: Command, key: IdempotencyKey, by: Principal) -> Result<Applied> {
        self.inner.apply(cmd, key, by)
    }
}

impl Leases for Retained {
    fn take(&self, name: &LeaseName, holder: &Principal, ttl: Duration) -> Result<Option<Token>> {
        self.inner.take(name, holder, ttl)
    }
    fn renew(&self, name: &LeaseName, token: Token, ttl: Duration) -> Result<bool> {
        self.inner.renew(name, token, ttl)
    }
    fn release(&self, name: &LeaseName, token: Token) -> Result<()> {
        Leases::release(&self.inner, name, token)
    }
}

impl Claims for Retained {
    fn claim(&self, row: &RowId, seat: &SeatId, ttl: Duration, fence: Option<&Fence>) -> Result<Token> {
        Claims::claim(&self.inner, row, seat, ttl, fence)
    }
    fn release(&self, row: &RowId, token: Token) -> Result<()> {
        Claims::release(&self.inner, row, token)
    }
}

impl Readings for Retained {}
impl Texts for Retained {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)> {
        self.inner.text(row, section)
    }
    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev> {
        self.inner.set_text(row, section, text, expect)
    }
}

// ---------------------------------------------------------------- step 2: the orders machine

fn composed_store(mode: Mode) -> MemStore {
    MemStore::with_domain(Arc::new(Composed::new(mode)))
}

fn send_to(seat: &str) -> Command {
    Command::OrderSend { to: SeatId(seat.into()), text: "look at wsp-121".into(), first: false }
}

/// The failure a reply carries, or a panic naming what it carried instead.
fn refused(r: Reply) -> wsp_api::Failure {
    match r.outcome {
        Outcome::Err(f) => {
            assert!(r.events.is_empty(), "a refusal writes nothing: {:?}", r.events);
            f
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn under_owned_an_order_send_is_applied_once_per_key_and_its_order_queued_reaches_a_subscriber() {
    let d = dir("owned-send");
    let store = composed_store(Mode::Owned);
    apply(&store, vec![Entity::Seat(seat("cpd-1", "mac"))], vec![], "seats");
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(10));
    let mut watcher = connect(&d);
    watcher.subscribe(None);
    assert!(matches!(watcher.next_feed(), Feed::State { .. }), "a subscription opens with the snapshot");

    let mut c = connect(&d);
    let first = c.command(send_to("cpd-1"), Some("send-1"));
    assert_eq!(first.outcome, Outcome::Ok(Answer::Applied), "{first:?}");
    assert_eq!(first.events.len(), 1, "order.send writes one event");
    let queued = first.events[0];
    assert_eq!(store.head().unwrap(), queued);

    let again = c.command(send_to("cpd-1"), Some("send-1"));
    assert_eq!(again.outcome, Outcome::Ok(Answer::Applied));
    assert_eq!(store.head().unwrap(), queued, "the same key sent twice applies once");
    let (state, _) = store.snapshot().unwrap();
    assert_eq!(state.orders.len(), 1, "one order: {:?}", state.orders);
    let order = state.orders.values().next().unwrap();
    assert_eq!((order.state, &order.to, &order.from), (OrderState::Queued, &SeatId("cpd-1".into()), &ed()));

    let seen = watcher.events_until(queued);
    let e = seen.last().unwrap();
    assert_eq!((e.id, e.kind, &e.subject), (queued, EventKind::OrderQueued, &Subject::Order(order.id.clone())));
    assert_eq!(e.principal, ed(), "the event is the sender's");

    // A seat sends and cancels too, each under its key; a command with no key is invalid.
    let mut s = connect_as(&d, Principal::Seat(SeatId("cpd-1".into())));
    assert_eq!(s.command(Command::OrderCancel { order: order.id.clone() }, Some("cancel-1")).outcome, Outcome::Ok(Answer::Applied));
    assert_eq!(store.snapshot().unwrap().0.orders[&order.id].state, OrderState::Cancelled);
    assert_eq!(refused(s.command(send_to("cpd-1"), None)).code, Code::Invalid);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn under_owned_a_seats_answer_is_unauthorised_and_a_mirror_putting_an_order_is_illegal_owned() {
    let d = dir("owned-refusals");
    let store = composed_store(Mode::Owned);
    apply(&store, vec![Entity::Seat(seat("cpd-1", "mac"))], vec![], "seats");
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(10));
    let mut ed_c = connect(&d);
    let sent = ed_c.command(send_to("cpd-1"), Some("send-1"));
    assert_eq!(sent.outcome, Outcome::Ok(Answer::Applied));
    let order = store.snapshot().unwrap().0.orders.into_keys().next().unwrap();
    let head = store.head().unwrap();

    let acked = Command::OrderAcked { order: order.clone(), turn_began: At(1) };
    let mut seat_c = connect_as(&d, Principal::Seat(SeatId("cpd-1".into())));
    assert_eq!(refused(seat_c.command(acked.clone(), Some("ack-1"))).code, Code::Unauthorised);
    assert_eq!(refused(ed_c.command(acked.clone(), Some("ack-2"))).code, Code::Unauthorised, "a person answers nothing");
    assert_eq!(store.head().unwrap(), head, "an unauthorised answer writes nothing");

    // The instance itself reaches the machine, which decides: acked on a queued order is illegal.
    let mut me = connect_as(&d, Principal::Wsp("wsp".into()));
    let f = refused(me.command(acked, Some("ack-3")));
    assert_eq!(f.code, Code::Illegal, "{f:?}");
    assert!(matches!(f.refusal, Some(Refusal::Illegal { ref state, .. }) if state == "queued"), "{f:?}");

    // A mirror that puts an order is refused, naming it; over the API it stays unsupported.
    let mirrored = Order {
        workspace: Default::default(),
        id: OrderId("cpd-1-first-1".into()),
        from: ed(),
        to: SeatId("cpd-1".into()),
        text: "go".into(),
        state: OrderState::Queued,
        attempts: 0,
        first: true,
        why: None,
    };
    let put = Command::Mirror { put: vec![Entity::Order(mirrored.clone())], gone: vec![] };
    assert_eq!(
        store.apply(put.clone(), IdempotencyKey("mirror-1".into()), ed()),
        Err(Error::Refused(Refusal::Illegal {
            subject: Subject::Order(mirrored.id.clone()),
            state: OWNED.into(),
            verb: "mirror".into()
        }))
    );
    assert_eq!(refused(ed_c.command(put, Some("mirror-2"))).code, Code::Unsupported);
    assert_eq!(store.head().unwrap(), head);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn under_mirrored_an_order_send_is_illegal_mirrored_and_the_mirror_puts_orders() {
    let d = dir("mirrored-send");
    let store = composed_store(Mode::Mirrored);
    apply(&store, everything(), vec![], "all");
    let head = store.head().unwrap();
    let server = start(Arc::new(store.handle()), &d, Duration::from_secs(10));
    let mut c = connect(&d);
    let f = refused(c.command(send_to("cpd-1"), Some("send-1")));
    assert_eq!(f.code, Code::Illegal, "{f:?}");
    assert_eq!(
        f.refusal,
        Some(Refusal::Illegal {
            subject: Subject::Seat(SeatId("cpd-1".into())),
            state: wsp_server::mirror::MIRRORED.into(),
            verb: "order.send".into()
        })
    );
    assert_eq!(store.head().unwrap(), head, "a refused order.send writes nothing");
    assert!(store.snapshot().unwrap().0.orders.contains_key(&OrderId("o-1".into())), "the mirror put its order");
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ------------------------------------------------------------ TLS (wsp-461)

/// TLS is the hub's: the daemon's own server sets none, and a server that has it admits and binds
/// exactly what it did without, unless it is handed ranges, which only the hub does.
#[test]
fn tls_leaves_the_daemons_peer_rule_as_it_was_and_a_public_peer_is_still_refused() {
    use std::net::IpAddr;
    let d = dir("tls-rule");
    let pki = wsp_server::testing::TestPki::new(&d, "t");
    let plain = Config::new(&d);
    assert!(plain.tls.is_none(), "the daemon's server is plain TCP");
    let mut with_tls = Config::new(&d);
    with_tls.tls = Some(wsp_server::tls::server_config(&pki.cert, &pki.key).unwrap());

    let public: [IpAddr; 4] = [[8, 8, 8, 8].into(), [10, 0, 0, 1].into(), [192, 168, 1, 2].into(), [0, 0, 0, 0].into()];
    let own: [IpAddr; 4] = [[127, 0, 0, 1].into(), [100, 64, 0, 1].into(), "::1".parse().unwrap(), "fd7a:115c:a1e0::1".parse().unwrap()];
    for cfg in [&plain, &with_tls] {
        for ip in &public {
            assert!(!cfg.admits(ip), "{ip} is a public peer: refused with or without TLS");
        }
        for ip in &own {
            assert!(cfg.admits(ip), "{ip} is loopback or the tailnet: admitted");
        }
    }
    // And a server that has TLS still will not bind an address the rule refuses.
    let mut cfg = with_tls;
    cfg.port = 0;
    cfg.addrs = vec![public[0]];
    let store: SharedStore = Arc::new(mirror_store().handle());
    assert!(matches!(Server::start(store, cfg), Err(StartError::Address(_))));
    let _ = fs::remove_dir_all(&d);
}

// ------------------------------------------------- a revoked token's connections (wsp-162)

/// A gate of the hub's shape over a store with the identity machine: a token is the active
/// credential whose hash it has, of its user, and names `person:<user>`.
struct Tokens(SharedStore);

impl wsp_server::Gate for Tokens {
    fn hello(&self, hello: &Hello, _: &ApiToken) -> std::result::Result<wsp_server::Welcomed, String> {
        let (state, _) = self.0.snapshot().map_err(|e| format!("{e:?}"))?;
        let hash = wsp_machines::token::hash(&hello.token.0);
        state
            .credentials
            .values()
            .find(|c| c.hash == hash && c.state == IdentityState::Active)
            .map(|c| wsp_server::Welcomed { by: Principal::Person(c.user.0.clone()), credential: Some(c.id.0.clone()) })
            .ok_or_else(|| "the token is none this hub has issued, or it is revoked".to_string())
    }
    fn admits(&self, _: &Principal, _: &Call) -> std::result::Result<(), wsp_api::Failure> {
        Ok(())
    }
    fn owns(&self, cmd: &Command) -> bool {
        wsp_machines::Identity::owns(cmd)
    }
}

fn identity_store() -> Arc<MemStore> {
    Arc::new(MemStore::with_domain(Arc::new(wsp_machines::Identity)))
}

fn apply_as(store: &MemStore, by: &str, key: &str, cmd: Command) {
    let by = if by == "wsp" { Principal::Wsp("wsp".into()) } else { Principal::Person(by.into()) };
    store.apply(cmd, IdempotencyKey(key.into()), by).unwrap();
}

/// `root` the admin, and each of `users` with a token `<user>-token` whose record is `<user>-1`.
fn seed_users(store: &MemStore, users: &[&str]) {
    apply_as(store, "wsp", "k-root", Command::HubUserAdd { name: UserName("root".into()), admin: true });
    for u in users {
        apply_as(store, "root", &format!("k-{u}"), Command::HubUserAdd { name: UserName((*u).into()), admin: false });
        apply_as(
            store,
            "root",
            &format!("k-{u}-t"),
            Command::HubTokenIssue {
                id: TokenId(format!("{u}-1")),
                user: UserName((*u).into()),
                hash: wsp_machines::token::hash(&format!("{u}-token")),
            },
        );
    }
}

fn start_gated(store: SharedStore, dir: &Path, beat: Duration) -> Server {
    let mut cfg = Config::new(dir);
    cfg.port = 0;
    cfg.beat = beat;
    cfg.gate = Some(Arc::new(Tokens(store.clone())));
    Server::start(store, cfg).unwrap()
}

/// A connection welcomed on `token`; its first frame after the hello is the welcome.
fn dial_token(dir: &Path, token: &str, user: &str) -> std::result::Result<Client, Refused> {
    let by = Principal::Person(user.into());
    let mut conn = Tcp::dial(addr(dir), WAIT).unwrap();
    let hello = Frame::Hello(Hello { token: ApiToken(token.into()), by: by.clone(), client: "wsp-server tests".into(), protocol: PROTOCOL });
    conn.send(&hello).unwrap();
    match conn.recv(WAIT).unwrap() {
        Some(Frame::Welcome(_)) => Ok(Client { conn, by, next: 1, feed: vec![] }),
        Some(Frame::Refused(r)) => Err(r),
        other => panic!("expected welcome or refused, got {other:?}"),
    }
}

/// Frames until the connection ends, the last `refused` seen among them; it must end within `within`.
fn until_closed(c: &mut Client, within: Duration) -> Option<Refused> {
    let deadline = Instant::now() + within;
    let mut refused = None;
    loop {
        assert!(Instant::now() < deadline, "the connection was not closed within {within:?}");
        match c.conn.recv(Duration::from_millis(50)) {
            Ok(Some(Frame::Refused(r))) => refused = Some(r),
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => return refused,
        }
    }
}

#[test]
fn a_revoked_tokens_open_connections_are_closed_within_a_beat_and_another_engineers_stay_up() {
    let d = dir("revoke");
    let store = identity_store();
    seed_users(&store, &["ann", "bob"]);
    let beat = Duration::from_millis(1500);
    let server = start_gated(store.clone(), &d, beat);

    let mut ann_reader = dial_token(&d, "ann-token", "ann").unwrap();
    let mut ann_feed = dial_token(&d, "ann-token", "ann").unwrap();
    ann_feed.subscribe(None);
    let mut bob_reader = dial_token(&d, "bob-token", "bob").unwrap();
    let mut bob_feed = dial_token(&d, "bob-token", "bob").unwrap();
    bob_feed.subscribe(None);
    assert!(matches!(ann_reader.read(Verb::Events { from: EventId(0), limit: Some(0) }), Answer::Events { .. }));
    while server.clients() < 4 {
        thread::sleep(Duration::from_millis(5));
    }

    let began = Instant::now();
    apply_as(&store, "root", "k-revoke", Command::HubTokenRevoke { id: TokenId("ann-1".into()) });

    let refused = until_closed(&mut ann_feed, beat).expect("the subscriber was told before the close");
    assert_eq!(refused.code, Code::Unauthorised, "{refused:?}");
    assert!(until_closed(&mut ann_reader, beat).is_none(), "the reader is closed, with no frame of its own");
    assert!(began.elapsed() < beat, "both closed within one beat: {:?}", began.elapsed());

    let refused = dial_token(&d, "ann-token", "ann").err().expect("a fresh hello with the token is refused");
    assert_eq!(refused.code, Code::Unauthorised);

    // Bob's connections never noticed: his reader answers, his feed still beats.
    assert!(matches!(bob_reader.read(Verb::Events { from: EventId(0), limit: Some(0) }), Answer::Events { .. }));
    assert!(matches!(bob_feed.next_feed(), Feed::State { .. } | Feed::Event(_)));
    // The cut connections' threads end and log a moment after the close.
    let deadline = Instant::now() + WAIT;
    loop {
        let log = fs::read_to_string(server.log_path()).unwrap();
        if server.clients() == 2 && log.matches("dropped, its token was revoked").count() == 2 {
            break;
        }
        assert!(Instant::now() < deadline, "bob's two remain and ann's two are logged: {} clients, {log}", server.clients());
        thread::sleep(Duration::from_millis(10));
    }

    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_revoked_user_loses_every_connection_it_held() {
    let d = dir("revoke-user");
    let store = identity_store();
    seed_users(&store, &["ann", "bob"]);
    let server = start_gated(store.clone(), &d, Duration::from_millis(1500));
    let mut ann = dial_token(&d, "ann-token", "ann").unwrap();
    let mut bob = dial_token(&d, "bob-token", "bob").unwrap();
    apply_as(&store, "root", "k-revoke", Command::HubUserRevoke { name: UserName("ann".into()) });
    assert!(until_closed(&mut ann, Duration::from_millis(1500)).is_none());
    assert!(matches!(bob.read(Verb::Events { from: EventId(0), limit: Some(0) }), Answer::Events { .. }));
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- scoped subscribe (wsp-174)

/// The mirror domain, and `hub.member.set` folding into `state.members`, so
/// that a test moves a person's scope with an event.
struct Members;

impl Domain for Members {
    fn decide(&self, state: &State, cmd: &Command, by: &Principal) -> std::result::Result<Vec<Draft>, Refusal> {
        match cmd {
            Command::HubMemberSet { workspace, user, member } => std::result::Result::Ok(vec![Draft {
                kind: EventKind::HubMemberSet,
                subject: Subject::Member { workspace: workspace.clone(), user: user.clone() },
                workspace: Default::default(),
                payload: json!({ "member": member }),
            }]),
            other => Mirror.decide(state, other, by),
        }
    }

    fn evolve(&self, mut state: State, event: &Event) -> State {
        if let (EventKind::HubMemberSet, Subject::Member { workspace, user }) = (event.kind, &event.subject) {
            let members = state.members.entry(workspace.clone()).or_default();
            if event.payload["member"] == json!(true) {
                members.insert(user.clone());
            } else {
                members.remove(user);
            }
            return state;
        }
        Mirror.evolve(state, event)
    }
}

/// A gate that trusts the hello and reads a person's workspaces off `state.members`.
struct ByMembers;

impl wsp_server::Gate for ByMembers {
    fn hello(&self, hello: &Hello, _token: &ApiToken) -> std::result::Result<wsp_server::Welcomed, String> {
        Ok(wsp_server::Welcomed { by: hello.by.clone(), credential: None })
    }

    fn admits(&self, _by: &Principal, _call: &Call) -> std::result::Result<(), wsp_api::Failure> {
        Ok(())
    }

    fn owns(&self, _cmd: &Command) -> bool {
        false
    }

    fn readable(&self, by: &Principal, state: &State) -> Option<std::collections::BTreeSet<SharedName>> {
        let Principal::Person(user) = by else { return None };
        let user = UserName(user.clone());
        Some(state.members.iter().filter(|(_, who)| who.contains(&user)).map(|(w, _)| w.clone()).collect())
    }
}

fn shared(name: &str) -> WorkspaceId {
    WorkspaceId::Shared(SharedName(name.into()))
}

fn row_in(id: &str, workspace: WorkspaceId) -> Entity {
    Entity::Row(Row { workspace, ..row(id, "x") })
}

fn member(store: &MemStore, workspace: &str, user: &str, key: &str) {
    let cmd = Command::HubMemberSet { workspace: SharedName(workspace.into()), user: UserName(user.into()), member: true };
    store.apply(cmd, IdempotencyKey(key.into()), Principal::Person("admin".into())).unwrap();
}

fn start_scoped(store: SharedStore, dir: &Path) -> Server {
    let mut cfg = Config::new(dir);
    cfg.port = 0;
    cfg.beat = Duration::from_secs(10);
    cfg.gate = Some(Arc::new(ByMembers));
    Server::start(store, cfg).unwrap()
}

fn next_event(c: &mut Client) -> Event {
    match c.next_feed() {
        Feed::Event(e) => e,
        other => panic!("expected an event, got {other:?}"),
    }
}

#[test]
fn a_scoped_subscriber_is_sent_a_record_whole_when_it_moves_in_and_a_gone_when_it_moves_out() {
    let d = dir("scoped-moves");
    let store = MemStore::with_domain(Arc::new(Members));
    member(&store, "core", "ed", "m1");
    apply(&store, vec![row_in("held", shared("core")), row_in("hidden", WorkspaceId::Private)], vec![], "seed");
    let server = start_scoped(Arc::new(store.handle()), &d);
    let mut ed_c = connect(&d);
    ed_c.subscribe(None);
    match ed_c.next_feed() {
        Feed::State { state, at } => {
            assert_eq!(at, store.head().unwrap());
            assert_eq!(state.rows.keys().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["held"], "only what ed may read");
        }
        other => panic!("expected the snapshot, got {other:?}"),
    }

    // jesper adds a private row: ed's feed has nothing of it (the next frame is the marker after it).
    apply(&store, vec![row_in("mine", WorkspaceId::Private)], vec![], "private");
    let marker = apply(&store, vec![row_in("marker", shared("core"))], vec![], "marker");
    let e = next_event(&mut ed_c);
    assert_eq!((e.id, &e.subject), (marker.events[0], &Subject::Row(RowId("marker".into()))), "the private row is not sent");

    // It moves to a workspace ed reads: ed is sent the whole record.
    let moved = row_in("mine", shared("core"));
    let into = apply(&store, vec![moved.clone()], vec![], "in");
    let e = next_event(&mut ed_c);
    assert_eq!((e.id, e.kind, &e.subject), (into.events[0], EventKind::MirrorPut, &Subject::Row(RowId("mine".into()))));
    assert_eq!(serde_json::from_value::<Entity>(e.payload).unwrap(), moved, "the whole entity");

    // And back: ed is told it is gone, and then hears nothing of it.
    let out = apply(&store, vec![row_in("mine", WorkspaceId::Private)], vec![], "out");
    assert_eq!(ed_c.next_feed(), Feed::Gone { subject: Subject::Row(RowId("mine".into())), at: out.events[0] });
    apply(&store, vec![row_in("mine", WorkspaceId::Private)], vec![], "quiet");
    let after = apply(&store, vec![row_in("after", shared("core"))], vec![], "after");
    assert_eq!(next_event(&mut ed_c).id, after.events[0], "a private edit of a record that left is not sent");
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_member_added_mid_stream_is_sent_a_resync_and_then_that_workspaces_events() {
    let d = dir("scoped-resync");
    let store = MemStore::with_domain(Arc::new(Members));
    member(&store, "core", "ed", "m1");
    apply(
        &store,
        vec![row_in("c-1", shared("core")), row_in("o-1", shared("other")), row_in("p-1", WorkspaceId::Private)],
        vec![],
        "seed",
    );
    let server = start_scoped(Arc::new(store.handle()), &d);
    let mut ed_c = connect(&d);
    ed_c.subscribe(None);
    assert!(matches!(ed_c.next_feed(), Feed::State { .. }));
    let mut other = connect_as(&d, Principal::Person("zed".into()));
    other.subscribe(None);
    assert!(matches!(other.next_feed(), Feed::State { .. }));

    // An event of a workspace ed is not in is not sent; the one after it, of core, is.
    apply(&store, vec![row_in("o-2", shared("other"))], vec![], "o2");
    let c2 = apply(&store, vec![row_in("c-2", shared("core"))], vec![], "c2");
    assert_eq!(next_event(&mut ed_c).id, c2.events[0]);

    member(&store, "other", "ed", "m2");
    let head = store.head().unwrap();
    match ed_c.next_feed() {
        Feed::Resync { state, at } => {
            assert_eq!(at, head, "the fresh snapshot is at the head");
            let mut rows: Vec<&str> = state.rows.keys().map(|r| r.0.as_str()).collect();
            rows.sort();
            assert_eq!(rows, ["c-1", "c-2", "o-1", "o-2"], "both workspaces, and no private row");
        }
        other => panic!("expected a resync, got {other:?}"),
    }
    let o3 = apply(&store, vec![row_in("o-3", shared("other"))], vec![], "o3");
    assert_eq!(next_event(&mut ed_c).id, o3.events[0], "that workspace's events follow");

    // A subscriber who is no member of anything reads none of it.
    assert!(other.feed.iter().all(|f| matches!(f, Feed::Beat { .. })), "{:?}", other.feed);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

fn unmember(store: &MemStore, workspace: &str, user: &str, key: &str) {
    let cmd = Command::HubMemberSet { workspace: SharedName(workspace.into()), user: UserName(user.into()), member: false };
    store.apply(cmd, IdempotencyKey(key.into()), Principal::Person("admin".into())).unwrap();
}

/// What ed's resubscribe from `last` is answered with: the first frame and the rows in it.
fn resubscribe_rows(d: &Path, last: EventId, head: EventId) -> Vec<String> {
    let mut c = connect(d);
    c.subscribe(Some(last));
    match c.next_feed() {
        Feed::Resync { state, at } => {
            assert_eq!(at, head, "the resync is at the head");
            let mut rows: Vec<String> = state.rows.keys().map(|r| r.0.clone()).collect();
            rows.sort();
            rows
        }
        other => panic!("expected a resync, got {other:?}"),
    }
}

#[test]
fn a_scope_change_while_the_subscriber_is_away_is_a_resync_on_resubscribe_from_its_last_id() {
    let d = dir("scoped-redial");
    let store = MemStore::with_domain(Arc::new(Members));
    member(&store, "core", "ed", "m1");
    member(&store, "other", "ed", "m2");
    apply(
        &store,
        vec![row_in("c-1", shared("core")), row_in("o-1", shared("other")), row_in("p-1", WorkspaceId::Private)],
        vec![],
        "seed",
    );
    let server = start_scoped(Arc::new(store.handle()), &d);
    let mut last = store.head().unwrap();

    // Away, only events of core: the replay is the events, as for an unscoped subscriber.
    let c2 = apply(&store, vec![row_in("c-2", shared("core"))], vec![], "c2");
    let mut c = connect(&d);
    c.subscribe(Some(last));
    assert_eq!(next_event(&mut c).id, c2.events[0]);
    last = c2.events[0];
    drop(c);

    // Away, a held record moves out: a resync without it, not a bare event or silence.
    apply(&store, vec![row_in("c-2", WorkspaceId::Private)], vec![], "c2-out");
    assert_eq!(resubscribe_rows(&d, last, store.head().unwrap()), ["c-1", "o-1"]);
    last = store.head().unwrap();

    // Away, a record moves in and out again: the resync has neither the move nor the record.
    apply(&store, vec![row_in("p-1", shared("core"))], vec![], "p1-in");
    apply(&store, vec![row_in("p-1", WorkspaceId::Private)], vec![], "p1-out");
    assert_eq!(resubscribe_rows(&d, last, store.head().unwrap()), ["c-1", "o-1"]);
    last = store.head().unwrap();

    // Away, ed leaves other: the resync drops its records, which the client held.
    unmember(&store, "other", "ed", "m3");
    assert_eq!(resubscribe_rows(&d, last, store.head().unwrap()), ["c-1"]);
    last = store.head().unwrap();

    // Away, ed is added to other: the resync has its earlier records.
    member(&store, "other", "ed", "m4");
    assert_eq!(resubscribe_rows(&d, last, store.head().unwrap()), ["c-1", "o-1"]);

    // After a resync the feed is live and scoped as it was.
    let mut c = connect(&d);
    c.subscribe(Some(store.head().unwrap()));
    apply(&store, vec![row_in("x", WorkspaceId::Private)], vec![], "x");
    let o2 = apply(&store, vec![row_in("o-2", shared("other"))], vec![], "o2");
    assert_eq!(next_event(&mut c).id, o2.events[0]);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}
