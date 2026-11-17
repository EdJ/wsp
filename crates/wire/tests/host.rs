//! wire's `Host` held to one contract under both implementations (wsp-025's
//! done-when): `host::InProcess`, and `host::Tcp` on a loopback manager. Both
//! run the same fake `compound-sup`, so the only difference is the path a call
//! takes: in this process, or as a 3.2 message to `wsp-manager`.
//!
//! Then what only the `Tcp` has: `WSP_HOST=manager` with no manager answering
//! is the named "host silent" error (never an empty answer), and the
//! subscription keeps its place per host and resubscribes `from` it after the
//! manager drops the connection.
//!
//! Then a spawn under `manager` (wsp-026, 4.3): `seat.spawn` carries the
//! work order as its first order, and `seat.started` and `order.acked` are
//! heard by subscription, with no `state` poll and no render (counted on a
//! wrapper of each); a refused or silent spawn is said, not hung. Under
//! `inproc` the backend has no such wait, and starts as before.
//!
//! Nothing here spawns claude: the fake's sessions are a sleeping process
//! group each, ended by the contract's stop, by [`Sweep`], or with this test
//! process at the latest.

use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use std::collections::BTreeSet;
use std::io::BufRead;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;
use wsp_base::util::Wall;
use wsp_host_api::{At, Epoch, EventId, Key, Message, RowId, Role, SeatId, SeatSaid, SeatState, Token};
use wsp_manager::{Config, Manager, TOKEN_FILE};
use wsp_wire::host::{self, Choice, Forked, Headless, Host, HostError, InProcess, Session, Spawn, Stop, Subscription, Tcp, LOCAL};
use wsp_wire::manager::{install_record, Local};
use wsp_wire::place::{
    new_order_id, Agent, Census, Delivery, Event, FirstOrder, Heard, Order, OrderId, Place, Refusal, Result as PResult, Seat, State,
};
use wsp_host::test_sups;
use wsp_wire::place_compound::Compound;

/// compound-sup as far as a seat's spawn, delivery and stop use it. `spawn`
/// prints the handshake and becomes the session itself, in a session and
/// process group of its own (as compound-sup daemonizes), so its pid is the
/// group a stop names under either host; its command still reads
/// `<sup> spawn --label <seat>`, as compound-sup's does, which is how a
/// manager finds the pid of a seat it adopts (wsp-029). `deliver <id>` answers once per id;
/// a text with `busy` fails `busy`.
const FAKE_SUP: &str = r#"#!/bin/sh
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
  echo "$$" > "$run/$name.sock"
  printf '%s' "$label" > "$run/$name.label"
  echo "$label $$" >> "$run/spawns"
  echo "$*" > "$run/$name.argv"
  printf 'sup %s\npid %s\nsocket %s\nrender %s\n' "$$" "$$" "$run/$name.sock" "$run/$name.rsock"
  exec perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die' sh -c "trap 'exit 0' TERM; while kill -0 $OWNER 2>/dev/null; do sleep 0.2; done" "$0" spawn --label "$label" </dev/null >/dev/null 2>&1
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
  if [ -f "$d/$id" ]; then cat "$d/$id"; exit 0; fi
  if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then echo "no session at $sock" >&2; exit 1; fi
  text=$(cat)
  echo "$id $text" >> "$d/typed"
  case "$text" in *slow*) sleep 2 ;; esac
  case "$text" in *busy*) ans="failed $id busy" ;; *) ans="delivered $id" ;; esac
  echo "$ans" > "$d/$id"
  cat "$d/$id"
  ;;
*) echo "fake compound-sup: no verb $verb" >&2; exit 2 ;;
esac
"#;

