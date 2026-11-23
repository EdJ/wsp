//! Verdicts and landings are fenced (wsp-468): an event object of `row.reviewed`
//! or `row.landed` on a row's lease carries the lease token, and the hub
//! refuses `fenced` a put of one, or a move to one, under a token that is not
//! the row's current one, leaving the row as its holder left it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use wsp_api::{fence_subject, fenced_event, Answer, Blob, Code, LeaseTake, Move, ObjectId, Outcome, RefMove, SyncCall, WorkspaceId};
use wsp_hub::Hub;
use wsp_machines::token;
use wsp_objects::encode;
use wsp_store_contract::{At, Command, EventKind, IdempotencyKey, Principal, RowId, SharedName, Token, TokenId, UserName};

const DAY: Duration = Duration::from_secs(24 * 3600);

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-fence-{name}-{}-{nanos}", std::process::id()));
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

fn team() -> WorkspaceId {
    WorkspaceId("shared:team".into())
}

fn row(id: &str) -> RowId {
    RowId(id.into())
}

/// A hub in `dir` with `ann` and `bo` (members of `team`) and `dee` (a member
/// of nothing); each user's token, by name.
fn seed(dir: &Path) -> (Hub, BTreeMap<String, String>) {
    let hub = Hub::open(dir).unwrap();
    let apply = |cmd: Command, by: &Principal| {
        let key = IdempotencyKey(format!("{cmd:?}"));
        hub.apply(cmd, key, by.clone()).expect("the identity machine takes it");
    };
    let root = person("root");
    apply(Command::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
    apply(Command::HubWorkspaceAdd { name: SharedName("team".into()) }, &root);
    let mut tokens = BTreeMap::new();
    for user in ["ann", "bo", "dee"] {
        apply(Command::HubUserAdd { name: UserName(user.into()), admin: false }, &root);
        if user != "dee" {
            apply(Command::HubMemberSet { workspace: SharedName("team".into()), user: UserName(user.into()), member: true }, &root);
        }
        let (secret, hash) = token::mint();
        apply(Command::HubTokenIssue { id: TokenId(format!("t-{user}")), user: UserName(user.into()), hash }, &root);
        tokens.insert(user.to_string(), secret);
    }
    (hub, tokens)
}

fn take(hub: &Hub, by: &str, id: &str) -> Outcome {
    hub.answer(&person(by), SyncCall::LeaseTake(LeaseTake { workspace: team(), row: row(id) }))
}

fn leased(outcome: Outcome) -> Token {
    match outcome {
        Outcome::Ok(Answer::Lease { token, .. }) => token,
        other => panic!("not a lease: {other:?}"),
    }
}

/// A verdict of `id` by `by` under `token`, on the head `parent`, and its id.
fn verdict(by: &str, id: &str, kind: EventKind, token: Option<Token>, parent: Option<ObjectId>) -> (ObjectId, Blob) {
    let mut object = fenced_event(team(), &row(id), kind, json!({ "commit": "abc123" }), token.unwrap_or(Token(0)), parent, person(by), At(1));
    if token.is_none() {
        if let wsp_objects::Object::Event { payload, .. } = &mut object {
            payload.as_object_mut().unwrap().remove(wsp_api::FENCE_TOKEN);
        }
    }
    let (oid, bytes) = encode(&object);
    (oid, Blob { id: oid, bytes })
}

fn put(hub: &Hub, by: &str, blob: Blob) -> Outcome {
    hub.answer(&person(by), SyncCall::Put { objects: vec![blob] })
}

fn move_to(hub: &Hub, by: &str, id: &str, from: Option<ObjectId>, to: ObjectId) -> Outcome {
    hub.answer(&person(by), SyncCall::Move(RefMove { workspace: team(), subject: fence_subject(&row(id)), from, to }))
}

fn head(hub: &Hub, id: &str) -> Option<ObjectId> {
    hub.heads(&team()).unwrap().get(&fence_subject(&row(id))).copied()
}

fn fenced(outcome: Outcome) -> String {
    match outcome {
        Outcome::Err(f) => {
            assert_eq!(f.code, Code::Fenced, "{f:?}");
            f.text
        }
        other => panic!("not fenced: {other:?}"),
    }
}

#[test]
fn a_verdict_and_a_landing_under_the_current_token_are_kept() {
    let dir = Scratch::new("current");
    let (hub, _) = seed(&dir.0);
    let token = leased(take(&hub, "ann", "wsp-1"));

    let (reviewed, blob) = verdict("ann", "wsp-1", EventKind::RowReviewed, Some(token), None);
    assert!(matches!(put(&hub, "ann", blob), Outcome::Ok(Answer::Stored { stored: 1 })));
    assert!(matches!(move_to(&hub, "ann", "wsp-1", None, reviewed), Outcome::Ok(Answer::Move(Move::Moved { .. }))));
    let (landed, blob) = verdict("ann", "wsp-1", EventKind::RowLanded, Some(token), Some(reviewed));
    assert!(matches!(put(&hub, "ann", blob), Outcome::Ok(Answer::Stored { stored: 1 })));
    assert!(matches!(move_to(&hub, "ann", "wsp-1", Some(reviewed), landed), Outcome::Ok(Answer::Move(Move::Moved { .. }))));
    assert_eq!(head(&hub, "wsp-1"), Some(landed));
}

/// ann's lease runs out and bo takes the row with a higher token: ann's
/// verdict under the old one is refused `fenced`, put or moved to, and the row
/// stays as bo left it.
#[test]
fn a_verdict_under_an_expired_token_is_refused_fenced_and_the_row_stays_as_its_new_holder_left_it() {
    let dir = Scratch::new("expired");
    let (hub, _) = seed(&dir.0);
    let old = leased(take(&hub, "ann", "wsp-1"));
    // Put while ann still holds it, to be moved only after: the move is held to the lease too.
    let (late, late_blob) = verdict("ann", "wsp-1", EventKind::RowReviewed, Some(old), None);
    assert!(matches!(put(&hub, "ann", late_blob), Outcome::Ok(_)));

    hub.clock().advance(8 * DAY);
    assert_eq!(hub.expire_leases().unwrap().len(), 1);
    let new = leased(take(&hub, "bo", "wsp-1"));
    assert!(new > old);
    let (theirs, blob) = verdict("bo", "wsp-1", EventKind::RowReviewed, Some(new), None);
    assert!(matches!(put(&hub, "bo", blob), Outcome::Ok(_)));
    assert!(matches!(move_to(&hub, "bo", "wsp-1", None, theirs), Outcome::Ok(Answer::Move(Move::Moved { .. }))));

    // ann's move of the object she put in time, and a verdict and a landing put now.
    let said = fenced(move_to(&hub, "ann", "wsp-1", Some(theirs), late));
    assert!(said.contains(&format!("current one ({})", new.0)), "{said}");
    for kind in [EventKind::RowReviewed, EventKind::RowLanded] {
        let (id, blob) = verdict("ann", "wsp-1", kind, Some(old), Some(theirs));
        fenced(put(&hub, "ann", blob));
        assert!(!hub.objects().has(&id), "a fenced object is not kept");
        // Moved to an object the hub does not hold, which it could not check: not moved.
        assert!(matches!(move_to(&hub, "ann", "wsp-1", Some(theirs), id), Outcome::Err(f) if f.code == Code::Invalid));
    }
    assert_eq!(head(&hub, "wsp-1"), Some(theirs), "the row stays as bo left it");
}

#[test]
fn an_expired_unswept_lease_a_missing_token_and_another_users_token_are_fenced_too() {
    let dir = Scratch::new("others");
    let (hub, _) = seed(&dir.0);
    // No lease at all.
    let (_, blob) = verdict("ann", "wsp-2", EventKind::RowReviewed, Some(Token(1)), None);
    assert!(fenced(put(&hub, "ann", blob)).contains("not leased"));

    let token = leased(take(&hub, "ann", "wsp-1"));
    let (_, blob) = verdict("ann", "wsp-1", EventKind::RowReviewed, None, None);
    assert!(fenced(put(&hub, "ann", blob)).contains("no lease token"));
    // bo with ann's right token is not the holder.
    let (_, blob) = verdict("bo", "wsp-1", EventKind::RowLanded, Some(token), None);
    assert!(fenced(put(&hub, "bo", blob)).contains("another user"));
    // Past the TTL and not yet swept: the lease is as good as gone.
    hub.clock().advance(8 * DAY);
    let (_, blob) = verdict("ann", "wsp-1", EventKind::RowReviewed, Some(token), None);
    fenced(put(&hub, "ann", blob));
    assert_eq!(head(&hub, "wsp-1"), None);
}
