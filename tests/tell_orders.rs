//! `wsp tell` under `WSP_ORDERS=store` (wsp-124), end to end: a `wsp daemon`
//! under `WSP_HOST=manager` against a loopback `wsp-manager` running a fake
//! `compound-sup`, the rig `tests/orders.rs` runs (copied, as each test binary
//! is its own crate).
//!
//! - `a_tell_under_store_says_what_files_says_and_is_one_acknowledged_order`:
//!   from a seat's shell, `wsp tell <seat> -` to an idle seat under `store`
//!   prints the same line, and with `--json` the same bytes, as the same tell
//!   under `files`; the store holds one order for the tell, `acknowledged`, and
//!   the files tell writes none.
//!
//! The answer's half (an answer to a busy asker is one order, its `Sent` hop
//! written once the order is acknowledged) is `cmd_message::tests`, against
//! the same orders machine and the daemon's own delivery worker.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use wsp_client::Client;
use wsp_host_api::{At, Epoch, Message, Role, SeatId as HostSeat, Token, Turn, TurnEdge};
use wsp_manager::{Config, Manager, TOKEN_FILE};
use wsp_store_contract::{OrderState, Principal, SeatState};
use wsp_wire::host::{InProcess, Tcp};
use wsp_wire::manager::{install_record, Local};
use wsp_wire::place::{new_order_id, Agent, FirstOrder, Heard, Order, Place};
use wsp_wire::place_compound::Compound;

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

/// The bound on a wait for something that is not under test (the daemon up,
/// the fold caught up): generous, and each wait ends as soon as it is seen.
const BUDGET: Duration = Duration::from_secs(60);

/// compound-sup as far as a seat's spawn, `vitals` and `deliver` use it.
/// `deliver <sock> <id>` answers an id once, and keeps it: asked again, it
/// gives the same answer and types nothing. Every typing is a line
/// `<id> <text>` in `<sock>.orders/typed`.
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
  printf '%s' "${WSP_SEAT_EPOCH:-0}" > "$run/$name.epoch"
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
  echo "delivered $id" > "$d/$id"
  cat "$d/$id"
  ;;
*) echo "fake compound-sup: no verb $verb" >&2; exit 2 ;;
esac
"#;

struct Rig {
    dir: PathBuf,
    state: PathBuf,
    manager: Option<Manager>,
    daemon: Option<Child>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        if let Some(m) = self.manager.take() {
            m.stop();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// `wsp` on this rig's store, as no seat, with nothing of the caller's `WSP_*`.
fn wsp(state: &Path) -> Command {
    let mut cmd = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        cmd.env_remove(k);
    }
    cmd.env("WSP_STATE", state).env("WSP_HOME", state.parent().unwrap().join("home"));
    cmd
}

impl Rig {
    /// A manager on loopback with the fake supervisor, the machine record
    /// naming it, an initialised store, and `wsp daemon` on it under `orders`.
    fn up(tag: &str, orders: Option<&str>) -> Rig {
        let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("wt-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let state = dir.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(dir.join("home")).unwrap();
        wsp_testkit::script(&dir.join("sup"), &FAKE_SUP.replace("__OWNER__", &std::process::id().to_string()));

        let mut cfg = Config::new(&dir.join("exec"), "test-host");
        cfg.port = 0;
        cfg.sup = Some(dir.join("sup"));
        let manager = Manager::start(cfg).expect("the manager starts on loopback");
        let mut local = Local::new(&dir.join("exec"), manager.addrs()[0].port());
        local.host = "test-host".into();
        install_record(&state, &local, Duration::from_secs(5)).unwrap();
        let init = wsp(&state).arg("init").output().unwrap();
        assert!(init.status.success(), "wsp init: {}", String::from_utf8_lossy(&init.stderr));

        let mut d = wsp(&state);
        d.arg("daemon")
            .env("WSP_HOST", "manager")
            .env("WSP_API_PORT", free_port().to_string())
            .env("COMPOUND_SUP", dir.join("sup"))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(o) = orders {
            d.env("WSP_ORDERS", o);
        }
        let daemon = d.spawn().expect("wsp daemon starts");
        Rig { dir, state, manager: Some(manager), daemon: Some(daemon) }
    }

    fn manager(&self) -> &Manager {
        self.manager.as_ref().unwrap()
    }

    fn token(&self) -> Token {
        Token(fs::read_to_string(self.dir.join("exec").join(TOKEN_FILE)).unwrap().trim().to_string())
    }

    /// A client of the daemon's API, once it listens.
    fn client(&self, by: Principal) -> Client {
        let end = Instant::now() + BUDGET;
        while !self.state.join("api.json").exists() {
            assert!(Instant::now() < end, "the daemon's API did not come up: {}", self.cycle_log());
            std::thread::sleep(Duration::from_millis(50));
        }
        Client::dial(&self.state, by, BUDGET).expect("the daemon's API answers")
    }

    fn cycle_log(&self) -> String {
        fs::read_to_string(self.state.join("cycle.log")).unwrap_or_default()
    }

    /// Every `<id> <text>` the fake supervisor typed.
    fn typed(&self) -> Vec<String> {
        let run = self.dir.join("exec").join(wsp_manager::seats::RUN_DIR);
        fs::read_dir(&run)
            .map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "orders")).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| fs::read_to_string(e.path().join("typed")).ok())
            .flat_map(|t| t.lines().map(str::to_string).collect::<Vec<_>>())
            .collect()
    }
}

