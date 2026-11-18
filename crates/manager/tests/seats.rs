//! The manager's seats over TCP on loopback, with a fake compound-sup
//! (wsp-022's done-when): two spawns with one key give one seat; a stop or an
//! order naming a stale epoch is refused; an order answers delivered, then
//! acked; and a manager killed with SIGKILL mid-order, restarted, still lists
//! the seat, answers the order once, and resumes events `from` the last id.
//! And (wsp-032) a delivered order and its seat's hooks journal one `turn`
//! began, carrying the order id, and one `turn` ended.
//!
//! Nothing here spawns claude. The fake's sessions are a sleeping process group
//! each, which ends with this test process at the latest; every test also
//! kills the ones it started ([`Sweep`]).

use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use wsp_host::Tcp;
use wsp_host_api::{
    AgentSpec, ConnError, Connection, Epoch, EventId, Frame, Gate, Hello, Key, ManagerHello, Message, OrderCancel,
    OrderDeliver, OrderFail, OrderId, Refusal, ReqId, Role, RowId, SeatId, SeatSpawn, SeatState, SeatStop, Seats,
    Subscribe, Token, Workdir, WspHello, PROTOCOL,
};
use wsp_manager::{Config, Manager, TOKEN_FILE};

const WAIT: Duration = Duration::from_secs(10);

/// compound-sup as far as the manager uses it: `spawn` (a handshake, a
/// `.label` sidecar, a plain file for a socket), `vitals`, and `deliver <id>`
/// at most once per id (an ask while one is pending waits for its answer). A
/// text with `slow` takes two seconds; one with `busy` fails `busy`.
///
/// Under bash, not `/bin/sh`: a session leads its own process group, as
/// compound-sup's does, by `set -m`, and dash (Debian's and Ubuntu's `sh`)
/// turns job control off without a tty, leaving the session in the fake's
/// group, where a stop by group never reaches it.
const FAKE_SUP: &str = r#"#!/usr/bin/env bash
OWNER=__OWNER__
verb=$1; shift
case "$verb" in
wire-version) echo "9 9" ;;
spawn)
  label=
  while [ $# -gt 0 ]; do
    case "$1" in
      --label) label=$2; shift 2 ;;
      --agent|--cwd|--ephemeral|--cols|--rows) shift 2 ;;
      --) shift; break ;;
      *) shift ;;
    esac
  done
  run=${COMPOUND_RUN_DIR:?}
  mkdir -p "$run"
  name=s$$
  set -m
  (trap 'exit 0' TERM; while kill -0 "$OWNER" 2>/dev/null; do sleep 0.2; done) </dev/null >/dev/null 2>&1 &
  pid=$!
  set +m
  echo "$pid" > "$run/$name.sock"
  printf '%s' "$label" > "$run/$name.label"
  echo "$label $pid" >> "$run/spawns"
  printf 'sup %s\npid %s\nsocket %s\nrender %s\n' "$pid" "$pid" "$run/$name.sock" "$run/$name.rsock"
  ;;
vitals)
  pid=$(cat "$1" 2>/dev/null)
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "renderer_attached false"; echo "output_at 0"
  else
    echo "no session at $1" >&2; exit 1
  fi
  ;;
deliver)
  sock=$1; id=$2; d=$sock.orders
  mkdir -p "$d"
  pid=$(cat "$sock" 2>/dev/null)
  alive() { [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; }
  gone() { echo "no session at $sock" >&2; exit 1; }
  if [ -f "$d/$id" ]; then cat "$d/$id"; exit 0; fi
  if ! mkdir "$d/$id.asked" 2>/dev/null; then
    while [ ! -f "$d/$id" ]; do alive || gone; sleep 0.1; done
    cat "$d/$id"; exit 0
  fi
  alive || { rmdir "$d/$id.asked"; gone; }
  text=$(cat)
  echo "$id $text" >> "$d/typed"
  case "$text" in *slow*) sleep 2 ;; esac
  case "$text" in *busy*) ans="failed $id busy" ;; *) ans="delivered $id" ;; esac
  echo "$ans" > "$d/$id.tmp" && mv "$d/$id.tmp" "$d/$id"
  cat "$d/$id"
  ;;
