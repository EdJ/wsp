//! The synced cache as processes (wsp-465): two spokes, each a `wsp daemon` in
//! a `--fake` sandbox logged in to one real hub run as an external program
//! (`wsp_spoke::testhub`: `WSP_HUB_BIN` or `WSP_HUB_IMAGE`), each behind a tap
//! that records every line both ways in the clear.
//!
//! 1. A row spoke A adds in a shared workspace is in spoke B's `wsp ls` within
//!    two of B's turns, as a head (`show` says so, its text is not there); once
//!    A allocates it to B's user, B has it whole.
//! 2. B offline across 1,000 changes reconnects with one heads diff: the stream
//!    carries a handful of frames, not a thousand `ref.moved`.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_objects::{Object, ObjectId, WorkspaceId};
use wsp_spoke::hub_client::Client;
use wsp_spoke::testhub::protocol::{Blob, Move, RefMove};
use wsp_spoke::testhub::{Setup, Tap, TestHub};
use wsp_store_contract::{At, Entity, RowId, Subject};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
const TURN_MS: &str = "500";
const WORKSPACE: &str = "shared:mp";

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
    proxy: Tap,
    daemon: Option<Child>,
}

impl Spoke {
    fn new(root: &Path, tag: &str, user: &str, token: &str, hub: &TestHub) -> Spoke {
        let dir = root.join(tag);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let proxy = Tap::start(hub);
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("hc{tag}{}", std::process::id());
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).env("WSP_SANDBOX_TETHER", std::process::id().to_string()).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        // The login, as `wsp login` writes it, under the sandbox's hub home (wsp-490).
        let hubs = std::path::PathBuf::from(&env["WSP_HUB_HOME"]).join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let mut login = serde_json::json!({ "url": proxy.url(), "token": token, "user": user, "at": "2026-11-23T00:00:00Z" });
        if let Some(ca) = proxy.ca() {
            login["ca"] = serde_json::json!(ca.display().to_string());
        }
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
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

impl Spoke {
    /// Its daemon, then its sandbox. Idempotent: the rig calls it before it
    /// removes the directory, and `Drop` calls it again.
    fn teardown(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        let name = std::mem::take(&mut self.name);
        if !name.is_empty() {
            let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &name]).output();
        }
    }
}

