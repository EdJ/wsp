//! The hub's leases on shared rows (wsp-466): `lease.take`, `lease.renew` and
//! `lease.release` with the config's TTL and a fencing token per row that only
//! grows; `held` while another user holds the row, `forbidden` for a row
//! allocated to another user (or a workspace the caller is no member of); and
//! expiry, on a clock the test advances, releasing the lease and announcing
//! `lease.expired` on the stream.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wsp_api::{Answer, ApiToken, Blob, Code, LeaseHold, LeaseTake, Move, Outcome, RefMove, SyncCall, WorkspaceId};
use wsp_client::{Client, Error as ClientError, Item};
use wsp_hub::{Hub, HubStore};
use wsp_machines::token;
use wsp_objects::{encode, Object};
use wsp_store_contract::{
    At, Command, Entity, EventKind, Events, Group, GroupId, IdempotencyKey, List, ListId, ListState,
    Principal, ProjectId, Refusal, Row, RowId, RowState, SharedName, Subject, Token, TokenId, UserName,
};

const WAIT: Duration = Duration::from_secs(5);
const DAY: Duration = Duration::from_secs(24 * 3600);
const WEEK_MS: u64 = 7 * 24 * 3600 * 1000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-leases-{name}-{}-{nanos}", std::process::id()));
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

fn renew(hub: &Hub, by: &str, id: &str, token: Token) -> Outcome {
    hub.answer(&person(by), SyncCall::LeaseRenew(LeaseHold { row: row(id), token }))
}

fn release(hub: &Hub, by: &str, id: &str, token: Token) -> Outcome {
    hub.answer(&person(by), SyncCall::LeaseRelease(LeaseHold { row: row(id), token }))
}

/// The lease an outcome answers.
fn leased(outcome: Outcome) -> (Token, At) {
    match outcome {
        Outcome::Ok(Answer::Lease { token, expires }) => (token, expires),
        other => panic!("not a lease: {other:?}"),
    }
}

/// The refusal an outcome is, with its code.
fn refusal(outcome: Outcome) -> (Code, Refusal) {
    match outcome {
        Outcome::Err(f) => (f.code, f.refusal.expect("a refusal")),
        other => panic!("not refused: {other:?}"),
    }
}

/// Puts a record of `entity` at `subject` in `team` as `by` and moves its head there.
fn publish(hub: &Hub, by: &str, subject: Subject, entity: Entity) {
    let object = Object::Record { workspace: team(), subject: subject.clone(), entity, parent: None, by: person(by), at: At(1) };
    let (id, bytes) = encode(&object);
    assert!(matches!(hub.answer(&person(by), SyncCall::Put { objects: vec![Blob { id, bytes }] }), Outcome::Ok(_)));
    let mv = RefMove { workspace: team(), subject, from: None, to: id };
    assert!(matches!(hub.answer(&person(by), SyncCall::Move(mv)), Outcome::Ok(Answer::Move(Move::Moved { .. }))));
}

fn a_row(id: &str, fields: &[(&str, &str)]) -> Entity {
    Entity::Row(Row {
        id: row(id),
        project: ProjectId("wsp".into()),
        workspace: Default::default(),
        title: id.into(),
        state: RowState::Todo,
        before_block: None,
        seat: None,
        group: None,
        commit: None,
        fields: fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    })
}

#[test]
fn a_take_answers_a_token_for_the_ttl_and_the_holder_renews_it() {
    let dir = Scratch::new("take");
    let (hub, _) = seed(&dir.0);
    assert_eq!(hub.lease_ttl(), 7 * DAY, "a week by default");

    let before = hub.clock().now().0;
    let (token, expires) = leased(take(&hub, "ann", "wsp-1"));
    assert!(token.0 >= 1);
    assert!(expires.0 >= before + WEEK_MS && expires.0 <= hub.clock().now().0 + WEEK_MS, "now plus 7 days: {expires:?}");

    // The holder taking it again keeps its token, renewed (the claim's shape).
    hub.clock().advance(DAY);
    let (again, later) = leased(take(&hub, "ann", "wsp-1"));
    assert_eq!(again, token, "the holder's re-take is the same lease");
    assert!(later.0 >= expires.0 + DAY.as_millis() as u64, "and runs from now: {later:?}");

    hub.clock().advance(DAY);
    let (renewed, until) = leased(renew(&hub, "ann", "wsp-1", token));
    assert_eq!(renewed, token);
    assert!(until.0 >= later.0 + DAY.as_millis() as u64, "a renew runs the TTL again from now");

    // Six days on from the renew it still holds: the renew moved the expiry.
    hub.clock().advance(6 * DAY);
    assert!(hub.expire_leases().unwrap().is_empty(), "nothing has expired");
    assert_eq!(leased(renew(&hub, "ann", "wsp-1", token)).0, token);

    // Renewing with a token that is not the lease's is stale; a row no one holds is not found.
    assert!(matches!(refusal(renew(&hub, "ann", "wsp-1", Token(token.0 + 7))), (Code::Stale, Refusal::Stale { .. })));
    assert!(matches!(refusal(renew(&hub, "ann", "wsp-9", token)), (Code::NotFound, Refusal::NotFound { .. })));
}

