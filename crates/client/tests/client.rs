//! `wsp-client` against a loopback `wsp-server` over `MemStore` (wsp-074's done
//! list): `dial` silent with no `api.json`, a wrong token and a stopped server,
//! each reason naming which; `dial_at` (wsp-306) welcomed at given addresses
//! with the right token, subscribing, and refused with a wrong one; `request` for each read verb; `subscribe` handing
//! out the snapshot and then each event once, across a server restart (a
//! `Resumed`, and no id twice) and through a `too_old` (a fresh `State`); and a
//! duplicate id dropped.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, to_value, Value};
use wsp_api::{Answer, ApiToken, Code, Listen, Outcome, Reply, LISTEN_FILE, MISSED_BEATS, TOKEN_FILE};
use wsp_client::{Client, Feed, Item, Silent};
use wsp_server::{Config, Mirror, Server, SharedStore};
use wsp_store_conformance::MemStore;
use wsp_store_contract::*;
use wsp_store_contract::Feed as Feed2;

/// The upper bound on every wait for an item, a reply or a state the feed comes
/// to: generous, so a machine under load is slow and not failed. Each wait ends
/// as soon as what it waits for is seen.
const WAIT: Duration = Duration::from_secs(120);
const BEAT: Duration = Duration::from_millis(150);

// ---------------------------------------------------------------- fixtures

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wsp-client-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

fn mirror_store() -> MemStore {
    MemStore::with_domain(Arc::new(Mirror))
}

fn start(store: SharedStore, dir: &Path) -> Server {
    let mut cfg = Config::new(dir);
    cfg.port = 0;
    cfg.beat = BEAT;
    Server::start(store, cfg).unwrap()
}

fn ed() -> Principal {
    Principal::Person("ed".into())
}

/// Polls the feed a beat at a time until `done` holds of it, and fails if an
/// item comes first or `WAIT` runs out.
fn quiet_until(f: &mut Feed, what: &str, done: impl Fn(&Feed) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done(f) {
        assert!(Instant::now() < deadline, "{what}: not within {WAIT:?}");
        assert_eq!(f.next_within(BEAT), None, "{what}: an item came first");
    }
}

fn dial(dir: &Path) -> Client {
    Client::dial(dir, ed(), WAIT).unwrap_or_else(|s| panic!("silent: {s}"))
}