/// A short directory for one test (the fake's paths stay inside a socket's 104
/// bytes) and the fake beside it.
fn place(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("wh-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let sup = dir.join("sup");
    fs::write(&sup, FAKE_SUP.replace("__OWNER__", &std::process::id().to_string())).unwrap();
    fs::set_permissions(&sup, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, sup)
}

/// Ends the sessions a test started, however it ends.
#[derive(Default)]
struct Sweep(Mutex<Vec<u32>>);

impl Sweep {
    fn add(&self, pid: u32) {
        self.0.lock().unwrap().push(pid);
    }
}

impl Drop for Sweep {
    fn drop(&mut self) {
        for pid in self.0.lock().unwrap().drain(..) {
            LOCAL.signal_group(pid, "KILL");
        }
    }
}

fn manager(dir: &Path, sup: &Path, port: u16) -> Manager {
    let mut cfg = Config::new(&dir.join("exec"), "test-host");
    cfg.port = port;
    cfg.sup = Some(sup.to_path_buf());
    start(cfg)
}

/// `Manager::start`, which a restart in this same process may find
/// `Locked` by itself for a few milliseconds: a child another test is
/// forking holds a copy of the lock's descriptor until its `exec` closes it.
/// A manager restarted for real is a new process, and never meets this.
fn start(cfg: Config) -> Manager {
    let since = Instant::now();
    loop {
        match Manager::start(cfg.clone()) {
            Ok(m) => return m,
            Err(wsp_manager::StartError::Locked { holder, .. })
                if holder == std::process::id().to_string() && since.elapsed() < Duration::from_secs(2) =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("the manager starts on loopback: {e}"),
        }
    }
}

fn token(dir: &Path) -> Token {
    Token(fs::read_to_string(dir.join("exec").join(TOKEN_FILE)).unwrap().trim().to_string())
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "{what} did not happen within 10s");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The contract: what a caller may rely on from either host.
fn contract(host: &dyn Host, dir: &Path, sweep: &Sweep) {
    let seat = Seat::new(format!("cpd-t{}", std::process::id()));
    let env = BTreeMap::from([("WSP_ROW".to_string(), "wsp-025".to_string())]);
    let args = vec!["-c".to_string(), "true".to_string()];
    let spawn = Spawn { key: Key { row: RowId("wsp-025".into()), attempt: 0 }, seat: seat.as_str(), kind: Some("claude".into()), cwd: dir.display().to_string(), env, program: "sh", args: &args };

    // seat.spawn: a session with its socket, its pid running.
    let session = host.spawn_session(&spawn).expect("the seat starts");
    sweep.add(session.pid);
    assert!(session.socket.exists(), "{} was announced and is not there", session.socket.display());
    assert!(session.render.is_some(), "the renderer's socket was announced");
    wait_until("the session running", || host.is_alive(session.pid));

    // order.deliver: acked; asked again under its id, answered again, typed once.
    let wait = Duration::from_secs(5);
    assert_eq!(host.deliver(&session.socket, "o-1", "go and do it", wait, &seat), Ok(Delivery::Acked));
    assert_eq!(host.deliver(&session.socket, "o-1", "go and do it", wait, &seat), Ok(Delivery::Acked));
    let typed = fs::read_to_string(session.socket.with_extension("sock.orders").join("typed")).unwrap();
    assert_eq!(typed.lines().filter(|l| l.starts_with("o-1 ")).count(), 1, "{typed}");

    // order.refused: compound-sup's reason, as the in-process host words it.
    assert_eq!(host.deliver(&session.socket, "o-2", "busy now", wait, &seat), Err(Refusal::Failed(format!("{seat}: busy"))));

    // seat.stop: the group ended within the grace.
    host.stop(Stop { group: session.pid, watch: Some(session.pid), grace: Duration::from_secs(2), poll: Duration::from_millis(50), clock: &Wall });
    wait_until("the session ended", || !host.is_alive(session.pid));

    // An order to a seat that has gone is no seat, not a failure to type.
    assert_eq!(host.deliver(&session.socket, "o-3", "still there?", wait, &seat), Err(Refusal::NoSeat(seat.clone())));
}

#[test]
fn the_in_process_host_holds_the_contract() {
    let (dir, sup) = place("inproc");
    let sweep = Sweep::default();
    contract(&InProcess::with_sup(&sup), &dir, &sweep);
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_tcp_host_holds_the_contract_on_a_loopback_manager() {
    let (dir, sup) = place("tcp");
    let sweep = Sweep::default();
    let m = manager(&dir, &sup, 0);
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    contract(&tcp, &dir, &sweep);

    // It went through the manager: the manager's run dir has the session, the
    // seat's env crossed on its argv, and one connection carried every call.
    let run = dir.join("exec").join(wsp_manager::seats::RUN_DIR);
    let spawns = fs::read_to_string(run.join("spawns")).unwrap();
    assert_eq!(spawns.lines().count(), 1, "{spawns}");
    let argv = fs::read_dir(&run).unwrap().flatten().find(|e| e.path().extension().is_some_and(|x| x == "argv")).unwrap();
    let argv = fs::read_to_string(argv.path()).unwrap();
    assert!(argv.starts_with("env -- WSP_ROW=wsp-025 sh -c true"), "{argv}");
    assert_eq!(m.clients(), 1);
    drop(tcp);
    m.stop();
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// The key is the caller's (row, attempt), not the seat: the same key twice is
/// one seat, and the next attempt is another.
#[test]
fn spawns_under_one_key_over_tcp_give_one_seat_and_another_attempt_another() {
    let (dir, sup) = place("key");
    let sweep = Sweep::default();
    let m = manager(&dir, &sup, 0);
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let args = vec!["-c".to_string(), "true".to_string()];
    fn mk<'a>(dir: &Path, args: &'a [String], seat: &'a str, attempt: u32) -> Spawn<'a> {
        Spawn {
        key: Key { row: RowId("wsp-026".into()), attempt },
        seat,
        kind: Some("claude".into()),
        cwd: dir.display().to_string(),
        env: BTreeMap::new(),
        program: "sh",
        args,
        }
    }
    let spawn = |seat: &'static str, attempt: u32| mk(&dir, &args, seat, attempt);
    let first = tcp.spawn_session(&spawn("cpd-k1", 0)).expect("the first spawn");
    sweep.add(first.pid);
    let again = tcp.spawn_session(&spawn("cpd-k1", 0)).expect("the same key again");
    assert_eq!((again.pid, &again.socket), (first.pid, &first.socket), "one key, one seat");
    let run = dir.join("exec").join(wsp_manager::seats::RUN_DIR);
    let spawns = |run: &Path| fs::read_to_string(run.join("spawns")).unwrap().lines().count();
    assert_eq!(spawns(&run), 1, "the second spawn forked nothing");

    let next = tcp.spawn_session(&spawn("cpd-k2", 1)).expect("the next attempt");
    sweep.add(next.pid);
    assert_ne!(next.pid, first.pid, "another attempt is another agent");
    assert_eq!(spawns(&run), 2);
    drop(tcp);
    m.stop();
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// A host that counts what a poll for readiness would ask of it: the
/// renderer's screen and its reading of it. Everything else is the host it
/// wraps.
struct Counting<'h> {
    inner: &'h dyn Host,
    renders: AtomicU32,
}

impl Counting<'_> {
    fn renders(&self) -> u32 {
        self.renders.load(Ordering::SeqCst)
    }
}

impl Host for Counting<'_> {
    fn hears_orders(&self) -> bool {
        self.inner.hears_orders()
    }
    fn spawn_ordered(&self, spawn: &Spawn<'_>, order: &FirstOrder, wait: Duration) -> PResult<(Session, Heard)> {
        self.inner.spawn_ordered(spawn, order, wait)
    }
    fn await_order(&self, order: &OrderId, wait: Duration) -> PResult<Heard> {
        self.inner.await_order(order, wait)
    }

    fn spawn_session(&self, spawn: &Spawn<'_>) -> PResult<Session> {
        self.inner.spawn_session(spawn)
    }
    fn spawn_headless(&self, spawn: &Headless<'_>) -> PResult<Forked> {
        self.inner.spawn_headless(spawn)
    }
    fn stop(&self, stop: Stop<'_>) {
        self.inner.stop(stop)
    }
    fn stop_soon(&self, group: u32) {
        self.inner.stop_soon(group)
    }
    fn clear_socket(&self, socket: &Path, sidecars: &[&str]) {
        self.inner.clear_socket(socket, sidecars)
    }
    fn unlink(&self, path: &Path) {
        self.inner.unlink(path)
    }
    fn alive(&self, pids: &[u32]) -> BTreeSet<u32> {
        self.inner.alive(pids)
    }
    fn started(&self, pid: u32) -> std::result::Result<Option<String>, ()> {
        self.inner.started(pid)
    }
    fn shells(&self) -> Option<String> {
        self.inner.shells()
    }
    fn seat_memory(&self) -> Option<String> {
        self.inner.seat_memory()
    }
    fn disk_free(&self, path: &Path) -> Option<String> {
        self.inner.disk_free(path)
    }
    fn disk_use(&self, path: &Path) -> Option<String> {
        self.inner.disk_use(path)
    }
    fn load(&self) -> Option<String> {
        self.inner.load()
    }
    fn thermal(&self) -> Option<String> {
        self.inner.thermal()
    }
    fn memory_pressure(&self) -> Option<String> {
        self.inner.memory_pressure()
    }
    fn swap(&self) -> Option<String> {
        self.inner.swap()
    }
    fn pageouts(&self) -> Option<String> {
        self.inner.pageouts()
    }
    fn idle(&self) -> Option<String> {
        self.inner.idle()
    }
    fn tree_of(&self, root: u32) -> Vec<u32> {
        self.inner.tree_of(root)
    }
    fn table(&self, columns: &str, env: bool) -> Option<String> {
        self.inner.table(columns, env)
    }
    fn rows_of(&self, pids: &[u32], columns: &str) -> Option<String> {
        self.inner.rows_of(pids, columns)
    }
    fn signal(&self, pids: &[u32], sig: &str) -> Option<bool> {
        self.inner.signal(pids, sig)
    }
    fn signal_group(&self, group: u32, sig: &str) {
        self.inner.signal_group(group, sig)
    }
    fn cwds(&self, pids: Option<&[u32]>) -> Option<String> {
        self.inner.cwds(pids)
    }
    fn answers(&self, socket: &Path) -> bool {
        self.inner.answers(socket)
    }
    fn read_screen(&self, render: &Path) -> PResult<String> {
        self.renders.fetch_add(1, Ordering::SeqCst);
        self.inner.read_screen(render)
    }
    fn screen_state(&self, render: &Path, kind: &str) -> Option<State> {
        self.renders.fetch_add(1, Ordering::SeqCst);
        self.inner.screen_state(render, kind)
    }
    fn osc_title(&self, socket: &Path) -> Option<String> {
        self.renders.fetch_add(1, Ordering::SeqCst);
        self.inner.osc_title(socket)
    }
    fn can_deliver(&self) -> bool {
        self.inner.can_deliver()
    }
    fn deliver(&self, session: &Path, id: &str, text: &str, wait: Duration, seat: &Seat) -> PResult<Delivery> {
        self.inner.deliver(session, id, text, wait, seat)
    }
    fn type_in(&self, render: &Path, text: &str) -> PResult<()> {
        self.inner.type_in(render, text)
    }
    fn press_enter(&self, render: &Path) -> PResult<()> {
        self.inner.press_enter(render)
    }
    fn append_line(&self, file: &Path, line: &str) -> PResult<()> {
        self.inner.append_line(file, line)
    }
    fn said(&self, dir: &Path) -> Value {
        self.inner.said(dir)
    }
    fn say(&self, dir: &Path, said: &Value) {
        self.inner.say(dir, said)
    }
    fn log_hook(&self, dir: &Path, line: &Value) {
        self.inner.log_hook(dir, line)
    }
    fn quiet_since(&self, dir: &Path) -> Option<i64> {
        self.inner.quiet_since(dir)
    }
    fn tally(&self, dir: &Path, payload: &Value) {
        self.inner.tally(dir, payload)
    }
    fn bank(&self, dir: &Path) {
        self.inner.bank(dir)
    }
    fn burn_under(&self, root: &Path) -> Vec<(String, Value)> {
        self.inner.burn_under(root)
    }
    fn transcript_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.inner.transcript_of(session, cwd)
    }
    fn pi_session_of(&self, session: &str, cwd: &str) -> Option<PathBuf> {
        self.inner.pi_session_of(session, cwd)
    }
    fn read_transcript(&self, file: &Path) -> Option<Box<dyn BufRead>> {
        self.inner.read_transcript(file)
    }
    fn transcript_bytes(&self, file: &Path) -> Option<Vec<u8>> {
        self.inner.transcript_bytes(file)
    }
    fn transcript_tail(&self, file: &Path, bytes: u64) -> Option<String> {
        self.inner.transcript_tail(file, bytes)
    }
    fn opencode_export(&self, session: &str) -> Option<Vec<u8>> {
        self.inner.opencode_export(session)
    }
    fn opencode_newest(&self, session: &str) -> Option<Vec<Value>> {
        self.inner.opencode_newest(session)
    }
    fn turn_began(&self, dir: &Path, since: i64) -> Option<bool> {
        self.inner.turn_began(dir, since)
    }
    fn wire_health(&self, problems: &mut Vec<String>, notes: &mut Vec<String>) {
        self.inner.wire_health(problems, notes)
    }
}

