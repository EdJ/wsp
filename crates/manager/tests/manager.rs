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
    let base = std::env::temp_dir();
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

// ---------------------------------------------------------------- hooks (wsp-023)

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hooks");

/// A recorded hook payload, its `transcript_path` pointed at `transcript`.
fn payload(name: &str, transcript: &Path) -> serde_json::Value {
    let text = fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap();
    serde_json::from_str(&text.replace("TRANSCRIPT", &transcript.to_string_lossy())).unwrap()
}

/// Append a recorded slice of the transcript, as Claude Code writes it.
fn transcript_gains(transcript: &Path, name: &str) {
    use std::io::Write;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(transcript).unwrap();
    f.write_all(&fs::read(Path::new(FIXTURES).join(name)).unwrap()).unwrap();
}

/// Every journalled event that arrives within `wait`, its time zeroed.
fn hook_events(conn: &mut Tcp, wait: Duration) -> Vec<Message> {
    let until = Instant::now() + wait;
    let mut out = Vec::new();
    while let Ok(Some(f)) = next(conn, until.saturating_duration_since(Instant::now())) {
        assert!(f.id.is_some(), "only events arrive after subscribe: {f:?}");
        out.push(match f.msg {
            Message::Turn(t) => Message::Turn(wsp_host_api::Turn { at: At(0), ..t }),
            Message::SeatSaid(s) => Message::SeatSaid(SeatSaid { at: At(0), ..s }),
            Message::SeatDid(d) => Message::SeatDid(wsp_host_api::SeatDid { at: At(0), ..d }),
            other => other,
        });
    }
    out
}

#[test]
fn the_hook_command_posts_to_the_manager_which_journals_turn_said_spent_and_did() {
    use wsp_host::hooks::{report, Dial, Sent};
    use wsp_host_api::{FileRef, HostId, OrderId, SeatDid, SeatSpent, Tokens, Turn, TurnEdge};

    let dir = exec_dir("hooks");
    let m = start(&dir);
    let addr = m.addrs()[0];
    let seat = SeatId("cpd-7".into());
    let epoch = Epoch(3);
    let transcript = dir.join("session.jsonl");
    let dial = Dial::Manager { addr, token_file: dir.join(TOKEN_FILE) };

    transcript_gains(&transcript, "transcript_1.jsonl");
    let post = |hook: &str, file: &str| report(&dial, &seat.0, epoch, hook, &payload(file, &transcript));
    assert_eq!(post("UserPromptSubmit", "user_prompt_submit.json"), Sent::Taken(1));
    assert_eq!(post("PostToolUse", "post_tool_use.json"), Sent::Taken(1));
    transcript_gains(&transcript, "transcript_2.jsonl");
    assert_eq!(post("Stop", "stop.json"), Sent::Taken(4));

    let model = "claude-opus-4-1-20250805";
    let first = wsp_host::price::cost(model, 120, 40, 3000, 500);
    let both = first + wsp_host::price::cost(model, 10, 25, 3500, 0);
    let turn = |edge, order: Option<&str>| {
        Message::Turn(Turn { seat: seat.clone(), epoch, edge, at: At(0), order: order.map(|o| OrderId(o.into())) })
    };
    // The tally's session, transcript and model ride along (wsp-865).
    let spent = |tokens, cost_micros| {
        Message::SeatSpent(SeatSpent {
            seat: seat.clone(),
            epoch,
            tokens,
            cost_micros,
            session_id: "5d0c7a52-1b9e-4c1f-9a35-2f6f0e4b7c11".into(),
            transcript: transcript.to_string_lossy().into_owned(),
            model: model.into(),
        })
    };
    let want = vec![
        // No order was delivered to this seat: its turn names none (the order
        // id is tests/seats.rs's, through a delivery).
        turn(TurnEdge::Began, None),
        spent(Tokens { input: 120, output: 40, cache_read: 3000, cache_write: 500 }, first),
        spent(Tokens { input: 130, output: 65, cache_read: 6500, cache_write: 500 }, both),
        Message::SeatSaid(SeatSaid {
            seat: seat.clone(),
            epoch,
            at: At(0),
            text: "Fixed: `add` subtracted. The test passes now.".into(),
        }),
        Message::SeatDid(SeatDid {
            seat: seat.clone(),
            epoch,
            at: At(0),
            digest: "asked: Fix the failing test in src/lib.rs; tools: Edit 1; edited: /Users/x/src/demo/src/lib.rs".into(),
            transcript: FileRef { host: HostId("test-host".into()), path: transcript.to_string_lossy().into_owned() },
        }),
        turn(TurnEdge::Ended, None),
    ];

    let (mut conn, hello) = connect(addr, token(&dir));
    assert_eq!(hello.journal_head, Some(EventId(6)));
    subscribe(&mut conn, None);
    assert_eq!(hook_events(&mut conn, Duration::from_millis(500)), want);

    // A second prompt begins a turn no order was awaited for.
    assert_eq!(post("UserPromptSubmit", "user_prompt_submit.json"), Sent::Taken(1));
    assert_eq!(hook_events(&mut conn, Duration::from_millis(500)), vec![turn(TurnEdge::Began, None)]);

    // A nested session's hook names the seat and says nothing of it (wsp-364).
    let mut child = payload("stop.json", &transcript);
    child["session_id"] = "a-claude-p-child".into();
    assert_eq!(report(&dial, &seat.0, epoch, "Stop", &child), Sent::Taken(0));

    // The wrong token is refused, logged, and journals nothing.
    let bad = dir.join("not-the-token");
    fs::write(&bad, "nope\n").unwrap();
    let wrong = Dial::Manager { addr, token_file: bad };
    assert!(matches!(report(&wrong, &seat.0, epoch, "Stop", &payload("stop.json", &transcript)), Sent::Unanswered(why) if why.contains("wrong token")));
    assert_eq!(m.head(), Some(EventId(7)));
    let log = fs::read_to_string(m.log_path()).unwrap();
    assert!(log.contains("a hook post for cpd-7 carried the wrong token"), "{log}");
    m.stop();
}

#[test]
fn with_no_manager_the_hook_command_says_so_and_posts_nothing() {
    use wsp_host::hooks::{report, Dial, Sent};
    let dir = exec_dir("hooks-none");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(TOKEN_FILE), "a-token\n").unwrap();
    // A port nothing listens on: bound, then let go.
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let began = Instant::now();
    let sent = report(&Dial::Manager { addr, token_file: dir.join(TOKEN_FILE) }, "cpd-7", Epoch(0), "Stop", &serde_json::json!({}));
    assert!(matches!(sent, Sent::Unanswered(_)), "{sent:?}");
    assert!(began.elapsed() < Duration::from_secs(1), "a refused dial on loopback is quick");
    let no_token = Dial::Manager { addr, token_file: dir.join("missing.token") };
    assert!(matches!(report(&no_token, "cpd-7", Epoch(0), "Stop", &serde_json::json!({})), Sent::Unanswered(_)));
    assert_eq!(report(&Dial::Inproc, "cpd-7", Epoch(0), "Stop", &serde_json::json!({})), Sent::Inproc);
}
