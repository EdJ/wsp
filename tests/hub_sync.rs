//! Spoke mode's sync worker as processes (wsp-464): a `wsp daemon` in a `--fake`
//! sandbox whose home holds a login, and a wsp-hub served in this process over
//! loopback TCP. Between them a proxy that records every byte both ways, so a
//! frame is read as the wire carried it, and that lets the hub be stopped and
//! started again behind one address.
//!
//! 1. A row added to a shared workspace reaches the hub as one object within
//!    two of the worker's turns; a private row never does: no object in the
//!    hub's store names it, and no frame carries its id.
//! 2. With the hub stopped, the worker says so in one line, keeps running, and
//!    syncs when the hub is back.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_hub::{Config, Hub};
use wsp_store_contract::{Command as Cmd, IdempotencyKey, Principal, RowId, SharedName, Subject, TokenId, UserName};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
/// The worker's turn here: half a second, not ten.
const TURN_MS: &str = "500";
const WORKSPACE: &str = "shared:mp";

// ─────────────────────────────── the proxy ───────────────────────────────

/// A loopback address that forwards each connection to the hub, while there is
/// one, and records what passed. With no hub it takes a connection and closes
/// it, as a stopped hub's port would refuse it.
struct Proxy {
    addr: SocketAddr,
    upstream: Arc<Mutex<Option<SocketAddr>>>,
    wire: Arc<Mutex<Vec<u8>>>,
}

impl Proxy {
    fn start() -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
        let wire = Arc::new(Mutex::new(Vec::new()));
        let (up, w) = (upstream.clone(), wire.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(client) = conn else { continue };
                let to = *up.lock().unwrap();
                let Some(hub) = to.and_then(|a| TcpStream::connect(a).ok()) else {
                    let _ = client.shutdown(Shutdown::Both);
                    continue;
                };
                for (from, into) in [(client.try_clone().unwrap(), hub.try_clone().unwrap()), (hub, client)] {
                    let w = w.clone();
                    std::thread::spawn(move || pipe(from, into, &w));
                }
            }
        });
        Proxy { addr, upstream, wire }
    }

    fn point_at(&self, hub: Option<SocketAddr>) {
        *self.upstream.lock().unwrap() = hub;
    }

    fn wire(&self) -> String {
        String::from_utf8_lossy(&self.wire.lock().unwrap()).into_owned()
    }
}

/// Copy `from` into `into` until either ends, recording each read; then close both.
fn pipe(mut from: TcpStream, mut into: TcpStream, wire: &Mutex<Vec<u8>>) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                wire.lock().unwrap().extend_from_slice(&buf[..n]);
                if into.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = into.shutdown(Shutdown::Both);
}

// ──────────────────────────────── the rig ────────────────────────────────

/// `wsp` with none of this seat's own `WSP_*`, `COMPOUND_*` or `CLAUDE*`.
fn bare(env: &BTreeMap<String, String>, home: &Path) -> Command {
    let mut c = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.envs(env)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("WSP_TEST_MACHINE", MACHINE)
        .env("TZ", "UTC")
        .env("WSP_HOST", "manager");
    c
}

/// A hub (its data directory, served while `running`), the proxy in front of
/// it, and a fake sandbox whose home is logged in to it as `ann`, a member of
/// `mp`.
struct Rig {
    dir: PathBuf,
    data: PathBuf,
    running: Option<wsp_hub::serve::Running>,
    proxy: Proxy,
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    home: PathBuf,
    daemon: Option<Child>,
}

