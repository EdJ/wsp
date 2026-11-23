//! The hub's store: objects and refs survive a reopen, a present object is not
//! rewritten, and of two concurrent `refs.move`s from one head exactly one wins.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wsp_api::{Answer, Blob, Code, Move, ObjectId, Outcome, RefMove, SyncCall, WorkspaceId};
use wsp_hub::Hub;
use wsp_store_contract::{
    Command, IdempotencyKey, LeaseName, Principal, RowId, SeatId, SharedName, Subject, UserName,
};

/// A directory of its own, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("wsp-hub-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An object (of a kind the hub does not read, in `shared:team`): canonical JSON, named by its hash.
fn blob(n: u64) -> Blob {
    let bytes = wsp_objects::canonical(
        format!("{{\"n\":{n},\"kind\":\"test\",\"workspace\":\"shared:team\"}}").as_bytes(),
    )
    .unwrap();
    Blob {
        id: ObjectId::of(&bytes),
        bytes,
    }
}

/// `ann`, a member of `shared:team`: who the verbs below are answered for.
fn ann() -> Principal {
    Principal::Person("ann".into())
}

/// A hub in `dir` where `ann` is a member of `shared:team`.
fn hub_with_ann(dir: &Path) -> Hub {
    let hub = Hub::open(dir).unwrap();
    let apply = |cmd: Command, by: &Principal| {
        let key = IdempotencyKey(format!("{cmd:?}"));
        hub.apply(cmd, key, by.clone()).expect("the identity machine takes it");
    };
    let root = Principal::Person("root".into());
    apply(Command::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("hub".into()));
    apply(Command::HubUserAdd { name: UserName("ann".into()), admin: false }, &root);
    apply(Command::HubWorkspaceAdd { name: SharedName("team".into()) }, &root);
    apply(
        Command::HubMemberSet { workspace: SharedName("team".into()), user: UserName("ann".into()), member: true },
        &root,
    );
    hub
}

fn ws() -> WorkspaceId {
    WorkspaceId("shared:team".into())
}

fn row(name: &str) -> Subject {
    Subject::Row(RowId(name.into()))
}

fn mv(subject: &Subject, from: Option<ObjectId>, to: ObjectId) -> RefMove {
    RefMove {
        workspace: ws(),
        subject: subject.clone(),
        from,
        to,
    }
}

#[test]
fn objects_and_refs_survive_a_reopen() {
    let dir = Scratch::new("reopen");
    let (a, b) = (blob(1), blob(2));
    {
        let hub = Hub::open(dir.path()).unwrap();
        assert_eq!(hub.put(&[a.clone(), b.clone()]).unwrap(), 2);
        assert_eq!(
            hub.move_ref(&mv(&row("r1"), None, a.id)).unwrap(),
            Move::Moved { head: a.id }
        );
        assert_eq!(
            hub.move_ref(&mv(&row("r1"), Some(a.id), b.id)).unwrap(),
            Move::Moved { head: b.id }
        );
        assert_eq!(
            hub.move_ref(&mv(&row("r2"), None, a.id)).unwrap(),
            Move::Moved { head: a.id }
        );
    }
    let hub = Hub::open(dir.path()).unwrap();
    assert_eq!(hub.missing(&[a.id, b.id, blob(3).id]), vec![blob(3).id]);
    assert_eq!(
        hub.get(&[b.id, blob(3).id, a.id]).unwrap(),
        vec![b.clone(), a.clone()]
    );
    let hex = a.id.hex();
    assert!(
        dir.path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
            .is_file(),
        "objects/ab/cdef…"
    );
    let refs = hub.heads(&ws()).unwrap();
    assert_eq!(refs.workspace, ws());
    assert_eq!(refs.get(&row("r1")), Some(&b.id));
    assert_eq!(refs.get(&row("r2")), Some(&a.id));
    assert_eq!(refs.heads.len(), 2);
    assert!(hub
        .heads(&WorkspaceId("private".into()))
        .unwrap()
        .heads
        .is_empty());
    // A move from the head before the reopen's last is stale, with the head it has.
    assert_eq!(
        hub.move_ref(&mv(&row("r1"), Some(a.id), a.id)).unwrap(),
        Move::Stale { head: Some(b.id) }
    );
}