fn seat_state(client: &mut Client, seat: &str) -> Option<SeatState> {
    let reply = client.request("seats", json!({})).ok()?;
    match reply.outcome {
        wsp_api::Outcome::Ok(wsp_api::Answer::Seats(seats)) => seats.into_iter().find(|s| s.id.0 == seat).map(|s| s.state),
        _ => None,
    }
}


/// A seat the manager runs, spawned and given its first order as `wsp spawn`
/// makes one, then read idle by the store.
fn idle_seat(rig: &Rig, client: &mut Client) -> (Tcp, wsp_wire::place::Seat) {
    let tcp = Tcp::with_local(rig.manager().addrs()[0], rig.token(), Role::Cli, "test", InProcess::with_sup(&rig.dir.join("sup")));
    let compound = Compound { host: &tcp, ..Compound::at(rig.state.join(wsp_wire::place_compound::SEATS)) };
    let seat = compound
        .open(&Order { label: "wsp-124".into(), cwd: Some(rig.dir.display().to_string()), ..Order::default() })
        .expect("a seat");
    let agent = Agent {
        kind: "claude".into(),
        name: "wsp-124".into(),
        args: vec![],
        key: Some(wsp_host_api::Key { row: wsp_host_api::RowId("wsp-124".into()), attempt: 0 }),
    };
    let first = FirstOrder { order: new_order_id(&seat), text: "your work".into() };
    assert_eq!(compound.start_ordered(&seat, &agent, &first, Duration::from_secs(20)), Some(Ok(Heard::Acked)));
    turn_ended(rig, &seat);
    let end = Instant::now() + BUDGET;
    while seat_state(client, seat.as_str()) != Some(SeatState::Idle) {
        assert!(Instant::now() < end, "the seat never read idle in the store: {}", rig.cycle_log());
        std::thread::sleep(Duration::from_millis(50));
    }
    (tcp, seat)
}

/// The seat's turn ends: the hook's word in it, and the manager's `turn`.
fn turn_ended(rig: &Rig, seat: &wsp_wire::place::Seat) {
    let said = rig.state.join(wsp_wire::place_compound::SEATS).join(seat.as_str()).join("said.json");
    fs::write(&said, json!({ "state": "idle", "at": wsp_base::util::now_iso() }).to_string()).unwrap();
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    rig.manager()
        .emit(Message::Turn(Turn { seat: HostSeat(seat.as_str().into()), epoch: Epoch(0), edge: TurnEdge::Ended, at: At(now_ms), order: None }))
        .unwrap();
}

