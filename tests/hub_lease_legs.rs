//! wsp-896: a shared row run as legs holds its hub lease while a leg runs.
//!
//! A leg's seat claims no row, so before wsp-896 a row worked by legs held no
//! hub lease: another user's spoke could take it, and `ls --json` drew it as
//! unheld. Each leg's spawn now takes the row's lease into the spoke's book;
//! it goes back once the last leg ends (review or a block).
//!
//! One real `wsp-hub` over TLS (`wsp_spoke::testhub`: `WSP_HUB_BIN` or
//! `WSP_HUB_IMAGE`; with neither the test is skipped, as the other hub tests
//! are), `ed` and `jesper` members of `shared:core`, and two spokes, each a
//! `wsp sandbox --fake` logged in as its user with a fake manager and a `wsp
//! daemon` syncing. A bare repository is the shared git remote and each
//! spoke's project `alpha` a clone of it.
//!
//! The rig is copied from `tests/multiplayer_v1.rs` (wsp-587's, which this
//! file does not share) and cut to what this case needs: no proxy, no list.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use wsp_objects::WorkspaceId;
use wsp_spoke::testhub::{protocol, Setup, TestHub};
use wsp_store_contract::{RowId, Subject};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
/// The sync worker's turn on both spokes.
const TURN_MS: &str = "300";
const SHARED: &str = "shared:core";
/// The bound on a wait for something that is not the thing under test.
const WAIT: Duration = Duration::from_secs(90);

/// The context-pack builder a spawn asks for: a stub that answers `meta` with
/// no likely files and anything else with one line.
const PACK: &str = r#"#!/bin/sh
case " $* " in
  *" --depth meta "*) echo '{"likely": []}' ;;
  *) echo "CONTEXT PACK (the test's stub)" ;;
esac
"#;

/// compound-sup as far as a seat's spawn and `vitals` use it: a spawn is a
/// process that lives as long as this test, `vitals` says whether it does.
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
*) echo "fake compound-sup: no verb $verb" >&2; exit 2 ;;
esac
"#;

// ─────────────────────────────── the hub ───────────────────────────────

/// The hub over TLS, its users' tokens, the shared remote.
struct HubRig {
    dir: PathBuf,
    hub: TestHub,
    tokens: BTreeMap<String, String>,
    remote: PathBuf,
}