#[test]
fn a_present_object_is_not_rewritten_and_a_bad_one_refuses_the_whole_put() {
    let dir = Scratch::new("present");
    let hub = hub_with_ann(dir.path());
    let a = blob(1);
    assert_eq!(hub.put(std::slice::from_ref(&a)).unwrap(), 1);
    // Mark the file: a rewrite would put the object's bytes back.
    let path = hub.objects().path_of(&a.id);
    fs::write(&path, b"marked").unwrap();
    assert_eq!(
        hub.put(std::slice::from_ref(&a)).unwrap(),
        0,
        "a held id is not written again"
    );
    assert_eq!(fs::read(&path).unwrap(), b"marked");
    fs::write(&path, &a.bytes).unwrap();

    let good = blob(2);
    let mut bad = blob(3);
    bad.bytes = b"{\"n\":4}".to_vec();
    match hub.answer(&ann(), SyncCall::Put {
        objects: vec![good.clone(), bad.clone()],
    }) {
        Outcome::Err(f) => {
            assert_eq!(f.code, Code::Invalid);
            assert!(f.text.contains(&bad.id.to_string()), "{}", f.text);
        }
        other => panic!("a bad object is refused, not {other:?}"),
    }
    assert!(
        !hub.objects().has(&good.id),
        "nothing of a refused put is kept"
    );
    assert!(
        fs::read_dir(dir.path().join("objects/tmp"))
            .unwrap()
            .next()
            .is_none(),
        "no temp file is left"
    );
}

#[test]
fn the_verbs_answer_as_memhub_does() {
    let dir = Scratch::new("verbs");
    let hub = hub_with_ann(dir.path());
    let (a, b) = (blob(1), blob(2));
    assert_eq!(
        hub.answer(&ann(), SyncCall::Have {
            ids: vec![a.id, b.id]
        }),
        Outcome::Ok(Answer::Missing(vec![a.id, b.id]))
    );
    assert_eq!(
        hub.answer(&ann(), SyncCall::Put {
            objects: vec![a.clone()]
        }),
        Outcome::Ok(Answer::Stored { stored: 1 })
    );
    let s = row("r1");
    assert_eq!(
        hub.answer(&ann(), SyncCall::Move(mv(&s, None, a.id))),
        Outcome::Ok(Answer::Move(Move::Moved { head: a.id }))
    );
    assert_eq!(
        hub.answer(&ann(), SyncCall::Move(mv(&s, None, b.id))),
        Outcome::Ok(Answer::Move(Move::Stale { head: Some(a.id) })),
        "a stale move is an ok answer, not an err"
    );
    // Held and referenced, an object is served; one not held is left out.
    assert_eq!(
        hub.answer(&ann(), SyncCall::Get {
            ids: vec![b.id, a.id]
        }),
        Outcome::Ok(Answer::Objects(vec![a.clone()]))
    );
    match hub.answer(&ann(), SyncCall::Heads { workspace: ws() }) {
        Outcome::Ok(Answer::Heads(refs)) => assert_eq!(refs.get(&s), Some(&a.id)),
        other => panic!("{other:?}"),
    }
}

