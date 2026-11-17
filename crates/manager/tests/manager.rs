//! The manager over TCP, on an ephemeral loopback port: the token, `hello`,
//! the journal and `subscribe {from}` (across a restart too), one manager per
//! port, the beat and its facts, and a client dropped for missed beats
//! (wsp-021's done-when).

use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wsp_host::Tcp;
use wsp_host_api::{
    At, ConnError, Connection, Epoch, EventId, Frame, Hello, ManagerHello, Message, Refusal, ReqId, Role, SeatId,
    SeatSaid, Seats, Subscribe, Token, WspHello, PROTOCOL,
};
use wsp_manager::{Config, Manager, StartError, JOURNAL_FILE, LOG_DIR, TOKEN_FILE};

const WAIT: Duration = Duration::from_secs(5);

fn exec_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("wsp-manager-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

fn config(dir: &Path, beat: Duration) -> Config {
    let mut cfg = Config::new(dir, "test-host");
    cfg.port = 0;
    cfg.beat = beat;
    cfg
}

fn start(dir: &Path) -> Manager {
    Manager::start(config(dir, Duration::from_secs(10))).expect("the manager starts")
}

fn token(dir: &Path) -> Token {
    Token(fs::read_to_string(dir.join(TOKEN_FILE)).unwrap().trim().to_string())
}

fn hello_frame(token: Token, req: u64) -> Frame {
    Frame {
        id: None,
        req: Some(ReqId(req)),
        msg: Message::Hello(Hello::Wsp(WspHello {
            role: Role::Cli,
            protocol: PROTOCOL,
            version: "test".into(),
            last_event: None,
            token,
        })),
    }
}

/// The next frame that is not a beat.
fn next(conn: &mut Tcp, wait: Duration) -> Result<Option<Frame>, ConnError> {
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        match conn.recv(left)? {
            Some(Frame { msg: Message::Beat(_), .. }) => continue,
            other => return Ok(other),
        }
    }
}

/// Dial and say a good hello; the manager's answer.
fn connect(addr: SocketAddr, token: Token) -> (Tcp, ManagerHello) {
    let mut conn = Tcp::dial(addr, WAIT).unwrap();
    conn.send(&hello_frame(token, 1)).unwrap();
    match next(&mut conn, WAIT).unwrap() {
        Some(Frame { req: Some(ReqId(1)), msg: Message::Hello(Hello::Manager(m)), .. }) => (conn, m),
        other => panic!("expected the manager's hello, got {other:?}"),
    }
}

fn said(n: u64) -> Message {
    Message::SeatSaid(SeatSaid { seat: SeatId("cpd-1".into()), epoch: Epoch(1), at: At(n), text: format!("event {n}") })
}

fn subscribe(conn: &mut Tcp, from: Option<u64>) {
    conn.send(&Frame { id: None, req: None, msg: Message::Subscribe(Subscribe { from: from.map(EventId) }) }).unwrap();
}

/// Every journalled event that arrives within `wait`, by id.
fn events(conn: &mut Tcp, wait: Duration) -> Vec<u64> {
    let until = Instant::now() + wait;
    let mut ids = Vec::new();
    while let Ok(Some(f)) = next(conn, until.saturating_duration_since(Instant::now())) {
        let id = f.id.expect("only events arrive after subscribe").0;
        assert_eq!(f.msg, said(id), "event {id} is the message journalled as {id}");
        ids.push(id);
    }
    ids
}

