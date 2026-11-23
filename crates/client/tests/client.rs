//! `wsp-client` against a loopback `wsp-server` over `MemStore` (wsp-074's done
//! list): `dial` silent with no `api.json`, a wrong token and a stopped server,
//! each reason naming which; `dial_at` (wsp-306) welcomed at given addresses
//! with the right token, subscribing, and refused with a wrong one; `request` for each read verb; `subscribe` handing
//! out the snapshot and then each event once, across a server restart (a
//! `Resumed`, and no id twice) and through a `too_old` (a fresh `State`); and a
//! duplicate id dropped; `throttle`, `spend` and `show` handing back the
//! server's documents, and their refusals (wsp-307).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, to_value, Value};
use wsp_api::{Answer, ApiToken, Code, Listen, Outcome, Reply, LISTEN_FILE, MISSED_BEATS, TOKEN_FILE};
use wsp_client::{Client, Credential, Feed, Item, Silent, ViewError};
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

/// The daemon's documents, faked, for the view calls (wsp-307).
struct Docs;

impl wsp_server::Views for Docs {
    fn throttle(&self) -> std::result::Result<Value, wsp_api::Failure> {
        Ok(json!({ "mode": "on", "in_force": true, "load": [2.5, 2.0, 1.75] }))
    }
    fn spend(&self) -> std::result::Result<Value, wsp_api::Failure> {
        Ok(json!({ "today": "2026-11-20", "currency": "USD" }))
    }
    fn show(&self, id: &str) -> std::result::Result<Value, wsp_api::Failure> {
        match id {
            "wsp-1" => Ok(json!({ "id": "wsp-1", "title": "one" })),
            _ => Err(wsp_api::Failure { code: Code::NotFound, text: format!("no task matches '{id}'"), refusal: None }),
        }
    }
}