/// Two handles on one data directory (two connections to its SQLite), two
/// threads released together, each moving one subject from the same head to a
/// head of its own: exactly one is moved, and the other is stale with the
/// winner's head.
#[test]
fn two_concurrent_moves_from_one_head_exactly_one_wins() {
    let dir = Scratch::new("race");
    let hubs = [
        Arc::new(Hub::open(dir.path()).unwrap()),
        Arc::new(Hub::open(dir.path()).unwrap()),
    ];
    let s = row("r1");
    let mut head: Option<ObjectId> = None;
    for round in 0..25u64 {
        let tos = [blob(2 * round + 100).id, blob(2 * round + 101).id];
        let gate = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let (hub, gate, m) = (hubs[i].clone(), gate.clone(), mv(&s, head, tos[i]));
                std::thread::spawn(move || {
                    gate.wait();
                    hub.move_ref(&m).unwrap()
                })
            })
            .collect();
        let got: Vec<Move> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let won: Vec<usize> = (0..2)
            .filter(|i| matches!(got[*i], Move::Moved { .. }))
            .collect();
        assert_eq!(
            won.len(),
            1,
            "round {round}: exactly one move wins, not {got:?}"
        );
        let (w, l) = (won[0], 1 - won[0]);
        assert_eq!(got[w], Move::Moved { head: tos[w] });
        assert_eq!(
            got[l],
            Move::Stale { head: Some(tos[w]) },
            "round {round}: the loser sees the winner's head"
        );
        assert_eq!(hubs[l].heads(&ws()).unwrap().get(&s), Some(&tos[w]));
        head = Some(tos[w]);
    }
}

/// Moves of two subjects of one workspace race on the same refs text; both
/// still move, since neither's head was changed by the other.
#[test]
fn concurrent_moves_of_two_subjects_both_win() {
    let dir = Scratch::new("subjects");
    let hubs = [
        Arc::new(Hub::open(dir.path()).unwrap()),
        Arc::new(Hub::open(dir.path()).unwrap()),
    ];
    for round in 0..10u64 {
        let gate = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let (hub, gate) = (hubs[i].clone(), gate.clone());
                let m = mv(
                    &row(&format!("r{round}-{i}")),
                    None,
                    blob(round * 2 + i as u64).id,
                );
                std::thread::spawn(move || {
                    gate.wait();
                    hub.move_ref(&m).unwrap()
                })
            })
            .collect();
        for h in handles {
            assert!(matches!(h.join().unwrap(), Move::Moved { .. }));
        }
    }
    assert_eq!(hubs[0].heads(&ws()).unwrap().heads.len(), 20);
}

#[test]
fn leases_are_the_sqlite_stores_with_rising_tokens() {
    let dir = Scratch::new("leases");
    let hub = Hub::open(dir.path()).unwrap();
    let name = LeaseName("hub:writer".into());
    let alice = Principal::Seat(SeatId("alice".into()));
    let bob = Principal::Seat(SeatId("bob".into()));
    let t1 = hub
        .take_lease(&name, &alice, Duration::from_secs(60))
        .unwrap()
        .expect("a free lease is taken");
    assert_eq!(
        hub.take_lease(&name, &bob, Duration::from_secs(60))
            .unwrap(),
        None,
        "a held lease is not"
    );
    hub.leases().release(&name, t1).unwrap();
    let t2 = hub
        .take_lease(&name, &bob, Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert!(t2 > t1, "tokens rise across a release: {t1:?} then {t2:?}");
    drop(hub);
    let hub = Hub::open(dir.path()).unwrap();
    assert!(
        !hub.leases()
            .renew(&name, t1, Duration::from_secs(60))
            .unwrap(),
        "an old token renews nothing"
    );
    assert!(
        hub.leases()
            .renew(&name, t2, Duration::from_secs(60))
            .unwrap(),
        "the lease outlives a reopen"
    );
}

/// The hub links none of the crates that run agents, builds or worktrees.
#[test]
fn cargo_tree_names_no_root_store_manager_trees_or_host() {
    const NEVER: &[&str] = &["wsp", "wsp-store", "wsp-manager", "wsp-trees", "wsp-host"];
    let out = std::process::Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "wsp-hub",
            "--prefix",
            "none",
            "-e",
            "normal,build,dev",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo tree: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let names: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert!(
        names.contains(&"wsp-store-sqlite"),
        "the tree was read: {text}"
    );
    let bad: Vec<&&str> = names.iter().filter(|n| NEVER.contains(n)).collect();
    assert!(bad.is_empty(), "wsp-hub links {bad:?}:\n{text}");
}
