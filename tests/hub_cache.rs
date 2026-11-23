//! The synced cache as processes (wsp-465): two spokes, each a `wsp daemon` in
//! a `--fake` sandbox logged in to one wsp-hub served in this process over
//! loopback TCP, each behind a proxy that records every byte both ways.
//!
//! 1. A row spoke A adds in a shared workspace is in spoke B's `wsp ls` within
//!    two of B's turns, as a head (`show` says so, its text is not there); once
//!    A allocates it to B's user, B has it whole.
//! 2. B offline across 1,000 changes reconnects with one heads diff: the stream
//!    carries a handful of frames, not a thousand `ref.moved`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_api::RefMove;
use wsp_client::{Client, Credential};
use wsp_hub::{Config, Hub};
use wsp_objects::{Object, WorkspaceId};
use wsp_store_contract::{At, Command as Cmd, Entity, IdempotencyKey, Principal, RowId, SharedName, Subject, TokenId, UserName};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
const TURN_MS: &str = "500";
const WORKSPACE: &str = "shared:mp";

// ─────────────────────────────── the proxy ───────────────────────────────

/// A loopback address that forwards each connection to the hub, while there is
/// one, and records what passed. `cut` closes what is open.
struct Proxy {
    addr: SocketAddr,
    upstream: Arc<Mutex<Option<SocketAddr>>>,
    wire: Arc<Mutex<Vec<u8>>>,
    open: Arc<Mutex<Vec<TcpStream>>>,
}

impl Proxy {
    fn start() -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
        let wire = Arc::new(Mutex::new(Vec::new()));
        let open: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let (up, w, o) = (upstream.clone(), wire.clone(), open.clone());
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
                    let w = w.clone();
                    std::thread::spawn(move || pipe(from, into, &w));
                }
            }
        });
        Proxy { addr, upstream, wire, open }
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

    fn wire(&self) -> String {
        String::from_utf8_lossy(&self.wire.lock().unwrap()).into_owned()
    }
}

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

/// One person's wsp: a fake sandbox whose home is logged in to the hub, behind
/// its own proxy.
struct Spoke {
    user: String,
    dir: PathBuf,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    home: PathBuf,
    name: String,
    proxy: Proxy,
    daemon: Option<Child>,
}

