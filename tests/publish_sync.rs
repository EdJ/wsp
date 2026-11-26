//! A published list reaches the other member (wsp-185): two spokes, ed and
//! jesper, each a `--fake` sandbox daemon with a login to one real hub run as
//! an external program (`wsp_spoke::testhub`: `WSP_HUB_BIN` or `WSP_HUB_IMAGE`),
//! both members of `shared:mp`. Ed adds three private rows and a list
//! holding them, publishes the list, and within a few turns jesper's store
//! holds the list and the three rows under their hub ids, while ed's old ids
//! still show the rows through ids.json.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_spoke::testhub::{Setup, TestHub};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
const TURN_MS: &str = "300";

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

/// One person's wsp: a fake sandbox, logged in to the hub, with a daemon.
struct Spoke {
    dir: PathBuf,
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    home: PathBuf,
    daemon: Option<Child>,
}

impl Spoke {
    fn up(root: &Path, who: &str, hub: &TestHub, token: &str) -> Spoke {
        let dir = root.join(who);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("pubsync-{who}-{}", std::process::id());
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).env("WSP_SANDBOX_TETHER", std::process::id().to_string()).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        // The login, as `wsp login` writes it, under the sandbox's hub home (wsp-490).
        let hubs = std::path::PathBuf::from(&env["WSP_HUB_HOME"]).join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let mut login = serde_json::json!({ "url": hub.url(), "token": token, "user": who, "at": "2026-11-23T00:00:00Z" });
        if let Some(ca) = hub.ca() {
            login["ca"] = serde_json::json!(ca.display().to_string());
        }
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = Spoke { dir, name, outer, env, home, daemon: None };
        s.ok(&["init"]);
        s.ok(&["project", "add", "alpha", "--name", "Alpha"]);
        s
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

    fn start_daemon(&mut self) {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let out = std::fs::File::create(self.dir.join("daemon.out")).unwrap();
        let err = out.try_clone().unwrap();
        let mut c = self.cmd(&["daemon"]);
        c.env("WSP_API_PORT", port.to_string()).env("WSP_HUB_SYNC_MS", TURN_MS).stdout(out).stderr(err);
        self.daemon = Some(c.spawn().unwrap());
    }

    fn store(&self) -> PathBuf {
        PathBuf::from(&self.env["WSP_HOME"])
    }

    fn state(&self) -> PathBuf {
        PathBuf::from(&self.env["WSP_STATE"])
    }

    fn turns(&self) -> u64 {
        std::fs::read_to_string(self.state().join("hub/sync.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["turns"].as_u64())
            .unwrap_or(0)
    }

    fn add(&self, title: &str) -> String {
        self.ok(&["add", title, "-p", "alpha"]);
        let v: Value = serde_json::from_str(&self.ok(&["ls", "--json"])).unwrap();
        let rows = v.as_array().cloned().or_else(|| v["rows"].as_array().cloned()).unwrap_or_default();
        rows.iter().find(|r| r["title"].as_str() == Some(title)).and_then(|r| r["id"].as_str()).unwrap().to_string()
    }

    /// The row is here: whole, or (rows another member owns, wsp-465) as a
    /// head in the synced cache.
    fn has_task(&self, id: &str) -> bool {
        self.store().join("tasks").join(format!("{id}.md")).exists()
            || std::fs::read_to_string(self.state().join("hub/heads.json")).is_ok_and(|t| t.contains(&format!("\"{id}\"")))
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

fn wait(what: &str, within: Duration, mut done: impl FnMut() -> bool, why: impl Fn() -> String) {
    let end = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < end, "{what} not within {within:?}: {}", why());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_published_list_and_its_three_rows_reach_the_other_member_under_hub_ids() {
    let Some(hub) = TestHub::start(Setup::default()) else { return };
    let tokens = hub.seed_members(&["mp"], &["ed", "jesper"]);
    let root = PathBuf::from("/tmp").join(format!("ps-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let mut ed = Spoke::up(&root, "ed", &hub, &tokens["ed"]);
    let mut jesper = Spoke::up(&root, "jesper", &hub, &tokens["jesper"]);
    ed.start_daemon();
    jesper.start_daemon();

    let old: Vec<String> = ["one", "two", "three"].iter().map(|t| ed.add(t)).collect();
    ed.ok(&["worklist", "new", "plan", "three rows"]);
    for id in &old {
        ed.ok(&["worklist", "add", "plan", id]);
    }
    ed.ok(&["publish", "plan", "--to", "shared:mp"]);

    // New ids: the old ones are gone as files, and `wsp show` still finds them.
    let listing = ed.ok(&["ls", "--json"]);
    for (o, title) in old.iter().zip(["one", "two", "three"]) {
        let shown = ed.ok(&["show", o]);
        assert!(shown.contains(title), "show {o}: {shown}");
    }
    let ids_now: Vec<String> = serde_json::from_str::<Value>(&listing)
        .unwrap()
        .as_array()
        .cloned()
        .or_else(|| serde_json::from_str::<Value>(&listing).unwrap()["rows"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(ids_now.len(), 3, "ed still holds three rows: {listing}");
    assert!(ids_now.iter().all(|id| !old.contains(id)), "the rows took new ids from the hub: {old:?} -> {ids_now:?}");

    // Jesper holds nothing of his own; he takes the list and the rows from the hub.
    let jturns = jesper.turns();
    let list_file = jesper.store().join("worklists/plan.md");
    wait(
        "jesper holds the list and its rows",
        Duration::from_secs(60),
        || list_file.exists() && ids_now.iter().all(|id| jesper.has_task(id)),
        || format!("turns {} -> {}; ed ids {ids_now:?}", jturns, jesper.turns()),
    );
    let text = std::fs::read_to_string(&list_file).unwrap();
    for id in &ids_now {
        assert!(text.contains(id.as_str()), "the list names {id}: {text}");
    }
    drop((ed, jesper));
    drop(hub);
    let _ = std::fs::remove_dir_all(&root);
}
