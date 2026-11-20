//! The orders door and the delivery worker (wsp-122), end to end: a `wsp
//! daemon` under `WSP_HOST=manager` against a loopback `wsp-manager` that runs
//! a fake `compound-sup` (nothing spawns claude), as wsp-028's tests run it.
//!
//! - `under_store_an_order_sent_from_outside_reaches_the_seat_once_and_is_acknowledged`:
//!   with `WSP_ORDERS=store`, an `order.send` from this process (a CLI process
//!   in the API's sense: `wsp-client`'s `apply`, which is `orders::send`'s path
//!   outside the daemon) is typed into the seat once, under the store's order
//!   id, and a subscriber reads `queued` → `typed` → `delivered` →
//!   `acknowledged` within 2 s of the seat reading idle.
//! - `with_orders_unset_no_order_is_written`: the daemon's domain mirrors
//!   orders as before, `order.send` is refused `illegal`, state `mirrored`,
//!   and the store holds no order.
//!
//! The worker's own cases (busy, three refusals, a worker killed mid-delivery,
//! the stale token) are `deliver::tests`, on the same machine with a scripted
//! supervisor.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use wsp_client::{Client, Item};
use wsp_host_api::{At, Epoch, Message, Role, SeatId as HostSeat, Token, Turn, TurnEdge};
use wsp_manager::{Config, Manager, TOKEN_FILE};
use wsp_store_contract::{Command as Cmd, EventKind, OrderId, OrderState, Principal, Refusal, SeatId, SeatState, Subject};
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
        let dir = base.join(format!("wo-{tag}-{}", std::process::id()));
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

#[test]
fn under_store_an_order_sent_from_outside_reaches_the_seat_once_and_is_acknowledged() {
    let rig = Rig::up("store", Some("store"));
    let mut ed = rig.client(Principal::Person("ed".into()));
    let watcher = rig.client(Principal::Person("watcher".into()));

    // A seat the manager runs, spawned for delivery and given its first order,
    // as `wsp spawn` makes one; its record is in the store's seat directory, where
    // the daemon's compound reads it.
    let tcp = Tcp::with_local(rig.manager().addrs()[0], rig.token(), Role::Cli, "test", InProcess::with_sup(&rig.dir.join("sup")));
    let compound = Compound { host: &tcp, ..Compound::at(rig.state.join(wsp_wire::place_compound::SEATS)) };
    let seat = compound
        .open(&Order { label: "wsp-122".into(), cwd: Some(rig.dir.display().to_string()), ..Order::default() })
        .expect("a seat");
    let agent = Agent {
        kind: "claude".into(),
        name: "wsp-122".into(),
        args: vec![],
        key: Some(wsp_host_api::Key { row: wsp_host_api::RowId("wsp-122".into()), attempt: 0 }),
    };
    let first = FirstOrder { order: new_order_id(&seat), text: "your work".into() };
    assert_eq!(compound.start_ordered(&seat, &agent, &first, Duration::from_secs(20)), Some(Ok(Heard::Acked)));

    // Its turn ends: the hook's word in the seat, and the manager's `turn`,
    // which the daemon's fold mirrors as `idle`.
    let said = rig.state.join(wsp_wire::place_compound::SEATS).join(seat.as_str()).join("said.json");
    fs::write(&said, json!({ "state": "idle", "at": wsp_base::util::now_iso() }).to_string()).unwrap();
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    rig.manager()
        .emit(Message::Turn(Turn { seat: HostSeat(seat.as_str().into()), epoch: Epoch(0), edge: TurnEdge::Ended, at: At(now_ms), order: None }))
        .unwrap();
    let end = Instant::now() + BUDGET;
    while seat_state(&mut ed, seat.as_str()) != Some(SeatState::Idle) {
        assert!(Instant::now() < end, "the seat never read idle in the store: {}", rig.cycle_log());
        std::thread::sleep(Duration::from_millis(50));
    }
    let idle_at = Instant::now();

    // From this process, through the API: the door's path outside the daemon.
    let mut feed = watcher.subscribe(Some(ed.head()));
    let to = SeatId(seat.as_str().into());
    ed.apply(&Cmd::OrderSend { to: to.clone(), text: "the second order".into(), first: false }).expect("order.send applies");

    let mut id: Option<OrderId> = None;
    let mut kinds = Vec::new();
    let mut whys = Vec::new();
    while !kinds.contains(&EventKind::OrderAcknowledged) {
        let left = Duration::from_secs(2).saturating_sub(idle_at.elapsed());
        assert!(!left.is_zero(), "not acknowledged within 2 s of idle: {kinds:?} {whys:?}\n{}", rig.cycle_log());
        match feed.next_within(left) {
            Some(Item::Event(e)) if matches!(&e.subject, Subject::Order(o) if o.0.starts_with(&to.0) && !o.0.contains("-first-")) => {
                let Subject::Order(o) = &e.subject else { unreachable!() };
                if e.kind == EventKind::OrderQueued {
                    id = Some(o.clone());
                }
                if id.as_ref() == Some(o) {
                    kinds.push(e.kind);
                    if e.kind == EventKind::OrderRefused {
                        whys.push(e.payload.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        kinds,
        [EventKind::OrderQueued, EventKind::OrderTyped, EventKind::OrderDelivered, EventKind::OrderAcknowledged]
    );
    let id = id.unwrap();
    let typed: Vec<String> = rig.typed().into_iter().filter(|t| t.ends_with(" the second order")).collect();
    assert_eq!(typed, [format!("{} the second order", id.0)], "typed once, under the store's order id");

    // Settled is settled: nothing more is typed for it.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.typed().iter().filter(|t| t.ends_with(" the second order")).count(), 1);
    let o = ed.await_order(&id, &[OrderState::Acknowledged], Duration::from_secs(5)).unwrap().expect("the order");
    assert_eq!(o.state, OrderState::Acknowledged);
    let _ = compound.stop(&seat);
}

#[test]
fn with_orders_unset_no_order_is_written() {
    let rig = Rig::up("files", None);
    let mut ed = rig.client(Principal::Person("ed".into()));
    let refused = ed.apply(&Cmd::OrderSend { to: SeatId("cpd-none".into()), text: "go".into(), first: false });
    match refused {
        Err(wsp_client::Error::Refused(Refusal::Illegal { state, verb, .. })) => {
            assert_eq!((state.as_str(), verb.as_str()), ("mirrored", "order.send"));
        }
        other => panic!("order.send under files: {other:?}"),
    }
    let reply = ed.request("orders", json!({})).unwrap();
    match reply.outcome {
        wsp_api::Outcome::Ok(wsp_api::Answer::Orders(orders)) => assert!(orders.is_empty(), "{orders:?}"),
        other => panic!("{other:?}"),
    }
}