#[test]
fn a_row_held_by_another_user_is_refused_held_naming_the_holder() {
    let dir = Scratch::new("held");
    let (hub, _) = seed(&dir.0);
    let (token, _) = leased(take(&hub, "ann", "wsp-1"));

    let (code, why) = refusal(take(&hub, "bo", "wsp-1"));
    assert_eq!(code, Code::Held);
    assert!(matches!(&why, Refusal::Held { holder, .. } if *holder == person("ann")), "{why:?}");
    // Nor may bo renew or release ann's lease, even with her token.
    assert!(matches!(refusal(renew(&hub, "bo", "wsp-1", token)), (Code::Held, Refusal::Held { .. })));
    assert!(matches!(refusal(release(&hub, "bo", "wsp-1", token)), (Code::Forbidden, Refusal::Forbidden { .. })));

    // Another row is another lease.
    assert!(matches!(take(&hub, "bo", "wsp-2"), Outcome::Ok(Answer::Lease { .. })));

    // Released, bo takes it, with a higher token.
    assert!(matches!(release(&hub, "ann", "wsp-1", token), Outcome::Ok(Answer::Released)));
    assert!(matches!(release(&hub, "ann", "wsp-1", token), Outcome::Ok(Answer::Released)), "a second release is the same");
    let (theirs, _) = leased(take(&hub, "bo", "wsp-1"));
    assert!(theirs > token, "{theirs:?} after {token:?}");
    // ann's old token releases nothing now.
    assert!(matches!(release(&hub, "ann", "wsp-1", token), Outcome::Ok(Answer::Released)));
    assert_eq!(hub.row_lease(&row("wsp-1")).unwrap().map(|l| l.holder), Some(person("bo")));
}

#[test]
fn a_row_allocated_to_another_user_is_forbidden_and_so_is_a_workspace_of_no_membership() {
    let dir = Scratch::new("forbidden");
    let (hub, _) = seed(&dir.0);
    publish(&hub, "ann", Subject::Row(row("wsp-1")), a_row("wsp-1", &[("for", "ann")]));

    let (code, why) = refusal(take(&hub, "bo", "wsp-1"));
    assert_eq!(code, Code::Forbidden);
    assert_eq!(why, Refusal::Forbidden { by: person("bo") });
    assert!(hub.row_lease(&row("wsp-1")).unwrap().is_none(), "a refused take takes nothing");
    assert!(matches!(take(&hub, "ann", "wsp-1"), Outcome::Ok(Answer::Lease { .. })), "its user takes it");

    // A row in a list allocated to ann is hers too; one allocated to nobody is anyone's.
    publish(&hub, "ann", Subject::Row(row("wsp-2")), a_row("wsp-2", &[]));
    let list = List {
        id: ListId("mp-coord".into()),
        workspace: Default::default(),
        state: ListState::Running,
        groups: vec![Group { id: GroupId { list: ListId("mp-coord".into()), n: 1 }, state: ListState::Running, members: vec![row("wsp-2")], attempt: 0, fields: BTreeMap::new() }],
        fields: [("for".to_string(), "ann".to_string())].into(),
    };
    publish(&hub, "ann", Subject::List(list.id.clone()), Entity::List(list));
    assert!(matches!(refusal(take(&hub, "bo", "wsp-2")), (Code::Forbidden, _)));
    publish(&hub, "ann", Subject::Row(row("wsp-3")), a_row("wsp-3", &[]));
    assert!(matches!(take(&hub, "bo", "wsp-3"), Outcome::Ok(Answer::Lease { .. })));

    // dee is no member of team.
    assert!(matches!(refusal(take(&hub, "dee", "wsp-4")), (Code::Forbidden, _)));
}