impl Rig {
    fn up(tag: &str) -> Rig {
        // Short: the fake backend's unix socket lives under it.
        let dir = PathBuf::from("/tmp").join(format!("hs{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // The hub: root, the workspace, and ann in it with a token.
        let data = dir.join("hub");
        let token = {
            let hub = Hub::open(&data).unwrap();
            let apply = |cmd: Cmd, by: &Principal| {
                hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
            };
            let root = Principal::Person("root".into());
            let mp = SharedName("mp".into());
            apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
            apply(Cmd::HubWorkspaceAdd { name: mp.clone() }, &root);
            let (secret, hash) = wsp_machines::token::mint();
            apply(Cmd::HubUserAdd { name: UserName("ann".into()), admin: false }, &root);
            apply(Cmd::HubMemberSet { workspace: mp, user: UserName("ann".into()), member: true }, &root);
            apply(Cmd::HubTokenIssue { id: TokenId("t-ann".into()), user: UserName("ann".into()), hash }, &root);
            secret
        };
        let proxy = Proxy::start();

        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("hubsync{tag}");
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        // The login, as `wsp login` writes it, under the sandbox's hub home (wsp-490).
        let hubs = std::path::PathBuf::from(&env["WSP_HUB_HOME"]).join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let login = serde_json::json!({ "url": format!("tcp://{}", proxy.addr), "token": token, "user": "ann", "at": "2026-11-23T00:00:00Z" });
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut rig = Rig { dir, data, running: None, proxy, name, outer, env, home, daemon: None };
        rig.ok(&["init"]);
        rig.ok(&["project", "add", "alpha", "--name", "Alpha"]);
        rig.start_hub();
        rig
    }

    fn start_hub(&mut self) {
        let cfg = Config {
            data_dir: self.data.clone(),
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: None,
            name: "wsp-hub".into(),
            lease_days: wsp_hub::DEFAULT_LEASE_DAYS,
        };
        let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
        self.proxy.point_at(Some(running.addrs()[0]));
        self.running = Some(running);
    }

    fn stop_hub(&mut self) {
        self.proxy.point_at(None);
        if let Some(r) = self.running.take() {
            r.stop();
        }
    }

    fn hub(&self) -> &Hub {
        self.running.as_ref().expect("the hub is up").hub()
    }

    /// The hub's head for `row` in the workspace, if it has one.
    fn hub_head(&self, row: &str) -> Option<wsp_api::ObjectId> {
        let refs = self.hub().heads(&wsp_api::WorkspaceId(WORKSPACE.into())).unwrap();
        refs.get(&Subject::Row(RowId(row.into()))).copied()
    }

    /// Every object file in the hub's store, as text.
    fn hub_objects(&self) -> Vec<String> {
        fn walk(dir: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if let Ok(b) = std::fs::read(&p) {
                    out.push(String::from_utf8_lossy(&b).into_owned());
                }
            }
        }
        let mut out = Vec::new();
        walk(self.hub().objects().root(), &mut out);
        out
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = bare(&self.env, &self.home);
        c.args(args);
        c
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(o.status.success(), "wsp {}: {}{}", args.join(" "), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
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

    fn daemon_alive(&mut self) -> bool {
        self.daemon.as_mut().is_some_and(|d| d.try_wait().unwrap().is_none())
    }

    fn daemon_out(&self) -> String {
        std::fs::read_to_string(self.dir.join("daemon.out")).unwrap_or_default()
    }

    fn threads(&self) -> Vec<String> {
        let pid = self.daemon.as_ref().expect("the daemon is up").id();
        let task = format!("/proc/{pid}/task");
        if !Path::new(&task).exists() {
            // No /proc (macOS): thread names cannot be read, so the worker is
            // known by what only it does, taking turns.
            return if self.turns() > 0 { vec!["hub-sync".to_string()] } else { Vec::new() };
        }
        std::fs::read_dir(task)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok())
            .map(|n| n.trim().to_string())
            .collect()
    }

    /// The worker's `sync.json`.
    fn status(&self) -> Value {
        std::fs::read_to_string(self.state().join("hub/sync.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
    }

    fn turns(&self) -> u64 {
        self.status()["turns"].as_u64().unwrap_or(0)
    }

    /// The worker's lines in `cycle.log`.
    fn said(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("cycle.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("hub-sync:"))
            .map(str::to_string)
            .collect()
    }

    fn wait(&mut self, what: &str, within: Duration, mut done: impl FnMut(&mut Rig) -> bool) {
        let end = Instant::now() + within;
        while !done(self) {
            assert!(
                Instant::now() < end,
                "{what} not within {within:?}; sync.json {}; said {:?}\ndaemon: {}",
                self.status(),
                self.said(),
                self.daemon_out()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Start the daemon and wait for the worker's first turn.
    fn daemon_turning(&mut self) {
        self.start_daemon();
        self.wait("the worker's first turn", Duration::from_secs(60), |r| r.turns() >= 1);
    }

    /// Add a row titled `title`, in `ws` (`None` is the private workspace); its id.
    fn add(&self, title: &str, ws: Option<&str>) -> String {
        let mut args = vec!["add", title, "-p", "alpha"];
        if let Some(w) = ws {
            args.extend(["--workspace", w]);
        }
        self.ok(&args);
        let ls = self.ok(&["ls", "--json"]);
        let v: Value = serde_json::from_str(&ls).unwrap_or_else(|e| panic!("ls --json: {e}: {ls}"));
        let rows = v.as_array().cloned().or_else(|| v["rows"].as_array().cloned()).unwrap_or_default();
        rows.iter()
            .find(|r| r["title"].as_str() == Some(title))
            .and_then(|r| r["id"].as_str())
            .unwrap_or_else(|| panic!("{title} is not in ls --json: {ls}"))
            .to_string()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        self.stop_hub();
        let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &self.name]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ──────────────────────────────── the tests ────────────────────────────────

#[test]
fn a_shared_row_reaches_the_hub_as_one_object_within_two_turns_and_a_private_one_never_does() {
    let mut rig = Rig::up("a");
    rig.daemon_turning();
    assert!(rig.threads().iter().any(|t| t == "hub-sync"), "no hub-sync thread: {:?}", rig.threads());

    let private = rig.add("Kept at home", None);
    let shared = rig.add("Told to the team", Some(WORKSPACE));
    let from = rig.turns();
    rig.wait("the shared row on the hub", Duration::from_secs(30), |r| r.hub_head(&shared).is_some());
    let took = rig.turns() - from;
    assert!(took <= 2, "the shared row took {took} turns to reach the hub");

    // Two more turns: nothing else is sent.
    let now = rig.turns();
    rig.wait("two more turns", Duration::from_secs(30), |r| r.turns() >= now + 2);
    let objects = rig.hub_objects();
    assert_eq!(objects.len(), 1, "the hub holds one object: {objects:?}");
    assert!(objects[0].contains(&shared) && objects[0].contains("Told to the team"), "the object is the shared row: {}", objects[0]);
    let head = rig.hub_head(&shared).unwrap();
    assert_eq!(rig.hub().get(&[head]).unwrap().len(), 1, "the head is the object held");

    // The private row: in no object and in no frame.
    for o in &objects {
        assert!(!o.contains(&private) && !o.contains("Kept at home"), "an object on the hub names the private row: {o}");
    }
    let wire = rig.proxy.wire();
    assert!(wire.contains(&shared), "the frames were not recorded (no shared row in them)");
    assert!(!wire.contains(&private), "a frame carries the private row's id {private}");
    assert!(!wire.contains("Kept at home"), "a frame carries the private row's title");
    assert_eq!(rig.status()["link"], "up");
}

#[test]
fn with_the_hub_stopped_the_worker_says_so_once_keeps_running_and_syncs_when_it_is_back() {
    let mut rig = Rig::up("b");
    rig.daemon_turning();
    let first = rig.add("Before the outage", Some(WORKSPACE));
    rig.wait("the first row on the hub", Duration::from_secs(30), |r| r.hub_head(&first).is_some());

    rig.stop_hub();
    let second = rig.add("During the outage", Some(WORKSPACE));
    let from = rig.turns();
    // Several tries: the change, then 1 s and 2 s of backoff.
    rig.wait("three turns with the hub down", Duration::from_secs(30), |r| r.turns() >= from + 3);
    assert_eq!(rig.status()["link"], "down", "{}", rig.status());
    let down: Vec<String> = rig.said().into_iter().filter(|l| l.contains("unreachable")).collect();
    assert_eq!(down.len(), 1, "one line for the hub going away: {:?}", rig.said());
    assert!(rig.daemon_alive(), "the daemon ended: {}", rig.daemon_out());
    assert!(rig.threads().iter().any(|t| t == "hub-sync"), "the worker ended: {:?}", rig.threads());

    rig.start_hub();
    rig.wait("the second row on the hub", Duration::from_secs(60), |r| r.hub_head(&second).is_some());
    // The turn that moved the ref writes `sync.json` when it ends.
    rig.wait("the link up", Duration::from_secs(10), |r| r.status()["link"] == "up");
    let said = rig.said();
    assert_eq!(said.iter().filter(|l| l.contains("unreachable")).count(), 1, "still one line for it: {said:?}");
    assert_eq!(said.iter().filter(|l| l.contains("answers again")).count(), 1, "one line for its return: {said:?}");
}