impl Drop for Spoke {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// A real hub (`wsp_spoke::testhub`) with `ann` and `bob`, members of `mp`;
/// two spokes, each behind a tap of its own.
struct Rig {
    dir: PathBuf,
    hub: TestHub,
    ann_token: String,
    a: Spoke,
    b: Spoke,
}

impl Rig {
    /// `None` when no hub is named: the test is skipped.
    fn up(tag: &str, setup: Setup) -> Option<Rig> {
        let hub = TestHub::start(setup)?;
        let tokens = hub.seed_members(&["mp"], &["ann", "bob"]);
        let dir = PathBuf::from("/tmp").join(format!("hc{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Spoke::new(&dir, "a", "ann", &tokens["ann"], &hub);
        let b = Spoke::new(&dir, "b", "bob", &tokens["bob"], &hub);
        Some(Rig { dir, hub, ann_token: tokens["ann"].clone(), a, b })
    }

    fn hub_head(&self, row: &str) -> Option<ObjectId> {
        let refs = self.direct().sync_heads(&WorkspaceId(WORKSPACE.into())).unwrap();
        refs.get(&Subject::Row(RowId(row.into()))).copied()
    }

    /// The object `id`, as ann reads it from the hub.
    fn object(&self, id: ObjectId) -> Vec<u8> {
        self.direct().sync_get(vec![id]).unwrap().remove(0).bytes
    }

    /// A client of the hub as ann, straight, past both taps.
    fn direct(&self) -> Client {
        self.hub.client(&self.ann_token)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // The spokes first (their daemons and fakes), then the directory. Not a
        // detached thread on a timer: it raced spoke b's `sandbox rm`, which then
        // found no state to read its fake from and left it at ppid 1 (wsp-778).
        self.a.teardown();
        self.b.teardown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ──────────────────────────────── the tests ────────────────────────────────

#[test]
fn a_row_spoke_a_adds_is_a_head_in_spoke_bs_ls_within_two_turns_and_whole_once_it_is_bobs() {
    let Some(mut rig) = Rig::up("w", Setup::default()) else { return };
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
fn a_head_only_row_bob_claims_is_whole_on_bobs_next_turn() {
    let Some(mut rig) = Rig::up("c", Setup::default()) else { return };
    rig.a.start_daemon();
    rig.b.start_daemon();
    rig.b.wait("B's first turn", Duration::from_secs(60), |s| s.turns() >= 1);
    rig.a.wait("A's first turn", Duration::from_secs(60), |s| s.turns() >= 1);

    let id = rig.a.add("Written on A");
    rig.a.wait("A's row on the hub", Duration::from_secs(30), |_| rig.hub_head(&id).is_some());
    rig.b.wait("the row a head in B's cache", Duration::from_secs(30), |s| {
        s.ls().contains(&id) && std::fs::read_to_string(s.state().join("hub/heads.json")).unwrap_or_default().contains(&id)
    });
    let shown = rig.b.ok(&["show", &id]);
    assert!(shown.contains("head only"), "the row is a head on B before it is leased: {shown}");
    let cache = std::fs::read_to_string(rig.b.state().join("hub/heads.json")).unwrap_or_default();
    assert!(cache.contains(&id), "heads.json: {cache}");

    // B leases it: the next turn fetches it whole and drops the cache entry.
    rig.b.ok(&["claim", &id, "--pane", "w1:p1"]);
    let now = rig.b.turns();
    rig.b.wait("B's next turn", Duration::from_secs(30), |s| s.turns() > now);
    rig.b.wait("the row whole on B", Duration::from_secs(30), |s| !s.ok(&["show", &id]).contains("head only"));
    let whole = rig.b.ok(&["show", &id]);
    assert!(whole.contains("Written on A"), "{whole}");
    let cache = std::fs::read_to_string(rig.b.state().join("hub/heads.json")).unwrap_or_default();
    assert!(!cache.contains(&id), "heads.json still holds the leased row: {cache}");
}

#[test]
fn b_offline_across_a_thousand_changes_reconnects_with_one_heads_diff_not_a_thousand_events() {
    let Some(mut rig) = Rig::up("g", Setup::default()) else { return };
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
        wsp_objects::decode(&rig.object(seed_head)).unwrap()
    else {
        panic!("the seed is a row record")
    };
    let id = "wsp-9001";
    let subject = Subject::Row(RowId(id.into()));
    // Every revision is put (a ref moves only to an object the hub holds), then the ref moves,
    // which are the thousand changes the hub streams, one per change.
    let mut chain = Vec::new();
    let mut blobs = Vec::new();
    let mut parent = None;
    for n in 1..=1000 {
        let mut row = row.clone();
        row.id = RowId(id.into());
        row.title = format!("Revision {n}");
        let obj = Object::Record { workspace: workspace.clone(), subject: subject.clone(), entity: Entity::Row(row), parent, by: by.clone(), at: At(n) };
        let (oid, bytes) = wsp_objects::encode(&obj);
        blobs.push(Blob { id: oid, bytes });
        chain.push((parent, oid));
        parent = Some(oid);
    }
    hub.sync_put(blobs).unwrap();
    for (from, to) in chain {
        let moved = hub.refs_move(RefMove { workspace: ws.clone(), subject: subject.clone(), from, to }).unwrap();
        assert!(matches!(moved, Move::Moved { .. }), "{moved:?}");
    }
    // B back: the frames on its wire from here on.
    let mark = rig.b.proxy.wire().len();
    rig.b.proxy.point_at(Some(&rig.hub));
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

// ───────────────────────── the offline journal (wsp-469) ─────────────────────────

impl Spoke {
    fn journal(&self) -> u64 {
        self.status()["journal"].as_u64().unwrap_or(0)
    }

    fn side(&self) -> Value {
        std::fs::read_to_string(self.state().join("hub/side.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
    }
}

impl Rig {
    fn record(&self, id: ObjectId) -> (wsp_store_contract::Row, Option<ObjectId>) {
        match wsp_objects::decode(&self.object(id)).unwrap() {
            Object::Record { entity: Entity::Row(row), parent, .. } => (row, parent),
            other => panic!("{id} is no row record: {other:?}"),
        }
    }

    /// Who holds `row`'s lease and under which token, as the hub's admin reads its state.
    fn holder(&self, row: &str) -> Option<(String, u64)> {
        let (holder, token) = self.hub.lease_of(row)?;
        Some((format!("{holder:?}"), token.0))
    }


}

#[test]
fn ann_loses_the_hub_mid_row_keeps_working_and_her_journal_fast_forwards_the_row_on_reconnect() {
    let Some(mut rig) = Rig::up("j", Setup::default()) else { return };
    rig.a.start_daemon();
    rig.b.start_daemon();
    rig.a.wait("A's first turn", Duration::from_secs(60), |s| s.turns() >= 1);
    rig.b.wait("B's first turn", Duration::from_secs(60), |s| s.turns() >= 1);

    // Ann leases a row of hers; Bob sees it as a head.
    let id = rig.a.add("Started online");
    rig.a.ok(&["claim", &id, "--pane", "w1:p1"]);
    rig.a.wait("Ann's lease on the hub", Duration::from_secs(30), |_| rig.holder(&id).is_some_and(|(h, _)| h.contains("ann")));
    let (_, token) = rig.holder(&id).unwrap();
    rig.b.wait("the row in Bob's ls", Duration::from_secs(30), |s| s.ls().contains("Started online"));
    let now = rig.a.turns();
    rig.a.wait("Ann's claim synced", Duration::from_secs(30), |s| s.turns() >= now + 2 && s.journal() == 0);
    let before = rig.hub_head(&id).expect("the row is on the hub");

    // The hub goes away mid-row; Ann keeps working.
    rig.a.proxy.cut();
    rig.a.wait("A's link down", Duration::from_secs(30), |s| s.status()["link"] == "down");
    rig.a.ok(&["rename", &id, "Finished offline"]);
    rig.a.wait("the rename journaled", Duration::from_secs(30), |s| s.journal() >= 1);
    let journal = std::fs::read_dir(rig.a.state().join("hub/journal")).unwrap().count();
    assert!(journal >= 1, "one file per object in the journal");
    assert_eq!(rig.hub_head(&id), Some(before), "nothing reached the hub while it was away");
    rig.a.ok(&["note", &id, "still going"]);
    let shown = rig.a.ok(&["show", &id]);
    assert!(shown.contains("w1:p1"), "the seat keeps the row while its lease is in date: {shown}");

    // Back: the journal fast-forwards the row under the same lease.
    rig.a.proxy.point_at(Some(&rig.hub));
    rig.a.wait("the journal pushed", Duration::from_secs(60), |s| s.journal() == 0 && s.status()["link"] == "up");
    rig.a.wait("the hub's head moved", Duration::from_secs(30), |_| rig.hub_head(&id) != Some(before));
    let head = rig.hub_head(&id).unwrap();
    let (row, _) = rig.record(head);
    assert_eq!(row.title, "Finished offline");
    // A fast-forward: the hub's line from the head before the outage leads to it.
    let mut at = Some(head);
    let mut seen = false;
    while let Some(i) = at {
        if i == before {
            seen = true;
            break;
        }
        at = rig.record(i).1;
    }
    assert!(seen, "the pushed line follows the head the hub had before the outage");
    assert_eq!(rig.holder(&id).map(|(_, t)| t), Some(token), "the lease held: the same token, no take again");
    rig.b.wait("Bob's cache shows the work", Duration::from_secs(30), |s| s.ls().contains("Finished offline"));
}

#[test]
fn ann_offline_past_the_ttl_with_the_row_re_leased_by_bob_lands_her_journal_as_a_side_chain() {
    // Short leases: Ann's runs out in real time while she is offline.
    let Some(mut rig) = Rig::up("s", Setup::default().lease_secs(8)) else { return };
    // Ann's code is a git repo; the row's branch is hers.
    let repo = rig.a.dir.join("code");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let o = Command::new("git").args(args).current_dir(&repo).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
        assert!(o.status.success(), "git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "master"]);
    git(&["-c", "user.name=ann", "-c", "user.email=ann@x", "commit", "-q", "--allow-empty", "-m", "trunk"]);
    rig.a.ok(&["project", "add", "beta", "--name", "Beta", "--root", repo.to_str().unwrap()]);
    rig.b.ok(&["project", "add", "beta", "--name", "Beta"]);
    rig.a.start_daemon();
    rig.b.start_daemon();
    rig.a.wait("A's first turn", Duration::from_secs(60), |s| s.turns() >= 1);
    rig.b.wait("B's first turn", Duration::from_secs(60), |s| s.turns() >= 1);

    rig.a.ok(&["add", "Ann's row", "-p", "beta", "--workspace", WORKSPACE]);
    let id = rig.a.ok(&["ls", "--json"]);
    let v: Value = serde_json::from_str(&id).unwrap();
    let rows = v.as_array().cloned().or_else(|| v["rows"].as_array().cloned()).unwrap_or_default();
    let id = rows.iter().find(|r| r["title"] == "Ann's row").and_then(|r| r["id"].as_str()).unwrap().to_string();
    rig.a.ok(&["claim", &id, "--pane", "w1:p1"]);
    rig.a.wait("Ann's lease on the hub", Duration::from_secs(30), |_| rig.holder(&id).is_some_and(|(h, _)| h.contains("ann")));
    git(&["checkout", "-q", "-b", &id]);
    git(&["-c", "user.name=ann", "-c", "user.email=ann@x", "commit", "-q", "--allow-empty", "-m", "ann's work"]);
    let work = git(&["rev-parse", &id]);
    git(&["checkout", "-q", "master"]);
    let now = rig.a.turns();
    rig.a.wait("Ann's claim synced", Duration::from_secs(30), |s| s.turns() >= now + 2 && s.journal() == 0);

    // Ann goes offline, and gives her verdict there.
    rig.a.proxy.cut();
    rig.a.wait("A's link down", Duration::from_secs(30), |s| s.status()["link"] == "down");
    rig.a.ok(&["done", &id, "--force"]);
    rig.a.wait("the verdict journaled", Duration::from_secs(30), |s| s.journal() >= 1);

    // The TTL passes on the hub: Ann's lease runs out and Bob leases the row.
    rig.hub.pass_ttl();
    rig.b.wait("the row in Bob's ls", Duration::from_secs(30), |s| s.ls().contains(&id));
    rig.b.ok(&["claim", &id, "--pane", "w1:p1"]);
    rig.b.wait("Bob's lease", Duration::from_secs(30), |_| rig.holder(&id).is_some_and(|(h, _)| h.contains("bob")));
    rig.b.wait("the row whole on Bob", Duration::from_secs(30), |s| !s.ok(&["show", &id]).contains("head only"));
    // Let Bob's claim settle, both turns of it, before he edits: a rename that
    // lands while the turn is still adopting the claim is written over by it.
    let now = rig.b.turns();
    rig.b.wait("Bob's claim settled", Duration::from_secs(30), |s| s.turns() >= now + 2 && s.journal() == 0);
    rig.b.ok(&["rename", &id, "Bob's take"]);
    rig.b.wait("Bob's work on the hub", Duration::from_secs(30), |_| rig.hub_head(&id).is_some_and(|h| rig.record(h).0.title == "Bob's take"));
    let bobs = rig.hub_head(&id).unwrap();
    let bob_lease = rig.holder(&id).unwrap();

    // Ann back: her journal lands beside the row, not on it.
    rig.a.proxy.point_at(Some(&rig.hub));
    rig.a.wait("the side chain on A", Duration::from_secs(60), |s| s.side()["rows"][&id]["mine"] == true);
    rig.a.wait("A's journal empty", Duration::from_secs(30), |s| s.journal() == 0);
    let side = rig.a.side()["rows"][&id].clone();
    assert_eq!(side["holder"], "bob", "{side}");
    let tip: ObjectId = format!("sha256:{}", side["tip"].as_str().unwrap()).parse().unwrap();
    // No ref leads to a side chain, and the hub serves only what a head leads back to: the
    // object is read from the hub's own object files.
    let kept = rig
        .hub
        .data_files()
        .into_iter()
        .find(|(p, _)| p.starts_with("objects") && p.to_string_lossy().replace('/', "").ends_with(&tip.hex()))
        .map(|(_, b)| b)
        .expect("the side chain's tip is kept on the hub");
    let theirs = match wsp_objects::decode(&kept).unwrap() {
        Object::Record { entity: Entity::Row(row), .. } => row,
        other => panic!("{tip} is no row record: {other:?}"),
    };
    assert_eq!(theirs.id.0, id, "the side chain is on the hub");
    assert_ne!(rig.record(bobs).0.state, theirs.state, "the side chain carries Ann's verdict, the row Bob's: {:?}", theirs.state);
    assert_eq!(rig.hub_head(&id), Some(bobs), "no ref moved for the side chain");
    assert_eq!(rig.holder(&id), Some(bob_lease), "Bob's lease stands");

    // Ann's verdict is refused: the row follows Bob's line on her spoke, and a new one is refused.
    rig.a.wait("Ann's file follows Bob's", Duration::from_secs(30), |s| s.ok(&["show", &id]).contains("Bob's take"));
    let shown = rig.a.ok(&["show", &id]);
    assert!(shown.contains("side chain"), "show says it: {shown}");
    let (ok, said) = rig.a.run(&["done", &id, "--force"]);
    assert!(!ok && said.contains("side chain") && said.contains("bob"), "Ann's verdict is refused: {said}");
    assert!(!rig.hub_head(&id).is_some_and(|h| h != bobs), "and the hub never took it");

    // Both are told.
    assert!(rig.a.said().iter().any(|l| l.contains("side chain") && l.contains(&id)), "Ann's log: {:?}", rig.a.said());
    rig.b.wait("Bob told", Duration::from_secs(30), |s| s.side()["rows"][&id]["old"] == "ann");
    assert!(rig.b.said().iter().any(|l| l.contains("side chain") && l.contains(&id)), "Bob's log: {:?}", rig.b.said());
    assert!(rig.b.ok(&["show", &id]).contains("side chain"));

    // Ann's code is still on her branch.
    assert_eq!(git(&["rev-parse", &id]), work, "the branch still holds Ann's commits");
}

/// wsp-194: `wsp doctor` on a logged-in spoke names the hub, its user, the
/// last sync and the journal; both are read from hubs.json and the worker's
/// status file, so nothing in the line is a literal.
#[test]
fn doctor_on_a_logged_in_spoke_names_the_hub_the_user_the_last_sync_and_the_journal() {
    let Some(mut rig) = Rig::up("d", Setup::default()) else { return };
    rig.a.start_daemon();
    rig.a.wait("A's first good turn", Duration::from_secs(60), |s| s.status()["last_ok"].as_i64().is_some());
    let out = rig.a.ok(&["doctor"]);
    let url = rig.a.proxy.url();
    assert!(out.contains(&format!("hub {url} as ann")), "{out}");
    assert!(out.contains("hub last synced"), "{out}");
    assert!(out.contains("hub journal:") && out.contains("entries"), "{out}");
    assert!(out.contains("hub leases held: none"), "{out}");
    assert!(!out.contains("standalone"), "{out}");
}
