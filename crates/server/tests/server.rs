//! `wsp-server` on an ephemeral loopback port over `MemStore` (wsp-073's done
//! list): `hello` and its token, each read verb, `wip`'s cut (wsp-695), commands `unsupported`,
//! malformed lines, `subscribe {}` and `subscribe {from}` (across a restart,
//! and `too_old`), the silent client dropped, two subscribers in one order,
//! the start refused while another server answers, and the mirror domain:
//! silent for an unchanged entity, and its fold equal to the snapshot (2.3
//! case 6, run with `Mirror` as the domain).

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
use wsp_server::{Config, Mirror, Server, SharedStore, StartError};
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

/// A welcomed client, and the frames it has read past while waiting for a reply.
struct Client {
    conn: Tcp,
    next: u64,
    feed: Vec<Feed>,
}

fn connect(dir: &Path) -> Client {
    let mut conn = Tcp::dial(addr(dir), WAIT).unwrap();
    conn.send(&hello(token(dir))).unwrap();
    match conn.recv(WAIT).unwrap() {
        Some(Frame::Welcome(w)) => assert_eq!((w.server.as_str(), w.protocol), ("wsp", PROTOCOL)),
        other => panic!("expected welcome, got {other:?}"),
    }
    Client { conn, next: 1, feed: vec![] }
}

impl Client {
    fn send(&mut self, call: Call) -> ReqId {
        let id = ReqId(self.next);
        self.next += 1;
        let req = Request { v: PROTOCOL, id, key: None, by: ed(), call };
        self.conn.send(&Frame::Request(req)).unwrap();
        id
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
            id: ListId("s1".into()),
            state: ListState::Running,
            groups: vec![],
            fields: Default::default(),
        }),
        Entity::Seat(seat("cpd-1", "mac")),
        Entity::Seat(seat("cpd-2", "pi")),
        Entity::Claim(Claim { row: RowId("wsp-1".into()), seat: SeatId("cpd-1".into()), token: Token(1), expires: At(9) }),
        Entity::Order(Order {
            id: OrderId("o-1".into()),
            from: ed(),
            to: SeatId("cpd-1".into()),
            text: "go".into(),
            state: OrderState::Queued,
            attempts: 0,
        }),
        Entity::Ask(Ask {
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
    assert_eq!(c.read(Verb::Lists(NoArgs {})), Answer::Lists(state.lists.values().cloned().collect()));
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
/// on rows not done, lists without theirs, raised asks only, and `state` beside
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
    put.push(Entity::List(List { id: ListId("s2".into()), state: ListState::Running, groups: vec![], fields: list_fields }));
    put.push(Entity::Ask(Ask {
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
        assert_eq!(state.lists[&ListId("s2".into())].fields.get(body), None, "a list loses {body}");
    }
    assert_eq!(fields("wsp-4").get("head").map(String::as_str), Some("a head"), "a done row keeps its head");
    assert_eq!(state.lists[&ListId("s2".into())].fields.get("name").map(String::as_str), Some("the list"));
    assert_eq!(state.asks.keys().cloned().collect::<Vec<_>>(), vec![AskId("a-1".into())], "raised asks only");
    assert_eq!((state.seats.clone(), state.claims.clone()), (full.seats.clone(), full.claims.clone()));
    assert!(state.orders.is_empty() && state.machines.is_empty());

    // `state` is unchanged: the done row's body is there.
    assert_eq!(c.read(Verb::State(NoArgs {})), Answer::State { state: full.clone(), at });
    assert!(full.rows[&RowId("wsp-4".into())].fields.contains_key("body"));
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

impl Texts for Retained {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)> {
        self.inner.text(row, section)
    }
    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev> {
        self.inner.set_text(row, section, text, expect)
    }
}
