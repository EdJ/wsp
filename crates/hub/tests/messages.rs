//! The hub's message routing (wsp-470): `msg.send` resolves `seat:<row>` to
//! the holder of the row's lease, `governor:<list>` to the holder of the
//! list's drive lease and `primary:<user>` to that user; the message is queued
//! for that user until a spoke of theirs takes and acknowledges it, announced
//! on the stream without its text, and a reply's `re` sends it back to the
//! sender with no lease.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wsp_api::{Answer, ApiToken, Code, LeaseTake, MsgSend, NoArgs, Outcome, SyncCall, WorkspaceId};
use wsp_client::{Client, Item};
use wsp_hub::Hub;
use wsp_machines::token;
use wsp_store_contract::{Command, EventKind, IdempotencyKey, Principal, RowId, SharedName, TokenId, UserName};

const WAIT: Duration = Duration::from_secs(5);

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-msgs-{name}-{}-{nanos}", std::process::id()));
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

/// A hub with `ed` and `jesper` (members of `team`) and `dee` (a member of
/// nothing); each user's token, by name.
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
    for user in ["ed", "jesper", "dee"] {
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

fn tell(to: &str, from: &str, text: &str, re: Option<&str>) -> MsgSend {
    MsgSend { to: to.into(), from: from.into(), seat: Some("cpd-3".into()), text: text.into(), re: re.map(str::to_string) }
}

fn send(hub: &Hub, by: &str, msg: MsgSend) -> Outcome {
    hub.answer(&person(by), SyncCall::MsgSend(msg))
}

fn take(hub: &Hub, by: &str) -> Vec<wsp_api::HubMessage> {
    match hub.answer(&person(by), SyncCall::MsgTake(NoArgs {})) {
        Outcome::Ok(Answer::Messages(m)) => m,
        other => panic!("not messages: {other:?}"),
    }
}

fn lease(hub: &Hub, by: &str, row: &str) {
    let ask = LeaseTake { workspace: team(), row: RowId(row.into()) };
    assert!(matches!(hub.answer(&person(by), SyncCall::LeaseTake(ask)), Outcome::Ok(Answer::Lease { .. })));
}

fn code(outcome: Outcome) -> Code {
    match outcome {
        Outcome::Err(f) => f.code,
        other => panic!("not refused: {other:?}"),
    }
}

/// The two-spoke case over the wire: Ed's seat tells `seat:wsp-1`, which
/// Jesper's spoke holds; Jesper's spoke hears it announced on its stream
/// (without the text), takes it, and its reply, sent with `re` to an address
/// that holds no lease, comes back to Ed.
#[test]
fn a_tell_reaches_the_holder_of_the_rows_lease_and_its_reply_comes_back() {
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
        lease_days: 7,
    };
    let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
    let dial = |who: &str| {
        let addrs = vec![running.addrs()[0].to_string()];
        Client::dial_at(addrs, ApiToken(tokens[who].clone()), person(who), WAIT).expect("the hub answers")
    };
    let (mut ed, mut jesper) = (dial("ed"), dial("jesper"));
    let mut feed = dial("jesper").subscribe(None);
    assert!(matches!(feed.next_within(WAIT), Some(Item::State { .. })));
    jesper.lease_take(&team(), &RowId("wsp-1".into())).unwrap();

    let (id, user) = ed.msg_send(tell("seat:wsp-1", "seat:wsp-9", "is wsp-1 landed?", None)).unwrap();
    assert_eq!(user, "jesper", "routed to the lease's holder");
    let heard = loop {
        match feed.next_within(WAIT) {
            Some(Item::Event(e)) if e.kind == EventKind::OrderQueued => break e,
            Some(_) => continue,
            None => panic!("no order.queued on the stream"),
        }
    };
    assert_eq!(heard.payload["user"], "jesper");
    assert_eq!(heard.payload["id"], id.as_str());
    assert!(!heard.payload.to_string().contains("landed"), "the stream carries no text: {}", heard.payload);

    let mail = jesper.msg_take().unwrap();
    assert_eq!(mail.len(), 1);
    let m = &mail[0];
    assert_eq!((m.id.as_str(), m.to.as_str(), m.from_user.as_str(), m.from.as_str()), (id.as_str(), "seat:wsp-1", "ed", "seat:wsp-9"));
    assert_eq!(m.text, "is wsp-1 landed?");
    assert_eq!(jesper.msg_ack(std::slice::from_ref(&id)).unwrap(), 1);
    assert!(ed.msg_take().unwrap().is_empty(), "nothing for ed yet");

    // The reply: to the sender's own address, which holds no lease on the hub; `re` routes it.
    let (back, to) = jesper.msg_send(tell(&m.from, "seat:wsp-1", "landed at abc123", Some(&id))).unwrap();
    assert_eq!(to, "ed");
    let replies = ed.msg_take().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!((replies[0].id.as_str(), replies[0].re.as_deref(), replies[0].to.as_str()), (back.as_str(), Some(id.as_str()), "seat:wsp-9"));
    assert_eq!(replies[0].from_user, "jesper");
    running.stop();
}

