//! `ids.alloc` (wsp-185): the hub hands out row ids from one counter per
//! project, so two spokes that publish at once get disjoint ids, none is ever
//! handed out again (a reopen included), each is above the caller's `past`,
//! and only a member of the workspace named is answered.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wsp_api::{Answer, ApiToken, Code, IdsAlloc, Outcome, SyncCall, WorkspaceId, MAX_IDS};
use wsp_client::Client;
use wsp_hub::Hub;
use wsp_machines::token;
use wsp_store_contract::{Command as Cmd, IdempotencyKey, Principal, SharedName, TokenId, UserName};

const WAIT: Duration = Duration::from_secs(5);

/// A directory of its own, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-ids-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
    }

    fn config(&self) -> PathBuf {
        let path = self.0.join("hub.json");
        std::fs::write(&path, format!(r#"{{"data_dir": {:?}, "port": 0}}"#, self.data().to_str().unwrap())).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running `wsp-hub serve`, killed when dropped.
struct Hubd(Child);

impl Hubd {
    fn start(config: &Path) -> Hubd {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wsp-hub"))
            .args(["serve", "--config"])
            .arg(config)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("wsp-hub starts");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert!(line.starts_with("wsp-hub: listening on 127.0.0.1:"), "the start line: {line:?}");
        Hubd(child)
    }
}

impl Drop for Hubd {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn mp() -> WorkspaceId {
    WorkspaceId("shared:mp".into())
}

fn person(name: &str) -> Principal {
    Principal::Person(name.into())
}

/// A hub in `data` with `shared:mp`, whose members are `members`, and `outsider`,
/// a user who is none's member; each user's token, by name.
fn seed(data: &Path, members: &[&str], outsider: &str) -> BTreeMap<String, String> {
    let hub = Hub::open(data).unwrap();
    let apply = |cmd: Cmd, by: &Principal| {
        let key = IdempotencyKey(format!("{cmd:?}"));
        hub.apply(cmd, key, by.clone()).expect("the identity machine takes it");
    };
    let root = person("root");
    apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
    apply(Cmd::HubWorkspaceAdd { name: SharedName("mp".into()) }, &root);
    let mut tokens = BTreeMap::new();
    for user in members.iter().chain([&outsider]) {
        let (secret, hash) = token::mint();
        apply(Cmd::HubUserAdd { name: UserName(user.to_string()), admin: false }, &root);
        if *user != outsider {
            apply(Cmd::HubMemberSet { workspace: SharedName("mp".into()), user: UserName(user.to_string()), member: true }, &root);
        }
        apply(Cmd::HubTokenIssue { id: TokenId(format!("t-{user}")), user: UserName(user.to_string()), hash }, &root);
        tokens.insert(user.to_string(), secret);
    }
    tokens
}

fn dial(data: &Path, tokens: &BTreeMap<String, String>, who: &str) -> Client {
    let listen = std::fs::read_to_string(data.join("api.json")).unwrap();
    let addrs = serde_json::from_str::<wsp_api::Listen>(&listen).unwrap().addrs;
    Client::dial_at(addrs, ApiToken(tokens[who].clone()), person(who), WAIT).expect("the hub answers its user's hello")
}

fn alloc(project: &str, n: u32, past: Option<u64>) -> SyncCall {
    SyncCall::Alloc(IdsAlloc { workspace: mp(), project: project.into(), n, past })
}

fn ids(outcome: Outcome) -> Vec<String> {
    match outcome {
        Outcome::Ok(Answer::Ids(ids)) => ids,
        other => panic!("not ids: {other:?}"),
    }
}

fn code(outcome: &Outcome) -> Option<Code> {
    match outcome {
        Outcome::Err(f) => Some(f.code),
        Outcome::Ok(_) => None,
    }
}

/// Two spokes, `ed` and `jesper`, each on its own connection to a served hub,
/// released together and asking for ids under `wsp` many times over: every id
/// either gets is one the other does not, and none comes twice.
#[test]
fn two_spokes_publishing_at_once_get_disjoint_ids() {
    let scratch = Scratch::new("disjoint");
    let tokens = seed(&scratch.data(), &["ed", "jesper"], "zed");
    let _hub = Hubd::start(&scratch.config());
    let start = Arc::new(Barrier::new(2));
    let got: Vec<Vec<String>> = ["ed", "jesper"]
        .into_iter()
        .map(|who| {
            let mut client = dial(&scratch.data(), &tokens, who);
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                let mut mine = Vec::new();
                for _ in 0..25 {
                    mine.extend(client.ids_alloc(&mp(), "wsp", 3, None).expect("a member is answered"));
                }
                mine
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    let (ed, jesper): (BTreeSet<&String>, BTreeSet<&String>) = (got[0].iter().collect(), got[1].iter().collect());
    assert_eq!(ed.len(), 75, "ed's ids are 75 and none twice: {:?}", got[0]);
    assert_eq!(jesper.len(), 75, "jesper's ids are 75 and none twice: {:?}", got[1]);
    assert!(ed.is_disjoint(&jesper), "ed and jesper share {:?}", ed.intersection(&jesper).collect::<Vec<_>>());
    // Together they are the counter's first 150, with no gap: each allocation
    // read the counter one of them last moved.
    let all: BTreeSet<String> = got.concat().into_iter().collect();
    assert_eq!(all, (1..=150).map(|n| wsp_api::row_id("wsp", n)).collect::<BTreeSet<_>>());
}

/// Two handles on one data directory (two processes' worth of SQLite
/// connections), many threads each: the compare-and-swap is the store's, not
/// one process's lock.
#[test]
fn allocations_through_two_handles_on_one_store_are_disjoint() {
    let scratch = Scratch::new("handles");
    seed(&scratch.data(), &["ed", "jesper"], "zed");
    let hubs = [Arc::new(Hub::open(scratch.data()).unwrap()), Arc::new(Hub::open(scratch.data()).unwrap())];
    let start = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let hub = hubs[i % 2].clone();
            let start = start.clone();
            let who = if i % 2 == 0 { "ed" } else { "jesper" };
            std::thread::spawn(move || {
                start.wait();
                (0..10).flat_map(|_| ids(hub.answer(&person(who), alloc("wsp", 2, None)))).collect::<Vec<_>>()
            })
        })
        .collect();
    let mut all = Vec::new();
    for t in threads {
        all.extend(t.join().unwrap());
    }
    let unique: BTreeSet<&String> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "an id was handed out twice: {all:?}");
    assert_eq!(all.len(), 160);
}

/// The counter is per project, survives a reopen, never goes back, and starts
/// above `past` where the caller names one.
#[test]
fn the_counter_is_per_project_survives_a_reopen_and_honours_past() {
    let scratch = Scratch::new("counter");
    seed(&scratch.data(), &["ed"], "zed");
    let ed = person("ed");
    {
        let hub = Hub::open(scratch.data()).unwrap();
        assert_eq!(ids(hub.answer(&ed, alloc("wsp", 3, None))), ["wsp-001", "wsp-002", "wsp-003"]);
        assert_eq!(ids(hub.answer(&ed, alloc("cpd", 1, None))), ["cpd-001"], "another project counts alone");
        // The spoke's own ids reach 40: the hub hands out past them.
        assert_eq!(ids(hub.answer(&ed, alloc("wsp", 2, Some(40)))), ["wsp-041", "wsp-042"]);
        // A `past` below the counter does not take it back.
        assert_eq!(ids(hub.answer(&ed, alloc("wsp", 1, Some(5)))), ["wsp-043"]);
        assert_eq!(ids(hub.answer(&ed, alloc("wsp", 0, None))), Vec::<String>::new(), "none asked, none spent");
    }
    let hub = Hub::open(scratch.data()).unwrap();
    assert_eq!(ids(hub.answer(&ed, alloc("wsp", 1, None))), ["wsp-044"], "a reopen hands out none again");
    assert_eq!(ids(hub.answer(&ed, alloc("wsp", 1, Some(1000)))), ["wsp-1001"], "past 999 the number is wider");
}

/// Only a member of the workspace named is answered; a project that is no id
/// prefix, and more than `MAX_IDS`, are refused `invalid` and spend nothing.
#[test]
fn a_non_member_is_forbidden_and_a_bad_ask_is_invalid() {
    let scratch = Scratch::new("refused");
    seed(&scratch.data(), &["ed"], "zed");
    let hub = Hub::open(scratch.data()).unwrap();
    assert_eq!(code(&hub.answer(&person("zed"), alloc("wsp", 1, None))), Some(Code::Forbidden), "zed is no member of mp");
    let elsewhere = SyncCall::Alloc(IdsAlloc { workspace: WorkspaceId("shared:other".into()), project: "wsp".into(), n: 1, past: None });
    assert_eq!(code(&hub.answer(&person("ed"), elsewhere)), Some(Code::Forbidden), "no such shared workspace");
    let private = SyncCall::Alloc(IdsAlloc { workspace: WorkspaceId("private".into()), project: "wsp".into(), n: 1, past: None });
    assert_eq!(code(&hub.answer(&person("ed"), private)), Some(Code::Forbidden), "private is no hub workspace");
    for bad in ["", "w sp", "wsp/1", "wsp:x"] {
        assert_eq!(code(&hub.answer(&person("ed"), alloc(bad, 1, None))), Some(Code::Invalid), "{bad:?}");
    }
    assert_eq!(code(&hub.answer(&person("ed"), alloc("wsp", MAX_IDS + 1, None))), Some(Code::Invalid));
    assert_eq!(ids(hub.answer(&person("ed"), alloc("wsp", 1, None))), ["wsp-001"], "the refusals spent no id");
}
