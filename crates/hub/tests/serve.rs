//! `wsp-hub serve` as a process: wsp-459's convergence case through it with
//! two clients, across a kill and a restart; the stream's `ref.moved`; the
//! hello's token; and the peer ranges the config names.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wsp_api::{ApiToken, Blob, EventId, ObjectId, Refs, WorkspaceId};
use wsp_client::{Client, Item, Silent, SyncReport};
use wsp_objects::{encode, Object};
use wsp_store_contract::{At, Entity, EventKind, Principal, ProjectId, Row, RowId, RowState, Subject};

const WAIT: Duration = Duration::from_secs(5);

/// A directory of its own, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-serve-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
    }

    /// A config of the data directory, an ephemeral port, and `extra` keys.
    fn config(&self, extra: &str) -> PathBuf {
        let path = self.0.join("hub.json");
        let data = self.data();
        std::fs::write(&path, format!(r#"{{"data_dir": {:?}, "port": 0{extra}}}"#, data.to_str().unwrap())).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running `wsp-hub serve`, killed when dropped.
struct Hubd {
    child: Child,
}

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
        Hubd { child }
    }

    /// Ends it the hard way, as a crash does.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for Hubd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn ws() -> WorkspaceId {
    WorkspaceId("shared:mp".into())
}

fn subject(row: &str) -> Subject {
    Subject::Row(RowId(row.into()))
}

fn dial(data: &Path, who: &str) -> Client {
    Client::dial(data, Principal::Person(who.into()), WAIT).expect("the hub answers a hello with its token")
}

/// One spoke: its heads and every object behind them, and the clock it stamps its writes with.
struct Spoke {
    by: Principal,
    refs: Refs,
    objects: BTreeMap<ObjectId, Vec<u8>>,
    clock: u64,
}

impl Spoke {
    fn new(name: &str) -> Spoke {
        Spoke { by: Principal::Person(name.into()), refs: Refs::new(ws()), objects: BTreeMap::new(), clock: 1_795_400_000_000 }
    }

    /// A new version of `row`, titled `title`, following the head it has.
    fn write(&mut self, row: &str, title: &str) -> ObjectId {
        self.clock += 1;
        let object = Object::Record {
            workspace: ws(),
            subject: subject(row),
            entity: Entity::Row(Row {
                workspace: Default::default(),
                id: RowId(row.into()),
                project: ProjectId("wsp".into()),
                title: title.into(),
                state: RowState::Todo,
                before_block: None,
                seat: None,
                group: None,
                commit: None,
                fields: BTreeMap::new(),
            }),
            parent: self.refs.get(&subject(row)).copied(),
            by: self.by.clone(),
            at: At(self.clock),
        };
        let (id, bytes) = encode(&object);
        self.objects.insert(id, bytes);
        self.refs.set(subject(row), id);
        id
    }

    fn sync(&mut self, client: &mut Client) -> SyncReport {
        let report = client.sync(&ws(), &self.refs, &self.objects).expect("the sync runs");
        for Blob { id, bytes } in &report.received {
            self.objects.insert(*id, bytes.clone());
        }
        self.refs = report.refs.clone();
        report
    }
}

#[test]
fn two_clients_converge_through_a_served_hub_and_it_keeps_it_all_across_a_restart() {
    let dir = Scratch::new("converge");
    let config = dir.config("");
    let hub = Hubd::start(&config);
    let (mut a, mut b) = (Spoke::new("ann"), Spoke::new("bo"));
    a.write("wsp-1", "first");
    a.write("wsp-2", "second");
    a.write("wsp-2", "second, edited");
    b.write("wsp-3", "third");
    b.write("wsp-4", "fourth");
    let (mut ca, mut cb) = (dial(&dir.data(), "ann"), dial(&dir.data(), "bo"));

    let ra = a.sync(&mut ca);
    assert_eq!((ra.sent, ra.received.len()), (3, 0), "ann sends her three objects: {ra:?}");
    let rb = b.sync(&mut cb);
    assert_eq!((rb.sent, rb.received.len()), (2, 3), "bo sends his two and fetches ann's three: {rb:?}");
    assert_eq!(rb.adopted, [subject("wsp-1"), subject("wsp-2")]);
    assert!(rb.stale.is_empty() && rb.diverged.is_empty());
    let ra2 = a.sync(&mut ca);
    assert_eq!((ra2.sent, ra2.received.len()), (0, 2), "{ra2:?}");
    assert_eq!(a.refs, b.refs);
    assert_eq!(a.objects, b.objects);
    assert_eq!(b.refs.heads.len(), 4);
    let all: Vec<ObjectId> = a.objects.keys().copied().collect();
    assert_eq!(all.len(), 5);

    // The hub goes down hard, and comes up again on another port.
    drop((ca, cb));
    hub.kill();
    let hub = Hubd::start(&config);
    let (mut ca, mut cb) = (dial(&dir.data(), "ann"), dial(&dir.data(), "bo"));
    assert!(ca.sync_have(all.clone()).unwrap().is_empty(), "every object is still held");
    assert_eq!(ca.sync_heads(&ws()).unwrap(), a.refs, "and every head is still where it was");
    for blob in cb.sync_get(all.clone()).unwrap() {
        assert_eq!(blob.bytes, a.objects[&blob.id], "bytes as they were put");
    }

    // A third spoke that never met the hub before takes the lot.
    let mut c = Spoke::new("cy");
    let mut cc = dial(&dir.data(), "cy");
    let rc = c.sync(&mut cc);
    assert_eq!((rc.sent, rc.received.len()), (0, 5), "{rc:?}");
    assert_eq!(c.refs, a.refs);
    assert_eq!(c.objects, a.objects);

    // And the heads still move from where they were: an edit syncs round to the others.
    a.write("wsp-1", "first, edited");
    let ra3 = a.sync(&mut ca);
    assert_eq!((ra3.sent, ra3.moved.as_slice()), (1, [subject("wsp-1")].as_slice()), "{ra3:?}");
    b.sync(&mut cb);
    c.sync(&mut cc);
    assert_eq!(a.refs, b.refs);
    assert_eq!(b.refs, c.refs);
    assert_eq!(a.objects, c.objects);
    drop(hub);
}

#[test]
fn the_stream_announces_each_move_and_a_replay_after_a_restart_still_does() {
    let dir = Scratch::new("stream");
    let config = dir.config("");
    let hub = Hubd::start(&config);
    let mut ann = dial(&dir.data(), "ann");
    let mut feed = dial(&dir.data(), "watcher").subscribe(None);
    assert!(matches!(feed.next_within(WAIT), Some(Item::State { .. })), "a subscription opens with the snapshot");

    let mut a = Spoke::new("ann");
    let one = a.write("wsp-1", "first");
    let two = a.write("wsp-2", "second");
    a.sync(&mut ann);

    let mut heard: Vec<(Subject, ObjectId)> = Vec::new();
    while heard.len() < 2 {
        match feed.next_within(WAIT) {
            Some(Item::Event(e)) if e.kind == EventKind::RefMoved => {
                assert_eq!(e.payload["workspace"], "shared:mp");
                let to: ObjectId = serde_json::from_value(e.payload["to"].clone()).unwrap();
                let moved: Subject = serde_json::from_value(e.payload["subject"].clone()).unwrap();
                assert_eq!(e.subject, moved);
                assert_eq!(e.workspace.to_string(), "shared:mp");
                heard.push((moved, to));
            }
            Some(_) => {}
            None => panic!("the stream announced {heard:?}, not both moves"),
        }
    }
    assert_eq!(heard, [(subject("wsp-1"), one), (subject("wsp-2"), two)], "in the order they moved");

    // A later move of one subject is announced with the head it moved to.
    let one_b = a.write("wsp-1", "first, edited");
    a.sync(&mut ann);
    loop {
        match feed.next_within(WAIT) {
            Some(Item::Event(e)) if e.kind == EventKind::RefMoved => {
                assert_eq!(serde_json::from_value::<ObjectId>(e.payload["to"].clone()).unwrap(), one_b);
                break;
            }
            Some(_) => {}
            None => panic!("the stream did not announce the later move"),
        }
    }

    // Down and up again: a subscriber that asks from the start is told of each subject.
    drop((feed, ann));
    hub.kill();
    let _hub = Hubd::start(&config);
    let mut replay = dial(&dir.data(), "late").subscribe(Some(EventId(0)));
    let mut told: BTreeMap<Subject, ObjectId> = BTreeMap::new();
    while told.len() < 2 {
        match replay.next_within(WAIT) {
            Some(Item::Event(e)) if e.kind == EventKind::RefMoved => {
                told.insert(e.subject.clone(), serde_json::from_value(e.payload["to"].clone()).unwrap());
            }
            Some(_) => {}
            None => panic!("the replay told of {told:?}"),
        }
    }
    assert_eq!(told, BTreeMap::from([(subject("wsp-1"), one_b), (subject("wsp-2"), two)]));
}

#[test]
fn the_hello_needs_the_admin_token_and_the_peer_ranges_are_the_configs() {
    let dir = Scratch::new("hello");
    let _hub = Hubd::start(&dir.config(""));
    let token = std::fs::read_to_string(dir.data().join("api.token")).unwrap();
    assert_eq!(token.trim().len(), 64, "the admin token is made on first start");
    let addrs = {
        let listen = std::fs::read_to_string(dir.data().join("api.json")).unwrap();
        serde_json::from_str::<wsp_api::Listen>(&listen).unwrap().addrs
    };
    let by = Principal::Person("mal".into());
    match Client::dial_at(addrs.clone(), ApiToken("not-the-token".into()), by.clone(), WAIT) {
        Err(Silent::Refused(_)) => {}
        other => panic!("a wrong token is refused: {:?}", other.map(|c| c.server().to_string())),
    }
    let ok = Client::dial_at(addrs, ApiToken(token.trim().into()), by, WAIT).expect("the right token");
    assert_eq!(ok.server(), "wsp-hub");

    // A config whose ranges leave loopback out admits nobody from it.
    let other = Scratch::new("ranges");
    let _hub = Hubd::start_outside(&other);
    assert!(Client::dial(&other.data(), Principal::Person("ann".into()), Duration::from_secs(1)).is_err());
}

#[test]
fn a_config_that_does_not_read_ends_the_start_with_the_reason() {
    let dir = Scratch::new("bad");
    let config = dir.config(r#", "peers": ["10.0.0.0/40"]"#);
    let out = Command::new(env!("CARGO_BIN_EXE_wsp-hub")).args(["serve", "--config"]).arg(&config).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("peers"), "{}", String::from_utf8_lossy(&out.stderr));
    let out = Command::new(env!("CARGO_BIN_EXE_wsp-hub")).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "no verb: the usage");
}

impl Hubd {
    /// A hub whose `peers` is a range loopback is not in, bound on loopback.
    fn start_outside(dir: &Scratch) -> Hubd {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wsp-hub"))
            .args(["serve", "--config"])
            .arg(dir.config(r#", "peers": ["10.0.0.0/8"]"#))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        Hubd { child }
    }
}
