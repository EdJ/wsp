//! Spoke mode's sync worker as processes (wsp-464): a `wsp daemon` in a `--fake`
//! sandbox whose home holds a login, and a real hub run as an external program
//! (`wsp_spoke::testhub`: `WSP_HUB_BIN` or `WSP_HUB_IMAGE`). Between them a tap
//! that records every line both ways in the clear, so a frame is read as the
//! wire carried it, and that lets the hub be stopped and started again behind
//! one address.
//!
//! 1. A row added to a shared workspace reaches the hub as one object within
//!    two of the worker's turns; a private row never does: no object in the
//!    hub's store names it, and no frame carries its id.
//! 2. With the hub stopped, the worker says so in one line, keeps running, and
//!    syncs when the hub is back.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_objects::{ObjectId, WorkspaceId};
use wsp_spoke::testhub::{Setup, Tap, TestHub};
use wsp_store_contract::{RowId, Subject};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
/// The worker's turn here: half a second, not ten.
const TURN_MS: &str = "500";
const WORKSPACE: &str = "shared:mp";

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

/// A real hub (`wsp_spoke::testhub`), the tap in front of it (which records
/// every line both ways, and stays one address across a stop and a start),
/// and a fake sandbox whose home is logged in to it as `ann`, a member of `mp`.
struct Rig {
    dir: PathBuf,
    hub: TestHub,
    token: String,
    proxy: Tap,
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    home: PathBuf,
    daemon: Option<Child>,
}

impl Rig {
    /// `None` when no hub is named: the test is skipped.
    fn up(tag: &str) -> Option<Rig> {
        // The hub: root, the workspace, and ann in it with a token.
        let hub = TestHub::start(Setup::default())?;
        let token = hub.seed_members(&["mp"], &["ann"])["ann"].clone();
        let proxy = Tap::start(&hub);
        // Short: the fake backend's unix socket lives under it.
        let dir = PathBuf::from("/tmp").join(format!("hs{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("hubsync{tag}");
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).env("WSP_SANDBOX_TETHER", std::process::id().to_string()).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        // The login, as `wsp login` writes it, under the sandbox's hub home (wsp-490).
        let hubs = std::path::PathBuf::from(&env["WSP_HUB_HOME"]).join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let mut login = serde_json::json!({ "url": proxy.url(), "token": token, "user": "ann", "at": "2026-11-23T00:00:00Z" });
        if let Some(ca) = proxy.ca() {
            login["ca"] = serde_json::json!(ca.display().to_string());
        }
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        let rig = Rig { dir, hub, token, proxy, name, outer, env, home, daemon: None };
        rig.ok(&["init"]);
        rig.ok(&["project", "add", "alpha", "--name", "Alpha"]);
        Some(rig)
    }

    fn start_hub(&mut self) {
        self.hub.restart();
        self.proxy.point_at(Some(&self.hub));
    }

    fn stop_hub(&mut self) {
        self.proxy.point_at(None);
        self.hub.stop();
    }

    /// The hub's head for `row` in the workspace, if it has one, as ann reads it.
    fn hub_head(&self, row: &str) -> Option<ObjectId> {
        let refs = self.hub.client(&self.token).sync_heads(&WorkspaceId(WORKSPACE.into())).unwrap();
        refs.get(&Subject::Row(RowId(row.into()))).copied()
    }

    /// Every object file in the hub's store, as text.
    fn hub_objects(&self) -> Vec<String> {
        self.hub.data_files().into_iter().filter(|(p, _)| p.starts_with("objects")).map(|(_, b)| String::from_utf8_lossy(&b).into_owned()).collect()
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
        let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &self.name]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ──────────────────────────────── the tests ────────────────────────────────

#[test]
fn a_shared_row_reaches_the_hub_as_one_object_within_two_turns_and_a_private_one_never_does() {
    let Some(mut rig) = Rig::up("a") else { return };
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
    assert_eq!(rig.hub.client(&rig.token).sync_get(vec![head]).unwrap().len(), 1, "the head is the object held");

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
    let Some(mut rig) = Rig::up("b") else { return };
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