*) echo "fake compound-sup: no verb $verb" >&2; exit 2 ;;
esac
"#;

/// A short exec dir (the fake's paths stay well inside a socket's 104 bytes)
/// and the fake beside it.
fn place(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("wm-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let sup = base.join(format!("wm-{tag}-{}.sup", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    fs::write(&sup, FAKE_SUP.replace("__OWNER__", &std::process::id().to_string())).unwrap();
    fs::set_permissions(&sup, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, sup)
}

/// Ends what a test started, however it ends: the fake's sessions, and a
/// manager run as its own process.
struct Sweep {
    run: PathBuf,
    manager: Option<Child>,
}

impl Sweep {
    fn new(dir: &Path) -> Sweep {
        Sweep { run: dir.join(wsp_manager::seats::RUN_DIR), manager: None }
    }
}

impl Drop for Sweep {
    fn drop(&mut self) {
        if let Some(mut m) = self.manager.take() {
            let _ = m.kill();
            let _ = m.wait();
        }
        for line in fs::read_to_string(self.run.join("spawns")).unwrap_or_default().lines() {
            if let Some(pid) = line.split_whitespace().last() {
                let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stderr(Stdio::null()).status();
            }
        }
    }
}

fn start(dir: &Path, sup: &Path) -> Manager {
    let mut cfg = Config::new(dir, "test-host");
    cfg.port = 0;
    cfg.sup = Some(sup.to_path_buf());
    Manager::start(cfg).expect("the manager starts")
}

fn token(dir: &Path) -> Token {
    Token(fs::read_to_string(dir.join(TOKEN_FILE)).unwrap().trim().to_string())
}

fn connect(addr: SocketAddr, token: Token) -> (Tcp, ManagerHello) {
    let mut conn = Tcp::dial(addr, WAIT).unwrap();
    let hello = Hello::Wsp(WspHello { role: Role::Cli, protocol: PROTOCOL, version: "test".into(), last_event: None, token });
    conn.send(&Frame { id: None, req: Some(ReqId(1)), msg: Message::Hello(hello) }).unwrap();
    loop {
        match conn.recv(WAIT).unwrap() {
            Some(Frame { msg: Message::Beat(_), .. }) => continue,
            Some(Frame { req: Some(ReqId(1)), msg: Message::Hello(Hello::Manager(m)), .. }) => return (conn, m),
            other => panic!("expected the manager's hello, got {other:?}"),
        }
    }
}

/// A connection that numbers its requests and keeps every event it hears.
struct Client {
    conn: Tcp,
    next: u64,
    /// Every journalled frame heard, in order.
    events: Vec<Frame>,
}

impl Client {
    fn new(addr: SocketAddr, dir: &Path) -> (Client, ManagerHello) {
        let (conn, hello) = connect(addr, token(dir));
        (Client { conn, next: 10, events: Vec::new() }, hello)
    }

    fn recv(&mut self, wait: Duration) -> Option<Frame> {
        let until = Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.conn.recv(left) {
                Ok(Some(Frame { msg: Message::Beat(_), .. })) => continue,
                Ok(Some(f)) => {
                    if f.id.is_some() {
                        self.events.push(f.clone());
                    }
                    return Some(f);
                }
                Ok(None) => return None,
                Err(ConnError::Closed) => panic!("the manager closed the connection"),
                Err(e) => panic!("{e}"),
            }
        }
    }

    fn send(&mut self, msg: Message) -> ReqId {
        self.next += 1;
        let req = ReqId(self.next);
        self.conn.send(&Frame { id: None, req: Some(req), msg }).unwrap();
        req
    }

    /// The next frame answering `req`.
    fn answer(&mut self, req: ReqId) -> Message {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if let Some(f) = self.recv(until.saturating_duration_since(Instant::now())) {
                if f.req == Some(req) {
                    return f.msg;
                }
            }
        }
        panic!("no answer to {req:?} within {WAIT:?}");
    }

    fn ask(&mut self, msg: Message) -> Message {
        let req = self.send(msg);
        self.answer(req)
    }

    fn subscribe(&mut self, from: Option<EventId>) {
        self.conn.send(&Frame { id: None, req: None, msg: Message::Subscribe(Subscribe { from }) }).unwrap();
    }

    /// Hear events until `done` holds of what was heard, or `wait` passes.
    fn hear_until(&mut self, wait: Duration, done: impl Fn(&[Frame]) -> bool) {
        let until = Instant::now() + wait;
        while !done(&self.events) && Instant::now() < until {
            self.recv(Duration::from_millis(100));
        }
    }
}