#[test]
fn throttle_spend_and_show_return_the_documents_and_a_refusal_says_why() {
    let d = dir("views");
    let mut cfg = Config::new(&d);
    cfg.port = 0;
    cfg.beat = BEAT;
    cfg.views = Some(Arc::new(Docs));
    let server = Server::start(Arc::new(mirror_store()), cfg).unwrap();
    let mut c = dial(&d);
    assert_eq!(c.throttle().unwrap(), json!({ "mode": "on", "in_force": true, "load": [2.5, 2.0, 1.75] }));
    assert_eq!(c.spend().unwrap(), json!({ "today": "2026-11-20", "currency": "USD" }));
    assert_eq!(c.show("wsp-1").unwrap(), json!({ "id": "wsp-1", "title": "one" }));
    match c.show("wsp-9") {
        Err(ViewError::Refused(f)) => assert_eq!((f.code, f.text.as_str()), (Code::NotFound, "no task matches 'wsp-9'")),
        other => panic!("a show of nothing gave {other:?}"),
    }
    server.stop();
    assert!(matches!(c.spend(), Err(ViewError::Request(_))), "the server is gone");
    let _ = fs::remove_dir_all(&d);

    // A server with no views refuses them `unsupported`, as one from before them would.
    let d = dir("no-views");
    let server = start(Arc::new(mirror_store()), &d);
    let mut c = dial(&d);
    assert!(matches!(c.throttle(), Err(ViewError::Refused(f)) if f.code == Code::Unsupported));
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

/// The census's reads, typed (wsp-743): `rows` and `lists` as heads keep
/// every row and list less its text, `asks` filters by kind and state, and
/// `readings` hands back what the store keeps by name, every one for none.
#[test]
fn the_census_reads_come_back_typed() {
    let d = dir("census");
    let store = mirror_store();
    let text = |f: &mut std::collections::BTreeMap<String, String>| {
        f.insert("body".into(), "long".into());
        f.insert("prose".into(), "longer".into());
        f.insert("status".into(), "todo".into());
    };
    let mut r = row("wsp-1", "one");
    text(&mut r.fields);
    let mut l = List { workspace: Default::default(), id: ListId("s1".into()), state: ListState::Running, groups: vec![], fields: Default::default() };
    text(&mut l.fields);
    let mut put = everything();
    put.retain(|e| !matches!(e, Entity::Row(x) if x.id.0 == "wsp-1") && !matches!(e, Entity::List(_)));
    put.push(Entity::Row(r));
    put.push(Entity::List(l));
    apply(&store, put, vec![], "census");
    store.set_reading("throttle", r#"{"mode":"auto"}"#.into()).unwrap();
    store.set_reading("spend", r#"{"landed":2}"#.into()).unwrap();
    let server = start(Arc::new(store.handle()), &d);
    let mut c = dial(&d);

    let heads = c.rows(RowFilter { heads: true, ..Default::default() }).unwrap();
    assert_eq!(heads, store.rows(&RowFilter { heads: true, ..Default::default() }).unwrap());
    let one = heads.iter().find(|r| r.id.0 == "wsp-1").unwrap();
    assert!(!one.fields.contains_key("body") && !one.fields.contains_key("prose"), "{one:?}");
    assert_eq!(one.fields.get("status").map(String::as_str), Some("todo"));
    assert_eq!(c.rows(RowFilter::default()).unwrap(), store.rows(&RowFilter::default()).unwrap(), "whole rows as before");

    let lists = c.lists(ListFilter { heads: true }).unwrap();
    assert_eq!(lists.len(), 1);
    assert!(!lists[0].fields.contains_key("body") && lists[0].fields.contains_key("status"), "{lists:?}");
    assert!(c.lists(ListFilter::default()).unwrap()[0].fields.contains_key("body"), "whole lists as before");

    assert_eq!(c.asks(vec![AskKind::Flag], vec![AskState::Raised]).unwrap().len(), 1);
    assert_eq!(c.asks(vec![AskKind::Ask], vec![]).unwrap(), vec![]);

    let all = c.readings(&[]).unwrap();
    assert_eq!(all.keys().collect::<Vec<_>>(), ["spend", "throttle"]);
    let one = c.readings(&["throttle", "nothing"]).unwrap();
    assert_eq!(one.keys().collect::<Vec<_>>(), ["throttle"]);
    assert_eq!(one["throttle"].json, r#"{"mode":"auto"}"#);
    assert_eq!(one["throttle"], all["throttle"]);
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

// ---------------------------------------------------------------- apply and await_order (wsp-119)

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// What a scripted server does with a request.
enum Say {
    Frame(wsp_api::Frame),
    /// Hang up on the connection.
    Close,
}

type Script = Box<dyn FnMut(usize, &Value) -> Vec<Say> + Send>;

/// A server of our own: it welcomes every connection (at `head`), and then asks
/// the script, for each request and the number of the connection it came on
/// (from 0), what to say. The real server answers no command until group 3.
struct Scripted {
    dir: PathBuf,
    gone: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Scripted {
    fn start(name: &str, head: u64, script: impl FnMut(usize, &Value) -> Vec<Say> + Send + 'static) -> Scripted {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let d = dir(name);
        fs::create_dir_all(&d).unwrap();
        let listen = Listen { addrs: vec![l.local_addr().unwrap().to_string()] };
        fs::write(d.join(LISTEN_FILE), serde_json::to_string(&listen).unwrap()).unwrap();
        fs::write(d.join(TOKEN_FILE), "t").unwrap();
        let gone = Arc::new(AtomicBool::new(false));
        let script: Arc<Mutex<Script>> = Arc::new(Mutex::new(Box::new(script)));
        let (stop, streams) = (gone.clone(), Arc::new(Mutex::new(Vec::<TcpStream>::new())));
        let accept = std::thread::spawn(move || {
            let mut n = 0;
            while !stop.load(Ordering::SeqCst) {
                match l.accept() {
                    Ok((s, _)) => {
                        s.set_nonblocking(false).unwrap();
                        streams.lock().unwrap().push(s.try_clone().unwrap());
                        let script = script.clone();
                        std::thread::spawn(move || serve(s, n, head, script));
                        n += 1;
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            drop(l);
            for s in streams.lock().unwrap().iter() {
                let _ = s.shutdown(Shutdown::Both);
            }
        });
        Scripted { dir: d, gone, accept: Some(accept) }
    }

    /// The server goes: its listener is closed and every connection hung up.
    fn stop(&mut self) {
        self.gone.store(true, Ordering::SeqCst);
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn serve(mut s: TcpStream, n: usize, head: u64, script: Arc<Mutex<Script>>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut text = String::new();
    if r.read_line(&mut text).unwrap_or(0) == 0 {
        return;
    }
    let welcome = json!({"type": "welcome", "server": "scripted", "protocol": 1, "head": head});
    if writeln!(s, "{welcome}").is_err() {
        return;
    }
    loop {
        text.clear();
        if r.read_line(&mut text).unwrap_or(0) == 0 {
            return;
        }
        let req: Value = serde_json::from_str(&text).unwrap();
        let says = (script.lock().unwrap())(n, &req);
        for say in says {
            match say {
                Say::Frame(f) => {
                    if writeln!(s, "{}", serde_json::to_string(&f).unwrap()).is_err() {
                        return;
                    }
                }
                Say::Close => {
                    let _ = s.shutdown(Shutdown::Both);
                    return;
                }
            }
        }
    }
}

fn reply_to(req: &Value, outcome: Outcome, events: Vec<EventId>) -> Say {
    let id = wsp_api::ReqId(req["id"].as_u64().unwrap());
    Say::Frame(wsp_api::Frame::Reply(Reply { id, outcome, events }))
}

fn says_ok(req: &Value, answer: Answer) -> Vec<Say> {
    vec![reply_to(req, Outcome::Ok(answer), vec![])]
}

fn cancel(order: &str) -> Command {
    Command::OrderCancel { order: OrderId(order.into()) }
}

#[test]
fn a_reply_dropped_once_is_retried_under_the_same_key_and_answers_with_the_first_result() {
    // The server applies on the first sight of a key and hangs up before it replies; on the second
    // sight of the key it replies with what it applied the first time.
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    let mut applied: Vec<(String, Vec<EventId>)> = vec![];
    let server = Scripted::start("retry", 0, move |_, req| {
        if req["verb"] != "order.cancel" {
            return vec![];
        }
        let key = req["key"].as_str().expect("a command carries a key").to_string();
        log.lock().unwrap().push(key.clone());
        match applied.iter().find(|(k, _)| *k == key) {
            Some((_, events)) => vec![reply_to(req, Outcome::Ok(Answer::Applied), events.clone())],
            None => {
                applied.push((key, vec![EventId(40 + applied.len() as u64)]));
                vec![Say::Close]
            }
        }
    });
    let mut c = dial(&server.dir);
    assert_eq!(c.apply(&cancel("o-1")).unwrap(), Applied { events: vec![EventId(40)] });
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "sent twice: {seen:?}");
        assert_eq!(seen[0], seen[1], "under one key");
    }

    // A call is a key of its own: the next command is not the first one again.
    assert_eq!(c.apply(&cancel("o-2")).unwrap(), Applied { events: vec![EventId(41)] });
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 4, "two sends each: {seen:?}");
    assert_ne!(seen[1], seen[2], "a key for each call");
}

#[test]
fn a_reply_that_never_comes_is_sent_again_under_the_same_key_within_the_one_timeout() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    let server = Scripted::start("silent-reply", 0, move |_, req| {
        let mut keys = log.lock().unwrap();
        keys.push(req["key"].as_str().unwrap().to_string());
        // Hears the first, says nothing; answers the second.
        if keys.len() == 1 {
            vec![]
        } else {
            vec![reply_to(req, Outcome::Ok(Answer::Applied), vec![EventId(7)])]
        }
    });
    let mut c = dial(&server.dir);
    c.set_timeout(Duration::from_millis(900));
    let began = Instant::now();
    assert_eq!(c.apply(&cancel("o-1")).unwrap().events, [EventId(7)]);
    assert!(began.elapsed() < Duration::from_millis(900), "within the one timeout: {:?}", began.elapsed());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1]);
}

#[test]
fn a_refusal_is_the_servers_refusal_and_is_never_retried() {
    let sent = Arc::new(AtomicUsize::new(0));
    let count = sent.clone();
    let refusal = Refusal::Illegal { subject: Subject::Order(OrderId("o-1".into())), state: "done".into(), verb: "order.cancel".into() };
    let said = refusal.clone();
    let server = Scripted::start("refused", 0, move |_, req| {
        count.fetch_add(1, Ordering::SeqCst);
        let failure = wsp_api::Failure { code: Code::Illegal, text: "o-1 is done".into(), refusal: Some(said.clone()) };
        vec![reply_to(req, Outcome::Err(failure), vec![])]
    });
    let mut c = dial(&server.dir);
    assert_eq!(c.apply(&cancel("o-1")), Err(wsp_client::Error::Refused(refusal)));
    assert_eq!(sent.load(Ordering::SeqCst), 1);

    // Nor is an `err` that is no refusal.
    let sent = Arc::new(AtomicUsize::new(0));
    let count = sent.clone();
    let server = Scripted::start("failed", 0, move |_, req| {
        count.fetch_add(1, Ordering::SeqCst);
        let failure = wsp_api::Failure { code: Code::Unsupported, text: "no".into(), refusal: None };
        vec![reply_to(req, Outcome::Err(failure), vec![])]
    });
    let mut c = dial(&server.dir);
    assert!(matches!(c.apply(&cancel("o-1")), Err(wsp_client::Error::Failed(f)) if f.code == Code::Unsupported));
    assert_eq!(sent.load(Ordering::SeqCst), 1);
}

fn order(id: &str, state: OrderState) -> Order {
    Order {
        workspace: Default::default(),
        id: OrderId(id.into()),
        from: ed(),
        to: SeatId("cpd-1".into()),
        text: "go".into(),
        state,
        attempts: 0,
        first: false,
        why: None,
    }
}

fn order_event(id: u64, order: &str, kind: EventKind) -> Say {
    let subject = Subject::Order(OrderId(order.into()));
    let e = Event { workspace: Default::default(), id: EventId(id), at: At(id), principal: ed(), kind, subject, payload: Value::Null };
    Say::Frame(wsp_api::Frame::Feed(wsp_api::Feed::Event(e)))
}

/// A server holding `orders`, at `head`, that says `follow` after it answers a subscribe.
/// `subscribed` counts the subscribes it heard, and `from` is what the last one asked from.
fn orders_server(
    name: &str,
    orders: Vec<Order>,
    head: u64,
    follow: fn() -> Vec<Say>,
    subscribed: Arc<AtomicUsize>,
    from: Arc<Mutex<Option<u64>>>,
) -> Scripted {
    Scripted::start(name, head, move |_, req| match req["verb"].as_str().unwrap() {
        "events" => says_ok(req, Answer::Events { events: vec![], head: EventId(head) }),
        "orders" => says_ok(req, Answer::Orders(orders.clone())),
        "subscribe" => {
            subscribed.fetch_add(1, Ordering::SeqCst);
            *from.lock().unwrap() = req["args"]["from"].as_u64();
            let mut says = says_ok(req, Answer::Subscribed);
            says.extend(follow());
            says
        }
        other => panic!("unexpected {other}"),
    })
}

fn no_events() -> Vec<Say> {
    vec![]
}

#[test]
fn await_order_answers_at_once_for_an_order_already_settled() {
    let (subscribed, from) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
    let held = vec![order("o-0", OrderState::Queued), order("o-1", OrderState::Acknowledged)];
    let server = orders_server("await-settled", held, 5, no_events, subscribed.clone(), from);
    let mut c = dial(&server.dir);
    let began = Instant::now();
    let got = c.await_order(&OrderId("o-1".into()), &[OrderState::Acknowledged, OrderState::Done], WAIT).unwrap();
    assert_eq!(got, Some(order("o-1", OrderState::Acknowledged)));
    assert!(began.elapsed() < Duration::from_secs(10), "at once: {:?}", began.elapsed());
    assert_eq!(subscribed.load(Ordering::SeqCst), 0, "a settled order is not followed");
}

#[test]
fn await_order_follows_the_events_of_a_queued_order_to_acknowledged() {
    fn follow() -> Vec<Say> {
        vec![
            order_event(6, "o-1", EventKind::OrderTyped),
            order_event(7, "o-2", EventKind::OrderAcknowledged), // another order's
            order_event(8, "o-1", EventKind::OrderDelivered),
            order_event(9, "o-1", EventKind::OrderAcknowledged),
            order_event(10, "o-1", EventKind::OrderDone),
        ]
    }
    let (subscribed, from) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
    let held = vec![order("o-1", OrderState::Queued), order("o-2", OrderState::Queued)];
    let server = orders_server("await-follow", held, 5, follow, subscribed.clone(), from.clone());
    let mut c = dial(&server.dir);
    let got = c.await_order(&OrderId("o-1".into()), &[OrderState::Acknowledged], WAIT).unwrap();
    assert_eq!(got, Some(order("o-1", OrderState::Acknowledged)), "o-2's acknowledgement is not o-1's");
    assert_eq!(subscribed.load(Ordering::SeqCst), 1);
    assert_eq!(*from.lock().unwrap(), Some(5), "from the head read before the order was");
}

#[test]
fn await_order_is_none_past_wait() {
    let (subscribed, from) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
    let server = orders_server("await-none", vec![order("o-1", OrderState::Queued)], 5, no_events, subscribed, from);
    let mut c = dial(&server.dir);
    let wait = Duration::from_millis(400);
    let began = Instant::now();
    assert_eq!(c.await_order(&OrderId("o-1".into()), &[OrderState::Done], wait).unwrap(), None);
    let took = began.elapsed();
    assert!(took >= wait && took < wait + Duration::from_secs(10), "{took:?}");
    // An order nothing has is none too.
    assert_eq!(c.await_order(&OrderId("o-9".into()), &[OrderState::Done], Duration::from_millis(200)).unwrap(), None);
}

#[test]
fn await_order_is_silent_within_the_dials_bound_when_the_server_goes() {
    let (subscribed, from) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
    let mut server = orders_server("await-silent", vec![order("o-1", OrderState::Queued)], 5, no_events, subscribed.clone(), from);
    let bound = Duration::from_millis(500);
    let mut c = Client::dial(&server.dir, ed(), bound).unwrap();
    let id = OrderId("o-1".into());
    // Followed for a while, then the server goes; the wait is far longer than the bound.
    let waiter = std::thread::spawn(move || {
        let began = Instant::now();
        let got = c.await_order(&id, &[OrderState::Done], WAIT);
        (got, began.elapsed())
    });
    let deadline = Instant::now() + WAIT;
    while subscribed.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "never subscribed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let gone_at = Instant::now();
    server.stop();
    let (got, _) = waiter.join().unwrap();
    let after = gone_at.elapsed();
    assert!(matches!(got, Err(wsp_client::Error::Silent(_))), "{got:?}");
    assert!(after < bound + Duration::from_secs(5), "within the bound, not the wait: {after:?}");
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

impl Readings for Retained {}
impl Texts for Retained {
    fn text(&self, row: &RowId, section: &str) -> Result<(String, Rev)> {
        self.inner.text(row, section)
    }
    fn set_text(&self, row: &RowId, section: &str, text: String, expect: Rev) -> Result<Rev> {
        self.inner.set_text(row, section, text, expect)
    }
}

// ------------------------------------------------------------ the hub (wsp-160)

#[test]
fn dial_hub_sends_the_users_token_and_never_reads_api_token() {
    let d = dir("hub");
    let store = mirror_store();
    let server = start(Arc::new(store.handle()), &d);
    // A loopback server stands in for the hub: it takes one token, here the one
    // the test holds as the "user's". Neither state-dir file is there to read.
    let listen: Listen = serde_json::from_str(&fs::read_to_string(d.join(LISTEN_FILE)).unwrap()).unwrap();
    let token = fs::read_to_string(d.join(TOKEN_FILE)).unwrap().trim().to_string();
    let url = format!("tcp://{}", listen.addrs[0]);
    fs::remove_file(d.join(LISTEN_FILE)).unwrap();
    fs::remove_file(d.join(TOKEN_FILE)).unwrap();
    let wait = Duration::from_millis(800);

    let mut c = Client::dial_hub(&url, Credential::User(token.clone()), WAIT).unwrap_or_else(|s| panic!("silent: {s}"));
    assert_eq!(c.server(), "wsp");
    assert!(c.wip().is_ok());
    // `whoami` is the principal the connection was welcomed as, and the instance (wsp-165).
    let (by, server_name) = c.whoami().expect("whoami");
    assert_eq!(server_name, "wsp");
    assert!(matches!(by, wsp_api::Principal::Person(_)), "{by:?}");
    // The same address bare is the same hub.
    assert!(Client::dial_hub(&listen.addrs[0], Credential::User(token), WAIT).is_ok());

    // A wrong user token is refused unauthorised.
    match Client::dial_hub(&url, Credential::User("not-the-token".into()), wait) {
        Err(Silent::Refused(r)) => assert_eq!(r.code, Code::Unauthorised),
        other => panic!("expected Refused, got {:?}", other.map(|_| ())),
    }
    // The instance credential is never sent to a hub, and nothing is dialled.
    let e = Client::dial_hub(&url, Credential::Instance, wait).err().expect("Instance dialled a hub");
    assert!(matches!(e, Silent::NotLoggedIn(_)), "{e:?}");
    // Not a host and port, or not over TCP, is said and not dialled.
    assert!(matches!(Client::dial_hub("https://hub.invalid", Credential::User("t".into()), wait), Err(Silent::BadListenFile(_))));
    drop(c);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn with_an_api_token_and_no_hub_token_a_hub_dial_is_not_logged_in() {
    let d = dir("hub-nologin");
    let store = mirror_store();
    let server = start(Arc::new(store.handle()), &d);
    assert!(d.join(TOKEN_FILE).is_file(), "the instance token is there to be (not) read");
    let home = dir("hub-nologin-home");
    fs::create_dir_all(&home).unwrap();
    let e = Credential::for_hub_in(None, &home).expect_err("a credential from nothing");
    assert!(matches!(e, Silent::NotLoggedIn(_)), "{e:?}");
    assert!(e.to_string().contains("not logged in"), "{e}");
    server.stop();
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn local_still_dials_with_the_instance_token() {
    // `Client::dial` is the `Local` path and takes no credential: the state dir's own.
    let d = dir("local");
    let store = mirror_store();
    let server = start(Arc::new(store.handle()), &d);
    let c = dial(&d);
    assert_eq!(c.server(), "wsp");
    drop(c);
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ------------------------------------------------------------ TLS (wsp-461)

/// A server whose every connection is inside TLS, with the certificate of a generated CA.
fn start_tls(store: SharedStore, dir: &Path, pki: &wsp_server::testing::TestPki) -> Server {
    let mut cfg = Config::new(dir);
    cfg.port = 0;
    cfg.beat = BEAT;
    cfg.tls = Some(wsp_server::tls::server_config(&pki.cert, &pki.key).expect("the test certificate reads"));
    Server::start(store, cfg).unwrap()
}

#[test]
fn a_tls_url_is_dialled_over_tls_against_the_pinned_ca_and_a_feed_follows_it() {
    use wsp_server::testing::TestPki;
    let d = dir("tls");
    let pki = TestPki::new(&d.join("pki"), "hub");
    let other = TestPki::new(&d.join("pki"), "other");
    let store = mirror_store();
    let server = start_tls(Arc::new(store.handle()), &d, &pki);
    let token = fs::read_to_string(d.join(TOKEN_FILE)).unwrap().trim().to_string();
    let port = server.addrs()[0].port();
    let url = format!("tls://localhost:{port}");
    let wait = Duration::from_secs(10);

    let mut c = Client::dial_hub_ca(&url, Credential::User(token.clone()), Some(&pki.ca), WAIT)
        .unwrap_or_else(|s| panic!("silent: {s}"));
    assert_eq!(c.server(), "wsp");
    assert!(c.wip().is_ok(), "a request is answered over TLS");
    let mut f = feed(&c, None);
    assert!(matches!(f.next_within(WAIT), Some(Item::State { .. })), "a feed over TLS opens with the state");
    let at = store.handle().head().unwrap();
    store.handle().apply(Command::Mirror { put: vec![Entity::Row(row("wsp-1", "one"))], gone: vec![] }, IdempotencyKey("k1".into()), Principal::Wsp("wsp".into())).unwrap();
    let mut told = false;
    while let Some(item) = f.next_within(WAIT) {
        if matches!(item, Item::Event(ref e) if e.id.0 > at.0) {
            told = true;
            break;
        }
    }
    assert!(told, "an event written after the snapshot arrives on the TLS feed");
    drop((c, f));

    // Without the CA, the platform's roots do not vouch for it: refused, naming the certificate.
    match Client::dial_hub(&url, Credential::User(token.clone()), wait) {
        Err(Silent::Tls(why)) => assert!(why.contains("UnknownIssuer"), "{why}"),
        other => panic!("expected a certificate refusal, got {:?}", other.map(|c| c.server().to_string())),
    }
    // Another CA pinned is the same: only the pinned one is trusted.
    match Client::dial_hub_ca(&url, Credential::User(token.clone()), Some(&other.ca), wait) {
        Err(Silent::Tls(why)) => assert!(why.contains("UnknownIssuer"), "{why}"),
        other => panic!("expected a certificate refusal, got {:?}", other.map(|c| c.server().to_string())),
    }
    // The right CA but the wrong name: the certificate is for `localhost`.
    let by_ip = format!("tls://{}", server.addrs()[0]);
    match Client::dial_hub_ca(&by_ip, Credential::User(token.clone()), Some(&pki.ca), wait) {
        Err(Silent::Tls(why)) => assert!(why.contains("not valid for name"), "{why}"),
        other => panic!("expected a name refusal, got {:?}", other.map(|c| c.server().to_string())),
    }
    // A CA file that is not one, or is not there, is said.
    let junk = d.join("junk.pem");
    fs::write(&junk, "not a pem").unwrap();
    for ca in [junk, d.join("absent.pem")] {
        match Client::dial_hub_ca(&url, Credential::User(token.clone()), Some(&ca), wait) {
            Err(Silent::Tls(why)) => assert!(why.contains(&ca.display().to_string()), "{why}"),
            other => panic!("expected the CA file refused, got {:?}", other.map(|c| c.server().to_string())),
        }
    }
    // Plain TCP to a TLS server is no welcome, and a TLS dial to a plain one is a refused handshake.
    assert!(Client::dial_hub(&format!("tcp://localhost:{port}"), Credential::User(token), Duration::from_secs(1)).is_err());
    server.stop();
    let _ = fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- a revoked token (wsp-162)

#[test]
fn a_feed_refused_unauthorised_after_the_welcome_ends_and_does_not_dial_again() {
    let subscribed = Arc::new(AtomicUsize::new(0));
    let heard = subscribed.clone();
    let server = Scripted::start("revoked-feed", 0, move |_, req| match req["verb"].as_str().unwrap() {
        "subscribe" => {
            heard.fetch_add(1, Ordering::SeqCst);
            let mut says = says_ok(req, Answer::Subscribed);
            says.push(order_event(1, "o-1", EventKind::OrderTyped));
            says.push(Say::Frame(wsp_api::Frame::Refused(wsp_api::Refused {
                code: Code::Unauthorised,
                text: "the token is revoked".into(),
            })));
            says.push(Say::Close);
            says
        }
        _ => vec![],
    });
    let c = dial(&server.dir);
    let mut f = feed(&c, Some(EventId(0)));
    assert!(matches!(f.next_within(WAIT), Some(Item::Event(e)) if e.id == EventId(1)), "what came first is handed out");
    assert_eq!(f.next_within(Duration::from_millis(100)), None);
    assert_eq!(f.refused(), Some("the token is revoked"));
    // A redial would come within the backoff (25 ms, doubling to a second): give it several.
    assert_eq!(f.next_within(Duration::from_secs(2)), None);
    assert_eq!(subscribed.load(Ordering::SeqCst), 1, "no second subscribe");
    assert_eq!(f.reconnects(), 0, "no redial");
    assert_eq!(f.by_ref().next(), None, "the iterator ends");
}
