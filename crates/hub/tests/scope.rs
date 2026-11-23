//! Who reads and writes what on the hub (wsp-173): a member reads the heads and
//! the objects of a shared workspace; a non-member asking by workspace is
//! `forbidden`, one asking for an object id it knows gets `not_found`, and its
//! put and ref move are `forbidden`. Admin is no exception for content.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use wsp_api::{Answer, Blob, Code, Move, ObjectId, Outcome, RefMove, SyncCall, WorkspaceId};
use wsp_hub::scope::Scope;
use wsp_hub::Hub;
use wsp_objects::{encode, Object};
use wsp_store_contract::{
    At, Command, Entity, IdempotencyKey, Principal, ProjectId, Row, RowId, RowState, SharedName, Subject, UserName,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-scope-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn person(name: &str) -> Principal {
    Principal::Person(name.into())
}

fn ws(name: &str) -> WorkspaceId {
    WorkspaceId(format!("shared:{name}"))
}

fn subject(row: &str) -> Subject {
    Subject::Row(RowId(row.into()))
}

/// A hub with `root` (admin), `ann` and `bo` (members of `team`), `cy` (a member
/// of `other` only) and `dee` (a member of nothing).
fn hub(dir: &Scratch) -> Hub {
    let hub = Hub::open(&dir.0).unwrap();
    let apply = |cmd: Command, by: &Principal| {
        let key = IdempotencyKey(format!("{cmd:?}"));
        hub.apply(cmd, key, by.clone()).expect("the identity machine takes it");
    };
    let root = person("root");
    apply(Command::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("hub".into()));
    for user in ["ann", "bo", "cy", "dee"] {
        apply(Command::HubUserAdd { name: UserName(user.into()), admin: false }, &root);
    }
    for name in ["team", "other"] {
        apply(Command::HubWorkspaceAdd { name: SharedName(name.into()) }, &root);
    }
    for (name, user) in [("team", "ann"), ("team", "bo"), ("other", "cy")] {
        apply(
            Command::HubMemberSet { workspace: SharedName(name.into()), user: UserName(user.into()), member: true },
            &root,
        );
    }
    hub
}

/// One version of `row` in `workspace`, following `parent`.
fn version(workspace: &str, row: &str, title: &str, parent: Option<ObjectId>, at: u64) -> Blob {
    let object = Object::Record {
        workspace: ws(workspace),
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
        parent,
        by: person("ann"),
        at: At(at),
    };
    let (id, bytes) = encode(&object);
    Blob { id, bytes }
}

fn put(hub: &Hub, by: &Principal, blobs: &[&Blob]) -> Outcome {
    hub.answer(by, SyncCall::Put { objects: blobs.iter().map(|b| (*b).clone()).collect() })
}

fn mv(workspace: &str, row: &str, from: Option<ObjectId>, to: ObjectId) -> SyncCall {
    SyncCall::Move(RefMove { workspace: ws(workspace), subject: subject(row), from, to })
}

fn code(outcome: &Outcome) -> Option<Code> {
    match outcome {
        Outcome::Ok(_) => None,
        Outcome::Err(f) => Some(f.code),
    }
}

/// `team` holds `first` and `second` (a head of `r1` after `first`); `other` holds `theirs`.
struct Fixture {
    _dir: Scratch,
    hub: Hub,
    first: Blob,
    second: Blob,
    theirs: Blob,
}

fn fixture(name: &str) -> Fixture {
    let dir = Scratch::new(name);
    let hub = hub(&dir);
    let first = version("team", "r1", "first", None, 1);
    let second = version("team", "r1", "second", Some(first.id), 2);
    let theirs = version("other", "r9", "theirs", None, 3);
    assert_eq!(put(&hub, &person("ann"), &[&first, &second]), Outcome::Ok(Answer::Stored { stored: 2 }));
    assert_eq!(put(&hub, &person("cy"), &[&theirs]), Outcome::Ok(Answer::Stored { stored: 1 }));
    let moved = hub.answer(&person("ann"), mv("team", "r1", None, second.id));
    assert_eq!(moved, Outcome::Ok(Answer::Move(Move::Moved { head: second.id })));
    let moved = hub.answer(&person("cy"), mv("other", "r9", None, theirs.id));
    assert_eq!(moved, Outcome::Ok(Answer::Move(Move::Moved { head: theirs.id })));
    Fixture { _dir: dir, hub, first, second, theirs }
}

#[test]
fn a_member_reads_the_heads_and_the_objects_of_its_workspace() {
    let f = fixture("member");
    for who in ["ann", "bo"] {
        let by = person(who);
        match f.hub.answer(&by, SyncCall::Heads { workspace: ws("team") }) {
            Outcome::Ok(Answer::Heads(refs)) => assert_eq!(refs.get(&subject("r1")), Some(&f.second.id), "{who}"),
            other => panic!("{who}: {other:?}"),
        }
        // The head and the version behind it, which the head's parent chain reaches.
        assert_eq!(
            f.hub.answer(&by, SyncCall::Get { ids: vec![f.first.id, f.second.id] }),
            Outcome::Ok(Answer::Objects(vec![f.first.clone(), f.second.clone()])),
            "{who}"
        );
        assert_eq!(
            f.hub.answer(&by, SyncCall::Have { ids: vec![f.second.id, f.first.id] }),
            Outcome::Ok(Answer::Missing(vec![])),
            "{who}"
        );
    }
}

#[test]
fn a_non_member_asking_by_workspace_is_forbidden_and_the_admin_is_no_exception() {
    let f = fixture("heads");
    for who in ["cy", "dee", "root"] {
        let outcome = f.hub.answer(&person(who), SyncCall::Heads { workspace: ws("team") });
        assert_eq!(code(&outcome), Some(Code::Forbidden), "{who}: {outcome:?}");
    }
    // A workspace that does not exist, and one that is not a shared one, read alike.
    for name in ["nowhere", "private"] {
        let outcome = f.hub.answer(&person("ann"), SyncCall::Heads { workspace: WorkspaceId(name.into()) });
        assert_eq!(code(&outcome), Some(Code::Forbidden), "{name}: {outcome:?}");
    }
}

#[test]
fn a_non_member_asking_for_a_known_object_id_gets_not_found() {
    let f = fixture("get");
    for who in ["cy", "dee", "root"] {
        let outcome = f.hub.answer(&person(who), SyncCall::Get { ids: vec![f.first.id, f.second.id] });
        assert_eq!(code(&outcome), Some(Code::NotFound), "{who}: {outcome:?}");
    }
    // The ids of another workspace are not served to a member of this one, either.
    assert_eq!(code(&f.hub.answer(&person("ann"), SyncCall::Get { ids: vec![f.theirs.id] })), Some(Code::NotFound));
    // Of a mixed ask a member gets what its workspaces hold, and nothing of the rest.
    assert_eq!(
        f.hub.answer(&person("ann"), SyncCall::Get { ids: vec![f.theirs.id, f.second.id] }),
        Outcome::Ok(Answer::Objects(vec![f.second.clone()]))
    );
    // `sync.have` tells a non-member nothing of what is held: all of it reads missing.
    assert_eq!(
        f.hub.answer(&person("cy"), SyncCall::Have { ids: vec![f.first.id, f.second.id, f.theirs.id] }),
        Outcome::Ok(Answer::Missing(vec![f.first.id, f.second.id]))
    );
    assert_eq!(
        f.hub.answer(&person("dee"), SyncCall::Have { ids: vec![f.first.id] }),
        Outcome::Ok(Answer::Missing(vec![f.first.id]))
    );
}

#[test]
fn an_object_a_workspace_was_moved_to_from_another_is_not_served_by_the_move() {
    let f = fixture("borrow");
    // Cy, a member of `other` only, knows a hash of `team`'s and points a head of its own at it.
    let moved = f.hub.answer(&person("cy"), mv("other", "r8", None, f.first.id));
    assert_eq!(moved, Outcome::Ok(Answer::Move(Move::Moved { head: f.first.id })));
    let got = f.hub.answer(&person("cy"), SyncCall::Get { ids: vec![f.first.id] });
    assert_eq!(code(&got), Some(Code::NotFound), "{got:?}");
}

#[test]
fn a_non_members_put_and_ref_move_are_forbidden() {
    let f = fixture("write");
    let mine = version("team", "r2", "mine", None, 4);
    for who in ["cy", "dee", "root"] {
        let by = person(who);
        let outcome = put(&f.hub, &by, &[&mine]);
        assert_eq!(code(&outcome), Some(Code::Forbidden), "{who}: {outcome:?}");
        let outcome = f.hub.answer(&by, mv("team", "r2", None, mine.id));
        assert_eq!(code(&outcome), Some(Code::Forbidden), "{who}: {outcome:?}");
    }
    assert!(!f.hub.objects().has(&mine.id), "nothing of a refused put is kept");
    assert_eq!(f.hub.heads(&ws("team")).unwrap().get(&subject("r2")), None, "and no head moved");
    // A member of one workspace may not put an object of another.
    assert_eq!(code(&put(&f.hub, &person("ann"), &[&f.theirs])), Some(Code::Forbidden));
    assert_eq!(code(&put(&f.hub, &person("cy"), &[&mine])), Some(Code::Forbidden));
    // And a member still may.
    assert_eq!(put(&f.hub, &person("bo"), &[&mine]), Outcome::Ok(Answer::Stored { stored: 1 }));
}

#[test]
fn a_scope_is_the_workspaces_a_person_is_a_member_of() {
    let dir = Scratch::new("scope");
    let hub = hub(&dir);
    let state = hub.state().unwrap();
    let of = |by: &Principal| Scope::of(&state, by).workspaces().collect::<Vec<_>>();
    assert_eq!(of(&person("ann")), [ws("team")]);
    assert_eq!(of(&person("cy")), [ws("other")]);
    assert!(of(&person("root")).is_empty() && of(&person("dee")).is_empty());
    assert!(of(&Principal::Wsp("hub".into())).is_empty(), "the hub itself reads no workspace");
}