fn spawn_msg(row: &str, attempt: u32, seat: &str, cwd: &Path) -> Message {
    Message::SeatSpawn(SeatSpawn {
        key: Key { row: RowId(row.into()), attempt },
        seat: SeatId(seat.into()),
        agent: AgentSpec { argv: vec!["true".into()], env: vec!["HOME".into()], kind: "claude".into(), model: None },
        workdir: Workdir::Cwd(cwd.display().to_string()),
        gate: Gate::None,
        first_order: None,
    })
}

fn stop_msg(seat: &str, epoch: u64) -> Message {
    Message::SeatStop(SeatStop { seat: SeatId(seat.into()), epoch: Epoch(epoch), grace_secs: 1 })
}

fn order(id: &str, seat: &str, epoch: u64, text: &str) -> Message {
    Message::OrderDeliver(OrderDeliver { order: OrderId(id.into()), seat: SeatId(seat.into()), epoch: Epoch(epoch), text: text.into(), now: false })
}

fn spawned(run: &Path) -> Vec<String> {
    fs::read_to_string(run.join("spawns")).unwrap_or_default().lines().map(str::to_string).collect()
}

/// The lines the fake typed into any session, for order `id`.
fn typed(run: &Path, id: &str) -> usize {
    let mut n = 0;
    for e in fs::read_dir(run).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().ends_with(".orders") {
            let t = fs::read_to_string(e.path().join("typed")).unwrap_or_default();
            n += t.lines().filter(|l| l.split_whitespace().next() == Some(id)).count();
        }
    }
    n
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

