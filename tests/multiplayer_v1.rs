//! The multiplayer v1 gate (wsp-419), in a sandbox on one machine: "Ed and I
//! don't collide on work, and I can actually help bring his ideas to life."
//!
//! One real `wsp-hub`, run as an external program over TLS
//! (`wsp_spoke::testhub`: `WSP_HUB_BIN` or `WSP_HUB_IMAGE`; with neither the
//! test is skipped, as the other hub tests are), with `ed` and `jesper`
//! members of `shared:core`. Two spokes, each a `wsp sandbox --fake` (the fake backend is
//! the fake manager: its seats are panes nobody types into, so the test does
//! what each seat's agent would) with its own `HOME`, `WSP_HOME` and
//! `WSP_STATE`, logged in as its user, and a `wsp daemon` syncing every
//! [`TURN_MS`]. Each spoke dials the hub through a TCP proxy of its own (TLS
//! end to end through it), so one spoke can lose the hub while the other keeps
//! it. A bare repository is the shared git remote: each spoke's project
//! `alpha` is a clone of it, and a shared row lands by pushing there.
//!
//! [`the_v1_gate_runs_end_to_end_on_two_spokes_and_one_hub`] runs the row's
//! Done when steps in order, each its own `step_N_…` function whose assertions
//! name the step:
//!
//! 1. Ed makes three rows and a list in his private workspace and publishes
//!    the list to `shared:core`;
//! 2. Jesper's spoke shows the list and its rows within two sync turns;
//! 3. Jesper leases two and spawns them on his own seats; the hub records it;
//! 4. Ed's spoke shows those two held by jesper, its run does not start them,
//!    drives the list and starts the third on Ed's seat;
//! 5. all three get verdicts and landings on the shared remote's trunk, both
//!    caches show them landed, and the barrier opens on Ed's spoke (each row's
//!    verifier is opened by the run of the spoke that holds it: Ed's on his
//!    row, Jesper's on his two, though Ed drives the list);
//! 6. a seat on Jesper's spoke tells `seat:<row>` for Ed's row; Ed's seat
//!    receives it and its reply comes back;
//! 7. Jesper's spoke loses the hub mid-row, keeps working, and on reconnect its
//!    journal fast-forwards the row and Ed sees the work;
//! 8. wsp-463's standalone test and wsp-180's privacy test still pass: both
//!    files are modules of this binary, and the step runs them in a child of it.
//!
//! The person's step (two real machines, `scripts/mp-gate.sh`) is the
//! `two-machines` leg's. [`the_two_machine_script_runs_on_the_sandbox_with_every_step_ok`]
//! rehearses that script here: owner on Ed's spoke, helper on Jesper's, side by
//! side on a sandbox of its own, with the agents' work and the cut link played
//! as this test plays them.
//!
//! [`a_row_run_as_legs_on_one_spoke_is_refused_to_the_other_while_a_leg_runs`]
//! (wsp-896): a shared row run as legs on Ed's spoke is his on the hub while
//! its leg runs, Jesper's spawn of it is refused, and the lease goes back at
//! the last leg's review.

// Step 8's tests, compiled into this binary so the gate can run them in order.
#[allow(dead_code, unused_imports)]
#[path = "standalone.rs"]
mod standalone;
#[allow(dead_code, unused_imports)]
#[path = "privacy.rs"]
mod privacy;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use wsp_objects::{Object, ObjectId, WorkspaceId};
use wsp_spoke::testhub::{protocol, Setup, TestHub};
use wsp_store_contract::{Entity, ListId, RowId, Subject};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
/// The sync worker's turn on both spokes.
const TURN_MS: &str = "300";
const SHARED: &str = "shared:core";
const LIST: &str = "mv1";
/// The bound on a wait for something that is not the thing under test.
const WAIT: Duration = Duration::from_secs(90);

/// The context-pack builder a run asks for before it spawns a member: a stub
/// that answers `meta` with no likely files and anything else with one line.
const PACK: &str = r#"#!/bin/sh
case " $* " in
  *" --depth meta "*) echo '{"likely": []}' ;;
  *) echo "CONTEXT PACK (the gate's stub)" ;;
esac
"#;

/// compound-sup as far as a seat's spawn, `vitals` and `deliver` use it, for
/// each spoke's `wsp-manager` (the rig `tests/tell_orders.rs` runs): a spawn is
/// a process that lives as long as this test, `vitals` says whether it does,
/// and `deliver <sock> <id>` writes `<id> <text>` to `<sock>.orders/typed`,
/// answering an id once and keeping it.
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

// ─────────────────────────────── the proxy ───────────────────────────────

/// A loopback address that carries each connection's bytes to the hub, while
/// it points at one; `cut` closes what is open. TLS passes through untouched.
struct Proxy {
    addr: SocketAddr,
    upstream: Arc<Mutex<Option<SocketAddr>>>,
    open: Arc<Mutex<Vec<TcpStream>>>,
}

impl Proxy {
    fn start(hub: SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream = Arc::new(Mutex::new(Some(hub)));
        let open: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let (up, o) = (upstream.clone(), open.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(client) = conn else { continue };
                let to = *up.lock().unwrap();
                let Some(hub) = to.and_then(|a| TcpStream::connect(a).ok()) else {
                    let _ = client.shutdown(Shutdown::Both);
                    continue;
                };
                o.lock().unwrap().push(client.try_clone().unwrap());
                o.lock().unwrap().push(hub.try_clone().unwrap());
                for (from, into) in [(client.try_clone().unwrap(), hub.try_clone().unwrap()), (hub, client)] {
                    std::thread::spawn(move || pipe(from, into));
                }
            }
        });
        Proxy { addr, upstream, open }
    }

    fn point_at(&self, hub: Option<SocketAddr>) {
        *self.upstream.lock().unwrap() = hub;
    }

    /// No hub behind it, and every open connection closed.
    fn cut(&self) {
        self.point_at(None);
        for c in self.open.lock().unwrap().drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }
}

fn pipe(mut from: TcpStream, mut into: TcpStream) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if into.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = into.shutdown(Shutdown::Both);
}

// ─────────────────────────────── the hub ───────────────────────────────

/// The hub over TLS, its users' tokens, the shared remote.
struct HubRig {
    dir: PathBuf,
    tag: String,
    hub: TestHub,
    tokens: BTreeMap<String, String>,
    remote: PathBuf,
}

