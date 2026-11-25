//! The daemon's mirror (wsp-075): a `wsp daemon` on a `wsp sandbox --fake` store
//! opens `state.db`, serves the API, and mirrors the store's files into it.
//!
//! The binary has no library, so the projections are not called from here: the
//! snapshot is compared, key by key, with what `wsp ls --json` says of the same
//! files (the row's id, title, `fields.status`, `prio`, `tags`, `caption`).
//! The pure projections are tested on a seeded store in `crates/engine/src/mirror.rs`.
//!
//! The daemon listens on `WSP_API_PORT` (7488 unless told), which each test
//! here takes free from the OS so that it never meets the live daemon's.
//!
//! - `a_snapshot_equals_the_files`: (a)
//! - `an_edit_and_a_block_each_reach_the_subscriber_then_quiet`: (b) and (c)
//! - `wsp_api_off_starts_nothing`: (d)

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_client::{Client, Item};
use wsp_store_contract::Principal;

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

/// The upper bound on every wait here for something to arrive: generous, so a
/// machine under load is slow and not failed. Each wait ends as soon as what it
/// waits for is seen.
const BUDGET: Duration = Duration::from_secs(120);

struct Sandbox {
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    state: PathBuf,
    daemon: Option<Child>,
    _dir: PathBuf,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Sandbox {
    fn up(tag: &str) -> Sandbox {
        // Short: the fake backend's unix socket lives under it, and macOS caps that path at 104 bytes.
        let dir = PathBuf::from("/tmp").join(format!("wm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("home").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        let run = |args: &[&str], env: &BTreeMap<String, String>| {
            Command::new(WSP).args(args).envs(env).env_remove("WSP_SEAT_ID").output().unwrap()
        };
        run(&["init"], &outer);
        let name = format!("m{tag}");
        let out = run(&["sandbox", "--fake", "--json", "--name", &name], &outer);
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> =
            v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        let state = PathBuf::from(&env["WSP_STATE"]);
        let sb = Sandbox { name, outer, env, state, daemon: None, _dir: dir };
        sb.wsp(&["init"]);
        sb
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(WSP);
        c.args(args).envs(&self.env).env_remove("WSP_SEAT_ID").env_remove("WSP_API").env_remove("WSP_API_PORT");
        c
    }

    fn wsp(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
    }

    fn start_daemon(&mut self, port: u16, api_off: bool) {
        let mut c = self.cmd(&["daemon"]);
        c.env("WSP_API_PORT", port.to_string()).stdout(Stdio::null()).stderr(Stdio::null());
        if api_off {
            c.env("WSP_API", "off");
        }
        self.daemon = Some(c.spawn().unwrap());
    }

    fn client(&self, wait: Duration) -> Client {
        // The daemon writes api.json once it listens; dial waits for the
        // server, not for the file.
        let end = Instant::now() + wait;
        while !self.state.join("api.json").exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        Client::dial(&self.state, Principal::Wsp("test".into()), wait).expect("the daemon's API answers")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = Command::new(WSP).args(["sandbox", "rm", &self.name]).envs(&self.outer).output();
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}

fn seeded(tag: &str, port: u16) -> Sandbox {
    let mut sb = Sandbox::up(tag);
    sb.wsp(&["add", "one", "--prio", "high", "-t", "a"]);
    sb.wsp(&["add", "two"]);
    sb.start_daemon(port, false);
    sb
}

/// The store the sandbox's files are, opened for seeding what no verb makes
/// here: a claim, a list with groups, an ask, a flag and a machine record.
fn files_of(sb: &Sandbox) -> wsp_store::store::Store {
    wsp_store::store::Store::at(PathBuf::from(&sb.env["WSP_HOME"]), sb.state.clone())
}

fn ask_message(id: &str, text: &str) -> wsp_store::message::Message {
    use wsp_store::message::{Kind, Message, Party, Waiting};
    let mut m = Message::question(
        Party::Pane { pane: "cpd-2".into(), workspace: "w".into() },
        Kind::Direction,
        text,
        Waiting::new("cpd-2", "m-two"),
    );
    m.id = id.into();
    m
}

fn snapshot_with(client: &Client, ready: impl Fn(&Value) -> bool) -> Value {
    feed_from_snapshot_with(client, ready).0
}

/// The first snapshot `ready` holds of, and the feed it came on, which goes on
/// with the events after it.
fn feed_from_snapshot_with(client: &Client, ready: impl Fn(&Value) -> bool) -> (Value, wsp_client::Feed) {
    let mut feed = client.subscribe(None);
    let deadline = Instant::now() + BUDGET;
    loop {
        match feed.next_within(Duration::from_millis(500)) {
            Some(Item::State { state, .. }) => {
                let v = serde_json::to_value(&state).unwrap();
                if ready(&v) {
                    return (v, feed);
                }
                // The mirror had not landed yet: resubscribe.
                feed = client.subscribe(None);
            }
            _ if Instant::now() > deadline => panic!("the snapshot never held the files"),
            _ => {}
        }
    }
}

#[test]
fn a_snapshot_equals_the_files() {
    use wsp_store::model::{self, Machine, Task, Worklist, WorklistStatus};
    let mut sb = Sandbox::up("snap");
    sb.wsp(&["add", "one", "--prio", "high", "-t", "a"]);
    sb.wsp(&["add", "two"]);
    sb.wsp(&["block", "inbox-002", "waiting on the test"]);
    let files: Vec<Value> = serde_json::from_str(&sb.wsp(&["ls", "--json"])).expect("ls --json");
    assert_eq!(files.len(), 2);
    let ids: Vec<String> = files.iter().map(|f| f["id"].as_str().unwrap().to_string()).collect();

    // What no verb makes here, written the way the store does.
    let store = files_of(&sb);
    store.set_claim(&ids[0], serde_json::json!({ "workspace_label": "cpd-2", "agent_id": "a-2" }));
    let mut w = Worklist::new("list-a", "A list");
    w.set_status(WorklistStatus::Running);
    let mut g1 = model::Group::default();
    g1.members = vec![ids[0].clone()];
    g1.verdict = "holds".into();
    let mut g2 = model::Group::default();
    g2.members = vec![ids[1].clone()];
    w.set_groups(&[g1, g2]);
    store.save_worklist(&w).unwrap();
    store.save_message(&ask_message("m-1", "may I?")).unwrap();
    store.set_legacy_flag(&ids[1], serde_json::json!({ "pane": "cpd-4", "reason": "stuck" }));
    let mut mf = Machine::default();
    mf.name = "far".into();
    mf.ssh = "far-alias".into();
    mf.os = "linux".into();
    mf.status = "active".into();
    store.save_machine(&mf).unwrap();
    let tasks: Vec<Task> = ids.iter().map(|i| store.task(i).unwrap()).collect();

    let port = free_port();
    sb.start_daemon(port, false);
    let client = sb.client(BUDGET);
    let state = snapshot_with(&client, |v| {
        let n = |k: &str| v[k].as_object().map_or(0, |o| o.len());
        n("rows") == 2 && n("claims") == 1 && n("lists") == 1 && n("asks") == 2 && v["machines"].get("far").is_some() && v["machines"].get("local").is_some()
    });

    // Rows: the files' `--json` and the task files, key by key.
    for (f, t) in files.iter().zip(&tasks) {
        let id = f["id"].as_str().unwrap();
        let row = &state["rows"][id];
        assert_eq!(row["title"], f["title"], "{id} title");
        assert_eq!(row["project"], "inbox", "{id} project");
        let fields = &row["fields"];
        assert_eq!(fields["status"], f["status"], "{id} status");
        assert_eq!(fields["prio"], f["priority"], "{id} prio");
        assert_eq!(fields["caption"], f["caption"]["text"], "{id} caption");
        let tags = f["tags"].as_array().map(|t| t.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","));
        // The projection leaves an empty value out of `fields`.
        assert_eq!(fields["tags"].as_str().unwrap_or(""), tags.unwrap_or_default(), "{id} tags");
        assert_eq!(fields["created"], f["created"], "{id} created");
        assert_eq!(fields["updated"].as_str().unwrap_or(""), t.updated, "{id} updated");
        assert_eq!(fields["after"].as_str().unwrap_or(""), t.after.join(","), "{id} after");
        assert_eq!(fields["refs"].as_str().unwrap_or(""), t.refs.join(","), "{id} refs");
    }
    let (r0, r1) = (&state["rows"][&ids[0]], &state["rows"][&ids[1]]);
    for (f, r) in files.iter().zip([r0, r1]) {
        // An inbox task waits like a todo one.
        let want = if f["status"] == "inbox" { "todo" } else { f["status"].as_str().unwrap() };
        assert_eq!(r["state"], want, "{} state", f["id"]);
    }
    assert!(files.iter().any(|f| f["status"] == "blocked") && files.iter().any(|f| f["status"] == "inbox"));
    assert_eq!(r0["seat"], "cpd-2");
    assert_eq!(r0["group"], serde_json::json!({ "list": "list-a", "n": 1 }));
    assert_eq!(r1["group"], serde_json::json!({ "list": "list-a", "n": 2 }));
    assert!(r1["seat"].is_null());
    assert_eq!(state["rows"].as_object().unwrap().len(), files.len(), "a row the files do not have");

    // Claims.
    let claims = state["claims"].as_object().unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!((claims[&ids[0]]["row"].as_str(), claims[&ids[0]]["seat"].as_str()), (Some(ids[0].as_str()), Some("cpd-2")));

    // Lists and groups.
    let list = &state["lists"]["list-a"];
    assert_eq!((list["state"].as_str(), list["fields"]["title"].as_str(), list["fields"]["status"].as_str()), (Some("running"), Some("A list"), Some("running")));
    let groups = list["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!((groups[0]["state"].as_str(), groups[0]["fields"]["verdict"].as_str()), (Some("passed"), Some("holds")));
    assert_eq!(groups[0]["members"], serde_json::json!([ids[0]]));
    assert_eq!(groups[1]["state"], "running");
    assert_eq!(groups[1]["members"], serde_json::json!([ids[1]]));

    // Asks: the message and the flag.
    let asks = state["asks"].as_object().unwrap();
    assert_eq!(asks.len(), 2);
    let q = &asks["m-1"];
    assert_eq!((q["kind"].as_str(), q["state"].as_str(), q["text"].as_str()), (Some("ask"), Some("raised"), Some("may I?")));
    assert_eq!(q["from"], serde_json::json!({ "seat": "cpd-2" }));
    let fl = &asks[&format!("flag:{}", ids[1])];
    assert_eq!((fl["kind"].as_str(), fl["fields"]["reason"].as_str(), fl["fields"]["pane"].as_str()), (Some("flag"), Some("stuck"), Some("cpd-4")));

    // Machines: the record, and the seat's own machine (wsp-308's
    // `seat_machine`), named for the `local` host the sandbox's daemon runs
    // under, and not one more.
    let machines = state["machines"].as_object().unwrap();
    assert_eq!(machines.keys().collect::<Vec<_>>(), vec!["far", "local"]);
    assert_eq!(machines["local"]["transport"], serde_json::json!("local"));
    assert_eq!(machines["far"]["transport"], serde_json::json!({ "tcp": { "addr": "far-alias" } }));
    assert_eq!((machines["far"]["facts"]["os"].as_str(), machines["far"]["facts"]["status"].as_str()), (Some("linux"), Some("active")));

    // The attention half moves on its own: a second ask reaches the subscriber.
    let mut feed = client.subscribe(None);
    store.save_message(&ask_message("m-2", "and may I again?")).unwrap();
    let end = Instant::now() + BUDGET;
    let mut put = false;
    while Instant::now() < end && !put {
        if let Some(Item::Event(e)) = feed.next_within(Duration::from_millis(100)) {
            let v = serde_json::to_value(&e).unwrap();
            put = v["kind"] == "mirror.put" && v.to_string().contains("m-2");
        }
    }
    assert!(put, "a raised ask did not reach the subscriber as mirror.put");
    // And answered: the same ask puts again, now answered.
    store.save_message(&{
        let mut m = ask_message("m-2", "and may I again?");
        m.state_raw = "answered".into();
        m
    })
    .unwrap();
    let state = snapshot_with(&client, |v| v["asks"]["m-2"]["state"] == "answered");
    assert_eq!(state["asks"]["m-2"]["state"], "answered");
}

#[test]
fn an_edit_and_a_block_each_reach_the_subscriber_then_quiet() {
    let sb = seeded("events", free_port());
    let client = sb.client(BUDGET);
    // A snapshot that holds both rows: the mirror's first pass has landed, and
    // what the feed brings after it is what the verbs below write.
    let (_, mut feed) = feed_from_snapshot_with(&client, |v| v["rows"].as_object().map_or(0, |o| o.len()) == 2);

    let puts_for = |feed: &mut wsp_client::Feed, row: &str, within: Duration| -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if let Some(Item::Event(e)) = feed.next_within(Duration::from_millis(100)) {
                let v = serde_json::to_value(&e).unwrap();
                if v["kind"] == "mirror.put" && v["subject"].to_string().contains(row) {
                    return true;
                }
            }
        }
        false
    };

    let mut edit = sb.cmd(&["edit", "inbox-001", "--overview", "--from", "-"]);
    edit.stdin(Stdio::piped());
    let mut child = edit.spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"a new overview\n").unwrap();
    }
    child.wait().unwrap();
    assert!(puts_for(&mut feed, "inbox-001", BUDGET), "wsp edit did not reach the subscriber");

    sb.wsp(&["block", "inbox-001", "waiting on the test"]);
    assert!(puts_for(&mut feed, "inbox-001", BUDGET), "wsp block did not reach the subscriber");

    // Quiet: nothing for 10 s.
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(Item::Event(e)) = feed.next_within(Duration::from_millis(200)) {
            let v = serde_json::to_value(&e).unwrap();
            if v["kind"].as_str().is_some_and(|k| k.starts_with("mirror")) {
                panic!("a quiet store sent an event: {v}");
            }
        }
    }
}