#[test]
fn two_spawns_with_one_key_give_one_seat() {
    let (dir, sup) = place("key");
    let _sweep = Sweep::new(&dir);
    let m = start(&dir, &sup);
    let run = dir.join(wsp_manager::seats::RUN_DIR);
    let (mut a, _) = Client::new(m.addrs()[0], &dir);
    let (mut b, _) = Client::new(m.addrs()[0], &dir);

    // Both asked before either is answered: one fork, one answer for both.
    let ra = a.send(spawn_msg("wsp-1", 1, "cpd-1", &dir));
    let rb = b.send(spawn_msg("wsp-1", 1, "cpd-1", &dir));
    let (Message::SeatStarted(first), Message::SeatStarted(second)) = (a.answer(ra), b.answer(rb)) else {
        panic!("both spawns are answered seat.started");
    };
    assert_eq!(first, second, "a retry gets the same seat.started");
    assert_eq!(first.epoch, Epoch(1));
    assert!(alive(first.pid), "the seat's session runs");
    // A retry after the answer: the same again.
    assert_eq!(a.ask(spawn_msg("wsp-1", 1, "cpd-1", &dir)), Message::SeatStarted(first.clone()));
    assert_eq!(spawned(&run).len(), 1, "one key started one session: {:?}", spawned(&run));

    let Message::SeatsList(list) = b.ask(Message::Seats(Seats {})) else { panic!("seats is answered seats.list") };
    assert_eq!(list.seats.len(), 1);
    assert_eq!((list.seats[0].seat.0.as_str(), list.seats[0].epoch, list.seats[0].state), ("cpd-1", Epoch(1), SeatState::Idle));
    assert!(list.seats[0].rss_bytes > 0, "rss from the one ps");

    // The same key for another seat, and the same seat under another key: refused.
    match a.ask(spawn_msg("wsp-1", 1, "cpd-2", &dir)) {
        Message::SeatRefused(r) => assert_eq!(r.why, Refusal::Duplicate, "{}", r.detail),
        other => panic!("expected seat.refused duplicate, got {other:?}"),
    }
    match a.ask(spawn_msg("wsp-1", 2, "cpd-1", &dir)) {
        Message::SeatRefused(r) => assert_eq!(r.why, Refusal::Duplicate, "{}", r.detail),
        other => panic!("expected seat.refused duplicate, got {other:?}"),
    }
    assert_eq!(spawned(&run).len(), 1);

    // Stopped: the pids it ended, and the key cannot be answered started again.
    match a.ask(stop_msg("cpd-1", 1)) {
        Message::SeatStopped(s) => {
            assert_eq!((s.seat.0.as_str(), s.epoch), ("cpd-1", Epoch(1)));
            assert!(s.ended.contains(&first.pid), "{:?} ends {}", s.ended, first.pid);
        }
        other => panic!("expected seat.stopped, got {other:?}"),
    }
    assert!(!alive(first.pid));
    match a.ask(spawn_msg("wsp-1", 1, "cpd-1", &dir)) {
        Message::SeatRefused(r) => assert_eq!(r.why, Refusal::Duplicate, "{}", r.detail),
        other => panic!("a stopped key is not started again, got {other:?}"),
    }
    let Message::SeatsList(list) = a.ask(Message::Seats(Seats {})) else { panic!() };
    assert!(list.seats.is_empty());
    m.stop();
}

#[test]
fn a_stop_or_an_order_naming_a_stale_epoch_is_refused() {
    let (dir, sup) = place("epoch");
    let _sweep = Sweep::new(&dir);
    let m = start(&dir, &sup);
    let (mut c, _) = Client::new(m.addrs()[0], &dir);

    let Message::SeatStarted(one) = c.ask(spawn_msg("wsp-2", 1, "cpd-e", &dir)) else { panic!("started") };
    assert_eq!(one.epoch, Epoch(1));
    assert!(matches!(c.ask(stop_msg("cpd-e", 1)), Message::SeatStopped(_)));
    // The same seat started again, under the row's next attempt: epoch 2.
    let Message::SeatStarted(two) = c.ask(spawn_msg("wsp-2", 2, "cpd-e", &dir)) else { panic!("started again") };
    assert_eq!(two.epoch, Epoch(2));

    match c.ask(stop_msg("cpd-e", 1)) {
        Message::Refused(r) => {
            assert_eq!(r.why, Refusal::StaleEpoch);
            assert!(r.detail.contains("older"), "the refusal says why: {}", r.detail);
        }
        other => panic!("a stop naming epoch 1 is refused, got {other:?}"),
    }
    match c.ask(order("o-stale", "cpd-e", 1, "for the old start")) {
        Message::OrderRefused(r) => assert_eq!(r.why, OrderFail::StaleEpoch),
        other => panic!("an order naming epoch 1 is refused, got {other:?}"),
    }
    assert!(alive(two.pid), "the stale stop ended nothing");
    assert_eq!(m.seats().iter().map(|s| (s.seat.0.as_str(), s.epoch)).collect::<Vec<_>>(), vec![("cpd-e", Epoch(2))]);
    match c.ask(stop_msg("cpd-e", 2)) {
        Message::SeatStopped(s) => assert_eq!(s.epoch, Epoch(2)),
        other => panic!("expected seat.stopped, got {other:?}"),
    }
    m.stop();
}