fn feed(c: &Client, from: Option<EventId>) -> Feed {
    let mut f = c.subscribe(from);
    f.set_beat(BEAT);
    f
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

// ---------------------------------------------------------------- dial

#[test]
fn dial_is_silent_with_no_file_a_wrong_token_or_a_stopped_server_and_says_which() {
    let d = dir("silent");
    let wait = Duration::from_millis(800);

    // No api.json: nothing has started here.
    fs::create_dir_all(&d).unwrap();
    let t = Instant::now();
    match Client::dial(&d, ed(), wait) {
        Err(s @ Silent::NoListenFile(_)) => assert!(s.to_string().contains(LISTEN_FILE), "{s}"),
        other => panic!("expected NoListenFile, got {:?}", other.map(|_| ())),
    }
    assert!(t.elapsed() < wait);

    let store = mirror_store();
    let server = start(Arc::new(store.handle()), &d);

    // The right token is welcomed, and the head is the store's.
    let c = Client::dial(&d, ed(), wait).unwrap();
    assert_eq!((c.server(), c.head()), ("wsp", store.head().unwrap()));

    // A wrong token is refused unauthorised.
    let good = fs::read_to_string(d.join(TOKEN_FILE)).unwrap();
    fs::write(d.join(TOKEN_FILE), "not-the-token\n").unwrap();
    let t = Instant::now();
    match Client::dial(&d, ed(), wait) {
        Err(Silent::Refused(r)) => {
            assert_eq!(r.code, Code::Unauthorised);
            assert!(Silent::Refused(r).to_string().contains("Unauthorised"));
        }
        other => panic!("expected Refused, got {:?}", other.map(|_| ())),
    }
    assert!(t.elapsed() < wait);

    // No token file is its own reason.
    fs::remove_file(d.join(TOKEN_FILE)).unwrap();
    match Client::dial(&d, ed(), wait) {
        Err(s @ Silent::NoToken(_)) => assert!(s.to_string().contains(TOKEN_FILE), "{s}"),
        other => panic!("expected NoToken, got {:?}", other.map(|_| ())),
    }
    fs::write(d.join(TOKEN_FILE), good).unwrap();

    // A stopped server leaves api.json behind, and nothing answers it.
    server.stop();
    let t = Instant::now();
    match Client::dial(&d, ed(), wait) {
        Err(s @ Silent::Unreachable(_)) => assert!(s.to_string().contains("nothing answered"), "{s}"),
        other => panic!("expected Unreachable, got {:?}", other.map(|_| ())),
    }
    assert!(t.elapsed() < wait);

    // A file that is no Listen is named too.
    fs::write(d.join(LISTEN_FILE), "{").unwrap();
    assert!(matches!(Client::dial(&d, ed(), wait), Err(Silent::BadListenFile(_))));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn an_address_that_never_welcomes_is_silent_within_the_wait() {
    // A listener that accepts and says nothing.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let d = dir("mute");
    fs::create_dir_all(&d).unwrap();
    let listen = Listen { addrs: vec![l.local_addr().unwrap().to_string()] };
    fs::write(d.join(LISTEN_FILE), serde_json::to_string(&listen).unwrap()).unwrap();
    fs::write(d.join(TOKEN_FILE), "t").unwrap();
    let wait = Duration::from_millis(300);
    let t = Instant::now();
    assert!(matches!(Client::dial(&d, ed(), wait), Err(Silent::NoWelcome(_))));
    assert!(t.elapsed() < wait + Duration::from_millis(500), "{:?}", t.elapsed());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn dial_at_dials_given_addresses_with_a_given_token_and_a_wrong_one_is_refused() {
    let d = dir("at");
    let store = mirror_store();
    let server = start(Arc::new(store.handle()), &d);
    // What a caller on another machine holds: the addresses and the token
    // (fetched over ssh), and no state dir. api.json goes, so that nothing
    // here, the feed's own dial included, can be reading it.
    let listen: Listen = serde_json::from_str(&fs::read_to_string(d.join(LISTEN_FILE)).unwrap()).unwrap();
    let token = ApiToken(fs::read_to_string(d.join(TOKEN_FILE)).unwrap().trim().to_string());
    assert!(listen.addrs.iter().all(|a| !a.ends_with(":0")), "an ephemeral port: {:?}", listen.addrs);
    fs::remove_file(d.join(LISTEN_FILE)).unwrap();
    let wait = Duration::from_millis(800);

    // The right token is welcomed, and a feed from it subscribes.
    let c = Client::dial_at(listen.addrs.clone(), token.clone(), ed(), WAIT).unwrap_or_else(|s| panic!("silent: {s}"));
    assert_eq!((c.server(), c.head()), ("wsp", store.head().unwrap()));
    let mut f = feed(&c, None);
    let (state, at) = store.snapshot().unwrap();
    assert_eq!(f.next_within(WAIT), Some(Item::State { state, at }));
    rows(&store, 0, 2);
    let got = until(&mut f, at.0 + 2);
    assert_eq!(ids(&got), vec![at.0 + 1, at.0 + 2], "{got:?}");

    // A wrong token is refused unauthorised.
    let t = Instant::now();
    match Client::dial_at(listen.addrs.clone(), ApiToken("not-the-token".into()), ed(), wait) {
        Err(Silent::Refused(r)) => assert_eq!(r.code, Code::Unauthorised),
        other => panic!("expected Refused, got {:?}", other.map(|_| ())),
    }
    assert!(t.elapsed() < wait);

    // No address at all is said, not dialled.
    assert!(matches!(Client::dial_at(vec![], token, ed(), wait), Err(Silent::BadListenFile(_))));
    drop(f);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- request

fn ok(r: Reply) -> Answer {
    match r.outcome {
        Outcome::Ok(a) => a,
        Outcome::Err(f) => panic!("refused: {f:?}"),
    }
}

#[test]
fn request_gets_each_read_verbs_answer_matched_by_id() {
    let d = dir("reads");
    let store = mirror_store();
    apply(&store, everything(), vec![], "all");
    let server = start(Arc::new(store.handle()), &d);
    let mut c = dial(&d);
    let (state, at) = store.snapshot().unwrap();
    let wsp1 = RowId("wsp-1".into());
    let cpd1 = SeatId("cpd-1".into());
    let s1 = ListId("s1".into());
    let mut ask = |verb: &str, args: Value| ok(c.request(verb, args).unwrap());

    assert_eq!(ask("state", json!({})), Answer::State { state: state.clone(), at });
    assert_eq!(ask("row", json!({"row": wsp1})), Answer::Row(store.row(&wsp1).unwrap()));
    assert_eq!(ask("row", json!({"row": "nope"})), Answer::Row(None));
    assert_eq!(ask("rows", json!({})), Answer::Rows(store.rows(&RowFilter::default()).unwrap()));
    let parked = RowFilter { states: vec![RowState::Parked], ..Default::default() };
    assert_eq!(ask("rows", to_value(&parked).unwrap()), Answer::Rows(store.rows(&parked).unwrap()));
    assert_eq!(ask("list", json!({"list": s1})), Answer::List(store.list(&s1).unwrap()));
    assert_eq!(ask("lists", Value::Null), Answer::Lists(state.lists.values().cloned().collect()));
    assert_eq!(ask("seat", json!({"seat": cpd1})), Answer::Seat(store.seat(&cpd1).unwrap()));
    let on_pi = SeatFilter { machine: Some(MachineName("pi".into())), ..Default::default() };
    assert_eq!(ask("seats", to_value(&on_pi).unwrap()), Answer::Seats(store.seats(&on_pi).unwrap()));
    assert_eq!(ask("orders", json!({"seat": cpd1})), Answer::Orders(store.orders_for(&cpd1, &[]).unwrap()));
    assert_eq!(ask("asks", json!({})), Answer::Asks(state.asks.values().cloned().collect()));
    assert_eq!(ask("machines", json!({})), Answer::Machines(state.machines.values().cloned().collect()));
    assert_eq!(
        ask("events", json!({"from": 2, "limit": 3})),
        Answer::Events { events: store.events_after(EventId(2), 3).unwrap(), head: at }
    );

    // A refusal is a reply with `err`, and a verb the server does not run is unsupported.
    let r = c.request("row.land", json!({})).unwrap();
    assert!(matches!(r.outcome, Outcome::Err(_)), "{r:?}");
    let r = c.request("no.such.verb", json!({})).unwrap();
    match r.outcome {
        Outcome::Err(f) => assert_eq!(f.code, Code::Unsupported),
        other => panic!("{other:?}"),
    }
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn wip_reads_the_cut_typed_bodies_only_on_open_rows() {
    let d = dir("wip");
    let store = mirror_store();
    let body = |r: &mut Row| drop(r.fields.insert("body".into(), "long prose".into()));
    let mut open = row("wsp-1", "open");
    body(&mut open);
    let mut done = row("wsp-2", "done");
    done.state = RowState::Done;
    body(&mut done);
    done.fields.insert("head".into(), "the head".into());
    apply(&store, vec![Entity::Row(open), Entity::Row(done)], vec![], "rows");
    let server = start(Arc::new(store.handle()), &d);
    let mut c = dial(&d);

    let (state, at) = c.wip().unwrap();
    assert_eq!((state.clone(), at), store.wip_snapshot().unwrap());
    let get = |id: &str| state.rows[&RowId(id.into())].fields.clone();
    assert_eq!(get("wsp-1").get("body").map(String::as_str), Some("long prose"));
    assert_eq!(get("wsp-2").get("body"), None);
    assert_eq!(get("wsp-2").get("head").map(String::as_str), Some("the head"));
    // `state` is unchanged beside it: the done row keeps its body there.
    match ok(c.request("state", json!({})).unwrap()) {
        Answer::State { state, .. } => assert!(state.rows[&RowId("wsp-2".into())].fields.contains_key("body")),
        other => panic!("{other:?}"),
    }
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_request_after_a_lost_connection_dials_again() {
    let d = dir("redial");
    let store = mirror_store();
    let server = start(Arc::new(store.handle()), &d);
    let mut c = dial(&d);
    ok(c.request("machines", json!({})).unwrap());
    server.stop();
    assert!(c.request("machines", json!({})).is_err(), "the server is gone");
    assert!(matches!(c.request("machines", json!({})), Err(wsp_client::Error::Silent(_))));
    let server = start(Arc::new(store.handle()), &d);
    ok(c.request("machines", json!({})).unwrap());
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- subscribe

/// Items until one is an event with id `upto`; beats are not items.
fn until(f: &mut Feed, upto: u64) -> Vec<Item> {
    let mut out = vec![];
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "no event {upto}; got {out:?}");
        if let Some(i) = f.next_within(Duration::from_millis(200)) {
            let done = matches!(&i, Item::Event(e) if e.id.0 >= upto);
            out.push(i);
            if done {
                return out;
            }
        }
    }
}

fn ids(items: &[Item]) -> Vec<u64> {
    items.iter().filter_map(|i| if let Item::Event(e) = i { Some(e.id.0) } else { None }).collect()
}

#[test]
fn subscribe_hands_out_the_snapshot_and_then_each_event_once() {
    let d = dir("sub");
    let store = mirror_store();
    rows(&store, 0, 4);
    let server = start(Arc::new(store.handle()), &d);
    let c = dial(&d);
    let mut f = feed(&c, None);

    let first = f.next_within(WAIT).unwrap();
    let (state, at) = store.snapshot().unwrap();
    assert_eq!(first, Item::State { state, at });
    rows(&store, 4, 3);
    let got = until(&mut f, 7);
    assert_eq!(ids(&got), [5, 6, 7]);
    assert!(got.iter().all(|i| matches!(i, Item::Event(_))));
    assert_eq!(f.last_id(), Some(EventId(7)));

    // Quiet for a few beats: beats are counted, never handed out.
    quiet_until(&mut f, "two beats counted", |f| f.beats() >= 2);
    assert_eq!(f.reconnects(), 0);

    // From an id: the events after it, with no snapshot.
    let mut g = feed(&c, Some(EventId(5)));
    assert_eq!(ids(&until(&mut g, 7)), [6, 7]);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn across_a_server_restart_the_feed_reconnects_says_resumed_and_gives_every_later_event_once() {
    let d = dir("restart");
    let store = mirror_store();
    rows(&store, 0, 2);
    let server = start(Arc::new(store.handle()), &d);
    let c = dial(&d);
    let mut f = feed(&c, None);
    assert!(matches!(f.next_within(WAIT), Some(Item::State { .. })));
    rows(&store, 2, 3);
    let mut seen = ids(&until(&mut f, 5));
    assert_eq!(seen, [3, 4, 5]);

    server.stop();
    // Written while no server runs: the new connection must bring them.
    rows(&store, 5, 2);
    // The feed notices, and keeps trying until a server answers again.
    quiet_until(&mut f, "the feed noticed its server stopped", |f| f.why_silent().is_some());
    let server = start(Arc::new(store.handle()), &d);
    let mut got = vec![];
    let deadline = Instant::now() + WAIT;
    while got.len() < 3 {
        assert!(Instant::now() < deadline, "got {got:?}");
        got.extend(f.next_within(Duration::from_millis(200)));
    }
    assert_eq!(got[0], Item::Resumed { from: EventId(5) });
    assert_eq!(ids(&got), [6, 7], "{got:?}");
    seen.extend(ids(&got));

    rows(&store, 7, 3);
    let more = until(&mut f, 10);
    assert!(!more.iter().any(|i| matches!(i, Item::Resumed { .. } | Item::State { .. })), "{more:?}");
    seen.extend(ids(&more));
    assert_eq!(seen, (3..=10).collect::<Vec<_>>(), "every id once, in order");
    assert_eq!(f.reconnects(), 1);
    assert_eq!(f.duplicates(), 0);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_feed_that_never_connected_says_no_resumed() {
    let d = dir("late");
    let store = mirror_store();
    rows(&store, 0, 1);
    let server = start(Arc::new(store.handle()), &d);
    let c = dial(&d);
    server.stop();
    let mut f = feed(&c, None);
    quiet_until(&mut f, "the feed found no server", |f| f.why_silent().is_some());
    let server = start(Arc::new(store.handle()), &d);
    assert!(matches!(f.next_within(WAIT), Some(Item::State { .. })), "a first State, not a Resumed");
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn too_old_leads_to_a_fresh_state_and_the_events_after_it() {
    let d = dir("too-old");
    let store = mirror_store();
    rows(&store, 0, 6);
    let kept = Retained { inner: store.handle(), oldest: EventId(3) };
    let server = start(Arc::new(kept), &d);
    let c = dial(&d);
    let mut f = feed(&c, Some(EventId(1)));

    let (state, at) = store.snapshot().unwrap();
    assert_eq!(f.next_within(WAIT), Some(Item::State { state, at }));
    assert_eq!(f.last_id(), Some(at));
    rows(&store, 6, 1);
    assert_eq!(ids(&until(&mut f, 7)), [7]);
    assert_eq!(f.reconnects(), 0, "it was one connection: the second subscribe was on it");

    // Still held back: from 3 is within retention, so no State.
    let mut g = feed(&c, Some(EventId(3)));
    assert!(matches!(g.next_within(WAIT), Some(Item::Event(e)) if e.id == EventId(4)));
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn an_event_id_already_handed_out_is_dropped_and_a_silent_server_costs_the_connection() {
    // A server of our own that sends an id twice, then goes quiet.
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let d = dir("dup");
    fs::create_dir_all(&d).unwrap();
    let listen = Listen { addrs: vec![l.local_addr().unwrap().to_string()] };
    fs::write(d.join(LISTEN_FILE), serde_json::to_string(&listen).unwrap()).unwrap();
    fs::write(d.join(TOKEN_FILE), "t").unwrap();
    let store = mirror_store();
    rows(&store, 0, 3);
    let evs = store.events_after(EventId(0), 10).unwrap();
    fn line(f: wsp_api::Feed) -> String {
        format!("{}\n", serde_json::to_string(&wsp_api::Frame::Feed(f)).unwrap())
    }
    // One thread per connection: the client's request connection says hello and stays quiet; the
    // feed's first sends the events (one id twice, and one out of order) and then nothing; its second
    // is let be.
    let subscriptions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = subscriptions.clone();
    let accepts = std::thread::spawn(move || {
        let mut threads = vec![];
        for stream in l.incoming().take(3) {
            let (mut s, evs, count) = (stream.unwrap(), evs.clone(), count.clone());
            threads.push(std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut text = String::new();
                r.read_line(&mut text).unwrap();
                writeln!(s, "{}", r#"{"type":"welcome","server":"fake","protocol":1,"head":3}"#).unwrap();
                text.clear();
                if r.read_line(&mut text).unwrap() == 0 {
                    return;
                }
                let n = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let id = serde_json::from_str::<Value>(&text).unwrap()["id"].clone();
                writeln!(s, "{}", json!({"type":"reply","id":id,"ok":"subscribed","events":[]})).unwrap();
                if n == 0 {
                    for e in [&evs[0], &evs[1], &evs[1], &evs[0], &evs[2]] {
                        s.write_all(line(wsp_api::Feed::Event((*e).clone())).as_bytes()).unwrap();
                    }
                }
                // Read what the client sends (its keepalives) until it hangs up, and answer nothing.
                loop {
                    text.clear();
                    if r.read_line(&mut text).unwrap_or(0) == 0 {
                        return;
                    }
                }
            }));
        }
        for t in threads {
            let _ = t.join();
        }
    });
    let c = Client::dial(&d, ed(), WAIT).unwrap();
    let mut f = c.subscribe(Some(EventId(0)));
    f.set_beat(Duration::from_millis(60));
    let got: Vec<Item> = (0..3).filter_map(|_| f.next_within(WAIT)).collect();
    assert_eq!(ids(&got), [1, 2, 3]);
    assert_eq!(f.duplicates(), 2);
    // Silent for MISSED_BEATS beats: counted, then it dials again.
    assert_eq!(f.next_within(WAIT), Some(Item::Resumed { from: EventId(3) }));
    assert!(f.missed_beats() >= u64::from(MISSED_BEATS), "{}", f.missed_beats());
    assert_eq!(f.reconnects(), 1);
    let deadline = Instant::now() + WAIT;
    while subscriptions.load(std::sync::atomic::Ordering::SeqCst) < 2 {
        assert!(Instant::now() < deadline, "the second subscribe never came");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(f);
    drop(c);
    // The listener takes three connections: the client's, the feed's and its second.
    let _ = accepts.join();
    let _ = fs::remove_dir_all(&d);
}

/// A server of our own at `d`, taking `conns` connections: each is welcomed, and
/// each subscribe is answered `subscribed` and then given `then(n, stream)` (n
/// counts subscribes from 0). Returns when each subscribe came, and the thread.
#[allow(clippy::type_complexity)]
fn fake_feed(
    d: &Path,
    conns: usize,
    then: fn(usize, &mut std::net::TcpStream),
) -> (Arc<std::sync::Mutex<Vec<Instant>>>, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    fs::create_dir_all(d).unwrap();
    let listen = Listen { addrs: vec![l.local_addr().unwrap().to_string()] };
    fs::write(d.join(LISTEN_FILE), serde_json::to_string(&listen).unwrap()).unwrap();
    fs::write(d.join(TOKEN_FILE), "t").unwrap();
    let came = Arc::new(std::sync::Mutex::new(Vec::new()));
    let times = came.clone();
    let accepts = std::thread::spawn(move || {
        let mut threads = vec![];
        for stream in l.incoming().take(conns) {
            let (mut s, times) = (stream.unwrap(), times.clone());
            threads.push(std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut text = String::new();
                if r.read_line(&mut text).unwrap_or(0) == 0 {
                    return;
                }
                writeln!(s, r#"{{"type":"welcome","server":"fake","protocol":1,"head":3}}"#).unwrap();
                text.clear();
                if r.read_line(&mut text).unwrap_or(0) == 0 {
                    return;
                }
                let n = {
                    let mut t = times.lock().unwrap();
                    t.push(Instant::now());
                    t.len() - 1
                };
                let id = serde_json::from_str::<Value>(&text).unwrap()["id"].clone();
                writeln!(s, "{}", json!({"type":"reply","id":id,"ok":"subscribed","events":[]})).unwrap();
                then(n, &mut s);
                loop {
                    text.clear();
                    if r.read_line(&mut text).unwrap_or(0) == 0 {
                        return;
                    }
                }
            }));
        }
        for t in threads {
            let _ = t.join();
        }
    });
    (came, accepts)
}

#[test]
fn a_connection_lost_before_it_delivered_anything_backs_off_and_each_wait_is_longer() {
    // wsp-702: the server answers `subscribed` and hangs up, as a client meets a
    // snapshot line it cannot read. The feed must not ask again at once, forever.
    let d = dir("backoff");
    // The client's request connection, then five subscribes.
    const SUBS: usize = 5;
    let (came, accepts) = fake_feed(&d, 1 + SUBS, |_, s| {
        let _ = s.shutdown(std::net::Shutdown::Both);
    });
    let c = Client::dial(&d, ed(), WAIT).unwrap();
    let mut f = c.subscribe(None);
    f.set_beat(BEAT);
    let deadline = Instant::now() + WAIT;
    while came.lock().unwrap().len() < SUBS {
        assert!(Instant::now() < deadline, "the subscribes never came: {:?}", f.why_silent());
        assert_eq!(f.next_within(Duration::from_millis(20)), None, "nothing is delivered");
    }
    drop(f);
    drop(c);
    let t = came.lock().unwrap().clone();
    let gaps: Vec<Duration> = t.windows(2).map(|w| w[1] - w[0]).collect();
    // The waits are 25, 50, 100, 200 ms: each a floor on its gap, so a machine
    // under load is slower and never failed. Before the fix every gap was the
    // dial's few ms.
    for (i, gap) in gaps.iter().enumerate() {
        let floor = Duration::from_millis(25 << i);
        assert!(*gap >= floor, "reconnect {} came after {gap:?}, under its backoff of {floor:?}: {gaps:?}", i + 1);
    }
    assert!(gaps[1] >= Duration::from_millis(50), "the second reconnect waits longer than the first: {gaps:?}");
    let _ = accepts.join();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_snapshot_in_parts_is_handed_out_as_one_state_and_the_events_after_it_follow() {
    // wsp-702: Parts closed by a State, then an event; the caller sees one
    // Item::State with every piece in it, as from a snapshot that came whole.
    let d = dir("parts");
    let (_, accepts) = fake_feed(&d, 2, |_, s| {
        use std::io::Write;
        let store = mirror_store();
        apply(&store, everything(), vec![], "all");
        rows(&store, 0, 40);
        let (state, at) = store.snapshot().unwrap();
        let mut pieces = state.split(600);
        assert!(pieces.len() > 3, "the fixture splits: {}", pieces.len());
        let last = pieces.pop().unwrap();
        let line = |f: Feed2| format!("{}\n", serde_json::to_string(&wsp_api::Frame::Feed(f)).unwrap());
        for part in pieces {
            s.write_all(line(Feed2::Part { part, at }).as_bytes()).unwrap();
        }
        s.write_all(line(Feed2::State { state: last, at }).as_bytes()).unwrap();
        rows(&store, 40, 1);
        let e = store.events_after(at, 1).unwrap().remove(0);
        s.write_all(line(Feed2::Event(e)).as_bytes()).unwrap();
    });
    let c = Client::dial(&d, ed(), WAIT).unwrap();
    let mut f = c.subscribe(None);
    f.set_beat(BEAT);
    let want = {
        let store = mirror_store();
        apply(&store, everything(), vec![], "all");
        rows(&store, 0, 40);
        store.snapshot().unwrap()
    };
    assert_eq!(f.next_within(WAIT), Some(Item::State { state: want.0, at: want.1 }));
    match f.next_within(WAIT) {
        Some(Item::Event(e)) => assert_eq!(e.id, EventId(want.1 .0 + 1)),
        other => panic!("expected the event after the snapshot, got {other:?}"),
    }
    drop(f);
    drop(c);
    let _ = accepts.join();
    let _ = fs::remove_dir_all(&d);
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