#[test]
fn a_hello_without_the_token_is_closed_and_one_with_it_is_answered() {
    let dir = exec_dir("hello");
    let m = start(&dir);
    let addr = m.addrs()[0];
    assert!(addr.ip().is_loopback());
    assert_eq!(fs::metadata(dir.join(TOKEN_FILE)).unwrap().permissions().mode() & 0o777, 0o600);

    // A wrong token: refused, then closed.
    let mut bad = Tcp::dial(addr, WAIT).unwrap();
    bad.send(&hello_frame(Token("not-the-token".into()), 7)).unwrap();
    match next(&mut bad, WAIT).unwrap() {
        Some(Frame { req: Some(ReqId(7)), msg: Message::Refused(r), .. }) => assert_eq!(r.why, Refusal::Unauthorised),
        other => panic!("expected refused unauthorised, got {other:?}"),
    }
    assert_eq!(bad.recv(WAIT), Err(ConnError::Closed));

    // No hello at all: anything else first is refused and closed.
    let mut rude = Tcp::dial(addr, WAIT).unwrap();
    rude.send(&Frame { id: None, req: None, msg: Message::Seats(Seats {}) }).unwrap();
    assert!(matches!(next(&mut rude, WAIT).unwrap(), Some(Frame { msg: Message::Refused(_), .. })));
    assert_eq!(rude.recv(WAIT), Err(ConnError::Closed));

    // A garbled first line: refused and closed.
    let mut garbled = Tcp::dial(addr, WAIT).unwrap();
    garbled.send_line("{\"type\":\"hello\"\n").unwrap();
    assert!(matches!(next(&mut garbled, WAIT).unwrap(), Some(Frame { msg: Message::Refused(_), .. })));
    assert_eq!(garbled.recv(WAIT), Err(ConnError::Closed));
    assert_eq!(m.clients(), 0);

    // The token: answered with the manager's facts.
    m.emit(said(1)).unwrap();
    let (mut good, hello) = connect(addr, token(&dir));
    assert_eq!(hello.host.0, "test-host");
    assert_eq!(hello.protocol, PROTOCOL);
    assert_eq!(hello.version, wsp_manager::VERSION);
    assert_eq!(hello.compound_sup.min, wsp_host::inproc::WIRE_VERSION);
    assert_eq!(hello.compound_sup.max, wsp_host::inproc::WIRE_VERSION);
    assert_eq!(hello.journal_head, Some(EventId(1)));
    assert!(hello.seats.is_empty());

    // It stays open and answers; a malformed line later is refused, not fatal.
    good.send_line("nonsense\n").unwrap();
    assert!(matches!(next(&mut good, WAIT).unwrap(), Some(Frame { msg: Message::Refused(r), .. }) if r.why == Refusal::Malformed));
    good.send(&Frame { id: None, req: Some(ReqId(2)), msg: Message::Seats(Seats {}) }).unwrap();
    match next(&mut good, WAIT).unwrap() {
        Some(Frame { req: Some(ReqId(2)), msg: Message::SeatsList(l), .. }) => assert!(l.seats.is_empty()),
        other => panic!("expected seats.list, got {other:?}"),
    }
    assert_eq!(m.clients(), 1);

    let log = fs::read_to_string(dir.join(LOG_DIR).join("manager.log")).unwrap();
    assert!(log.contains("started: host test-host"), "{log}");
    assert!(log.contains("wrong token"), "{log}");
    m.stop();
}

#[test]
fn a_client_that_reconnects_from_n_gets_exactly_the_events_after_n() {
    let dir = exec_dir("from");
    let m = start(&dir);
    let addr = m.addrs()[0];
    for n in 1..=5 {
        assert_eq!(m.emit(said(n)).unwrap(), EventId(n));
    }
    let (mut first, _) = connect(addr, token(&dir));
    subscribe(&mut first, None);
    assert_eq!(events(&mut first, Duration::from_millis(300)), vec![1, 2, 3, 4, 5]);
    // Live, after the replay.
    m.emit(said(6)).unwrap();
    assert_eq!(events(&mut first, Duration::from_millis(300)), vec![6]);
    drop(first);

    // Missed while away.
    m.emit(said(7)).unwrap();
    m.emit(said(8)).unwrap();
    let (mut back, hello) = connect(addr, token(&dir));
    assert_eq!(hello.journal_head, Some(EventId(8)));
    subscribe(&mut back, Some(6));
    assert_eq!(events(&mut back, Duration::from_millis(300)), vec![7, 8]);
    m.emit(said(9)).unwrap();
    assert_eq!(events(&mut back, Duration::from_millis(300)), vec![9]);

    // A client that has not subscribed hears no events.
    let (mut quiet, _) = connect(addr, token(&dir));
    m.emit(said(10)).unwrap();
    assert_eq!(events(&mut quiet, Duration::from_millis(300)), Vec::<u64>::new());
    assert_eq!(events(&mut back, Duration::from_millis(300)), vec![10]);
    m.stop();
}

#[test]
fn the_journal_serves_from_n_across_a_manager_restart() {
    let dir = exec_dir("restart");
    let m = start(&dir);
    let addr = m.addrs()[0];
    for n in 1..=3 {
        m.emit(said(n)).unwrap();
    }
    let (mut client, _) = connect(addr, token(&dir));
    subscribe(&mut client, None);
    assert_eq!(events(&mut client, Duration::from_millis(300)), vec![1, 2, 3]);
    let before = token(&dir);
    m.stop();
    assert!(matches!(next(&mut client, WAIT), Err(ConnError::Closed)), "a stopped manager closes its clients");

    // The same exec dir and the same port.
    let mut cfg = config(&dir, Duration::from_secs(10));
    cfg.port = addr.port();
    let m = Manager::start(cfg).expect("a manager restarts on its own port");
    assert_eq!(m.addrs()[0], addr);
    assert_eq!(token(&dir), before, "the token survives a restart");
    assert_eq!(m.head(), Some(EventId(3)));
    assert_eq!(m.emit(said(4)).unwrap(), EventId(4), "ids run on from the journal");
    m.emit(said(5)).unwrap();

    let (mut back, hello) = connect(addr, token(&dir));
    assert_eq!(hello.journal_head, Some(EventId(5)));
    subscribe(&mut back, Some(3));
    assert_eq!(events(&mut back, Duration::from_millis(300)), vec![4, 5]);
    m.stop();

    let lines = fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap();
    assert_eq!(lines.lines().count(), 5);
}