#[test]
fn past_the_ttl_the_lease_is_released_announced_and_retaken_with_a_higher_token() {
    let dir = Scratch::new("expiry");
    let (hub, _) = seed(&dir.0);
    let hub = Arc::new(hub);
    let store = HubStore::new(hub.clone());
    let (token, _) = leased(take(&hub, "ann", "wsp-1"));

    // A day short of the TTL: held.
    hub.clock().advance(6 * DAY);
    assert!(hub.expire_leases().unwrap().is_empty());
    assert!(matches!(refusal(take(&hub, "bo", "wsp-1")), (Code::Held, _)));

    let head = store.head().unwrap();
    hub.clock().advance(2 * DAY);
    let expired = hub.expire_leases().unwrap();
    assert_eq!(expired.len(), 1, "{expired:?}");
    assert_eq!((expired[0].row.clone(), expired[0].holder.clone(), expired[0].token), (row("wsp-1"), person("ann"), token));
    assert!(hub.row_lease(&row("wsp-1")).unwrap().is_none(), "the expiry released it");
    assert!(hub.expire_leases().unwrap().is_empty(), "and expires it once");

    // The stream says so, in the row's workspace.
    let events = store.events_after(head, 100).unwrap();
    let said: Vec<_> = events.iter().filter(|e| e.kind == EventKind::LeaseExpired).collect();
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].workspace.to_string(), "shared:team");
    assert_eq!(said[0].payload["row"], "wsp-1");
    assert_eq!(said[0].payload["workspace"], "shared:team");
    assert_eq!(serde_json::from_value::<Principal>(said[0].payload["holder"].clone()).unwrap(), person("ann"));
    assert_eq!(said[0].payload["token"], token.0);

    // ann's renew is too late; bo takes it with a higher token.
    assert!(matches!(refusal(renew(&hub, "ann", "wsp-1", token)), (Code::NotFound, _)));
    let (theirs, expires) = leased(take(&hub, "bo", "wsp-1"));
    assert!(theirs > token, "{theirs:?} after {token:?}");
    assert!(expires.0 > hub.clock().now().0 + WEEK_MS - 60_000, "a week on the hub's clock: {expires:?}");
    assert!(hub.expire_leases().unwrap().is_empty(), "the new lease has not expired as it was taken");

    // A take of an expired lease expires it itself, announcing it, without a sweep.
    hub.clock().advance(8 * DAY);
    let head = store.head().unwrap();
    assert!(matches!(refusal(renew(&hub, "bo", "wsp-1", theirs)), (Code::Stale, _)), "a renew past the TTL is stale");
    let (third, _) = leased(take(&hub, "ann", "wsp-1"));
    assert!(third > theirs);
    let kinds: Vec<_> = store.events_after(head, 100).unwrap().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds.iter().filter(|k| **k == EventKind::LeaseExpired).count(), 1, "{kinds:?}");
}

#[test]
fn tokens_survive_a_reopen_and_only_grow() {
    let dir = Scratch::new("reopen");
    let first = {
        let (hub, _) = seed(&dir.0);
        let (t, _) = leased(take(&hub, "ann", "wsp-1"));
        assert!(matches!(release(&hub, "ann", "wsp-1", t), Outcome::Ok(Answer::Released)));
        t
    };
    let hub = Hub::open(&dir.0).unwrap();
    let (second, _) = leased(take(&hub, "bo", "wsp-1"));
    assert!(second > first, "{second:?} after {first:?} across a reopen");
}

/// Two spokes over the wire: ann leases, bo is refused naming her, and past the
/// TTL the sweep releases it, bo's subscription hears `lease.expired`, and bo
/// takes it with a higher token.
#[test]
fn over_the_wire_a_lease_is_held_expires_on_the_stream_and_passes_on() {
    let dir = Scratch::new("wire");
    let data = dir.0.join("data");
    let (hub, tokens) = seed(&data);
    drop(hub);
    let cfg = wsp_hub::Config {
        data_dir: data.clone(),
        bind: vec![],
        port: 0,
        peers: vec!["127.0.0.0/8".into()],
        tls: None,
        name: "wsp-hub".into(),
        lease_days: 2,
    };
    let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
    assert_eq!(running.hub().lease_ttl(), 2 * DAY, "the config's TTL");
    let dial = |who: &str| {
        let addrs = vec![running.addrs()[0].to_string()];
        Client::dial_at(addrs, ApiToken(tokens[who].clone()), person(who), WAIT).expect("the hub answers")
    };
    let (mut ann, mut bo) = (dial("ann"), dial("bo"));
    let mut feed = dial("bo").subscribe(None);
    assert!(matches!(feed.next_within(WAIT), Some(Item::State { .. })));

    let (token, expires) = ann.lease_take(&team(), &row("wsp-1")).unwrap();
    assert!(expires.0 >= running.hub().clock().now().0 + 2 * DAY.as_millis() as u64 - 60_000);
    match bo.lease_take(&team(), &row("wsp-1")) {
        Err(ClientError::Refused(Refusal::Held { holder, .. })) => assert_eq!(holder, person("ann")),
        other => panic!("bo was not refused held: {other:?}"),
    }
    assert_eq!(ann.lease_renew(&row("wsp-1"), token).unwrap().0, token);

    running.hub().clock().advance(3 * DAY);
    assert_eq!(running.hub().expire_leases().unwrap().len(), 1);
    let heard = loop {
        match feed.next_within(WAIT) {
            Some(Item::Event(e)) if e.kind == EventKind::LeaseExpired => break e,
            Some(_) => continue,
            None => panic!("no lease.expired on the stream"),
        }
    };
    assert_eq!(heard.payload["row"], "wsp-1");
    assert_eq!(heard.payload["token"], token.0);

    assert!(matches!(ann.lease_renew(&row("wsp-1"), token), Err(ClientError::Refused(_))));
    let (theirs, _) = bo.lease_take(&team(), &row("wsp-1")).unwrap();
    assert!(theirs > token);
    bo.lease_release(&row("wsp-1"), theirs).unwrap();
    assert!(running.hub().row_lease(&row("wsp-1")).unwrap().is_none());
    running.stop();
}