/// A place that counts the readings a readiness or turn poll takes: its
/// `state`, and the prompt read off its screen. Everything else is the place
/// it wraps.
struct Polls<'p> {
    inner: &'p dyn Place,
    polls: AtomicU32,
}

impl Polls<'_> {
    fn polls(&self) -> u32 {
        self.polls.load(Ordering::SeqCst)
    }
}

impl Place for Polls<'_> {
    fn open(&self, o: &Order) -> PResult<Seat> {
        self.inner.open(o)
    }
    fn here(&self) -> Option<Seat> {
        self.inner.here()
    }
    fn start(&self, s: &Seat, a: &Agent) -> PResult<()> {
        self.inner.start(s, a)
    }
    fn start_ordered(&self, s: &Seat, a: &Agent, o: &FirstOrder, w: Duration) -> Option<PResult<Heard>> {
        self.inner.start_ordered(s, a, o, w)
    }
    fn await_order(&self, s: &Seat, o: &OrderId, w: Duration) -> Option<PResult<Heard>> {
        self.inner.await_order(s, o, w)
    }
    fn tell(&self, s: &Seat, t: &str) -> PResult<Delivery> {
        self.inner.tell(s, t)
    }
    fn nudge(&self, s: &Seat) -> PResult<()> {
        self.inner.nudge(s)
    }
    fn typed(&self, s: &Seat) -> Option<bool> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.inner.typed(s)
    }
    fn stop(&self, s: &Seat) -> PResult<()> {
        self.inner.stop(s)
    }
    fn state(&self, s: &Seat) -> PResult<State> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.inner.state(s)
    }
    fn census(&self) -> PResult<Census> {
        self.inner.census()
    }
    fn watch(&self, f: &mut dyn FnMut(Event) -> bool) -> PResult<()> {
        self.inner.watch(f)
    }
}

fn agent(attempt: u32) -> Agent {
    Agent { kind: "claude".into(), name: "wsp-026".into(), args: vec![], key: Some(Key { row: RowId("wsp-026".into()), attempt }) }
}

fn opened(place: &dyn Place, dir: &Path) -> Seat {
    place.open(&Order { label: "wsp-026".into(), cwd: Some(dir.display().to_string()), ..Order::default() }).expect("a seat")
}

/// Every order the fake compound-sup was asked to type, as `<id> <text>`.
fn typed_under(run: &Path) -> Vec<String> {
    fs::read_dir(run)
        .map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "orders")).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| fs::read_to_string(e.path().join("typed")).ok())
        .flat_map(|t| t.lines().map(str::to_string).collect::<Vec<_>>())
        .collect()
}