/// `wsp tell <seat> -` from the shell of seat `cpd-from`, under `orders`
/// (`None`: unset, which is `store` under the manager since wsp-128), `extra`
/// after the text's dash.
fn tell(rig: &Rig, seat: &str, text: &str, orders: Option<&str>, extra: &[&str]) -> (i32, String, String) {
    use std::io::Write;
    let mut cmd = wsp(&rig.state);
    cmd.args(["tell", seat, "-"])
        .args(extra)
        .env("WSP_HOST", "manager")
        .env("COMPOUND_SUP", rig.dir.join("sup"))
        .env("WSP_SEAT_ID", "cpd-from")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(o) = orders {
        cmd.env("WSP_ORDERS", o);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The seat's orders the tells made: every one but the spawn's first.
fn tells_ordered(client: &mut Client, seat: &str) -> Vec<wsp_store_contract::Order> {
    let reply = client.request("orders", json!({ "seat": seat, "states": [] })).unwrap();
    match reply.outcome {
        wsp_api::Outcome::Ok(wsp_api::Answer::Orders(orders)) => orders.into_iter().filter(|o| !o.first).collect(),
        other => panic!("orders: {other:?}"),
    }
}

#[test]
fn a_tell_under_store_says_what_files_says_and_is_one_acknowledged_order() {
    let rig = Rig::up("tell", Some("store"));
    let mut ed = rig.client(Principal::Person("ed".into()));
    let (_tcp, seat) = idle_seat(&rig, &mut ed);
    let s = seat.as_str();

    // The line: under store, then the same sentence under files (`--again`,
    // past the repeat guard the first one armed).
    let (code, stored, err) = tell(&rig, s, "the line", Some("store"), &[]);
    assert_eq!(code, 0, "under store: {stored}{err}\n{}", rig.cycle_log());
    let ordered = tells_ordered(&mut ed, s);
    assert_eq!(ordered.len(), 1, "one order for one tell: {ordered:?}");
    assert_eq!(ordered[0].state, OrderState::Acknowledged, "{ordered:?}");
    assert!(ordered[0].text.contains("the line"), "{ordered:?}");

    turn_ended(&rig, &seat);
    let (code, filed, err) = tell(&rig, s, "the line", Some("files"), &["--again"]);
    assert_eq!(code, 0, "under files: {filed}{err}");
    assert_eq!(stored, filed, "the same line under store as under files");
    assert!(stored.contains("delivered"), "acknowledged prints as delivered: {stored}");
    assert_eq!(tells_ordered(&mut ed, s).len(), 1, "the files tell writes no order");

    // `--json`: the same bytes, a fresh sentence each way.
    turn_ended(&rig, &seat);
    let end = Instant::now() + BUDGET;
    while seat_state(&mut ed, s) != Some(SeatState::Idle) {
        assert!(Instant::now() < end, "the seat never read idle again: {}", rig.cycle_log());
        std::thread::sleep(Duration::from_millis(50));
    }
    let (code, stored, err) = tell(&rig, s, "the json", Some("store"), &["--json"]);
    assert_eq!(code, 0, "under store --json: {stored}{err}\n{}", rig.cycle_log());
    turn_ended(&rig, &seat);
    let (code, filed, err) = tell(&rig, s, "the json", Some("files"), &["--json", "--again"]);
    assert_eq!(code, 0, "under files --json: {filed}{err}");
    assert_eq!(stored, filed, "--json is byte-identical");
    let v: serde_json::Value = serde_json::from_str(stored.trim()).unwrap();
    assert_eq!((v["told"].as_bool(), v["turn"].as_bool()), (Some(true), Some(true)), "{v}");

    let ordered = tells_ordered(&mut ed, s);
    assert_eq!(ordered.len(), 2, "one order per tell under store: {ordered:?}");
    assert!(ordered.iter().all(|o| o.state == OrderState::Acknowledged), "{ordered:?}");
    let typed: Vec<String> = rig.typed().into_iter().filter(|t| t.contains("the line") || t.contains("the json")).collect();
    assert_eq!(typed.len(), 4, "each tell typed once: {typed:?}");
}
