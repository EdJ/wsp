//! A published list reaches the other member (wsp-185): two spokes, ed and
//! jesper, each a `--fake` sandbox daemon with a login to one in-process
//! wsp-hub, both members of `shared:mp`. Ed adds three private rows and a list
//! holding them, publishes the list, and within a few turns jesper's store
//! holds the list and the three rows under their hub ids, while ed's old ids
//! still show the rows through ids.json.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_hub::{Config, Hub};
use wsp_store_contract::{Command as Cmd, IdempotencyKey, Principal, SharedName, TokenId, UserName};

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
    fn up(root: &Path, who: &str, url: &str, token: &str) -> Spoke {
        let dir = root.join(who);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let hubs = home.join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let login = serde_json::json!({ "url": url, "token": token, "user": who, "at": "2026-11-23T00:00:00Z" });
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("pubsync-{who}-{}", std::process::id());
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
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

    fn has_task(&self, id: &str) -> bool {
        self.store().join("tasks").join(format!("{id}.md")).exists()
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
    let root = PathBuf::from("/tmp").join(format!("ps-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let data = root.join("hub");
    let (ed_token, jesper_token) = {
        let hub = Hub::open(&data).unwrap();
        let apply = |cmd: Cmd, by: &Principal| {
            hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
        };
        let root_user = Principal::Person("root".into());
        let mp = SharedName("mp".into());
        apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
        apply(Cmd::HubWorkspaceAdd { name: mp.clone() }, &root_user);
        let mut tokens = Vec::new();
        for who in ["ed", "jesper"] {
            let (secret, hash) = wsp_machines::token::mint();
            apply(Cmd::HubUserAdd { name: UserName(who.into()), admin: false }, &root_user);
            apply(Cmd::HubMemberSet { workspace: mp.clone(), user: UserName(who.into()), member: true }, &root_user);
            apply(Cmd::HubTokenIssue { id: TokenId(format!("t-{who}")), user: UserName(who.into()), hash }, &root_user);
            tokens.push(secret);
        }
        (tokens.remove(0), tokens.remove(0))
    };
    let cfg = Config {
        data_dir: data,
        bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        port: 0,
        peers: vec!["127.0.0.0/8".into()],
        tls: None,
        name: "wsp-hub".into(),
    };
    let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
    let url = format!("tcp://{}", running.addrs()[0]);

    let mut ed = Spoke::up(&root, "ed", &url, &ed_token);
    let mut jesper = Spoke::up(&root, "jesper", &url, &jesper_token);
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
    running.stop();
    let _ = std::fs::remove_dir_all(&root);
}