impl Spoke {
    fn new(root: &Path, tag: &str, user: &str, token: &str) -> Spoke {
        let dir = root.join(tag);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let proxy = Proxy::start();
        let hubs = home.join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let login = serde_json::json!({ "url": format!("tcp://{}", proxy.addr), "token": token, "user": user, "at": "2026-11-23T00:00:00Z" });
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("hc{tag}{}", std::process::id());
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        let s = Spoke { user: user.into(), dir, outer, env, home, name, proxy, daemon: None };
        s.ok(&["init"]);
        s.ok(&["project", "add", "alpha", "--name", "Alpha"]);
        s
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = bare(&self.env, &self.home);
        c.args(args);
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

    fn said(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("cycle.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("hub-sync:"))
            .map(str::to_string)
            .collect()
    }

    fn wait(&self, what: &str, within: Duration, mut done: impl FnMut(&Spoke) -> bool) {
        let end = Instant::now() + within;
        while !done(self) {
            assert!(
                Instant::now() < end,
                "{}: {what} not within {within:?}; sync.json {}; said {:?}\ndaemon: {}",
                self.user,
                self.status(),
                self.said(),
                std::fs::read_to_string(self.dir.join("daemon.out")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn ls(&self) -> String {
        self.run(&["ls"]).1
    }

    /// Add a shared row; its id.
    fn add(&self, title: &str) -> String {
        self.ok(&["add", title, "-p", "alpha", "--workspace", WORKSPACE]);
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

impl Drop for Spoke {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &self.name]).output();
    }
}

/// A hub with `ann` and `bob`, members of `mp`; two spokes; the hub served
/// behind both proxies.
struct Rig {
    dir: PathBuf,
    running: Option<wsp_hub::serve::Running>,
    data: PathBuf,
    ann_token: String,
    a: Spoke,
    b: Spoke,
}

impl Rig {
    fn up(tag: &str) -> Rig {
        let dir = PathBuf::from("/tmp").join(format!("hc{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("hub");
        let (ann, bob) = {
            let hub = Hub::open(&data).unwrap();
            let apply = |cmd: Cmd, by: &Principal| {
                hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
            };
            let root = Principal::Person("root".into());
            let mp = SharedName("mp".into());
            apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
            apply(Cmd::HubWorkspaceAdd { name: mp.clone() }, &root);
            let mut tokens = Vec::new();
            for who in ["ann", "bob"] {
                let (secret, hash) = wsp_machines::token::mint();
                apply(Cmd::HubUserAdd { name: UserName(who.into()), admin: false }, &root);
                apply(Cmd::HubMemberSet { workspace: mp.clone(), user: UserName(who.into()), member: true }, &root);
                apply(Cmd::HubTokenIssue { id: TokenId(format!("t-{who}")), user: UserName(who.into()), hash }, &root);
                tokens.push(secret);
            }
            (tokens[0].clone(), tokens[1].clone())
        };
        let a = Spoke::new(&dir, "a", "ann", &ann);
        let b = Spoke::new(&dir, "b", "bob", &bob);
        let mut rig = Rig { dir, running: None, data, ann_token: ann, a, b };
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
        };
        let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
        for p in [&self.a.proxy, &self.b.proxy] {
            p.point_at(Some(running.addrs()[0]));
        }
        self.running = Some(running);
    }

    fn hub(&self) -> &Hub {
        self.running.as_ref().expect("the hub is up").hub()
    }

    fn hub_head(&self, row: &str) -> Option<wsp_api::ObjectId> {
        let refs = self.hub().heads(&WorkspaceId(WORKSPACE.into())).unwrap();
        refs.get(&Subject::Row(RowId(row.into()))).copied()
    }

    /// A client of the hub as ann, straight, past both proxies.
    fn direct(&self) -> Client {
        let url = format!("tcp://{}", self.running.as_ref().unwrap().addrs()[0]);
        Client::dial_hub(&url, Credential::User(self.ann_token.clone()), Duration::from_secs(5)).expect("dials the hub")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop();
        }
        // The spokes drop first (their daemons), then the directory.
        let dir = self.dir.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = std::fs::remove_dir_all(dir);
        });
    }
}

// ──────────────────────────────── the tests ────────────────────────────────

#[test]
fn a_row_spoke_a_adds_is_a_head_in_spoke_bs_ls_within_two_turns_and_whole_once_it_is_bobs() {
    let mut rig = Rig::up("w");
    rig.a.start_daemon();
    rig.b.start_daemon();
    rig.b.wait("B's first turn", Duration::from_secs(60), |s| s.turns() >= 1);
    rig.a.wait("A's first turn", Duration::from_secs(60), |s| s.turns() >= 1);

    let id = rig.a.add("Written on A");
    rig.a.wait("A's row on the hub", Duration::from_secs(30), |_| rig.hub_head(&id).is_some());
    let from = rig.b.turns();
    rig.b.wait("the row in B's ls", Duration::from_secs(30), |s| s.ls().contains(&id));
    let took = rig.b.turns() - from;
    assert!(took <= 2, "the row took {took} of B's turns to reach its ls");

    // A head: `show` says so, and the prose is not there.
    let shown = rig.b.ok(&["show", &id]);
    assert!(shown.contains("Written on A"), "{shown}");
    assert!(shown.contains("head only") && shown.contains("--fetch"), "show says it is a head: {shown}");
    let cache = std::fs::read_to_string(rig.b.state().join("hub/heads.json")).unwrap_or_default();
    assert!(cache.contains(&id), "heads.json: {cache}");
    let stream: Value = serde_json::from_str(&std::fs::read_to_string(rig.b.state().join("hub/stream.json")).unwrap()).unwrap();
    assert!(stream["position"].as_u64().is_some() && stream["workspaces"].to_string().contains("shared:mp"), "{stream}");
    // And B pushes nothing back for it: the hub's head stays A's.
    let head = rig.hub_head(&id).unwrap();
    let now = rig.b.turns();
    rig.b.wait("two more turns", Duration::from_secs(30), |s| s.turns() >= now + 2);
    assert_eq!(rig.hub_head(&id), Some(head), "B moved the hub's head with its stub");

    // A allocates it to bob: it is B's own, whole.
    rig.a.ok(&["allocate", &id, "bob"]);
    rig.b.wait("the row whole on B", Duration::from_secs(30), |s| !s.ok(&["show", &id]).contains("head only"));
    let whole = rig.b.ok(&["show", &id]);
    assert!(whole.contains("Written on A"), "{whole}");
    assert!(!std::fs::read_to_string(rig.b.state().join("hub/heads.json")).unwrap_or_default().contains(&id));

    // A second row, fetched by hand.
    let second = rig.a.add("Another on A");
    rig.b.wait("the second row in B's ls", Duration::from_secs(30), |s| s.ls().contains(&second));
    assert!(rig.b.ok(&["show", &second]).contains("head only"));
    let fetched = rig.b.ok(&["show", &second, "--fetch"]);
    assert!(!fetched.contains("head only"), "--fetch brings it whole: {fetched}");
}

#[test]
fn b_offline_across_a_thousand_changes_reconnects_with_one_heads_diff_not_a_thousand_events() {
    let mut rig = Rig::up("g");
    rig.a.start_daemon();
    rig.b.start_daemon();
    rig.b.wait("B's first turn", Duration::from_secs(60), |s| s.turns() >= 1);
    let seed = rig.a.add("The row the changes copy");
    rig.a.wait("A's row on the hub", Duration::from_secs(30), |_| rig.hub_head(&seed).is_some());
    rig.b.wait("the row in B's ls", Duration::from_secs(30), |s| s.ls().contains(&seed));

    // B off the hub: connections closed, nothing behind its proxy.
    rig.b.proxy.cut();
    rig.b.wait("B's link down", Duration::from_secs(30), |s| s.status()["link"] == "down");

    // A thousand changes of one row, straight at the hub.
    let mut hub = rig.direct();
    let ws = WorkspaceId(WORKSPACE.into());
    let seed_head = rig.hub_head(&seed).unwrap();
    let Object::Record { workspace, entity: Entity::Row(row), by, .. } =
        wsp_objects::decode(&rig.hub().get(&[seed_head]).unwrap()[0].bytes).unwrap()
    else {
        panic!("the seed is a row record")
    };
    let id = "wsp-9001";
    let subject = Subject::Row(RowId(id.into()));
    let mut parent = None;
    for n in 1..=1000 {
        let mut row = row.clone();
        row.id = RowId(id.into());
        row.title = format!("Revision {n}");
        let obj = Object::Record { workspace: workspace.clone(), subject: subject.clone(), entity: Entity::Row(row), parent, by: by.clone(), at: At(n) };
        let (oid, bytes) = wsp_objects::encode(&obj);
        hub.sync_put(vec![wsp_api::Blob { id: oid, bytes }]).unwrap();
        let moved = hub.refs_move(RefMove { workspace: ws.clone(), subject: subject.clone(), from: parent, to: oid }).unwrap();
        assert!(matches!(moved, wsp_api::Move::Moved { .. }), "{moved:?}");
        parent = Some(oid);
    }

    // B back: the frames on its wire from here on.
    let mark = rig.b.proxy.wire().len();
    let running = rig.running.as_ref().unwrap().addrs()[0];
    rig.b.proxy.point_at(Some(running));
    rig.b.wait("the last revision in B's ls", Duration::from_secs(60), |s| s.ls().contains("Revision 1000"));
    std::thread::sleep(Duration::from_secs(2));
    let wire = rig.b.proxy.wire();
    let after = &wire[mark..];
    let moved = after.matches("ref.moved").count();
    let heads = after.matches("sync.heads").count();
    assert!(moved <= 5, "B was sent {moved} ref.moved frames after its reconnect, not a heads diff");
    assert!(heads >= 1, "B asked for the hub's heads");
    assert!(rig.b.said().iter().any(|l| l.contains("heads diff")), "the log says the stream skipped ahead: {:?}", rig.b.said());
}