#[test]
fn an_order_answers_delivered_then_acked() {
    let (dir, sup) = place("order");
    let _sweep = Sweep::new(&dir);
    let m = start(&dir, &sup);
    let run = dir.join(wsp_manager::seats::RUN_DIR);
    let (mut c, _) = Client::new(m.addrs()[0], &dir);
    c.subscribe(None);
    let Message::SeatStarted(s) = c.ask(spawn_msg("wsp-3", 1, "cpd-o", &dir)) else { panic!("started") };

    let req = c.send(order("o-1", "cpd-o", s.epoch.0, "do the work"));
    match (c.answer(req), c.answer(req)) {
        (Message::OrderDelivered(d), Message::OrderAcked(a)) => {
            assert_eq!((d.order.0.as_str(), d.seat.0.as_str()), ("o-1", "cpd-o"));
            assert_eq!(a.order.0, "o-1");
            assert!(a.turn_began.0 > 0);
        }
        other => panic!("expected order.delivered then order.acked, got {other:?}"),
    }
    c.hear_until(WAIT, |ev| ev.iter().any(|f| matches!(&f.msg, Message::OrderAcked(_))));
    c.hear_until(Duration::from_millis(300), |_| false);
    let kinds: Vec<&str> = c
        .events
        .iter()
        .map(|f| match &f.msg {
            Message::SeatStarted(_) => "seat.started",
            Message::OrderDelivered(_) => "order.delivered",
            Message::OrderAcked(_) => "order.acked",
            Message::Turn(_) => "turn",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["seat.started", "order.delivered", "order.acked"], "each event once, in order; a turn is the hooks' to say");

    // Asked again: the same answer, and nothing typed twice.
    let req = c.send(order("o-1", "cpd-o", s.epoch.0, "do the work"));
    assert!(matches!(c.answer(req), Message::OrderDelivered(_)));
    assert!(matches!(c.answer(req), Message::OrderAcked(_)));
    assert_eq!(typed(&run, "o-1"), 1);

    // compound-sup's FailWhy, as it said it.
    match c.ask(order("o-2", "cpd-o", s.epoch.0, "busy, try later")) {
        Message::OrderRefused(r) => assert_eq!(r.why, OrderFail::Busy),
        other => panic!("expected order.refused busy, got {other:?}"),
    }
    // Withdrawn before it was asked: refused cancelled, and never typed.
    match c.ask(Message::OrderCancel(OrderCancel { order: OrderId("o-3".into()), seat: s.seat.clone() })) {
        Message::OrderRefused(r) => assert_eq!(r.why, OrderFail::Cancelled),
        other => panic!("expected order.refused cancelled, got {other:?}"),
    }
    match c.ask(order("o-3", "cpd-o", s.epoch.0, "too late")) {
        Message::OrderRefused(r) => assert_eq!(r.why, OrderFail::Cancelled),
        other => panic!("a cancelled order stays cancelled, got {other:?}"),
    }
    assert_eq!(typed(&run, "o-3"), 0);
    let Message::SeatsList(list) = c.ask(Message::Seats(Seats {})) else { panic!() };
    assert_eq!(list.seats[0].state, SeatState::Working, "an acked order began a turn");
    assert!(list.seats[0].last_turn.is_some());
    assert!(fs::read_dir(dir.join(wsp_manager::seats::ORDERS_DIR)).map_or(true, |d| d.count() == 0), "no order is left in flight");

    assert!(matches!(c.ask(stop_msg("cpd-o", s.epoch.0)), Message::SeatStopped(_)));
    m.stop();
}

/// wsp-032: one owner of `turn`. An order delivered to a seat, then the seat's
/// hook command posting `UserPromptSubmit` and `Stop` to this manager, give
/// exactly one `turn` began, carrying the order's id, and one `turn` ended.
#[test]
fn a_delivered_order_and_its_hooks_journal_one_turn_began_with_the_order_and_one_ended() {
    use wsp_host::hooks::{report, Dial, Sent};
    use wsp_host_api::TurnEdge;

    let (dir, sup) = place("turn");
    let _sweep = Sweep::new(&dir);
    let m = start(&dir, &sup);
    let (mut c, _) = Client::new(m.addrs()[0], &dir);
    c.subscribe(None);
    let Message::SeatStarted(s) = c.ask(spawn_msg("wsp-32", 1, "cpd-t", &dir)) else { panic!("started") };

    let req = c.send(order("o-t", "cpd-t", s.epoch.0, "do the work"));
    assert!(matches!(c.answer(req), Message::OrderDelivered(_)));
    assert!(matches!(c.answer(req), Message::OrderAcked(_)));

    // The seat's hook command, as `wsp report <hook>` runs it.
    let dial = Dial::Manager { addr: m.addrs()[0], token_file: dir.join(TOKEN_FILE) };
    let payload = serde_json::json!({ "session_id": "sess-t" });
    assert_eq!(report(&dial, "cpd-t", s.epoch, "UserPromptSubmit", &payload), Sent::Taken(1));
    assert_eq!(state_in(&mut c, "cpd-t"), SeatState::Working, "a turn its hooks opened reads working");
    assert_eq!(report(&dial, "cpd-t", s.epoch, "Stop", &payload), Sent::Taken(1));
    // wsp-038: the order's ack alone left this Working for good.
    assert_eq!(state_in(&mut c, "cpd-t"), SeatState::Idle, "its Stop reads idle, over the order's ack");

    c.hear_until(WAIT, |ev| ev.iter().any(|f| matches!(&f.msg, Message::Turn(t) if t.edge == TurnEdge::Ended)));
    c.hear_until(Duration::from_millis(300), |_| false);
    let turns: Vec<_> = c
        .events
        .iter()
        .filter_map(|f| match &f.msg {
            Message::Turn(t) => Some((t.seat.0.as_str(), t.epoch, t.edge, t.order.as_ref().map(|o| o.0.as_str()))),
            _ => None,
        })
        .collect();
    assert_eq!(
        turns,
        vec![("cpd-t", s.epoch, TurnEdge::Began, Some("o-t")), ("cpd-t", s.epoch, TurnEdge::Ended, None)],
        "one began with the order, one ended: {:?}",
        c.events
    );

    assert!(matches!(c.ask(stop_msg("cpd-t", s.epoch.0)), Message::SeatStopped(_)));
    m.stop();
}

/// The state the manager's `seats` reply gives `seat`: what `wip` reads.
fn state_in(c: &mut Client, seat: &str) -> SeatState {
    let Message::SeatsList(list) = c.ask(Message::Seats(Seats {})) else { panic!("seats is answered seats.list") };
    list.seats.iter().find(|r| r.seat.0 == seat).map(|r| r.state).unwrap_or_else(|| panic!("{seat} is listed: {list:?}"))
}

/// wsp-038: `wip` drew seats mid-turn as `idle ← needs you`. A turn the
/// person typed at the pane has no order and so no ack: the seat's hooks are
/// all that say it is working. A tool hook with its prompt unheard (a manager
/// started mid-turn) says so too, and makes no `turn` of its own.
#[test]
fn a_turn_with_no_order_reads_working_while_open_and_idle_at_its_stop() {
    use wsp_host::hooks::{report, Dial, Sent};

    let (dir, sup) = place("open");
    let _sweep = Sweep::new(&dir);
    let m = start(&dir, &sup);
    let (mut c, _) = Client::new(m.addrs()[0], &dir);
    let Message::SeatStarted(s) = c.ask(spawn_msg("wsp-38", 1, "cpd-p", &dir)) else { panic!("started") };
    assert_eq!(state_in(&mut c, "cpd-p"), SeatState::Idle, "nothing heard yet");

    let dial = Dial::Manager { addr: m.addrs()[0], token_file: dir.join(TOKEN_FILE) };
    let payload = serde_json::json!({ "session_id": "sess-p" });
    assert_eq!(report(&dial, "cpd-p", s.epoch, "PreToolUse", &payload), Sent::Taken(0));
    assert_eq!(state_in(&mut c, "cpd-p"), SeatState::Working, "a tool running is a turn open");
    assert_eq!(report(&dial, "cpd-p", s.epoch, "Stop", &payload), Sent::Taken(1));
    assert_eq!(state_in(&mut c, "cpd-p"), SeatState::Idle, "a Stop reads idle");

    assert_eq!(report(&dial, "cpd-p", s.epoch, "UserPromptSubmit", &payload), Sent::Taken(1));
    assert_eq!(state_in(&mut c, "cpd-p"), SeatState::Working, "a prompt the person typed, without its Stop, reads working");
    // A child session's Stop, posted under the seat's name, ends nothing.
    assert_eq!(report(&dial, "cpd-p", s.epoch, "Stop", &serde_json::json!({ "session_id": "child" })), Sent::Taken(0));
    assert_eq!(state_in(&mut c, "cpd-p"), SeatState::Working);

    assert!(matches!(c.ask(stop_msg("cpd-p", s.epoch.0)), Message::SeatStopped(_)));
    m.stop();
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The manager as its own process, so it can be killed with SIGKILL.
fn run_manager(dir: &Path, sup: &Path, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_wsp-manager"))
        .arg("--exec-dir")
        .arg(dir)
        .args(["--host", "test-host", "--port", &port.to_string(), "--no-tailnet", "--compound-sup"])
        .arg(sup)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("wsp-manager runs")
}

/// Dial until the manager answers.
fn reach(port: u16, dir: &Path) -> (Client, ManagerHello) {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let until = Instant::now() + WAIT;
    loop {
        if dir.join(TOKEN_FILE).exists() && Tcp::dial(addr, Duration::from_millis(200)).is_ok() {
            return Client::new(addr, dir);
        }
        assert!(Instant::now() < until, "the manager did not answer on {addr}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_manager_killed_mid_order_restarts_with_its_seat_and_answers_the_order_once() {
    let (dir, sup) = place("kill");
    let mut sweep = Sweep::new(&dir);
    let run = dir.join(wsp_manager::seats::RUN_DIR);
    let port = free_port();
    sweep.manager = Some(run_manager(&dir, &sup, port));
    let (mut c, _) = reach(port, &dir);
    c.subscribe(None);
    let Message::SeatStarted(s) = c.ask(spawn_msg("wsp-4", 1, "cpd-k", &dir)) else { panic!("started") };

    // An order compound-sup takes two seconds over; the manager is killed once
    // it has asked.
    c.send(order("o-slow", "cpd-k", s.epoch.0, "slow work"));
    let until = Instant::now() + WAIT;
    while !fs::read_dir(&run).unwrap().flatten().any(|e| e.path().join("o-slow.asked").exists()) {
        assert!(Instant::now() < until, "the order was never asked of compound-sup");
        std::thread::sleep(Duration::from_millis(20));
    }
    c.hear_until(Duration::from_millis(200), |_| false);
    let last = c.events.last().and_then(|f| f.id).expect("seat.started was heard as an event");
    assert!(!c.events.iter().any(|f| matches!(f.msg, Message::OrderDelivered(_))), "killed before the answer");
    let mut m = sweep.manager.take().unwrap();
    m.kill().unwrap(); // SIGKILL
    m.wait().unwrap();
    assert!(alive(s.pid), "the seat outlives its manager");

    sweep.manager = Some(run_manager(&dir, &sup, port));
    let (mut back, hello) = reach(port, &dir);
    assert_eq!(hello.seats.iter().map(|e| (e.seat.0.as_str(), e.epoch)).collect::<Vec<_>>(), vec![("cpd-k", s.epoch)]);
    let Message::SeatsList(list) = back.ask(Message::Seats(Seats {})) else { panic!("seats.list") };
    assert_eq!(list.seats.len(), 1, "the restarted manager lists the seat");
    assert_eq!((list.seats[0].seat.0.as_str(), list.seats[0].epoch), ("cpd-k", s.epoch));

    // From the last id heard: the order's answer, once, and nothing before it.
    back.subscribe(Some(last));
    back.hear_until(WAIT, |ev| ev.iter().any(|f| matches!(&f.msg, Message::OrderAcked(a) if a.order.0 == "o-slow")));
    back.hear_until(Duration::from_millis(500), |_| false);
    assert!(back.events.iter().all(|f| f.id > Some(last)), "only events after {last:?}");
    let delivered = back.events.iter().filter(|f| matches!(&f.msg, Message::OrderDelivered(d) if d.order.0 == "o-slow")).count();
    let acked = back.events.iter().filter(|f| matches!(&f.msg, Message::OrderAcked(a) if a.order.0 == "o-slow")).count();
    assert_eq!((delivered, acked), (1, 1), "the order is answered once: {:?}", back.events);
    assert_eq!(back.events.first().map(|f| f.id), Some(Some(EventId(last.0 + 1))), "ids run on from the last");
    assert_eq!(typed(&run, "o-slow"), 1, "typed once, across the kill");

    // Asked again by wsp after the restart: the same answer, still typed once.
    let req = back.send(order("o-slow", "cpd-k", s.epoch.0, "slow work"));
    assert!(matches!(back.answer(req), Message::OrderDelivered(_)));
    assert_eq!(typed(&run, "o-slow"), 1);

    match back.ask(stop_msg("cpd-k", s.epoch.0)) {
        Message::SeatStopped(st) => assert!(st.ended.contains(&s.pid)),
        other => panic!("the adopted seat stops, got {other:?}"),
    }
    assert!(!alive(s.pid));
}

/// wsp-094: a seat whose session socket is gone while a process still runs
/// under its pid (after a restart the number is reused) is ended, not read
/// `starting` for good: the `seats` reply leaves it out and `seat.stopped` is
/// journalled. And after a manager restart, such a seat is not adopted.
#[test]
fn a_seat_whose_socket_is_gone_under_a_reused_pid_is_ended_not_starting() {
    let (dir, sup) = place("reused");
    let mut sweep = Sweep::new(&dir);
    let port = free_port();
    sweep.manager = Some(run_manager(&dir, &sup, port));
    let (mut c, _) = reach(port, &dir);
    c.subscribe(None);
    let Message::SeatStarted(a) = c.ask(spawn_msg("wsp-94", 1, "cpd-ra", &dir)) else { panic!("started") };
    let Message::SeatStarted(b) = c.ask(spawn_msg("wsp-94", 2, "cpd-rb", &dir)) else { panic!("started") };

    // In the manager's life: the socket goes, the pid runs on.
    fs::remove_file(&a.sockets[0]).unwrap();
    assert!(alive(a.pid), "the pid still runs, as a reused one would");
    let Message::SeatsList(list) = c.ask(Message::Seats(Seats {})) else { panic!("seats.list") };
    let listed: Vec<&str> = list.seats.iter().map(|r| r.seat.0.as_str()).collect();
    assert_eq!(listed, ["cpd-rb"], "a seat with no socket is not listed starting: {list:?}");
    c.hear_until(WAIT, |ev| ev.iter().any(|f| matches!(&f.msg, Message::SeatStopped(s) if s.seat.0 == "cpd-ra")));
    assert!(c.events.iter().any(|f| matches!(&f.msg, Message::SeatStopped(s) if s.seat.0 == "cpd-ra")), "seat.stopped journalled");

    // Across a restart: the manager killed, the other seat's socket gone too.
    let mut m = sweep.manager.take().unwrap();
    m.kill().unwrap();
    m.wait().unwrap();
    fs::remove_file(&b.sockets[0]).unwrap();
    sweep.manager = Some(run_manager(&dir, &sup, port));
    let (mut back, hello) = reach(port, &dir);
    assert!(hello.seats.is_empty(), "nothing adopted: {:?}", hello.seats);
    let Message::SeatsList(list) = back.ask(Message::Seats(Seats {})) else { panic!("seats.list") };
    assert!(list.seats.is_empty(), "the restarted manager lists neither: {list:?}");
}