#[test]
fn a_second_manager_on_the_same_port_refuses_to_start() {
    let dir = exec_dir("second");
    let m = start(&dir);
    let addr = m.addrs()[0];

    // Another exec dir, the same port: the first answers there.
    let other = exec_dir("second-other");
    let mut cfg = config(&other, Duration::from_secs(10));
    cfg.port = addr.port();
    match Manager::start(cfg) {
        Err(StartError::Answered { addr: at, who }) => {
            assert_eq!(at, addr);
            assert!(who.contains("a manager"), "{who}");
        }
        Err(e) => panic!("expected refusal for the port, got {e}"),
        Ok(_) => panic!("a second manager started on a port the first answers"),
    }

    // The same exec dir on another port: the first holds it.
    match Manager::start(config(&dir, Duration::from_secs(10))) {
        Err(StartError::Locked { holder, .. }) => assert_eq!(holder, std::process::id().to_string()),
        Err(e) => panic!("expected refusal for the exec dir, got {e}"),
        Ok(_) => panic!("a second manager started on an exec dir the first holds"),
    }

    // The first is untouched by both refusals.
    let (_c, hello) = connect(addr, token(&dir));
    assert_eq!(hello.host.0, "test-host");
    m.stop();
}

#[test]
fn the_beat_carries_the_host_facts_on_its_own_timer() {
    let dir = exec_dir("beat");
    let m = Manager::start(config(&dir, Duration::from_millis(200))).unwrap();
    let (mut c, _) = connect(m.addrs()[0], token(&dir));
    let until = Instant::now() + WAIT;
    let mut beats = Vec::new();
    while beats.len() < 2 && Instant::now() < until {
        // The client beats too, or it would be dropped before its second beat.
        c.send(&Frame { id: None, req: None, msg: Message::Beat(wsp_host_api::Beat { at: wsp_manager::now(), host: None, seats: vec![] }) }).unwrap();
        if let Some(Frame { msg: Message::Beat(b), id, .. }) = c.recv(Duration::from_millis(250)).unwrap() {
            assert_eq!(id, None, "a beat is not journalled");
            beats.push(b);
        }
    }
    assert_eq!(beats.len(), 2, "two beats within {WAIT:?} at a 200 ms period");
    let facts = beats[1].host.clone().expect("the manager's beat carries the host's facts");
    assert!(facts.cpus >= 1);
    assert_eq!(facts.disks.len(), 1, "one volume configured: the exec dir's");
    assert!(facts.disks[0].total_bytes >= facts.disks[0].free_bytes && facts.disks[0].total_bytes > 0);
    assert!(beats[1].at.0 >= beats[0].at.0);
    let now = wsp_manager::now().0;
    assert!(now.saturating_sub(beats[1].at.0) < 5_000, "the beat is stamped when it is sent");
    #[cfg(target_os = "macos")]
    assert!(facts.load_centi > 0, "a Mac answers its load");
    assert_eq!(fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap_or_default(), "", "beats never reach the journal");
    m.stop();
}

#[test]
fn a_client_has_the_last_beat_just_after_its_hello() {
    let dir = exec_dir("first-beat");
    // A period far longer than the test: any beat it hears is the cached one.
    let m = Manager::start(config(&dir, Duration::from_secs(600))).unwrap();
    let until = Instant::now() + WAIT;
    let facts = loop {
        assert!(Instant::now() < until, "no beat came just after a hello within {WAIT:?}");
        let (mut c, _) = connect(m.addrs()[0], token(&dir));
        // The first beat may still be being read when this client connects.
        if let Ok(Some(Frame { msg: Message::Beat(b), .. })) = c.recv(Duration::from_millis(300)) {
            break b.host.expect("with the facts");
        }
    };
    assert!(facts.cpus >= 1);
    m.stop();
}

#[test]
fn a_client_that_misses_three_beats_is_dropped() {
    let dir = exec_dir("missed");
    let beat = Duration::from_millis(100);
    let m = Manager::start(config(&dir, beat)).unwrap();
    let addr = m.addrs()[0];

    // One that beats stays.
    let (mut lively, _) = connect(addr, token(&dir));
    // One that says hello and then nothing is dropped.
    let (mut silent, _) = connect(addr, token(&dir));
    let began = Instant::now();
    let mut silent_closed = None;
    while began.elapsed() < Duration::from_secs(2) {
        lively
            .send(&Frame { id: None, req: None, msg: Message::Beat(wsp_host_api::Beat { at: wsp_manager::now(), host: None, seats: vec![] }) })
            .unwrap();
        let _ = lively.recv(Duration::from_millis(20));
        if silent_closed.is_none() {
            match silent.recv(Duration::from_millis(20)) {
                Err(ConnError::Closed) => silent_closed = Some(began.elapsed()),
                Err(e) => panic!("{e}"),
                Ok(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let after = silent_closed.expect("the silent client is dropped");
    assert!(after >= beat * 3 - Duration::from_millis(50), "dropped after {after:?}, before three beats");
    assert_eq!(m.clients(), 1, "the client that beats is kept");
    let log = fs::read_to_string(m.log_path()).unwrap();
    assert!(log.contains("dropped, nothing heard for 3 beats"), "{log}");
    m.stop();
}