impl HubRig {
    /// `None` when no hub is named: the test is skipped. `tag` keeps two
    /// tests of this binary, run side by side, out of each other's files.
    fn up(tag: &str) -> Option<HubRig> {
        let hub = TestHub::start(Setup::default().tls())?;
        let core: &[&str] = &["core"];
        let tokens = hub.seed(&["core"], &[("ed", core), ("jesper", core)]);
        // Under /tmp, as the other hub tests: a fake's socket path must fit a sockaddr.
        let dir = PathBuf::from("/tmp").join(format!("mv1{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // The shared remote: a bare repository whose trunk has a first commit
        // with a directory the pack builder reads.
        let remote = dir.join("remote.git");
        git(&dir, &["init", "-q", "--bare", "-b", "master", remote.to_str().unwrap()]);
        let seed = dir.join("seed");
        git(&dir, &["clone", "-q", remote.to_str().unwrap(), seed.to_str().unwrap()]);
        std::fs::create_dir_all(seed.join("src")).unwrap();
        std::fs::write(seed.join("src/lib.rs"), "// alpha\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "trunk"]);
        git(&seed, &["push", "-q", "origin", "master"]);
        Some(HubRig { dir, tag: tag.into(), hub, tokens, remote })
    }

    fn addr(&self) -> SocketAddr {
        self.hub.addr()
    }

    fn ca(&self) -> &Path {
        self.hub.ca().expect("a TLS hub")
    }

    /// The heads of `shared:core`, as a member (ed) reads them.
    fn heads(&self) -> BTreeMap<Subject, ObjectId> {
        self.hub.client(&self.tokens["ed"]).sync_heads(&WorkspaceId(SHARED.into())).unwrap().heads
    }

    fn row_head(&self, row: &str) -> Option<ObjectId> {
        self.heads().get(&Subject::Row(RowId(row.into()))).copied()
    }

    /// The user whose spoke holds `row`'s lease, and its token.
    fn holder(&self, row: &str) -> Option<(String, protocol::Token)> {
        let (holder, token) = self.hub.lease_of(row)?;
        let who = match holder {
            protocol::Principal::Person(u) => u.to_string(),
            other => format!("{other:?}"),
        };
        Some((who, token))
    }

    /// A row record the hub holds, and its parent.
    fn record(&self, id: ObjectId) -> (wsp_store_contract::Row, Option<ObjectId>) {
        let blobs = self.hub.client(&self.tokens["ed"]).sync_get(vec![id]).unwrap();
        match wsp_objects::decode(&blobs[0].bytes).unwrap() {
            Object::Record { entity: Entity::Row(row), parent, .. } => (row, parent),
            other => panic!("{id} is no row record: {other:?}"),
        }
    }

    /// The trunk of the shared remote, newest first, as `<sha> <subject>`.
    fn trunk(&self) -> Vec<String> {
        git(&self.remote, &["log", "--format=%H %s", "master"]).lines().map(str::to_string).collect()
    }
}

impl Drop for HubRig {
    fn drop(&mut self) {
        // The spokes are dropped before the hub (their daemons, managers and
        // sandboxes), so what is left is files; MV1_KEEP keeps them to look at.
        if std::env::var_os("MV1_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// `git` in `dir`, with no global configuration and a fixed author; its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(["-c", "user.name=mv1", "-c", "user.email=mv1@example.invalid", "-c", "init.defaultBranch=master"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {} in {}: {}", args.join(" "), dir.display(), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

// ─────────────────────────────── a spoke ───────────────────────────────

fn bare(env: &BTreeMap<String, String>, home: &Path) -> Command {
    bare_of(Path::new(WSP), env, home)
}

/// `program` under a spoke's environment, as [`bare`] runs `wsp`.
fn bare_of(program: &Path, env: &BTreeMap<String, String>, home: &Path) -> Command {
    let mut c = Command::new(program);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.envs(env)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "mv1")
        .env("GIT_AUTHOR_EMAIL", "mv1@example.invalid")
        .env("GIT_COMMITTER_NAME", "mv1")
        .env("GIT_COMMITTER_EMAIL", "mv1@example.invalid")
        .env("WSP_TEST_MACHINE", MACHINE)
        .env("TZ", "UTC")
        .env("NO_COLOR", "1")
        .env("WSP_HOST", "manager");
    c
}

/// One person's wsp: a fake sandbox logged in to the hub through its proxy,
/// with project `alpha` a clone of the shared remote, and a daemon.
struct Spoke {
    user: String,
    dir: PathBuf,
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    home: PathBuf,
    repo: PathBuf,
    proxy: Proxy,
    manager: Option<Arc<wsp_manager::Manager>>,
    agents: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    daemon: Option<Child>,
    nudged: Mutex<Option<Instant>>,
}

impl Spoke {
    fn up(rig: &HubRig, who: &str) -> Spoke {
        let dir = rig.dir.join(who);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let proxy = Proxy::start(rig.addr());
        let pack = dir.join("ixv2");
        std::fs::write(&pack, PACK).unwrap();
        std::fs::set_permissions(&pack, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("mv1{}{}{}", rig.tag, &who[..1], std::process::id());
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).env("WSP_SANDBOX_TETHER", std::process::id().to_string()).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let mut env: BTreeMap<String, String> =
            v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        env.insert("WSP_PACK_BIN".into(), pack.display().to_string());
        // The login, as `wsp login` writes it, under the sandbox's hub home.
        let hubs = PathBuf::from(&env["WSP_HUB_HOME"]).join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let login = serde_json::json!({
            "url": format!("tls://localhost:{}", proxy.addr.port()), "token": rig.tokens[who], "user": who,
            "at": "2026-11-23T00:00:00Z", "ca": rig.ca().display().to_string(),
        });
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        // The spoke's fake manager: a wsp-manager on loopback whose supervisor
        // is the fake, and the record that names it in the spoke's state.
        let sup = dir.join("sup");
        wsp_testkit::script(&sup, &FAKE_SUP.replace("__OWNER__", &std::process::id().to_string()));
        let mut cfg = wsp_manager::Config::new(&dir.join("exec"), &format!("{who}-host"));
        cfg.port = 0;
        cfg.sup = Some(sup.clone());
        let manager = Arc::new(wsp_manager::Manager::start(cfg).expect("the spoke's manager starts on loopback"));
        let mut local = wsp_wire::manager::Local::new(&dir.join("exec"), manager.addrs()[0].port());
        local.host = format!("{who}-host");
        wsp_wire::manager::install_record(Path::new(&env["WSP_STATE"]), &local, Duration::from_secs(5)).expect("the manager record");
        env.insert("COMPOUND_SUP".into(), sup.display().to_string());
        let agents = play_first_turns(manager.clone(), PathBuf::from(&env["WSP_STATE"]));
        let repo = dir.join("repo");
        git(&dir, &["clone", "-q", rig.remote.to_str().unwrap(), repo.to_str().unwrap()]);
        let s = Spoke { user: who.into(), dir, name, outer, env, home, repo, proxy, manager: Some(manager), agents: Some(agents), daemon: None, nudged: Mutex::new(None) };
        s.ok(&["init"]);
        s.ok(&["project", "add", "alpha", "--name", "Alpha"]);
        s.ok(&["project", "set", "alpha", &format!("roots={}", s.repo.display())]);
        s
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = bare(&self.env, &self.home);
        c.args(args).current_dir(&self.dir);
        c
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let o = self.cmd(args).output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, out) = self.run(args);
        assert!(ok, "{}: wsp {}: {out}", self.user, args.join(" "));
        out
    }

    /// `wsp …` from a seat's shell (`WSP_SEAT_ID`), `input` on stdin.
    fn as_seat(&self, seat: &str, args: &[&str], input: &str) -> (bool, String) {
        let mut c = self.cmd(args);
        c.env("WSP_SEAT_ID", seat).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn state(&self) -> PathBuf {
        PathBuf::from(&self.env["WSP_STATE"])
    }

    fn start_daemon(&mut self) {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let out = std::fs::File::create(self.dir.join("daemon.out")).unwrap();
        let err = out.try_clone().unwrap();
        let mut c = self.cmd(&["daemon"]);
        c.env("WSP_API_PORT", port.to_string()).env("WSP_HUB_SYNC_MS", TURN_MS).stdout(out).stderr(err);
        self.daemon = Some(c.spawn().unwrap());
    }

    fn status(&self) -> Value {
        std::fs::read_to_string(self.state().join("hub/sync.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
    }

    fn turns(&self) -> u64 {
        self.status()["turns"].as_u64().unwrap_or(0)
    }

    fn journal(&self) -> u64 {
        self.status()["journal"].as_u64().unwrap_or(0)
    }

    fn cycle_log(&self) -> String {
        std::fs::read_to_string(self.state().join("cycle.log")).unwrap_or_default()
    }

    /// Every `<id> <text>` the spoke's fake supervisor typed into a seat.
    fn typed(&self) -> Vec<String> {
        let run = self.dir.join("exec").join(wsp_manager::seats::RUN_DIR);
        let mut out = Vec::new();
        for e in std::fs::read_dir(&run).into_iter().flatten().flatten() {
            if e.path().extension().is_some_and(|x| x == "orders") {
                if let Ok(t) = std::fs::read_to_string(e.path().join("typed")) {
                    out.extend(t.lines().map(str::to_string));
                }
            }
        }
        out
    }

    fn json(&self, file: &str) -> Value {
        std::fs::read_to_string(self.state().join("hub").join(file)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
    }

    fn wait(&self, step: &str, what: &str, within: Duration, mut done: impl FnMut(&Spoke) -> bool) {
        let end = Instant::now() + within;
        while !done(self) {
            assert!(
                Instant::now() < end,
                "{step}: {}: {what}, not within {within:?}; sync.json {}\ncycle.log (tail):\n{}\ndaemon: {}",
                self.user,
                self.status(),
                tail(&self.cycle_log(), 30),
                tail(&std::fs::read_to_string(self.dir.join("daemon.out")).unwrap_or_default(), 30),
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The run asked to step the list now, as a person's `wsp worklist
    /// advance` does, at most every three seconds (the daemon's tick is 20 s).
    fn nudge(&self) {
        let mut last = self.nudged.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            return;
        }
        *last = Some(Instant::now());
        let _ = self.run(&["worklist", "advance", LIST]);
    }

    /// Two whole sync turns from now.
    fn settle(&self, step: &str) {
        let from = self.turns();
        self.wait(step, "two more sync turns", WAIT, |s| s.turns() >= from + 2);
    }

    /// `ls --json` on stdout alone: what it says on stderr is not JSON.
    fn rows(&self) -> Vec<Value> {
        let o = self.cmd(&["ls", "--json", "--all"]).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "{}: ls --json: {out}{}", self.user, String::from_utf8_lossy(&o.stderr));
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("ls --json: {e}: {out}"));
        v.as_array().cloned().or_else(|| v["rows"].as_array().cloned()).unwrap_or_default()
    }

    fn ids(&self) -> BTreeSet<String> {
        self.rows().iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect()
    }

    fn show(&self, row: &str) -> String {
        self.run(&["show", row, "--log"]).1
    }

    /// The seat a claim here names for `row` (`agent  <seat>` in `show`).
    fn seat_of(&self, row: &str) -> Option<String> {
        self.show(row).lines().find_map(|l| l.strip_prefix("agent").map(|s| s.trim().to_string())).filter(|s| !s.is_empty())
    }

    fn claimed(&self, row: &str) -> bool {
        self.show(row).lines().any(|l| l.starts_with("claimed "))
    }

    /// The work of `row`'s seat: a file committed in its tree.
    fn commit_in_tree(&self, row: &str, file: &str) -> String {
        let tree = self.repo.join(".worktrees").join(row);
        assert!(tree.is_dir(), "{}: {row} has no tree at {}", self.user, tree.display());
        std::fs::write(tree.join(file), format!("{row}\n")).unwrap();
        git(&tree, &["add", file]);
        git(&tree, &["commit", "-q", "-m", &format!("{row}: {file}")]);
        git(&tree, &["rev-parse", "HEAD"])
    }
}

impl Drop for Spoke {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        if let Some((stop, t)) = self.agents.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = t.join();
        }
        if let Some(m) = self.manager.take().and_then(|m| Arc::try_unwrap(m).ok()) {
            m.stop();
        }
        // The env each spoke ran under, for a look after a failure under MV1_KEEP.
        let env: Vec<String> = self.env.iter().map(|(k, v)| format!("export {k}='{v}'")).collect();
        let _ = std::fs::write(self.dir.join("env.sh"), format!("{}\nexport HOME='{}'\n", env.join("\n"), self.home.display()));
        if std::env::var_os("MV1_KEEP").is_none() {
            let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &self.name]).output();
        }
    }
}

/// The fake agents' first turn: every seat the spoke's manager starts begins a
/// turn (what a spawn waits for to read the agent ready) and, a moment later,
/// ends it with the hook's word that it is idle, as a real agent at its prompt.
fn play_first_turns(manager: Arc<wsp_manager::Manager>, state: PathBuf) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    use wsp_host_api::{At, Message, Turn, TurnEdge};
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let t = std::thread::spawn(move || {
        let now = || At(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64);
        let mut began: BTreeMap<(String, u64), Instant> = BTreeMap::new();
        let mut ended: BTreeSet<(String, u64)> = BTreeSet::new();
        while !flag.load(Ordering::SeqCst) {
            for s in manager.seats() {
                let key = (s.seat.0.clone(), s.epoch.0 as u64);
                if !began.contains_key(&key) {
                    let _ = manager.emit(Message::Turn(Turn { seat: s.seat.clone(), epoch: s.epoch, edge: TurnEdge::Began, at: now(), order: None }));
                    began.insert(key.clone(), Instant::now());
                } else if !ended.contains(&key) && began[&key].elapsed() > Duration::from_secs(1) {
                    let said = state.join(wsp_wire::place_compound::SEATS).join(&s.seat.0).join("said.json");
                    if let Some(dir) = said.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(&said, serde_json::json!({ "state": "idle", "at": wsp_base::util::now_iso() }).to_string());
                    let _ = manager.emit(Message::Turn(Turn { seat: s.seat.clone(), epoch: s.epoch, edge: TurnEdge::Ended, at: now(), order: None }));
                    ended.insert(key);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    (stop, t)
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// A `wsp: landed <sha>` line's commit, the newest in `shown`.
fn landed_in(shown: &str) -> Option<String> {
    shown.lines().filter_map(|l| l.split_once("wsp: landed ").map(|(_, r)| r.split_whitespace().next().unwrap_or("").to_string())).last()
}

// ──────────────────────────────── the scenario ────────────────────────────────

/// What the steps hand each other.
#[derive(Default)]
struct Gate {
    /// The three rows, by hub id, in the order Ed made them.
    rows: Vec<String>,
    /// The seats each spoke put on its rows.
    seats: BTreeMap<String, String>,
    /// The commit each row landed.
    landed: BTreeMap<String, String>,
}

impl Gate {
    fn jespers(&self) -> [String; 2] {
        [self.rows[0].clone(), self.rows[1].clone()]
    }

    fn eds(&self) -> &str {
        &self.rows[2]
    }
}

const STEP1: &str = "step 1 (Ed makes three rows and a list privately, then publishes the list to shared:core)";
fn step_1_ed_publishes_a_private_list_of_three_rows(rig: &HubRig, ed: &Spoke, g: &mut Gate) {
    let mut private = Vec::new();
    for title in ["Row one", "Row two", "Row three"] {
        let out = ed.ok(&["add", title, "-p", "alpha"]);
        let id = out
            .split_whitespace()
            .find(|w| w.starts_with("alpha-"))
            .unwrap_or_else(|| panic!("{STEP1}: add printed no id: {out}"))
            .to_string();
        ed.ok(&["caption", &id, title]);
        private.push(id);
    }
    ed.ok(&["worklist", "new", LIST, "The v1 gate's list", "-p", "alpha"]);
    let mut add = vec!["worklist", "add", LIST];
    add.extend(private.iter().map(String::as_str));
    ed.ok(&add);
    for id in &private {
        let shown = ed.show(id);
        assert!(!shown.contains(SHARED), "{STEP1}: {id} is private before the publish: {shown}");
    }
    assert!(rig.heads().is_empty(), "{STEP1}: nothing reached the hub while the work was private: {:?}", rig.heads());

    let out = ed.ok(&["publish", LIST, "--to", SHARED]);
    let renamed: BTreeMap<String, String> = out
        .lines()
        .filter_map(|l| l.split_once(" -> ").map(|(a, b)| (a.trim().to_string(), b.trim().to_string())))
        .collect();
    g.rows = private.iter().map(|old| renamed.get(old).cloned().unwrap_or_else(|| panic!("{STEP1}: {old} not renumbered: {out}"))).collect();
    let listed = ed.ok(&["worklist", "show", LIST]);
    for id in &g.rows {
        assert!(listed.contains(id.as_str()), "{STEP1}: Ed's list holds {id}: {listed}");
    }
    ed.wait(STEP1, "the list and its three rows on the hub", WAIT, |_| {
        let heads = rig.heads();
        heads.contains_key(&Subject::List(ListId(LIST.into()))) && g.rows.iter().all(|r| heads.contains_key(&Subject::Row(RowId(r.clone()))))
    });
}

const STEP2: &str = "step 2 (Jesper's spoke shows the list and its three rows within two sync turns)";
fn step_2_jespers_spoke_shows_the_list_within_two_turns(jesper: &Spoke, g: &Gate) {
    let from = jesper.turns();
    jesper.wait(STEP2, "the list and its rows", WAIT, |s| {
        let ids = s.ids();
        g.rows.iter().all(|r| ids.contains(r)) && s.run(&["worklist", "show", LIST]).0
    });
    let took = jesper.turns().saturating_sub(from);
    assert!(took <= 2, "{STEP2}: it took {took} of Jesper's sync turns");
    let listed = jesper.ok(&["worklist", "show", LIST]);
    for id in &g.rows {
        assert!(listed.contains(id.as_str()), "{STEP2}: Jesper's list shows {id}: {listed}");
    }
}

const STEP3: &str = "step 3 (Jesper leases two of the rows, his spoke spawns them on his own seats, the hub records his leases)";
fn step_3_jesper_leases_two_and_spawns_them_on_his_seats(rig: &HubRig, jesper: &Spoke, g: &mut Gate) {
    for row in &g.jespers() {
        let out = jesper.ok(&["spawn", row, "-p", "alpha", "--agent"]);
        let seat = out
            .lines()
            .find_map(|l| l.trim().strip_prefix("bound to "))
            .unwrap_or_else(|| panic!("{STEP3}: the spawn of {row} bound no seat: {out}"))
            .trim()
            .to_string();
        g.seats.insert(row.to_string(), seat);
    }
    for row in &g.jespers() {
        assert_eq!(rig.holder(row).map(|h| h.0).as_deref(), Some("jesper"), "{STEP3}: the hub records Jesper's lease on {row}");
        assert!(jesper.claimed(row), "{STEP3}: Jesper's spoke holds a claim on {row}: {}", jesper.show(row));
        assert!(jesper.repo.join(".worktrees").join(row).is_dir(), "{STEP3}: {row} has a tree on Jesper's spoke");
    }
    assert_eq!(rig.holder(g.eds()), None, "{STEP3}: nobody holds the third row yet");
    // Whole on Jesper's spoke once leased (a head before), for its seat to work on.
    for row in &g.jespers() {
        jesper.wait(STEP3, &format!("{row} whole on Jesper's spoke"), WAIT, |s| !s.show(row).contains("head only"));
    }
}

const STEP4: &str =
    "step 4 (Ed's spoke shows Jesper's two as held by jesper and does not start them; it drives the list and starts the third on Ed's seat)";
fn step_4_eds_spoke_drives_the_list_and_starts_only_the_third(rig: &HubRig, ed: &Spoke, jesper: &Spoke, g: &mut Gate) {
    ed.ok(&["worklist", "go", LIST, "start the gate's list"]);
    let third = g.eds().to_string();
    let end = Instant::now() + WAIT;
    while !ed.claimed(&third) {
        if Instant::now() > end {
            let said: Vec<String> = ed.cycle_log().lines().filter(|l| l.contains(third.as_str()) && !l.contains("tell ")).map(str::to_string).collect();
            panic!("{STEP4}: {third} was not started on Ed's seat within {WAIT:?}\nshow:\n{}\ncycle.log on it:\n{}", ed.show(&third), said.join("\n"));
        }
        ed.nudge();
        std::thread::sleep(Duration::from_millis(200));
    }
    let listed = ed.ok(&["worklist", "show", LIST]);
    assert!(listed.contains("driven by ed"), "{STEP4}: Ed's spoke drives the list: {listed}");
    for row in &g.jespers() {
        let line = listed.lines().find(|l| l.trim_start().starts_with(row.as_str())).unwrap_or_default();
        assert!(line.contains("running on jesper's wsp"), "{STEP4}: Ed's spoke shows {row} held by jesper: {listed}");
        assert!(!ed.claimed(row), "{STEP4}: Ed's run did not start {row}: {}", ed.show(row));
        assert_eq!(rig.holder(row).map(|h| h.0).as_deref(), Some("jesper"), "{STEP4}: {row} is still Jesper's on the hub");
    }
    assert_eq!(rig.holder(&third).map(|h| h.0).as_deref(), Some("ed"), "{STEP4}: the hub records Ed's lease on {third}");
    assert_eq!(rig.holder(&format!("drive:{LIST}")).map(|h| h.0).as_deref(), Some("ed"), "{STEP4}: Ed's spoke holds the drive lease");
    let seat = ed.seat_of(&third).unwrap_or_else(|| panic!("{STEP4}: no seat on {third}: {}", ed.show(&third)));
    g.seats.insert(third, seat);
    let log = ed.cycle_log();
    assert!(log.contains(&format!("{LIST}: driven here, by ed's wsp")), "{STEP4}: Ed's cycle.log says it drives: {}", tail(&log, 20));
    // Jesper's spoke reads the list as Ed's to drive and starts nothing of it.
    jesper.wait(STEP4, "the list read as driven by ed", WAIT, |s| {
        s.nudge();
        s.cycle_log().contains(&format!("{LIST}: driven by ed — this spoke does not advance it"))
    });
    assert!(!jesper.claimed(g.eds()), "{STEP4}: Jesper's spoke did not start {}: {}", g.eds(), jesper.show(g.eds()));
}

const STEP5: &str = "step 5 (all three get verdicts and landings on the shared remote's trunk; both caches show them landed; the barrier opens on Ed's spoke)";
fn step_5_verdicts_and_landings_reach_the_remote_and_the_barrier_opens(rig: &HubRig, ed: &Spoke, jesper: &Spoke, g: &mut Gate) {
    // Each seat's agent does its work and reviews it.
    let work = [(jesper, g.rows[0].clone(), "one.txt"), (jesper, g.rows[1].clone(), "two.txt"), (ed, g.rows[2].clone(), "three.txt")];
    for (spoke, row, file) in &work {
        spoke.commit_in_tree(row, file);
        let (ok, out) = spoke.as_seat(&g.seats[row], &["review", row, &format!("Wrote {file}; nothing left.")], "");
        assert!(ok, "{STEP5}: {}'s seat reviews {row}: {out}", spoke.user);
        let shown = spoke.show(row);
        assert!(shown.contains("wsp: ready"), "{STEP5}: {row} is marked ready for its verifier on {}'s spoke: {shown}", spoke.user);
    }
    // The verdicts. Each spoke's run opens a verifier on the member it holds:
    // Ed's drives the list, and Jesper's opens one for each of his two without
    // advancing it (wsp-604). The test gives each pass its verdict.
    let verdict = "summary: the file is there and says what it should\n\n\
        I read the row's one commit: it adds the file its review names, with the row's id in it, and nothing else.\n";
    for (spoke, row, _) in &work {
        spoke.wait(STEP5, &format!("a verifier on {row} opened by {}'s run", spoke.user), WAIT, |s| {
            s.nudge();
            s.show(row).contains("## Verification")
        });
        let (ok, out) = spoke.as_seat("", &["verified", row, "--holds", "--from", "-", "--caption", "the work holds"], verdict);
        assert!(ok, "{STEP5}: the verdict on {row} on {}'s spoke: {out}", spoke.user);
        let shown = spoke.show(row);
        let sha = landed_in(&shown).unwrap_or_else(|| panic!("{STEP5}: {row} has no landed line on {}'s spoke: {shown}", spoke.user));
        assert!(shown.contains("pushed to origin"), "{STEP5}: {row}'s landing was pushed to the shared remote: {shown}");
        g.landed.insert(row.clone(), sha);
    }
    // Each landing is on the shared remote's trunk.
    let tip = git(&rig.remote, &["rev-parse", "master"]);
    for (row, sha) in &g.landed {
        let on = Command::new("git").args(["merge-base", "--is-ancestor", sha, &tip]).current_dir(&rig.remote).status().unwrap().success();
        assert!(on, "{STEP5}: {row}'s landing {sha} is on the shared remote's trunk: {:?}", rig.trunk());
    }
    // Both caches show all three landed: a row's own landed line, else the
    // landing the hub carried into the cache's head.
    for spoke in [ed, jesper] {
        for row in &g.rows {
            spoke.wait(STEP5, &format!("{row} shown landed"), WAIT, |s| {
                landed_in(&s.show(row)).is_some() || s.json("heads.json")[row.as_str()]["landed"].as_str().is_some()
            });
        }
    }
    // The barrier opens on Ed's spoke.
    ed.wait(STEP5, "the barrier of group 1 open", WAIT, |s| {
        s.nudge();
        s.cycle_log().contains(&format!("started {LIST} · barrier 1"))
    });
}

const STEP6: &str = "step 6 (a seat on Jesper's spoke tells seat:<Ed's row>; Ed's seat receives it, and its reply comes back)";
fn step_6_a_tell_crosses_to_eds_seat_and_the_reply_comes_back(ed: &Spoke, jesper: &Spoke, g: &Gate) {
    let (mine, theirs) = (g.rows[0].clone(), g.eds().to_string());
    let text = "is the third row landed on the remote?";
    let (ok, out) = jesper.as_seat(&g.seats[&mine], &["tell", &format!("seat:{theirs}"), "-"], text);
    assert!(ok, "{STEP6}: Jesper's seat on {mine} tells seat:{theirs}: {out}\nEd's {theirs}:\n{}", ed.show(&theirs));
    // Ed's seat on the row has it typed, from Jesper's seat named with his user,
    // with the footer that says how to answer it.
    ed.wait(STEP6, "the tell typed into Ed's seat", WAIT, |s| s.typed().iter().any(|l| l.contains(text)));
    let told = ed.typed().into_iter().find(|l| l.contains(text)).unwrap();
    assert!(told.contains(&format!("seat:{mine}@jesper says: {text}")), "{STEP6}: Ed's seat reads who spoke: {told}");
    let log = ed.cycle_log();
    assert!(
        log.contains(&format!("told the seat on {theirs}: from seat:{mine}@jesper through the hub")),
        "{STEP6}: Ed's spoke says it told the seat on {theirs}: {}",
        tail(&log, 20)
    );
    let footer = told.split("reply: ").nth(1).unwrap_or_else(|| panic!("{STEP6}: the tell carries no reply footer: {told}"));
    let id = footer.split_whitespace().skip_while(|w| *w != "--re").nth(1).unwrap_or_else(|| panic!("{STEP6}: no --re in {footer}"));
    let id = id.trim_end_matches(']');
    // Ed's seat answers as the footer says.
    let reply = "yes, it is on the shared trunk";
    let (ok, out) = ed.as_seat(&g.seats[&theirs], &["tell", &format!("seat:{mine}"), "--re", id, "-"], reply);
    assert!(ok, "{STEP6}: Ed's seat on {theirs} replies --re {id}: {out}");
    jesper.wait(STEP6, "the reply typed into Jesper's seat", WAIT, |s| s.typed().iter().any(|l| l.contains(reply)));
    let back = jesper.typed().into_iter().find(|l| l.contains(reply)).unwrap();
    assert!(back.contains(&format!("seat:{theirs}@ed says: {reply}")), "{STEP6}: Jesper's seat reads Ed's seat spoke: {back}");
}

const STEP7: &str = "step 7 (Jesper's spoke loses the hub mid-row, keeps working, and on reconnect its journal fast-forwards and Ed sees the work)";
fn step_7_jesper_offline_mid_row_and_his_journal_fast_forwards(rig: &HubRig, ed: &Spoke, jesper: &Spoke) {
    // A fourth shared row, Jesper's, leased and synced.
    let out = jesper.ok(&["add", "Row four", "-p", "alpha", "--workspace", SHARED]);
    let row = out.split_whitespace().find(|w| w.starts_with("alpha-")).unwrap_or_else(|| panic!("{STEP7}: add printed no id: {out}")).to_string();
    jesper.ok(&["caption", &row, "Row four"]);
    jesper.ok(&["spawn", &row, "-p", "alpha", "--agent"]);
    assert_eq!(rig.holder(&row).map(|h| h.0).as_deref(), Some("jesper"), "{STEP7}: Jesper leases the fourth row");
    let (_, token) = rig.holder(&row).unwrap();
    jesper.wait(STEP7, "the fourth row on the hub", WAIT, |_| rig.row_head(&row).is_some());
    jesper.wait(STEP7, "the journal empty before the outage", WAIT, |s| s.journal() == 0);
    jesper.settle(STEP7);
    ed.wait(STEP7, "the fourth row in Ed's ls", WAIT, |s| s.ids().contains(&row));
    let before = rig.row_head(&row).unwrap();

    // The hub goes away mid-row; Jesper keeps working.
    jesper.proxy.cut();
    jesper.wait(STEP7, "the link down", WAIT, |s| s.status()["link"] == "down");
    jesper.ok(&["rename", &row, "Row four, finished offline"]);
    jesper.wait(STEP7, "the rename journaled", WAIT, |s| s.journal() >= 1);
    assert_eq!(rig.row_head(&row), Some(before), "{STEP7}: nothing reached the hub while it was away");
    jesper.ok(&["note", &row, "kept working while the hub was away"]);
    assert!(jesper.claimed(&row), "{STEP7}: the seat keeps the row offline: {}", jesper.show(&row));

    // Back: the journal fast-forwards the row under the same lease.
    jesper.proxy.point_at(Some(rig.addr()));
    jesper.wait(STEP7, "the journal pushed", WAIT, |s| s.journal() == 0 && s.status()["link"] == "up");
    jesper.wait(STEP7, "the hub's head moved", WAIT, |_| rig.row_head(&row) != Some(before));
    let head = rig.row_head(&row).unwrap();
    assert_eq!(rig.record(head).0.title, "Row four, finished offline", "{STEP7}: the hub holds the offline work");
    let mut at = Some(head);
    let mut seen = false;
    while let Some(i) = at {
        if i == before {
            seen = true;
            break;
        }
        at = rig.record(i).1;
    }
    assert!(seen, "{STEP7}: a fast-forward: the head before the outage is on the hub's line to the new one");
    assert_eq!(rig.holder(&row).map(|h| h.1), Some(token), "{STEP7}: the same lease, never taken again");
    assert!(!jesper.json("side.json").to_string().contains(&row), "{STEP7}: no side chain: {}", jesper.json("side.json"));
    ed.wait(STEP7, "Ed sees the offline work", WAIT, |s| s.ok(&["ls", "--all"]).contains("finished offline"));
}

const STEP8: &str = "step 8 (wsp-463's standalone test and wsp-180's privacy test still pass)";
fn step_8_the_standalone_and_privacy_tests_still_pass() {
    let me = std::env::current_exe().unwrap();
    let out = Command::new(&me)
        .args(["--exact", "--test-threads", "2"])
        .args([
            "standalone::a_wsp_with_no_login_adds_no_thread_socket_or_file",
            "standalone::a_planted_outbound_socket_is_seen_off_loopback",
            "standalone::a_planted_socket_in_this_process_is_read_off_the_machine",
            "standalone::a_planted_file_is_outside_the_allow_list",
            "standalone::a_planted_hub_line_or_thread_is_seen",
            "standalone::a_planted_change_in_wip_is_seen_against_the_golden",
            "privacy::what_ed_keeps_private_reaches_no_frame_no_object_and_no_other_user",
            "privacy::the_tap_and_the_store_scan_find_a_private_id_planted_in_a_frame_and_an_object",
            "privacy::base64_reads_what_a_blob_carries",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{STEP8}: {said}");
    assert!(said.contains("test result: ok. 9 passed"), "{STEP8}: all nine ran and passed: {said}");
}

#[test]
fn the_v1_gate_runs_end_to_end_on_two_spokes_and_one_hub() {
    let Some(rig) = HubRig::up("") else { return };
    let mut ed = Spoke::up(&rig, "ed");
    let mut jesper = Spoke::up(&rig, "jesper");
    ed.start_daemon();
    jesper.start_daemon();
    for s in [&ed, &jesper] {
        s.wait("setup", "a first sync turn", WAIT, |s| s.turns() >= 1);
    }
    let mut g = Gate::default();
    step_1_ed_publishes_a_private_list_of_three_rows(&rig, &ed, &mut g);
    step_2_jespers_spoke_shows_the_list_within_two_turns(&jesper, &g);
    step_3_jesper_leases_two_and_spawns_them_on_his_seats(&rig, &jesper, &mut g);
    step_4_eds_spoke_drives_the_list_and_starts_only_the_third(&rig, &ed, &jesper, &mut g);
    step_5_verdicts_and_landings_reach_the_remote_and_the_barrier_opens(&rig, &ed, &jesper, &mut g);
    step_6_a_tell_crosses_to_eds_seat_and_the_reply_comes_back(&ed, &jesper, &g);
    step_7_jesper_offline_mid_row_and_his_journal_fast_forwards(&rig, &ed, &jesper);
    drop((ed, jesper));
    step_8_the_standalone_and_privacy_tests_still_pass();
}

// ──────────────────────── a row run as legs holds its lease ────────────────────────

const LEGS: &str = "legs (wsp-896: a shared row run as legs on Ed's spoke holds his hub lease while a leg runs)";

/// wsp-896, beside the gate: a shared row run as legs on Ed's spoke is leased
/// to him on the hub while its leg runs, though a leg's seat claims nothing;
/// his `ls --json` reads it held by him and his; Jesper's spoke sees the row
/// and is refused it, naming Ed; once the last leg ends by review the lease
/// is gone from the hub and from Ed's `ls --json`.
#[test]
fn a_row_run_as_legs_on_one_spoke_is_refused_to_the_other_while_a_leg_runs() {
    let Some(rig) = HubRig::up("l") else { return };
    let mut ed = Spoke::up(&rig, "ed");
    let mut jesper = Spoke::up(&rig, "jesper");
    ed.start_daemon();
    jesper.start_daemon();
    for s in [&ed, &jesper] {
        s.wait("setup", "a first sync turn", WAIT, |s| s.turns() >= 1);
    }

    // Ed's row, published to shared:core, with one leg staged on it.
    let out = ed.ok(&["add", "Legged row", "-p", "alpha"]);
    let private = out.split_whitespace().find(|w| w.starts_with("alpha-")).unwrap_or_else(|| panic!("{LEGS}: add printed no id: {out}")).to_string();
    ed.ok(&["caption", &private, "Legged row"]);
    let out = ed.ok(&["publish", &private, "--to", SHARED]);
    let row = out
        .lines()
        .find_map(|l| l.split_once(" -> ").filter(|(a, _)| a.trim() == private).map(|(_, b)| b.trim().to_string()))
        .unwrap_or_else(|| panic!("{LEGS}: {private} not renumbered: {out}"));
    ed.wait(LEGS, "the row on the hub", WAIT, |_| rig.row_head(&row).is_some());
    let order = ed.dir.join("build.order");
    std::fs::write(&order, "Write legs.txt in the row's tree and review it.\n").unwrap();
    ed.ok(&["leg", "add", &row, "build", "--order", order.to_str().unwrap()]);
    assert_eq!(rig.holder(&row), None, "{LEGS}: nobody holds {row} before its leg starts");

    // The leg starts: its seat claims nothing, and the row is Ed's on the hub.
    let out = ed.ok(&["spawn", &row, "-p", "alpha", "--agent", "--leg", "build"]);
    assert!(!ed.claimed(&row), "{LEGS}: a leg's seat claims no row: {}", ed.show(&row));
    let legs: Value = serde_json::from_str(&ed.ok(&["leg", "show", &row, "--json"])).unwrap_or_else(|e| panic!("{LEGS}: leg show --json: {e}: {out}"));
    let seat = legs[0]["pane"].as_str().filter(|_| legs[0]["state"] == "running").unwrap_or_else(|| panic!("{LEGS}: leg build is not running on a seat: {legs}\n{out}")).to_string();
    assert_eq!(
        rig.holder(&row).map(|h| h.0).as_deref(),
        Some("ed"),
        "{LEGS}: the hub records Ed's lease on {row} while its leg runs\nspawn: {out}\ndrive.json: {}\ncycle.log (tail):\n{}\nhub leases: {:?}",
        ed.json("drive.json"),
        tail(&ed.cycle_log(), 20),
        rig.hub.state().leases
    );
    let mine = |s: &Spoke| s.rows().into_iter().find(|r| r["id"].as_str() == Some(row.as_str())).unwrap_or(Value::Null);
    let shown = mine(&ed);
    // `ls --json` writes the add-on's row facts as strings (`"mine": "true"`).
    assert_eq!((shown["held_by"].as_str(), shown["mine"].as_str()), (Some("ed"), Some("true")), "{LEGS}: Ed's ls --json reads {row} held by him: {shown}");

    // Jesper's spoke sees the row, and its spawn is refused while the leg runs.
    jesper.wait(LEGS, &format!("{row} on Jesper's spoke"), WAIT, |s| s.ids().contains(&row));
    let (ok, said) = jesper.run(&["spawn", &row, "-p", "alpha", "--agent"]);
    assert!(!ok && said.contains("leased by ed"), "{LEGS}: Jesper's spawn of {row} is refused naming Ed: {said}");
    assert!(!jesper.claimed(&row), "{LEGS}: Jesper holds no claim on {row}: {}", jesper.show(&row));
    assert_eq!(rig.holder(&row).map(|h| h.0).as_deref(), Some("ed"), "{LEGS}: {row} is still Ed's on the hub");

    // The last leg ends by review: the lease goes back to the hub.
    let tree = ed.repo.join(".worktrees").join(&row);
    if tree.is_dir() {
        ed.commit_in_tree(&row, "legs.txt");
    }
    let (ok, said) = ed.as_seat(&seat, &["review", &row, "Wrote legs.txt; nothing left."], "");
    assert!(ok, "{LEGS}: the leg's seat reviews {row}: {said}");
    ed.wait(LEGS, &format!("{row}'s lease gone from the hub"), WAIT, |_| rig.holder(&row).is_none());
    let shown = mine(&ed);
    assert!(shown["held_by"].is_null() || shown["held_by"].as_str().is_none(), "{LEGS}: Ed's ls --json reads {row} unheld once its last leg ended: {shown}");
}

// ──────────────────────── the two-machine script, rehearsed ────────────────────────

/// `scripts/mp-gate.sh <role>` on this spoke, as its person would run it: the
/// spoke's environment, this build's `wsp` first on `PATH`, `MP_GATE_YES=1`
/// (the prompts only print), its output to `<spoke>/gate.out`.
fn run_gate_script(s: &Spoke, role: &str) -> Child {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/mp-gate.sh");
    let bin = Path::new(WSP).parent().unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let out = std::fs::File::create(s.dir.join("gate.out")).unwrap();
    let err = out.try_clone().unwrap();
    let mut c = bare_of(Path::new("bash"), &s.env, &s.home);
    c.arg(&script)
        .args([role, "--project", "alpha", "--workspace", "core", "--list", LIST, "--repo", s.repo.to_str().unwrap()])
        .current_dir(&s.dir)
        .env("PATH", path)
        .env("MP_GATE_YES", "1")
        .env("MP_GATE_WAIT", WAIT.as_secs().to_string())
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    c.spawn().unwrap()
}

/// What the people and their agents do while the two scripts run, done once
/// each as the gate test does it: a seat on one of the list's rows commits its
/// file and reviews; a verifier pass gets its verdict; a tell typed into a seat
/// is answered with the command its footer names; and Jesper's link to the hub
/// is cut and restored when his script asks for it.
#[derive(Default)]
struct Players {
    reviewed: BTreeSet<String>,
    verdicts: BTreeSet<String>,
    replied: BTreeSet<String>,
    cut: bool,
    restored: bool,
}

impl Players {
    fn play(&mut self, rig: &HubRig, s: &Spoke) {
        let rows = s.rows();
        // As in the gate test, a seat reviews once the list has started: a
        // review before it writes no `wsp: ready`, and no verifier follows.
        let started = s.run(&["worklist", "show", LIST]).1.lines().any(|l| l.starts_with("status") && l.contains("executing"));
        let title = |id: &str| -> String {
            rows.iter().find(|r| r["id"].as_str() == Some(id)).and_then(|r| r["title"].as_str()).unwrap_or("").to_string()
        };
        for r in &rows {
            let Some(id) = r["id"].as_str() else { continue };
            let t = r["title"].as_str().unwrap_or("");
            if !["Row one", "Row two", "Row three"].contains(&t) {
                continue;
            }
            let key = format!("{}:{id}", s.user);
            // As the gate test: a seat works its row once the row is whole here.
            let whole = || !s.show(id).contains("head only");
            if started && !self.reviewed.contains(&key) && s.claimed(id) && s.repo.join(".worktrees").join(id).is_dir() && whole() {
                if let Some(seat) = s.seat_of(id) {
                    s.commit_in_tree(id, &format!("{id}.txt"));
                    let (ok, out) = s.as_seat(&seat, &["review", id, &format!("Wrote {id}.txt; nothing left.")], "");
                    assert!(ok, "rehearsal: {}'s seat reviews {id}: {out}", s.user);
                    assert!(s.show(id).contains("wsp: ready"), "rehearsal: {id} is marked ready on {}'s spoke: {}", s.user, s.show(id));
                    self.reviewed.insert(key.clone());
                }
            }
            if self.reviewed.contains(&key) && !self.verdicts.contains(&key) && s.show(id).contains("## Verification") {
                let verdict = "summary: the file is there and says what it should\n\nI read the row's one commit: it adds the file its review names.\n";
                let (ok, out) = s.as_seat("", &["verified", id, "--holds", "--from", "-", "--caption", "the work holds"], verdict);
                assert!(ok, "rehearsal: the verdict on {id} on {}'s spoke: {out}", s.user);
                self.verdicts.insert(key);
            }
        }
        // A tell typed into a seat on a message row: its agent replies as the
        // footer says, from the seat it was typed into.
        for line in s.typed() {
            let Some((id, _)) = line.split_once(' ') else { continue };
            if self.replied.contains(id) || !line.contains("is the third row landed") {
                continue;
            }
            let Some(footer) = line.split("reply: ").nth(1) else { continue };
            let words: Vec<&str> = footer.split_whitespace().collect();
            let (Some(to), Some(re)) = (words.get(2), words.iter().skip_while(|w| **w != "--re").nth(1)) else { continue };
            let answer = rows.iter().filter_map(|r| r["id"].as_str()).find(|i| title(i).starts_with("Answer for "));
            let Some(seat) = answer.and_then(|a| s.seat_of(a)) else { continue };
            let (ok, out) = s.as_seat(&seat, &["tell", to, "--re", re.trim_end_matches(']'), "-"], "yes, it is on the shared trunk");
            assert!(ok, "rehearsal: {}'s seat replies to {to}: {out}", s.user);
            self.replied.insert(id.to_string());
        }
        // The helper's person cuts the link, and restores it, when asked.
        let said = std::fs::read_to_string(s.dir.join("gate.out")).unwrap_or_default();
        if !self.cut && said.contains(">>   cut this machine's link") {
            s.proxy.cut();
            self.cut = true;
        }
        if self.cut && !self.restored && said.contains(">>   restore the link") {
            s.proxy.point_at(Some(rig.addr()));
            self.restored = true;
        }
    }
}

/// The two-machine leg's script, rehearsed on this sandbox: `mp-gate.sh owner`
/// on Ed's spoke and `mp-gate.sh helper` on Jesper's, side by side, each ends
/// with every step `ok`.
#[test]
fn the_two_machine_script_runs_on_the_sandbox_with_every_step_ok() {
    let Some(rig) = HubRig::up("s") else { return };
    let mut ed = Spoke::up(&rig, "ed");
    let mut jesper = Spoke::up(&rig, "jesper");
    ed.start_daemon();
    jesper.start_daemon();
    for s in [&ed, &jesper] {
        s.wait("setup", "a first sync turn", WAIT, |s| s.turns() >= 1);
    }
    let mut owner = run_gate_script(&ed, "owner");
    let mut helper = run_gate_script(&jesper, "helper");
    let mut players = Players::default();
    let end = Instant::now() + Duration::from_secs(600);
    let (mut o, mut h) = (None, None);
    while o.is_none() || h.is_none() {
        if Instant::now() > end {
            let _ = owner.kill();
            let _ = helper.kill();
        }
        players.play(&rig, &ed);
        players.play(&rig, &jesper);
        o = o.or_else(|| owner.try_wait().unwrap());
        h = h.or_else(|| helper.try_wait().unwrap());
        std::thread::sleep(Duration::from_millis(500));
    }
    let said = |s: &Spoke| std::fs::read_to_string(s.dir.join("gate.out")).unwrap_or_default();
    let (eds, jespers) = (said(&ed), said(&jesper));
    let both = format!(
        "owner (ed):\n{eds}\nhelper (jesper):\n{jespers}\ned's cycle.log (tail):\n{}\njesper's cycle.log (tail):\n{}\ned's list:\n{}",
        tail(&ed.cycle_log(), 40),
        tail(&jesper.cycle_log(), 40),
        ed.run(&["worklist", "show", LIST]).1,
    );
    let shows: Vec<String> = [&ed, &jesper]
        .iter()
        .flat_map(|s| s.rows().into_iter().filter_map(|r| r["id"].as_str().map(str::to_string)).map(move |id| format!("{} {id}:\n{}", s.user, tail(&s.show(&id), 25))))
        .collect();
    let both = format!("{both}\nplayed: reviewed {:?} verdicts {:?} replied {:?}\n{}", players.reviewed, players.verdicts, players.replied, shows.join("\n"));
    assert!(o.unwrap().success() && eds.contains("mp-gate: owner finished"), "the owner's script ends ok:\n{both}");
    assert!(h.unwrap().success() && jespers.contains("mp-gate: helper finished"), "the helper's script ends ok:\n{both}");
    assert!(!both.contains("FAIL"), "no step failed:\n{both}");
    for step in 1..=8 {
        assert!(both.contains(&format!("== step {step}:")), "step {step} ran:\n{both}");
    }
    assert!(players.cut && players.restored, "the helper's script asked for the link to be cut and restored:\n{both}");
    println!("owner (ed):\n{eds}\nhelper (jesper):\n{jespers}");
}