/// `WSP_HOST=manager`: the spawn is `seat.spawn` with the work order in it,
/// and the order's `order.acked` is heard on a subscription. No `state` is
/// polled and no screen is rendered on the way.
#[test]
fn a_spawn_under_manager_carries_its_order_and_hears_it_acked_with_no_poll_and_no_render() {
    let (dir, sup) = place("ord");
    let m = manager(&dir, &sup, 0);
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let host = Counting { inner: &tcp, renders: AtomicU32::new(0) };
    let compound = Compound { host: &host, ..Compound::at(dir.join("seats")) };
    let place = Polls { inner: &compound, polls: AtomicU32::new(0) };

    let seat = opened(&place, &dir);
    let order = FirstOrder { order: new_order_id(&seat), text: "go and do it".into() };
    let start = Instant::now();
    assert_eq!(place.start_ordered(&seat, &agent(0), &order, Duration::from_secs(20)), Some(Ok(Heard::Acked)));
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert_eq!(place.polls(), 0, "no state poll, no prompt read");
    assert_eq!(host.renders(), 0, "no screen rendered");

    // The manager typed it, once, under the id the spawn carried: it went in
    // the seat.spawn, not a later tell.
    let run = dir.join("exec").join(wsp_manager::seats::RUN_DIR);
    let typed = typed_under(&run);
    assert_eq!(typed, vec![format!("{} go and do it", order.order.0)], "{typed:?}");

    // An order the work refuses is heard refused, worded, and not hung on.
    let busy = opened(&place, &dir);
    let order = FirstOrder { order: new_order_id(&busy), text: "busy now".into() };
    let start = Instant::now();
    match place.start_ordered(&busy, &agent(1), &order, Duration::from_secs(20)) {
        Some(Ok(Heard::Refused(why))) => assert_eq!(why, format!("{busy}: busy")),
        other => panic!("{other:?}"),
    }
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());

    // A seat the manager refuses (the key already runs another seat) is the
    // spawn's error, said at once.
    let twin = opened(&place, &dir);
    let order = FirstOrder { order: new_order_id(&twin), text: "go".into() };
    match place.start_ordered(&twin, &agent(0), &order, Duration::from_secs(20)) {
        Some(Err(Refusal::Backend(why))) => assert!(why.contains("the manager refused seat") && why.contains("Duplicate"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(place.polls(), 0);
    assert_eq!(host.renders(), 0);

    for s in [&seat, &busy, &twin] {
        let _ = place.stop(s);
    }
    drop(tcp);
    m.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A manager that does not answer is "host silent" from the spawn, within
/// the second `SILENT` allows, and nothing is started in this process instead.
#[test]
fn a_spawn_under_a_silent_manager_is_host_silent_not_hung() {
    let (dir, sup) = place("osil");
    let tcp = Tcp::with_local(SocketAddr::from(([127, 0, 0, 1], free_port())), Token("t".into()), Role::Cli, "test", InProcess::with_sup(&sup));
    let compound = Compound { host: &tcp, ..Compound::at(dir.join("seats")) };
    let seat = opened(&compound, &dir);
    let order = FirstOrder { order: new_order_id(&seat), text: "go".into() };
    let start = Instant::now();
    match compound.start_ordered(&seat, &agent(0), &order, Duration::from_secs(20)) {
        Some(Err(Refusal::Unreachable(why))) => assert!(why.starts_with("host silent"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
    assert_eq!(compound.root(&seat), None, "nothing was started in this process in the manager's place");
    let _ = fs::remove_dir_all(&dir);
}

/// The manager gone after `seat.started` and before the first order's
/// `order.acked` (wsp-033): the seat runs, so the start stands, with its
/// record, and only the order is left open: unanswered at the spawn, and a
/// later wait says the host is silent rather than that nothing started.
#[test]
fn a_spawn_whose_manager_drops_after_seat_started_keeps_the_seat_and_leaves_the_order_open() {
    let (dir, sup) = place("odrop");
    let sweep = Sweep::default();
    let m = manager(&dir, &sup, 0);
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let compound = Compound { host: &tcp, ..Compound::at(dir.join("seats")) };
    let seat = opened(&compound, &dir);
    // The fake types a `slow` order two seconds late: the manager is stopped
    // once the seat is forked, inside that gap, so no ack is ever sent.
    let order = FirstOrder { order: new_order_id(&seat), text: "slow work".into() };
    let spawns = dir.join("exec").join(wsp_manager::seats::RUN_DIR).join("spawns");
    let stopper = std::thread::spawn(move || {
        wait_until("the seat forked", || spawns.exists());
        std::thread::sleep(Duration::from_millis(300));
        m.stop();
    });
    let start = Instant::now();
    let heard = compound.start_ordered(&seat, &agent(0), &order, Duration::from_secs(20));
    stopper.join().unwrap();
    assert_eq!(heard, Some(Ok(Heard::Unanswered)), "the seat started; only its order is unconfirmed");
    assert!(start.elapsed() < Duration::from_secs(15), "{:?}", start.elapsed());
    let pid = compound.root(&seat).expect("the seat's record is kept");
    sweep.add(pid);

    // A later wait on the order: the manager is still gone, which is said.
    match compound.await_order(&seat, &order.order, Duration::from_secs(2)) {
        Some(Err(Refusal::Unreachable(why))) => assert!(why.starts_with("host silent"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(compound.root(&seat), Some(pid), "and the record stands after it");
    drop(tcp);
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// `WSP_HOST=inproc`: the backend has no events to wait by, so it starts
/// nothing on `start_ordered` and the spawn goes the old way, which starts.
#[test]
fn a_spawn_in_process_has_no_ordered_start_and_starts_as_before() {
    let (dir, sup) = place("oin");
    let sweep = Sweep::default();
    let inproc = InProcess::with_sup(&sup);
    let compound = Compound { host: &inproc, ..Compound::at(dir.join("seats")) };
    let seat = opened(&compound, &dir);
    let order = FirstOrder { order: new_order_id(&seat), text: "go".into() };
    assert_eq!(compound.start_ordered(&seat, &agent(0), &order, Duration::from_secs(5)), None);
    compound.start(&seat, &agent(0)).expect("the old way starts");
    let pid = compound.root(&seat).expect("a session pid");
    sweep.add(pid);
    assert!(inproc.is_alive(pid));
    let _ = compound.stop(&seat);
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// `WSP_HOST=manager`: a stop is `seat.stop`, and what the stop says it ended
/// is the manager's `seat.stopped` pids, found by a process that started
/// nothing. A seat the manager does not run, and a manager that is gone, are
/// said plainly and clear nothing.
#[test]
fn a_stop_under_manager_reports_the_pids_seat_stopped_names() {
    let (dir, sup) = place("end");
    let m = manager(&dir, &sup, 0);
    let spawner = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let compound = Compound { host: &spawner, ..Compound::at(dir.join("seats")) };
    let seat = opened(&compound, &dir);
    let order = FirstOrder { order: new_order_id(&seat), text: "go".into() };
    assert_eq!(compound.start_ordered(&seat, &agent(0), &order, Duration::from_secs(20)), Some(Ok(Heard::Acked)));
    let pid = compound.root(&seat).expect("the session pid");

    // A second process: it knows the seat only from the manager's list.
    let other = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let despawner = Compound { host: &other, ..Compound::at(dir.join("seats")) };
    assert!(despawner.hosted());
    let ended = despawner.end(&seat).expect("the manager stops it");
    assert!(ended.contains(&pid), "the manager's pids name the session: {ended:?} vs {pid}");
    assert_eq!(despawner.root(&seat), None, "the seat's record went with it");

    // A seat the manager does not run (wsp-040): a terminal this process
    // opened and nothing started in under the manager. It is ended in-process,
    // and its record goes, rather than refused and left holding its claim.
    let bare = opened(&despawner, &dir);
    let bare_pid = despawner.root(&bare);
    let sweep = Sweep::default();
    if let Some(pid) = bare_pid {
        sweep.add(pid);
    }
    despawner.end(&bare).expect("an unmanaged seat ends in-process");
    assert_eq!(despawner.root(&bare), None, "the unmanaged seat's record went with it");
    if let Some(pid) = bare_pid {
        let gone = (0..100).any(|_| {
            let dead = !InProcess::with_sup(&sup).is_alive(pid);
            if !dead {
                std::thread::sleep(Duration::from_millis(50));
            }
            dead
        });
        assert!(gone, "the unmanaged seat's group was ended: {pid}");
    }
    drop(sweep);
    let ghost = Seat::new("cpd-nobody");

    drop((spawner, other));
    m.stop();

    // The manager gone: host silent, and the seat's record is still there.
    let gone = Tcp::with_local(SocketAddr::from(([127, 0, 0, 1], free_port())), Token("t".into()), Role::Cli, "test", InProcess::with_sup(&sup));
    match gone.end(ghost.as_str(), Stop { group: 1, watch: None, grace: Duration::from_millis(10), poll: Duration::from_millis(10), clock: &Wall }) {
        Err(Refusal::Unreachable(why)) => assert!(why.starts_with("host silent"), "{why}"),
        other => panic!("{other:?}"),
    }
    let _ = fs::remove_dir_all(&dir);
}

/// wsp-037: under the manager a start is judged by its events, as
/// `Tcp::await_up` hears them, never by a state read just after `seat.started`
/// (which a healthy seat failed, and was stopped and started again at epoch 2).
/// A plain spawn holds nothing open after it: `await_up` subscribes after the
/// start's own `seat.started`, so what came before it asked is replayed.
#[test]
fn under_manager_await_up_hears_a_start_by_its_first_turn_its_stop_or_neither() {
    use wsp_host::hooks::{report, Dial, Sent};
    use wsp_wire::place::Up;

    let (dir, sup) = place("up");
    let sweep = Sweep::default();
    let m = manager(&dir, &sup, 0);
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let dial = Dial::Manager { addr: m.addrs()[0], token_file: dir.join("exec").join(TOKEN_FILE) };
    let args = vec!["-c".to_string(), "true".to_string()];
    let spawn = |seat: &'static str, attempt: u32| Spawn {
        key: Key { row: RowId(format!("wsp-u-{seat}")), attempt },
        seat,
        kind: Some("claude".into()),
        cwd: dir.display().to_string(),
        env: BTreeMap::new(),
        program: "sh",
        args: &args,
    };
    let epoch_of = |seat: &str| tcp.seats().unwrap().into_iter().find(|r| r.seat.0 == seat).map(|r| r.epoch).expect("the manager runs it");
    let run = dir.join("exec").join(wsp_manager::seats::RUN_DIR);
    let spawns_of = |seat: &str| fs::read_to_string(run.join("spawns")).unwrap_or_default().lines().filter(|l| l.split(' ').next() == Some(seat)).count();
    let wait = Duration::from_secs(5);

    // Without `by_turn`, `seat.started` is the seat up: nothing to hear.
    let plain = tcp.spawn_session(&spawn("cpd-u0", 0)).expect("the seat starts");
    sweep.add(plain.pid);
    assert_eq!(m.clients(), 1, "a plain spawn holds no second connection open");
    assert_eq!(tcp.await_up("cpd-u0", false, wait), Ok(Up::Ready));
    assert_eq!(m.clients(), 1, "and asks nothing more");

    // With it: the first turn begun, posted by the seat's hook before anyone
    // asked, is ready. One spawn, one `seat.started`, no `seat.stop`.
    let up = tcp.spawn_session(&spawn("cpd-u1", 0)).expect("the seat starts");
    sweep.add(up.pid);
    let epoch = epoch_of("cpd-u1");
    let payload = serde_json::json!({ "session_id": "sess-u1" });
    assert_eq!(report(&dial, "cpd-u1", epoch, "UserPromptSubmit", &payload), Sent::Taken(1));
    assert_eq!(tcp.await_up("cpd-u1", true, wait), Ok(Up::Ready));
    assert_eq!(spawns_of("cpd-u1"), 1, "started once");
    assert!(tcp.is_alive(up.pid), "the healthy start is left running");
    let mut all = Subscription::after(m.addrs()[0], token(&dir), Role::Cli, "test", EventId(0)).unwrap();
    let mut started = 0;
    let mut stopped = 0;
    while let Some(f) = all.next(Duration::from_millis(300)).unwrap() {
        match f.msg {
            Message::SeatStarted(s) if s.seat.0 == "cpd-u1" => started += 1,
            Message::SeatStopped(s) if s.seat.0 == "cpd-u1" => stopped += 1,
            _ => {}
        }
    }
    drop(all);
    assert_eq!((started, stopped), (1, 0), "one seat.started, epoch {epoch:?}, and no seat.stopped");

    // A start that exits before its first turn is the start that died.
    let dies = tcp.spawn_session(&spawn("cpd-u2", 0)).expect("the seat starts");
    sweep.add(dies.pid);
    // The pid itself, not its group: the fake may not have called setsid yet.
    let killed = std::process::Command::new("kill").args(["-KILL", &dies.pid.to_string()]).status().unwrap();
    assert!(killed.success());
    assert_eq!(tcp.await_up("cpd-u2", true, wait), Ok(Up::Stopped));

    // Silence is neither, at the wait: a turn for another seat, or for an
    // earlier start of this one, is not this start's.
    let mute = tcp.spawn_session(&spawn("cpd-u3", 0)).expect("the seat starts");
    sweep.add(mute.pid);
    let mute_epoch = epoch_of("cpd-u3");
    assert_eq!(report(&dial, "cpd-u1", epoch, "Stop", &payload), Sent::Taken(1));
    assert_eq!(report(&dial, "cpd-u1", epoch, "UserPromptSubmit", &payload), Sent::Taken(1), "cpd-u1's next turn begins");
    // Whatever the manager makes of a hook under an epoch it never started,
    // it is not this start's first turn.
    let _ = report(&dial, "cpd-u3", Epoch(mute_epoch.0 + 7), "UserPromptSubmit", &payload);
    let start = Instant::now();
    assert_eq!(tcp.await_up("cpd-u3", true, Duration::from_millis(600)), Ok(Up::Unready));
    assert!(start.elapsed() >= Duration::from_millis(600), "waited the whole wait: {:?}", start.elapsed());
    assert!(tcp.is_alive(mute.pid), "judging it touches nothing");

    drop(tcp);
    m.stop();
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// wsp-040: a verifier spawned on a row whose member seat has stopped. The
/// member ran under the row's key (`wsp-038`, 0), so once it stopped that key
/// is spent and a spawn under it is refused, as `spawn wsp-038 --agent --verify`
/// was live. The verifier goes out keyless (cmd_spawn never keys a verifier by
/// the row) after `open`'s bare terminal ran under the seat's `(seat, 0)`, and
/// is accepted under `(seat, 1)`, not refused as `(seat, 0)` already run. Its
/// first start stopped and the seat left standing, as wsp-037's retry leaves
/// one, the re-start is accepted under `(seat, 2)`. Read off the journal.
#[test]
fn a_verifier_on_a_row_whose_member_stopped_is_accepted_and_so_is_its_restart() {
    let (dir, sup) = place("vrf");
    let sweep = Sweep::default();
    let m = manager(&dir, &sup, 0);
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let compound = Compound { host: &tcp, ..Compound::at(dir.join("seats")) };
    let keyed = |key: Option<Key>| Agent { kind: "claude".into(), name: "wsp-038".into(), args: vec![], key };
    let row = |attempt| Some(Key { row: RowId("wsp-038".into()), attempt });
    let refused_as_ran = |said: PResult<()>| match said {
        Err(Refusal::Backend(why)) => assert!(why.contains("Duplicate") && why.contains("already ran"), "{why}"),
        other => panic!("{other:?}"),
    };

    // The member: started under the row's key, then stopped.
    let member = opened(&compound, &dir);
    compound.start(&member, &keyed(row(0))).expect("the member starts");
    sweep.add(compound.root(&member).expect("the member's pid"));
    compound.end(&member).expect("the member's seat stops");
    let spent = opened(&compound, &dir);
    if let Some(pid) = compound.root(&spent) {
        sweep.add(pid);
    }
    refused_as_ran(compound.start(&spent, &keyed(row(0))));
    let _ = compound.end(&spent);

    // The verifier, keyless: accepted.
    let verifier = opened(&compound, &dir);
    let agent = keyed(None);
    compound.start(&verifier, &agent).expect("the verifier is accepted with the member's key spent");
    let first = compound.root(&verifier).expect("the verifier's pid");
    sweep.add(first);

    // Its first start stopped and the seat left standing, as a start judged
    // dead is: the same keyless agent again is the seat's next attempt.
    tcp.stop(Stop { group: first, watch: Some(first), grace: Duration::from_secs(2), poll: Duration::from_millis(50), clock: &Wall });
    wait_until("the verifier's first start ended", || !tcp.is_alive(first));
    compound.start(&verifier, &agent).expect("the verifier's re-start is accepted under the next key");
    let second = compound.root(&verifier).expect("the re-started pid");
    sweep.add(second);
    assert_ne!(second, first, "a re-start is another agent");
    let rows = tcp.seats().expect("the manager answers seats");
    assert!(rows.iter().any(|r| r.seat.0 == verifier.as_str()), "the manager runs the verifier: {rows:?}");
    let journal = fs::read_to_string(dir.join("exec").join(wsp_manager::JOURNAL_FILE)).unwrap();
    let keys: Vec<Value> = journal
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|f| f["type"] == "seat.started" && f["seat"] == verifier.as_str())
        .map(|f| f["key"].clone())
        .collect();
    let at = |attempt: u32| serde_json::json!({ "row": verifier.as_str(), "attempt": attempt });
    assert_eq!(keys, vec![at(0), at(1), at(2)], "open's terminal, the verifier, its re-start: {journal}");

    compound.end(&verifier).expect("the verifier stops");
    drop(tcp);
    m.stop();
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// wsp-040: `wsp despawn` of a seat started in-process (`cpd-250`) while the
/// manager runs, which the manager does not run and never adopted. The
/// despawn's hosted place ends it through the in-process half, with its group
/// among the pids, its session gone and its record removed, so the despawn
/// goes on to release the claim; never "the manager does not run" and the
/// claim kept.
#[test]
fn despawn_of_a_seat_the_manager_does_not_run_ends_it_in_process() {
    let (dir, sup) = place("unrun");
    let sweep = Sweep::default();
    let m = manager(&dir, &sup, 0);
    let inproc = InProcess::with_sup(&sup);
    let here = Compound { host: &inproc, ..Compound::at(dir.join("seats")).insisting() };
    let seat = opened(&here, &dir);
    here.start(&seat, &agent(0)).expect("the seat starts in-process");
    let pid = here.root(&seat).expect("a session pid");
    sweep.add(pid);

    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let despawner = Compound { host: &tcp, ..Compound::at(dir.join("seats")) };
    assert!(despawner.hosted());
    let rows = tcp.seats().expect("the manager answers seats");
    assert!(rows.iter().all(|r| r.seat.0 != seat.as_str()), "the manager does not run {seat}: {rows:?}");

    let ended = despawner.end(&seat).expect("a seat the manager does not run is ended, not refused");
    assert!(ended.contains(&pid), "its group was ended: {ended:?} vs {pid}");
    assert_eq!(despawner.root(&seat), None, "its record went with it");
    wait_until("the in-process session ended", || !inproc.is_alive(pid));

    drop(tcp);
    m.stop();
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// A port nothing listens on: bound, then let go.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn record(state: &Path, exec: &Path, port: u16) {
    fs::create_dir_all(exec).unwrap();
    fs::write(exec.join(TOKEN_FILE), "some-token\n").unwrap();
    let mut local = Local::new(exec, port);
    local.host = "test-host".into();
    install_record(state, &local, Duration::from_secs(1)).unwrap();
}

#[test]
fn wsp_host_manager_with_no_manager_running_is_host_silent() {
    let (dir, _) = place("silent");
    let state = dir.join("state");
    record(&state, &dir.join("exec"), free_port());

    // The default is the manager (wsp-029); `inproc` is the rollback, and asks nobody.
    assert_eq!(Choice::parse(None), Ok(Choice::Manager));
    assert_eq!(Choice::parse(Some("")), Ok(Choice::Manager));
    assert_eq!(Choice::default(), Choice::Manager);
    assert_eq!(Choice::parse(Some("manager")), Ok(Choice::Manager));
    assert_eq!(Choice::parse(Some("inproc")), Ok(Choice::Inproc));
    assert!(matches!(Choice::parse(Some("tcp")), Err(HostError::Choice(_))));
    assert!(host::choose(Choice::Inproc, &state, "test").is_ok());

    let err = host::choose(Choice::Manager, &state, "test").err().expect("no manager answers");
    assert!(matches!(err, HostError::Silent { .. }), "{err:?}");
    assert!(err.to_string().starts_with("host silent: "), "{err}");

    // Something that accepts and never says hello is silent too, within the second.
    let mute = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = mute.local_addr().unwrap().port();
    record(&state, &dir.join("exec"), port);
    let start = Instant::now();
    let err = host::choose(Choice::Manager, &state, "test").err().expect("a mute listener is no manager");
    assert!(matches!(err, HostError::Silent { .. }), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());

    // A call through the trait says it as a refusal, never as an empty answer.
    let tcp = Tcp::from_record(&state, Role::Cli, "test").unwrap();
    let args: Vec<String> = vec![];
    let spawn = Spawn { key: Key { row: RowId("cpd-x".into()), attempt: 0 }, seat: "cpd-x", kind: None, cwd: String::new(), env: BTreeMap::new(), program: "true", args: &args };
    match tcp.spawn_session(&spawn) {
        Err(Refusal::Unreachable(why)) => assert!(why.starts_with("host silent"), "{why}"),
        other => panic!("{:?}", other.map(|s| s.pid)),
    }
    assert!(matches!(tcp.seats(), Err(HostError::Silent { .. })));
    drop(mute);

    // No record at all is said as that.
    let none = dir.join("nostate");
    assert!(matches!(host::choose(Choice::Manager, &none, "test"), Err(HostError::NoRecord(_))));
    let _ = fs::remove_dir_all(&dir);
}

fn said(m: &Manager, n: u64) -> EventId {
    m.emit(Message::SeatSaid(SeatSaid { seat: SeatId("cpd-s".into()), epoch: Epoch(1), at: At(n), text: format!("word {n}") })).unwrap()
}

/// The ids of the events heard within `wait`, beats left out.
fn heard(sub: &mut Subscription, want: usize) -> Vec<u64> {
    let mut ids = vec![];
    let start = Instant::now();
    while ids.len() < want && start.elapsed() < Duration::from_secs(10) {
        if let Some(f) = sub.next(Duration::from_millis(500)).unwrap() {
            ids.extend(f.id.map(|i| i.0));
        }
    }
    ids
}

#[test]
fn the_subscription_resubscribes_from_the_last_id_after_a_dropped_connection() {
    let (dir, sup) = place("sub");
    let state = dir.join("state");
    let m = manager(&dir, &sup, 0);
    let addr: SocketAddr = m.addrs()[0];
    let tok = token(&dir);
    let first: Vec<u64> = (1..=3).map(|n| said(&m, n).0).collect();

    let mut sub = Subscription::new(addr, tok.clone(), "test-host", &state, "test");
    assert_eq!(heard(&mut sub, 3), first);
    assert_eq!(sub.subscribed, 1);

    // The manager goes and comes back on the same port and journal: the
    // connection drops, and the next ask subscribes again from the last id.
    m.stop();
    let m = manager(&dir, &sup, addr.port());
    let later: Vec<u64> = (4..=5).map(|n| said(&m, n).0).collect();
    assert_eq!(heard(&mut sub, 2), later, "only what came after the last id heard");
    assert_eq!(sub.subscribed, 2);
    sub.close();
    assert_eq!(wsp_wire::host_tcp::read_last(&state, "test-host"), Some(EventId(*later.last().unwrap())));

    // A new subscription (the next daemon) starts where the last left off.
    let next = said(&m, 6).0;
    let mut again = Subscription::new(addr, tok, "test-host", &state, "test");
    assert_eq!(heard(&mut again, 1), vec![next]);
    drop(again);
    m.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// `WSP_HOST=manager`: a tell is `order.deliver`, acked and typed once; to a
/// manager that is stopped it is `Unreachable` (which `wsp tell` holds), and
/// asked again under the same order id once the manager answers, it is typed
/// once more at most and never twice under one id.
#[test]
fn a_tell_under_manager_is_delivered_and_a_silent_manager_is_unreachable_then_delivered_on_return() {
    let (dir, sup) = place("tell");
    let m = manager(&dir, &sup, 0);
    let port = m.addrs()[0].port();
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let compound = Compound { host: &tcp, ..Compound::at(dir.join("seats")).insisting() };
    let seat = opened(&compound, &dir);
    let order = FirstOrder { order: new_order_id(&seat), text: "go".into() };
    assert_eq!(compound.start_ordered(&seat, &agent(0), &order, Duration::from_secs(20)), Some(Ok(Heard::Acked)));

    assert_eq!(compound.tell(&seat, "first sentence"), Ok(Delivery::Acked));
    let run = dir.join("exec").join(wsp_manager::seats::RUN_DIR);
    assert!(typed_under(&run).iter().any(|t| t.ends_with(" first sentence")), "{:?}", typed_under(&run));

    // The manager stopped: silent, and the tell is Unreachable — the refusal
    // `wsp tell` holds on.
    drop(tcp);
    m.stop();
    let gone = Tcp::with_local(SocketAddr::from(([127, 0, 0, 1], port)), Token("t".into()), Role::Cli, "test", InProcess::with_sup(&sup));
    let silent = Compound { host: &gone, ..Compound::at(dir.join("seats")).insisting() };
    match silent.tell(&seat, "held sentence") {
        Err(Refusal::Unreachable(_)) => {}
        other => panic!("{other:?}"),
    }
    assert!(!typed_under(&run).iter().any(|t| t.ends_with(" held sentence")));
    drop(gone);

    // It is back: the flush's ask is delivered, once.
    let m = manager(&dir, &sup, port);
    let back = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let again = Compound { host: &back, ..Compound::at(dir.join("seats")).insisting() };
    assert_eq!(again.tell(&seat, "held sentence"), Ok(Delivery::Acked));
    let n = typed_under(&run).iter().filter(|t| t.ends_with(" held sentence")).count();
    assert_eq!(n, 1, "{:?}", typed_under(&run));

    let _ = again.stop(&seat);
    drop(back);
    m.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// wsp-029, 6.2: a seat the in-process host started before the switch is in
/// the manager's `seats` once the manager starts (its table rebuilt from the
/// session sockets, the in-process run directory's among them), and is told
/// and stopped through the manager as a seat of its own.
#[test]
fn a_seat_started_in_process_is_adopted_by_the_manager_then_told_and_stopped_through_it() {
    let (dir, sup) = place("adin");
    let sweep = Sweep::default();
    let inproc = InProcess::with_sup(&sup);
    let before = Compound { host: &inproc, ..Compound::at(dir.join("seats")).insisting() };
    // A label no other test's session carries: the manager finds an adopted
    // seat's compound-sup by `spawn --label <seat>` in one `ps`, and every
    // test's fake is a `sup` running `cpd-1` in this process's run directory,
    // so two under one label are left alone (it does not guess).
    fs::create_dir_all(dir.join("seats")).unwrap();
    fs::write(dir.join("seats").join("next"), std::process::id().to_string()).unwrap();
    let seat = opened(&before, &dir);
    assert_eq!(seat.as_str(), format!("cpd-{}", std::process::id()));
    before.start(&seat, &agent(0)).expect("the seat starts in-process");
    let pid = before.root(&seat).expect("a session pid");
    sweep.add(pid);
    assert_eq!(before.tell(&seat, &format!("{seat} before the switch")), Ok(Delivery::Acked));
    // The bare shell `open` stood in the seat ends a moment after `start`
    // replaced it; until then two sessions carry the label, and a manager
    // adopts neither (it does not guess). A seat at the switch is long started.
    let spawns = test_sups::run_dir().join("spawns");
    let others = || -> Vec<u32> {
        let text = fs::read_to_string(&spawns).unwrap_or_default();
        text.lines()
            .filter_map(|l| l.split_once(' ').filter(|(label, _)| *label == seat.as_str()).and_then(|(_, p)| p.trim().parse().ok()))
            .filter(|p| *p != pid)
            .collect()
    };
    assert!(!others().is_empty(), "open's bare shell was spawned under the label");
    wait_until("the bare shell ended", || others().iter().all(|p| !inproc.is_alive(*p)));

    // The switch: the manager starts, and reads the in-process host's run
    // directory as well as its own.
    let mut cfg = Config::new(&dir.join("exec"), "test-host");
    cfg.port = 0;
    cfg.sup = Some(sup.clone());
    cfg.inproc_runs = vec![test_sups::run_dir()];
    let m = Manager::start(cfg).expect("the manager starts on loopback");
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let after = Compound { host: &tcp, ..Compound::at(dir.join("seats")).insisting() };
    assert!(after.hosted());

    let rows = tcp.seats().expect("the manager answers seats");
    let row = rows.iter().find(|r| r.seat.0 == seat.as_str()).unwrap_or_else(|| panic!("{seat} adopted: {rows:?}"));
    assert_eq!((row.epoch, row.state), (Epoch(1), SeatState::Idle), "{row:?}");
    // Its `seat.started` is journalled with no row: the manager cannot know
    // it, and the daemon's fold claims nothing for an empty one.
    let journal = fs::read_to_string(dir.join("exec").join(wsp_manager::JOURNAL_FILE)).unwrap();
    let keys: Vec<Value> = journal
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|f| f["type"] == "seat.started" && f["seat"] == seat.as_str())
        .map(|f| f["key"].clone())
        .collect();
    assert_eq!(keys, vec![serde_json::json!({ "row": "", "attempt": 0 })], "{journal}");

    // Told through the manager: `order.deliver`, typed into the same session.
    assert_eq!(after.tell(&seat, &format!("{seat} after the switch")), Ok(Delivery::Acked));
    let typed = typed_under(&test_sups::run_dir());
    for said in ["before", "after"] {
        let n = typed.iter().filter(|t| t.ends_with(&format!(" {seat} {said} the switch"))).count();
        assert_eq!(n, 1, "{said}: {typed:?}");
    }

    // Stopped through it: `seat.stop` ends the session's group.
    let ended = after.end(&seat).expect("the manager stops it");
    assert!(ended.contains(&pid), "the manager's pids name the session: {ended:?} vs {pid}");
    assert!(!inproc.is_alive(pid));
    assert!(tcp.seats().unwrap().iter().all(|r| r.seat.0 != seat.as_str()), "gone from seats");

    drop(tcp);
    m.stop();
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}

/// wsp-029, 6.2's rollback: with `WSP_HOST=inproc` again, a seat the manager
/// started (compound-sup outlives the manager) is adopted by its socket, the
/// one its record holds, and keeps working: told in this process, and
/// stopped by its group.
#[test]
fn with_inproc_again_a_seat_the_manager_started_is_adopted_by_its_socket() {
    let (dir, sup) = place("adout");
    let sweep = Sweep::default();
    let m = manager(&dir, &sup, 0);
    let tcp = Tcp::with_local(m.addrs()[0], token(&dir), Role::Cli, "test", InProcess::with_sup(&sup));
    let before = Compound { host: &tcp, ..Compound::at(dir.join("seats")).insisting() };
    let seat = opened(&before, &dir);
    let order = FirstOrder { order: new_order_id(&seat), text: "go".into() };
    assert_eq!(before.start_ordered(&seat, &agent(0), &order, Duration::from_secs(20)), Some(Ok(Heard::Acked)));
    let pid = before.root(&seat).expect("the session pid");
    sweep.add(pid);

    // The rollback: the manager is gone, and the host is this process's.
    assert_eq!(Choice::parse(Some("inproc")), Ok(Choice::Inproc));
    drop(tcp);
    m.stop();
    let inproc = InProcess::with_sup(&sup);
    let after = Compound { host: &inproc, ..Compound::at(dir.join("seats")).insisting() };
    assert!(!after.hosted());
    assert!(inproc.is_alive(pid), "the seat outlived its manager");
    assert!(after.state(&seat).expect("its record").is_running());

    assert_eq!(after.tell(&seat, "after the rollback"), Ok(Delivery::Acked));
    let run = dir.join("exec").join(wsp_manager::seats::RUN_DIR);
    let typed = typed_under(&run);
    assert_eq!(typed.iter().filter(|t| t.ends_with(" after the rollback")).count(), 1, "{typed:?}");

    after.stop(&seat).expect("stopped in-process");
    wait_until("the session ended", || !inproc.is_alive(pid));
    drop(sweep);
    let _ = fs::remove_dir_all(&dir);
}
