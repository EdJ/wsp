//! `wsp-hub admin init` (wsp-163), the binary run on a data directory: it
//! prints the first admin's token once and refuses a second; the admin's token
//! logs in to the hub served from that directory, adds a user, a shared
//! workspace and a member; the new user's token logs in and, no admin, is
//! `forbidden` to add one; and no token is in the data directory's log or
//! database (the hub holds hashes only).

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wsp_client::{Client, Credential, Error};
use wsp_hub::{serve, Config};
use wsp_machines::token;
use wsp_store_contract::{Command, Refusal, SharedName, TokenId, UserName};

const WAIT: Duration = Duration::from_secs(5);

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-admin-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config_file(dir: &Path) -> PathBuf {
    let file = dir.join("hub.json");
    std::fs::write(&file, r#"{"data_dir": "data", "port": 0, "name": "hub-t"}"#).unwrap();
    file
}

fn init(file: &Path, admin: &str) -> std::process::Output {
    Process::new(env!("CARGO_BIN_EXE_wsp-hub"))
        .args(["admin", "init", "--config"])
        .arg(file)
        .args(["--admin", admin])
        .output()
        .unwrap()
}

/// Every 64-hex word of `text`: a token's shape.
fn tokens_in(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|w| w.len() == 64)
        .map(str::to_string)
        .collect()
}

fn dial(addr: std::net::SocketAddr, token: &str) -> Client {
    Client::dial_hub(&format!("tcp://{addr}"), Credential::User(token.into()), WAIT).expect("the token logs in")
}

fn every_file(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            every_file(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn init_prints_a_token_once_a_second_is_refused_and_the_admin_runs_the_hub() {
    let dir = Dir::new("flow");
    let file = config_file(&dir.0);

    let first = init(&file, "root");
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let out = String::from_utf8(first.stdout).unwrap();
    let printed = tokens_in(&out);
    assert_eq!(printed.len(), 1, "one token, printed once: {out}");
    assert!(out.contains("wsp login tcp://127.0.0.1:"), "it says how to log in: {out}");
    let root_token = printed[0].clone();
    assert!(tokens_in(&String::from_utf8_lossy(&first.stderr)).is_empty());

    let second = init(&file, "other");
    assert!(!second.status.success(), "a second init is refused");
    let why = String::from_utf8(second.stderr).unwrap();
    assert!(why.contains("already has an admin"), "{why}");
    assert!(tokens_in(&String::from_utf8_lossy(&second.stdout)).is_empty(), "and prints no token");

    let cfg = Config::load(&file).unwrap();
    let running = serve::start(&cfg).expect("the hub starts");
    let addr = running.addrs()[0];

    // The admin's token logs in; the admin adds a user, a shared workspace and a member.
    let mut admin = dial(addr, &root_token);
    admin.apply(&Command::HubUserAdd { name: UserName("ed".into()), admin: false }).unwrap();
    let (secret, hash) = token::mint();
    admin.apply(&Command::HubTokenIssue { id: TokenId("t-ed".into()), user: UserName("ed".into()), hash }).unwrap();
    admin.apply(&Command::HubWorkspaceAdd { name: SharedName("core".into()) }).unwrap();
    admin
        .apply(&Command::HubMemberSet { workspace: SharedName("core".into()), user: UserName("ed".into()), member: true })
        .unwrap();

    // The new user's token logs in; no admin, adding a user is forbidden.
    let mut ed = dial(addr, &secret);
    match ed.apply(&Command::HubUserAdd { name: UserName("mallory".into()), admin: false }) {
        Err(Error::Refused(Refusal::Forbidden { .. })) => {}
        other => panic!("a non-admin's hub.user.add is forbidden, not {other:?}"),
    }

    // The bootstrap token is no way back in once there is an admin.
    let bootstrap = std::fs::read_to_string(running.token_path()).unwrap().trim().to_string();
    assert!(Client::dial_hub(&format!("tcp://{addr}"), Credential::User(bootstrap), WAIT).is_err());

    drop((admin, ed));
    running.stop();

    // No token in the hub's log or database, only hashes; only api.token holds the bootstrap's.
    let mut files = Vec::new();
    every_file(&dir.0.join("data"), &mut files);
    let log = files.iter().find(|f| f.file_name().is_some_and(|n| n == "api.log"));
    assert!(log.is_some(), "the hub logs to api.log: {files:?}");
    for f in files.iter().filter(|f| f.file_name().is_some_and(|n| n != "api.token")) {
        let bytes = std::fs::read(f).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for t in [&root_token, &secret] {
            assert!(!text.contains(t.as_str()), "{} holds a token", f.display());
        }
    }
}

#[test]
fn init_wants_a_name() {
    let dir = Dir::new("noname");
    let file = config_file(&dir.0);
    let out = init(&file, " ");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--admin needs a name"));
}
