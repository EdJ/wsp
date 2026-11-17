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

use wsp_base::util::Wall;
use wsp_host_api::{At, Epoch, EventId, Key, Message, RowId, Role, SeatId, SeatSaid, Token};
use wsp_manager::{Config, Manager, TOKEN_FILE};
use wsp_wire::host::{self, Choice, Host, HostError, InProcess, Spawn, Stop, Subscription, Tcp, LOCAL};
use wsp_wire::manager::{install_record, Local};
use wsp_wire::place::{Delivery, Refusal, Seat};

/// compound-sup as far as a seat's spawn, delivery and stop use it. `spawn`
/// prints the handshake and becomes the session itself, in a session and
/// process group of its own (as compound-sup daemonizes), so its pid is the
/// group a stop names under either host. `deliver <id>` answers once per id;
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
  exec perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die' sh -c "trap 'exit 0' TERM; while kill -0 $OWNER 2>/dev/null; do sleep 0.2; done" </dev/null >/dev/null 2>&1
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
    Manager::start(cfg).expect("the manager starts on loopback")
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

    // The default is in-process, and asks nobody.
    assert_eq!(Choice::parse(None), Ok(Choice::Inproc));
    assert_eq!(Choice::parse(Some("manager")), Ok(Choice::Manager));
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