impl HubRig {
    /// `None` when no hub is named: the test is skipped.
    fn up() -> Option<HubRig> {
        let hub = TestHub::start(Setup::default().tls())?;
        let core: &[&str] = &["core"];
        let tokens = hub.seed(&["core"], &[("ed", core), ("jesper", core)]);
        // Under /tmp: a fake's socket path must fit a sockaddr.
        let dir = PathBuf::from("/tmp").join(format!("hll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let remote = dir.join("remote.git");
        git(&dir, &["init", "-q", "--bare", "-b", "master", remote.to_str().unwrap()]);
        let seed = dir.join("seed");
        git(&dir, &["clone", "-q", remote.to_str().unwrap(), seed.to_str().unwrap()]);
        std::fs::create_dir_all(seed.join("src")).unwrap();
        std::fs::write(seed.join("src/lib.rs"), "// alpha\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "trunk"]);
        git(&seed, &["push", "-q", "origin", "master"]);
        Some(HubRig { dir, hub, tokens, remote })
    }

    fn ca(&self) -> &Path {
        self.hub.ca().expect("a TLS hub")
    }

    fn row_head(&self, row: &str) -> bool {
        let heads = self.hub.client(&self.tokens["ed"]).sync_heads(&WorkspaceId(SHARED.into())).unwrap().heads;
        heads.contains_key(&Subject::Row(RowId(row.into())))
    }

    /// The user whose spoke holds `row`'s lease, as `wsp hub lease ls` reads
    /// the hub's state. A hub names a row's lease with the row quoted in it,
    /// or (one built before that) as `row:<id>`: both are read.
    fn holder(&self, row: &str) -> Option<String> {
        let (quoted, plain) = (format!("\"{row}\""), format!("row:{row}"));
        let lease = self.hub.state().leases.into_values().find(|l| l.name.0.contains(&quoted) || l.name.0 == plain)?;
        Some(match lease.holder {
            protocol::Principal::Person(u) => u.to_string(),
            other => format!("{other:?}"),
        })
    }

    fn leases(&self) -> String {
        format!("{:?}", self.hub.state().leases)
    }
}

impl Drop for HubRig {
    fn drop(&mut self) {
        if std::env::var_os("HLL_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// `git` in `dir`, with no global configuration and a fixed author; its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(["-c", "user.name=hll", "-c", "user.email=hll@example.invalid", "-c", "init.defaultBranch=master"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {} in {}: {}", args.join(" "), dir.display(), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

// ─────────────────────────────── a spoke ───────────────────────────────

/// `wsp` under a spoke's environment, with nothing of the caller's `WSP_*`.
fn bare(env: &BTreeMap<String, String>, home: &Path) -> Command {
    let mut c = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.envs(env)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "hll")
        .env("GIT_AUTHOR_EMAIL", "hll@example.invalid")
        .env("GIT_COMMITTER_NAME", "hll")
        .env("GIT_COMMITTER_EMAIL", "hll@example.invalid")
        .env("WSP_TEST_MACHINE", MACHINE)
        .env("TZ", "UTC")
        .env("NO_COLOR", "1")
        .env("WSP_HOST", "manager");
    c
}

/// One person's wsp: a fake sandbox logged in to the hub, with project
/// `alpha` a clone of the shared remote, a fake manager and a daemon.
struct Spoke {
    user: String,
    dir: PathBuf,
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    home: PathBuf,
    repo: PathBuf,
    manager: Option<Arc<wsp_manager::Manager>>,
    agents: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    daemon: Option<Child>,
}

impl Spoke {
    fn up(rig: &HubRig, who: &str) -> Spoke {
        let dir = rig.dir.join(who);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let pack = dir.join("ixv2");
        std::fs::write(&pack, PACK).unwrap();
        std::fs::set_permissions(&pack, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("hll{}{}", &who[..1], std::process::id());
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).env("WSP_SANDBOX_TETHER", std::process::id().to_string()).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let mut env: BTreeMap<String, String> = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        env.insert("WSP_PACK_BIN".into(), pack.display().to_string());
        // The login, as `wsp login` writes it, under the sandbox's hub home.
        let hubs = PathBuf::from(&env["WSP_HUB_HOME"]).join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let login = serde_json::json!({
            "url": format!("tls://localhost:{}", rig.hub.addr().port()), "token": rig.tokens[who], "user": who,
            "at": "2026-11-23T00:00:00Z", "ca": rig.ca().display().to_string(),
        });
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        // The spoke's fake manager, and the record that names it.
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
        let s = Spoke { user: who.into(), dir, name, outer, env, home, repo, manager: Some(manager), agents: Some(agents), daemon: None };
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

    fn turns(&self) -> u64 {
        std::fs::read_to_string(self.state().join("hub/sync.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["turns"].as_u64())
            .unwrap_or(0)
    }

    fn cycle_log(&self) -> String {
        std::fs::read_to_string(self.state().join("cycle.log")).unwrap_or_default()
    }

    fn book(&self) -> String {
        std::fs::read_to_string(self.state().join("hub/drive.json")).unwrap_or_default()
    }

    fn wait(&self, step: &str, what: &str, mut done: impl FnMut(&Spoke) -> bool) {
        let end = Instant::now() + WAIT;
        while !done(self) {
            assert!(
                Instant::now() < end,
                "{step}: {}: {what}, not within {WAIT:?}\ncycle.log (tail):\n{}\ndaemon: {}",
                self.user,
                tail(&self.cycle_log(), 30),
                tail(&std::fs::read_to_string(self.dir.join("daemon.out")).unwrap_or_default(), 30),
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// `ls --json` on stdout alone. Not `--all`: on this case's spoke `ls
    /// --json --all` answered `[]` where `ls --json` listed the row.
    fn rows(&self) -> Vec<Value> {
        let o = self.cmd(&["ls", "--json"]).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "{}: ls --json: {out}{}", self.user, String::from_utf8_lossy(&o.stderr));
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("ls --json: {e}: {out}"));
        v.as_array().cloned().or_else(|| v["rows"].as_array().cloned()).unwrap_or_default()
    }

    fn row(&self, id: &str) -> Value {
        self.rows().into_iter().find(|r| r["id"].as_str() == Some(id)).unwrap_or(Value::Null)
    }

    fn ids(&self) -> BTreeSet<String> {
        self.rows().iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect()
    }

    fn show(&self, row: &str) -> String {
        self.run(&["show", row, "--log"]).1
    }

    fn claimed(&self, row: &str) -> bool {
        self.show(row).lines().any(|l| l.starts_with("claimed "))
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
        if std::env::var_os("HLL_KEEP").is_none() {
            let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &self.name]).output();
        }
    }
}

/// The fake agents' first turn: every seat the spoke's manager starts begins a
/// turn (what a spawn waits for to read the agent ready) and, a moment later,
/// ends it with the hook's word that it is idle.
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

// ──────────────────────────────── the case ────────────────────────────────

const LEGS: &str = "wsp-896 (a shared row run as legs on Ed's spoke holds his hub lease while a leg runs)";

/// A shared row run as legs on Ed's spoke is leased to him on the hub while
/// its leg runs, though a leg's seat claims nothing; his `ls --json` reads it
/// held by him and his; Jesper's spoke sees the row and is refused it, naming
/// Ed; once the last leg ends by review the lease is gone from the hub and
/// from Ed's `ls --json`.
#[test]
fn a_row_run_as_legs_on_one_spoke_is_refused_to_the_other_while_a_leg_runs() {
    let Some(rig) = HubRig::up() else { return };
    let mut ed = Spoke::up(&rig, "ed");
    let mut jesper = Spoke::up(&rig, "jesper");
    ed.start_daemon();
    jesper.start_daemon();
    for s in [&ed, &jesper] {
        s.wait("setup", "a first sync turn", |s| s.turns() >= 1);
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
    ed.wait(LEGS, "the row on the hub", |_| rig.row_head(&row));
    let order = ed.dir.join("build.order");
    std::fs::write(&order, "Write legs.txt in the row's tree and review it.\n").unwrap();
    ed.ok(&["leg", "add", &row, "build", "--order", order.to_str().unwrap()]);
    assert_eq!(rig.holder(&row), None, "{LEGS}: nobody holds {row} before its leg starts");

    // The leg starts: its seat claims nothing, and the row is Ed's on the hub.
    let out = ed.ok(&["spawn", &row, "-p", "alpha", "--agent", "--leg", "build"]);
    assert!(!ed.claimed(&row), "{LEGS}: a leg's seat claims no row: {}", ed.show(&row));
    let legs: Value = serde_json::from_str(&ed.ok(&["leg", "show", &row, "--json"])).unwrap_or_else(|e| panic!("{LEGS}: leg show --json: {e}: {out}"));
    let seat = legs[0]["pane"]
        .as_str()
        .filter(|_| legs[0]["state"] == "running")
        .unwrap_or_else(|| panic!("{LEGS}: leg build is not running on a seat: {legs}\n{out}"))
        .to_string();
    assert_eq!(
        rig.holder(&row).as_deref(),
        Some("ed"),
        "{LEGS}: the hub records Ed's lease on {row} while its leg runs\nspawn: {out}\nbook: {}\nhub leases: {}",
        ed.book(),
        rig.leases()
    );
    // `ls --json` writes the add-on's row facts as strings (`"mine": "true"`).
    let shown = ed.row(&row);
    assert_eq!((shown["held_by"].as_str(), shown["mine"].as_str()), (Some("ed"), Some("true")), "{LEGS}: Ed's ls --json reads {row} held by him: {shown}\nids: {:?}\nls: {:?}", ed.ids(), ed.run(&["ls"]));

    // Jesper's spoke sees the row, and its spawn is refused while the leg runs.
    jesper.wait(LEGS, &format!("{row} on Jesper's spoke"), |s| s.ids().contains(&row));
    let (ok, said) = jesper.run(&["spawn", &row, "-p", "alpha", "--agent"]);
    assert!(!ok && said.contains("leased by ed"), "{LEGS}: Jesper's spawn of {row} is refused naming Ed: {said}");
    assert!(!jesper.claimed(&row), "{LEGS}: Jesper holds no claim on {row}: {}", jesper.show(&row));
    assert_eq!(rig.holder(&row).as_deref(), Some("ed"), "{LEGS}: {row} is still Ed's on the hub");

    // The last leg ends by review: the lease goes back to the hub.
    let tree = ed.repo.join(".worktrees").join(&row);
    if tree.is_dir() {
        std::fs::write(tree.join("legs.txt"), format!("{row}\n")).unwrap();
        git(&tree, &["add", "legs.txt"]);
        git(&tree, &["commit", "-q", "-m", &format!("{row}: legs.txt")]);
    }
    let (ok, said) = ed.as_seat(&seat, &["review", &row, "Wrote legs.txt; nothing left."], "");
    assert!(ok, "{LEGS}: the leg's seat reviews {row}: {said}");
    ed.wait(LEGS, &format!("{row}'s lease gone from the hub"), |_| rig.holder(&row).is_none());
    let shown = ed.row(&row);
    assert!(shown["held_by"].as_str().is_none(), "{LEGS}: Ed's ls --json reads {row} unheld once its last leg ended: {shown}");
}