/// wsp-171: a row in a shared workspace is mirrored with it, and its events carry it;
/// a hand edit that moves it to another is a put with the new one (a private record
/// is mirrored with none, so a store from before workspaces reads as it did).
#[test]
fn a_rows_workspace_is_mirrored_and_a_move_is_a_put_with_the_new_one() {
    use wsp_store::model::{Task, WorkspaceId};
    let shared = |n: &str| format!("shared:{n}").parse::<WorkspaceId>().unwrap();
    let mut sb = Sandbox::up("ws");
    let store = files_of(&sb);
    for (id, ws) in [("ws-1", shared("jesper")), ("ws-2", WorkspaceId::Private)] {
        let mut t = Task::new(&format!("title of {id}"), id);
        t.project = Some("wsp".into());
        t.status_raw = "todo".into();
        t.workspace = ws;
        store.save_task(&t).unwrap();
    }
    sb.start_daemon(free_port(), false);
    let client = sb.client(BUDGET);
    let (state, mut feed) = feed_from_snapshot_with(&client, |v| v["rows"].as_object().map_or(0, |o| o.len()) == 2);
    assert_eq!(state["rows"]["ws-1"]["workspace"], "shared:jesper");
    assert!(state["rows"]["ws-2"].get("workspace").is_none(), "a private row is mirrored without one");

    let mut t = store.task("ws-1").unwrap();
    t.workspace = shared("core");
    store.save_task(&t).unwrap();
    let end = Instant::now() + BUDGET;
    let mut moved = None;
    while Instant::now() < end && moved.is_none() {
        if let Some(Item::Event(e)) = feed.next_within(Duration::from_millis(100)) {
            let v = serde_json::to_value(&e).unwrap();
            if v["kind"] == "mirror.put" && v["subject"].to_string().contains("ws-1") {
                moved = Some(v);
            }
        }
    }
    let moved = moved.expect("the move did not reach the subscriber as a put");
    assert_eq!(moved["workspace"], "shared:core", "{moved}");
    assert_eq!(moved["payload"]["row"]["workspace"], "shared:core", "{moved}");
    let state = snapshot_with(&client, |v| v["rows"]["ws-1"]["workspace"] == "shared:core");
    assert_eq!(state["rows"]["ws-1"]["workspace"], "shared:core");
}

#[test]
fn wsp_api_off_starts_nothing() {
    let mut sb = Sandbox::up("off");
    sb.wsp(&["add", "one"]);
    let port = free_port();
    sb.start_daemon(port, true);
    // The daemon stamps daemon.tick at the top of every pass of its loop, which
    // runs after the point where the API would have started: once it is there,
    // nothing more starts.
    let end = Instant::now() + BUDGET;
    while !sb.state.join("daemon.tick").exists() {
        assert!(Instant::now() < end, "the daemon never ticked");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "something listens on {port}");
    assert!(!sb.state.join("state.db").exists(), "state.db was made under WSP_API=off");
    assert!(!sb.state.join("api.json").exists(), "api.json was written under WSP_API=off");
}
