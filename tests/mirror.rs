//! The daemon's mirror (wsp-075): a `wsp daemon` on a `wsp sandbox --fake` store
//! opens `state.db`, serves the API, and mirrors the store's files into it.
//!
//! The binary has no library, so the projections are not called from here: the
//! snapshot is compared, key by key, with what `wsp ls --json` says of the same
//! files (the row's id, title, `fields.status`, `prio`, `tags`, `caption`).
//! The pure projections are tested on a seeded store in `src/mirror.rs`.
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

#[test]
fn a_snapshot_equals_the_files() {
    let mut sb = Sandbox::up("snap");
    sb.wsp(&["add", "one", "--prio", "high", "-t", "a"]);
    sb.wsp(&["add", "two"]);
    let port = free_port();
    sb.start_daemon(port, false);
    // Let the first pass land before the snapshot is taken.
    let client = sb.client(Duration::from_secs(20));
    let mut feed = client.subscribe(None);
    let files: Vec<Value> = serde_json::from_str(&sb.wsp(&["ls", "--json"])).expect("ls --json");
    assert_eq!(files.len(), 2);
    let deadline = Instant::now() + Duration::from_secs(10);
    let state = loop {
        match feed.next_within(Duration::from_millis(500)) {
            Some(Item::State { state, .. }) => {
                let v = serde_json::to_value(&state).unwrap();
                if v["rows"].as_object().is_some_and(|r| r.len() == files.len()) {
                    break v;
                }
                // The mirror had not landed yet: resubscribe.
                feed = client.subscribe(None);
            }
            _ if Instant::now() > deadline => panic!("the snapshot never held the files' rows"),
            _ => {}
        }
    };
    for f in &files {
        let id = f["id"].as_str().unwrap();
        let row = &state["rows"][id];
        assert!(row.is_object(), "{id} is not in the snapshot: {state}");
        assert_eq!(row["title"], f["title"], "{id} title");
        let fields = &row["fields"];
        assert_eq!(fields["prio"], f["priority"], "{id} prio");
        assert_eq!(fields["caption"], f["caption"]["text"], "{id} caption");
        let tags = f["tags"].as_array().map(|t| t.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","));
        // The projection leaves an empty value out of `fields`.
        assert_eq!(fields["tags"].as_str().unwrap_or(""), tags.unwrap_or_default(), "{id} tags");
        assert_eq!(fields["created"], f["created"], "{id} created");
    }
    // No row the files do not have.
    assert_eq!(state["rows"].as_object().unwrap().len(), files.len());
}

#[test]
fn an_edit_and_a_block_each_reach_the_subscriber_then_quiet() {
    let sb = seeded("events", free_port());
    let client = sb.client(Duration::from_secs(20));
    let mut feed = client.subscribe(None);
    // The snapshot, and the first pass if it came after it.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        feed.next_within(Duration::from_millis(500));
    }

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
    assert!(puts_for(&mut feed, "inbox-001", Duration::from_secs(2)), "wsp edit did not reach the subscriber");

    sb.wsp(&["block", "inbox-001", "waiting on the test"]);
    assert!(puts_for(&mut feed, "inbox-001", Duration::from_secs(2)), "wsp block did not reach the subscriber");

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

#[test]
fn wsp_api_off_starts_nothing() {
    let mut sb = Sandbox::up("off");
    sb.wsp(&["add", "one"]);
    let port = free_port();
    sb.start_daemon(port, true);
    // Long enough for the mirror's first pass, had there been one.
    std::thread::sleep(Duration::from_secs(4));
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "something listens on {port}");
    assert!(!sb.state.join("state.db").exists(), "state.db was made under WSP_API=off");
    assert!(!sb.state.join("api.json").exists(), "api.json was written under WSP_API=off");
}