/// A message to a spoke that is not connected waits in its user's queue; the
/// spoke that connects takes it, and once acknowledged no take, on that
/// connection or a later one, hands it out again.
#[test]
fn a_message_to_an_offline_spoke_is_delivered_on_reconnect_exactly_once() {
    let dir = Scratch::new("offline");
    let data = dir.0.join("data");
    let (hub, tokens) = seed(&data);
    lease(&hub, "jesper", "wsp-1");
    // Jesper's spoke is away: nobody of his is connected while Ed sends.
    let Outcome::Ok(Answer::Queued { id, .. }) = send(&hub, "ed", tell("seat:wsp-1", "primary", "while you were out", None)) else {
        panic!("not queued")
    };
    drop(hub);

    let cfg = wsp_hub::Config {
        data_dir: data.clone(),
        bind: vec![],
        port: 0,
        peers: vec!["127.0.0.0/8".into()],
        tls: None,
        name: "wsp-hub".into(),
        lease_days: 7,
    };
    let running = wsp_hub::serve::start(&cfg).expect("the hub starts, the queue kept");
    let dial = || {
        let addrs = vec![running.addrs()[0].to_string()];
        Client::dial_at(addrs, ApiToken(tokens["jesper"].clone()), person("jesper"), WAIT).expect("the hub answers")
    };
    let mut first = dial();
    let mail = first.msg_take().unwrap();
    assert_eq!(mail.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), vec![id.clone()], "delivered on reconnect");
    // Cut off before the ack: the same message again, not lost.
    drop(first);
    let mut second = dial();
    assert_eq!(second.msg_take().unwrap().len(), 1, "unacknowledged, it is handed out again");
    assert_eq!(second.msg_ack(std::slice::from_ref(&id)).unwrap(), 1);
    assert!(second.msg_take().unwrap().is_empty(), "acknowledged, never again");
    assert_eq!(second.msg_ack(std::slice::from_ref(&id)).unwrap(), 0, "a second ack acks nothing");
    drop(second);
    let mut third = dial();
    assert!(third.msg_take().unwrap().is_empty(), "nor on a later connection");
    running.stop();
}

/// `governor:<list>` is the drive lease's holder, `primary:<user>` that user;
/// what the hub cannot resolve is `not_found`, and what the caller has no
/// workspace in common with is `forbidden`.
#[test]
fn governor_and_primary_route_by_the_drive_lease_and_the_user_and_the_rest_is_refused() {
    let dir = Scratch::new("routes");
    let (hub, _) = seed(&dir.0.join("data"));
    lease(&hub, "jesper", "drive:run");
    let routed = |by: &str, to: &str| match send(&hub, by, tell(to, "primary", "hi", None)) {
        Outcome::Ok(Answer::Queued { user, .. }) => user,
        other => panic!("{to} not queued: {other:?}"),
    };
    assert_eq!(routed("ed", "governor:run"), "jesper");
    assert_eq!(routed("ed", "primary:jesper"), "jesper");
    assert_eq!(routed("jesper", "primary:ed"), "ed");
    assert_eq!(take(&hub, "jesper").len(), 2);

    assert_eq!(code(send(&hub, "ed", tell("seat:wsp-404", "primary", "hi", None))), Code::NotFound, "no lease");
    assert_eq!(code(send(&hub, "ed", tell("governor:idle", "primary", "hi", None))), Code::NotFound, "no drive");
    assert_eq!(code(send(&hub, "ed", tell("primary:nobody", "primary", "hi", None))), Code::NotFound);
    assert_eq!(code(send(&hub, "ed", tell("list:run", "primary", "hi", None))), Code::NotFound, "not routed");
    assert_eq!(code(send(&hub, "dee", tell("governor:run", "primary", "hi", None))), Code::Forbidden, "no member");
    assert_eq!(code(send(&hub, "ed", tell("primary:dee", "primary", "hi", None))), Code::Forbidden, "no workspace shared");
    let seat = Principal::Seat(wsp_store_contract::SeatId("cpd-1".into()));
    assert_eq!(code(hub.answer(&seat, SyncCall::MsgTake(NoArgs {}))), Code::Forbidden, "only a user");
    // A `re` that names a message the caller was never sent routes nothing.
    assert_eq!(code(send(&hub, "dee", tell("seat:wsp-404", "primary", "hi", Some("m-1")))), Code::NotFound);
    assert!(take(&hub, "ed").len() == 1 && take(&hub, "dee").is_empty());
}
